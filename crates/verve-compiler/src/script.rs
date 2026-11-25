use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::CompileDiagnostic;

/// Resource limits enforced when evaluating user override scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxLimits {
    /// Maximum heap memory permitted for the script runtime in bytes.
    pub memory_limit_bytes: usize,
    /// Maximum wall-clock execution duration before interruption.
    pub timeout: Duration,
    /// Maximum number of console log lines retained from a single script run.
    pub max_log_entries: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            memory_limit_bytes: 32 * 1024 * 1024,
            timeout: Duration::from_secs(2),
            max_log_entries: 256,
        }
    }
}

/// Result returned by a [`ScriptEngine`] invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptOutput {
    /// Transformed configuration object returned by `main(config, profileName)`.
    pub config: serde_json::Value,
    /// Console output lines captured during script execution.
    pub console_logs: Vec<String>,
}

/// Extension point for sandboxed JavaScript override execution.
pub trait ScriptEngine: Send + Sync + 'static {
    /// Evaluates `script` against `config` and `profile_name` within `limits`.
    fn eval_override(
        &self,
        script: &str,
        config: serde_json::Value,
        profile_name: &str,
        limits: SandboxLimits,
    ) -> Result<ScriptOutput, CompileDiagnostic>;
}

/// Default no-op [`ScriptEngine`] used in `0.0.1` before the `0.2.0` QuickJS sandbox lands.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopScriptEngine;

impl ScriptEngine for NoopScriptEngine {
    fn eval_override(
        &self,
        _script: &str,
        config: serde_json::Value,
        _profile_name: &str,
        _limits: SandboxLimits,
    ) -> Result<ScriptOutput, CompileDiagnostic> {
        Ok(ScriptOutput {
            config,
            console_logs: Vec::new(),
        })
    }
}
