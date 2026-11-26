use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use verve_kernel::{ControlEndpoint, LogLevel, ProxyMode};

/// Structured diagnostic emitted when YAML parsing, merging, scripting,
/// runtime injection, or kernel preflight fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileDiagnostic {
    /// Pipeline stage where the failure occurred.
    pub stage: String,
    /// 1-based line number in the offending YAML/script source, if available.
    pub line: Option<usize>,
    /// 1-based column number in the offending YAML/script source, if available.
    pub column: Option<usize>,
    /// Human-readable error message.
    pub message: String,
}

impl fmt::Display for CompileDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => {
                write!(
                    f,
                    "[{}] line {}, column {}: {}",
                    self.stage, line, column, self.message
                )
            }
            (Some(line), None) => write!(f, "[{}] line {}: {}", self.stage, line, self.message),
            _ => write!(f, "[{}] {}", self.stage, self.message),
        }
    }
}

impl std::error::Error for CompileDiagnostic {}

/// Structured informational log emitted by a [`crate::CompilerStage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageLog {
    /// Static name of the emitting stage.
    pub stage: &'static str,
    /// Log message content.
    pub message: String,
}

/// Authoritative TUN interface settings injected by [`crate::RuntimeInjectionStage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TunInjectionConfig {
    /// Network stack (`"system"`, `"gvisor"`, or `"mixed"`).
    pub stack: String,
    /// Whether automatic route configuration is enabled (`auto-route`).
    pub auto_route: bool,
    /// Whether strict route enforcement is enabled (`strict-route`).
    pub strict_route: bool,
    /// DNS hijack patterns (`dns-hijack`).
    pub dns_hijack: Vec<String>,
    /// Virtual interface MTU (`mtu`).
    pub mtu: u16,
    /// Excluded CIDR address ranges (`route-exclude-address`).
    pub route_exclude_address: Vec<String>,
    /// Optional custom TUN device name (`device`).
    pub interface_name: Option<String>,
}

impl Default for TunInjectionConfig {
    fn default() -> Self {
        let defaults = verve_config::TunConfig::default();
        Self {
            stack: defaults.stack,
            auto_route: defaults.auto_route,
            strict_route: defaults.strict_route,
            dns_hijack: defaults.dns_hijack,
            mtu: defaults.mtu,
            route_exclude_address: defaults.route_exclude_address,
            interface_name: None,
        }
    }
}

/// Runtime network and control-plane settings injected by [`crate::RuntimeInjectionStage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInjectionConfig {
    /// Combined HTTP/SOCKS5 inbound port (`mixed-port`).
    pub mixed_port: u16,
    /// Whether LAN clients are permitted (`allow-lan`).
    pub allow_lan: bool,
    /// Whether IPv6 is enabled (`ipv6`).
    pub ipv6: bool,
    /// Global proxy routing mode (`mode`).
    pub mode: ProxyMode,
    /// Kernel log verbosity (`log-level`).
    pub log_level: LogLevel,
    /// Local control-plane IPC endpoint (`external-controller-pipe` or `external-controller-unix`).
    pub control_endpoint: ControlEndpoint,
    /// Whether TUN capture is enabled.
    pub tun_enabled: bool,
    /// Detailed TUN settings injected when `tun_enabled` is `true`.
    #[serde(default)]
    pub tun: TunInjectionConfig,
}

/// Immutable inputs consumed across all stages of [`crate::CompilerPipeline`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileContext {
    /// Display name of the profile being compiled (passed to `ScriptEngine`).
    pub profile_name: String,
    /// Optional per-profile declarative Merge YAML.
    pub profile_merge_yaml: Option<String>,
    /// Optional per-profile JS override script.
    pub profile_script: Option<String>,
    /// Optional global declarative Merge YAML.
    pub global_merge_yaml: Option<String>,
    /// Optional global JS override script.
    pub global_script: Option<String>,
    /// Quick rules prepended ahead of profile/merged rules.
    pub quick_rules: Vec<String>,
    /// Runtime control-plane and listener overrides.
    pub runtime: RuntimeInjectionConfig,
}

/// Output of an in-memory [`crate::CompilerPipeline::compile_to_yaml`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledConfig {
    /// Final rendered YAML ready for kernel validation and execution.
    pub yaml: String,
    /// Final compiled JSON/YAML AST object.
    pub document: serde_json::Value,
    /// Logs emitted across all executed stages.
    pub logs: Vec<StageLog>,
}

/// Output of a preflight-verified atomic [`crate::CompilerPipeline::compile_and_commit`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedConfig {
    /// Path to the newly committed `active.yaml`.
    pub active_path: PathBuf,
    /// Path to `last_known_good.yaml` if a previous `active.yaml` was backed up.
    pub backup_path: Option<PathBuf>,
    /// Final rendered YAML written to `active.yaml`.
    pub yaml: String,
    /// Logs emitted across all executed stages.
    pub logs: Vec<StageLog>,
}
