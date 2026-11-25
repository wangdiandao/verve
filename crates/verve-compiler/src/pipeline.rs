use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use verve_config::atomic_write_bytes;
use verve_kernel::ConfigDiagnostic;

use crate::merge::parse_yaml_object;
use crate::stages::{
    GlobalMergeStage, GlobalScriptStage, ProfileMergeStage, ProfileScriptStage, QuickRulesStage,
    RuntimeInjectionStage,
};
use crate::{
    CommittedConfig, CompileContext, CompileDiagnostic, CompiledConfig, CompilerStage,
    NoopScriptEngine, SandboxLimits, ScriptEngine,
};

/// Ordered, immutable 6-stage configuration compilation pipeline.
pub struct CompilerPipeline {
    stages: Vec<Box<dyn CompilerStage>>,
}

impl Default for CompilerPipeline {
    fn default() -> Self {
        Self::new(Arc::new(NoopScriptEngine))
    }
}

impl CompilerPipeline {
    /// Builds the standard 6-stage pipeline using `script_engine` and default [`SandboxLimits`].
    pub fn new(script_engine: Arc<dyn ScriptEngine>) -> Self {
        Self::with_limits(script_engine, SandboxLimits::default())
    }

    /// Builds the standard 6-stage pipeline using `script_engine` and custom [`SandboxLimits`].
    pub fn with_limits(script_engine: Arc<dyn ScriptEngine>, limits: SandboxLimits) -> Self {
        Self {
            stages: vec![
                Box::new(ProfileMergeStage),
                Box::new(ProfileScriptStage::new(Arc::clone(&script_engine), limits)),
                Box::new(GlobalMergeStage),
                Box::new(GlobalScriptStage::new(script_engine, limits)),
                Box::new(QuickRulesStage),
                Box::new(RuntimeInjectionStage),
            ],
        }
    }

    /// Builds a pipeline from an explicit sequence of [`CompilerStage`] implementations.
    pub fn with_stages(stages: Vec<Box<dyn CompilerStage>>) -> Self {
        Self { stages }
    }

    /// Compiles `raw_profile_yaml` with `ctx` purely in memory into final YAML.
    pub fn compile_to_yaml(
        &self,
        raw_profile_yaml: &str,
        ctx: &CompileContext,
    ) -> Result<CompiledConfig, CompileDiagnostic> {
        let root_map = parse_yaml_object(raw_profile_yaml, "parse", false)?.ok_or_else(|| {
            CompileDiagnostic {
                stage: "parse".into(),
                line: None,
                column: None,
                message: "profile YAML must not be empty".into(),
            }
        })?;

        let mut doc = Value::Object(root_map);
        let mut logs = Vec::new();

        for stage in &self.stages {
            let stage_logs = stage.apply(ctx, &mut doc)?;
            logs.extend(stage_logs);
        }

        let yaml = serde_yaml_ng::to_string(&doc).map_err(|err| CompileDiagnostic {
            stage: "serialize".into(),
            line: None,
            column: None,
            message: format!("failed to render compiled YAML: {err}"),
        })?;

        Ok(CompiledConfig {
            yaml,
            document: doc,
            logs,
        })
    }

    /// Compiles `raw_profile_yaml`, writes a synced candidate file inside `runtime_dir`,
    /// executes `preflight`, and atomically commits `active.yaml` (backing up any prior
    /// `active.yaml` to `last_known_good.yaml`) or rolls back cleanly on failure.
    pub async fn compile_and_commit<F, Fut>(
        &self,
        raw_profile_yaml: &str,
        ctx: &CompileContext,
        runtime_dir: &Path,
        preflight: F,
    ) -> Result<CommittedConfig, CompileDiagnostic>
    where
        F: FnOnce(&Path) -> Fut,
        Fut: Future<Output = Result<(), Vec<ConfigDiagnostic>>>,
    {
        let compiled = self.compile_to_yaml(raw_profile_yaml, ctx)?;

        std::fs::create_dir_all(runtime_dir).map_err(|err| CompileDiagnostic {
            stage: "preflight".into(),
            line: None,
            column: None,
            message: format!("failed to prepare runtime directory: {err}"),
        })?;

        let mut candidate =
            tempfile::NamedTempFile::new_in(runtime_dir).map_err(|err| CompileDiagnostic {
                stage: "preflight".into(),
                line: None,
                column: None,
                message: format!("failed to create candidate runtime file: {err}"),
            })?;
        candidate
            .write_all(compiled.yaml.as_bytes())
            .map_err(|err| CompileDiagnostic {
                stage: "preflight".into(),
                line: None,
                column: None,
                message: format!("failed to write candidate runtime file: {err}"),
            })?;
        candidate
            .as_file()
            .sync_all()
            .map_err(|err| CompileDiagnostic {
                stage: "preflight".into(),
                line: None,
                column: None,
                message: format!("failed to flush candidate runtime file: {err}"),
            })?;

        // Close the file handle before passing the path to an external preflight checker
        // (important on Windows), while preserving RAII auto-cleanup on Drop.
        let temp_path = candidate.into_temp_path();

        if let Err(diags) = preflight(&temp_path).await {
            drop(temp_path);
            return Err(preflight_diagnostics_to_compile_diag(diags));
        }

        let active_path = runtime_dir.join("active.yaml");
        let backup_path = runtime_dir.join("last_known_good.yaml");

        let backed_up = if active_path.exists() {
            let existing = std::fs::read(&active_path).map_err(|err| CompileDiagnostic {
                stage: "commit".into(),
                line: None,
                column: None,
                message: format!("failed to read previous active.yaml for backup: {err}"),
            })?;
            atomic_write_bytes(&backup_path, &existing).map_err(|err| CompileDiagnostic {
                stage: "commit".into(),
                line: None,
                column: None,
                message: format!("failed to write last_known_good.yaml: {err}"),
            })?;
            true
        } else {
            false
        };

        temp_path
            .persist(&active_path)
            .map_err(|err| CompileDiagnostic {
                stage: "commit".into(),
                line: None,
                column: None,
                message: format!("failed to atomically activate active.yaml: {}", err.error),
            })?;

        Ok(CommittedConfig {
            active_path,
            backup_path: backed_up.then_some(backup_path),
            yaml: compiled.yaml,
            logs: compiled.logs,
        })
    }
}

fn preflight_diagnostics_to_compile_diag(diags: Vec<ConfigDiagnostic>) -> CompileDiagnostic {
    if diags.is_empty() {
        return CompileDiagnostic {
            stage: "preflight".into(),
            line: None,
            column: None,
            message: "kernel preflight validation failed".into(),
        };
    }

    let first_line = diags[0].line;
    let first_column = diags[0].column;
    let message = diags
        .into_iter()
        .map(|diag| diag.message)
        .collect::<Vec<_>>()
        .join("; ");

    CompileDiagnostic {
        stage: "preflight".into(),
        line: first_line,
        column: first_column,
        message,
    }
}
