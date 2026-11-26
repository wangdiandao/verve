use std::sync::Arc;

use serde_json::{Map, Value};
use verve_kernel::{ControlEndpoint, LogLevel, ProxyMode};

use crate::merge::{apply_merge_yaml, prepend_list_items};
use crate::{CompileContext, CompileDiagnostic, SandboxLimits, ScriptEngine, StageLog};

/// Pluggable immutable transformation stage inside [`crate::CompilerPipeline`].
pub trait CompilerStage: Send + Sync {
    /// Static identifier for this stage used in logs and diagnostics.
    fn name(&self) -> &'static str;

    /// Applies this stage's transformation to `doc` using `ctx`.
    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic>;
}

/// Stage 1: applies per-profile declarative Merge YAML (`ctx.profile_merge_yaml`).
#[derive(Debug, Default)]
pub struct ProfileMergeStage;

impl CompilerStage for ProfileMergeStage {
    fn name(&self) -> &'static str {
        "profile-merge"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        let Some(merge_yaml) = ctx.profile_merge_yaml.as_deref() else {
            return Ok(Vec::new());
        };
        if apply_merge_yaml(doc, merge_yaml, self.name())? {
            Ok(vec![StageLog {
                stage: self.name(),
                message: "applied profile merge overlay".into(),
            }])
        } else {
            Ok(Vec::new())
        }
    }
}

/// Stage 2: executes per-profile override script (`ctx.profile_script`) via [`ScriptEngine`].
pub struct ProfileScriptStage {
    engine: Arc<dyn ScriptEngine>,
    limits: SandboxLimits,
}

impl ProfileScriptStage {
    /// Creates a profile script stage backed by `engine` and `limits`.
    pub fn new(engine: Arc<dyn ScriptEngine>, limits: SandboxLimits) -> Self {
        Self { engine, limits }
    }
}

impl CompilerStage for ProfileScriptStage {
    fn name(&self) -> &'static str {
        "profile-script"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        run_script_stage(
            self.name(),
            ctx.profile_script.as_deref(),
            &ctx.profile_name,
            &*self.engine,
            self.limits,
            doc,
        )
    }
}

/// Stage 3: applies global declarative Merge YAML (`ctx.global_merge_yaml`).
#[derive(Debug, Default)]
pub struct GlobalMergeStage;

impl CompilerStage for GlobalMergeStage {
    fn name(&self) -> &'static str {
        "global-merge"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        let Some(merge_yaml) = ctx.global_merge_yaml.as_deref() else {
            return Ok(Vec::new());
        };
        if apply_merge_yaml(doc, merge_yaml, self.name())? {
            Ok(vec![StageLog {
                stage: self.name(),
                message: "applied global merge overlay".into(),
            }])
        } else {
            Ok(Vec::new())
        }
    }
}

/// Stage 4: executes global override script (`ctx.global_script`) via [`ScriptEngine`].
pub struct GlobalScriptStage {
    engine: Arc<dyn ScriptEngine>,
    limits: SandboxLimits,
}

impl GlobalScriptStage {
    /// Creates a global script stage backed by `engine` and `limits`.
    pub fn new(engine: Arc<dyn ScriptEngine>, limits: SandboxLimits) -> Self {
        Self { engine, limits }
    }
}

impl CompilerStage for GlobalScriptStage {
    fn name(&self) -> &'static str {
        "global-script"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        run_script_stage(
            self.name(),
            ctx.global_script.as_deref(),
            &ctx.profile_name,
            &*self.engine,
            self.limits,
            doc,
        )
    }
}

/// Stage 5: prepends declarative quick rules (`ctx.quick_rules`) ahead of `doc["rules"]`.
#[derive(Debug, Default)]
pub struct QuickRulesStage;

impl CompilerStage for QuickRulesStage {
    fn name(&self) -> &'static str {
        "quick-rules"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        if ctx.quick_rules.is_empty() {
            return Ok(Vec::new());
        }

        let root_map = doc.as_object_mut().ok_or_else(|| CompileDiagnostic {
            stage: self.name().into(),
            line: None,
            column: None,
            message: "compiled document root must remain a mapping".into(),
        })?;

        let items: Vec<Value> = ctx
            .quick_rules
            .iter()
            .map(|rule| Value::String(rule.clone()))
            .collect();
        let count = items.len();
        prepend_list_items(root_map, "rules", items, self.name())?;

        Ok(vec![StageLog {
            stage: self.name(),
            message: format!("prepended {count} quick rule(s)"),
        }])
    }
}

/// Stage 6: injects authoritative runtime listeners, routing mode, log level,
/// local Named Pipe / UDS control endpoint, and TUN toggle.
#[derive(Debug, Default)]
pub struct RuntimeInjectionStage;

impl CompilerStage for RuntimeInjectionStage {
    fn name(&self) -> &'static str {
        "runtime-injection"
    }

    fn apply(
        &self,
        ctx: &CompileContext,
        doc: &mut Value,
    ) -> Result<Vec<StageLog>, CompileDiagnostic> {
        let root_map = doc.as_object_mut().ok_or_else(|| CompileDiagnostic {
            stage: self.name().into(),
            line: None,
            column: None,
            message: "compiled document root must remain a mapping".into(),
        })?;

        let runtime = &ctx.runtime;
        root_map.insert(
            "mixed-port".into(),
            Value::Number(runtime.mixed_port.into()),
        );
        root_map.insert("allow-lan".into(), Value::Bool(runtime.allow_lan));
        root_map.insert("ipv6".into(), Value::Bool(runtime.ipv6));
        root_map.insert(
            "mode".into(),
            Value::String(proxy_mode_str(runtime.mode).into()),
        );
        root_map.insert(
            "log-level".into(),
            Value::String(log_level_str(runtime.log_level).into()),
        );

        // TCP external controller is disabled unless explicitly added in a future opt-in stage.
        root_map.remove("external-controller");

        match &runtime.control_endpoint {
            ControlEndpoint::NamedPipe(pipe) => {
                root_map.remove("external-controller-unix");
                root_map.insert(
                    "external-controller-pipe".into(),
                    Value::String(pipe.clone()),
                );
            }
            ControlEndpoint::UnixSocket(path) => {
                root_map.remove("external-controller-pipe");
                root_map.insert(
                    "external-controller-unix".into(),
                    Value::String(path.to_string_lossy().into_owned()),
                );
            }
        }

        if runtime.tun_enabled {
            let tun_map = match root_map.get_mut("tun") {
                Some(Value::Object(tun_map)) => tun_map,
                None | Some(Value::Null) => {
                    root_map.insert("tun".into(), Value::Object(Map::new()));
                    match root_map.get_mut("tun") {
                        Some(Value::Object(tun_map)) => tun_map,
                        _ => unreachable!("inserted tun object"),
                    }
                }
                Some(_) => {
                    return Err(CompileDiagnostic {
                        stage: self.name().into(),
                        line: None,
                        column: None,
                        message: "`tun` field must be a YAML mapping".into(),
                    });
                }
            };

            tun_map.insert("enable".into(), Value::Bool(true));
            tun_map.insert("stack".into(), Value::String(runtime.tun.stack.clone()));
            tun_map.insert("auto-route".into(), Value::Bool(runtime.tun.auto_route));
            tun_map.insert("strict-route".into(), Value::Bool(runtime.tun.strict_route));
            tun_map.insert("mtu".into(), Value::Number(runtime.tun.mtu.into()));

            if !runtime.tun.dns_hijack.is_empty() {
                tun_map.insert(
                    "dns-hijack".into(),
                    Value::Array(
                        runtime
                            .tun
                            .dns_hijack
                            .iter()
                            .map(|entry| Value::String(entry.clone()))
                            .collect(),
                    ),
                );
            }

            if !runtime.tun.route_exclude_address.is_empty() {
                tun_map.insert(
                    "route-exclude-address".into(),
                    Value::Array(
                        runtime
                            .tun
                            .route_exclude_address
                            .iter()
                            .map(|entry| Value::String(entry.clone()))
                            .collect(),
                    ),
                );
            }

            if let Some(ref name) = runtime.tun.interface_name {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    tun_map.insert("device".into(), Value::String(trimmed.to_owned()));
                }
            }
        } else if let Some(Value::Object(tun_map)) = root_map.get_mut("tun") {
            tun_map.insert("enable".into(), Value::Bool(false));
        }

        Ok(vec![StageLog {
            stage: self.name(),
            message: format!(
                "injected runtime config (mode={}, mixed-port={}, tun={})",
                proxy_mode_str(runtime.mode),
                runtime.mixed_port,
                runtime.tun_enabled
            ),
        }])
    }
}

fn run_script_stage(
    stage_name: &'static str,
    script: Option<&str>,
    profile_name: &str,
    engine: &dyn ScriptEngine,
    limits: SandboxLimits,
    doc: &mut Value,
) -> Result<Vec<StageLog>, CompileDiagnostic> {
    let Some(script) = script else {
        return Ok(Vec::new());
    };
    if script.trim().is_empty() {
        return Ok(Vec::new());
    }

    let input = std::mem::replace(doc, Value::Null);
    let output = engine
        .eval_override(script, input, profile_name, limits)
        .map_err(|mut diag| {
            if diag.stage.is_empty() {
                diag.stage = stage_name.to_owned();
            }
            diag
        })?;

    if !output.config.is_object() {
        return Err(CompileDiagnostic {
            stage: stage_name.to_owned(),
            line: None,
            column: None,
            message: "script override must return a configuration mapping".into(),
        });
    }

    *doc = output.config;
    Ok(output
        .console_logs
        .into_iter()
        .map(|message| StageLog {
            stage: stage_name,
            message,
        })
        .collect())
}

fn proxy_mode_str(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Rule => "rule",
        ProxyMode::Global => "global",
        ProxyMode::Direct => "direct",
    }
}

fn log_level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warning => "warning",
        LogLevel::Error => "error",
    }
}
