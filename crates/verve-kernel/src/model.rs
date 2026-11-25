//! Kernel-agnostic proxy domain model types and errors.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Global routing mode applied by the proxy kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// Route traffic according to the configured rule set.
    #[default]
    Rule,
    /// Route all traffic through the global selector.
    Global,
    /// Bypass proxies and connect directly.
    Direct,
}

/// Local control-plane IPC endpoint exposed by a proxy kernel process.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControlEndpoint {
    /// Windows Named Pipe path (for example `\\.\pipe\verve-mihomo-default`).
    NamedPipe(String),
    /// Unix Domain Socket filesystem path.
    UnixSocket(PathBuf),
}

impl ControlEndpoint {
    /// Returns the platform-appropriate default control endpoint for a session tag.
    pub fn default_for_session(tag: &str) -> Self {
        let sanitized: String = tag
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '-'
                }
            })
            .collect();
        let slug = if sanitized.is_empty() {
            "default"
        } else {
            sanitized.as_str()
        };

        #[cfg(windows)]
        {
            Self::NamedPipe(format!(r"\\.\pipe\verve-mihomo-{slug}"))
        }

        #[cfg(not(windows))]
        {
            Self::UnixSocket(std::env::temp_dir().join(format!("verve-mihomo-{slug}.sock")))
        }
    }

    /// Returns a string representation suitable for IPC transport arguments.
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::NamedPipe(pipe) => std::borrow::Cow::Borrowed(pipe.as_str()),
            Self::UnixSocket(path) => path.to_string_lossy(),
        }
    }
}

/// Capabilities supported by a [`crate::ProxyKernel`] implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KernelCapabilities {
    /// Whether the kernel supports TUN interface capture.
    pub supports_tun: bool,
    /// Whether the kernel exposes live traffic rate telemetry.
    pub supports_traffic_stream: bool,
    /// Whether the kernel exposes live memory usage telemetry.
    pub supports_memory_stream: bool,
    /// Whether the kernel exposes live active connection snapshots.
    pub supports_connections_stream: bool,
    /// Whether the kernel exposes live structured log streaming.
    pub supports_logs_stream: bool,
    /// Whether the kernel supports node latency testing.
    pub supports_delay_test: bool,
    /// Whether the kernel supports external proxy/rule providers.
    pub supports_providers: bool,
}

/// Complete proxy group and node topology fetched from the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProxyTopology {
    /// Ordered proxy groups exposed to the user.
    pub groups: Vec<ProxyGroup>,
    /// All known proxy nodes indexed by unique node name.
    pub nodes: BTreeMap<String, ProxyNode>,
}

/// A selectable or automatic proxy group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyGroup {
    /// Display name of the proxy group.
    pub name: String,
    /// Group selection/failover strategy.
    pub kind: ProxyGroupKind,
    /// Currently active member proxy or sub-group name, if known.
    pub now: Option<String>,
    /// Ordered list of member proxy or sub-group names.
    pub all: Vec<String>,
    /// Whether the group should be hidden from primary UI selectors.
    pub hidden: bool,
}

/// Strategy kind of a [`ProxyGroup`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyGroupKind {
    /// Manual selector group.
    Select,
    /// Lowest-latency automated selector.
    UrlTest,
    /// Ordered availability fallback selector.
    Fallback,
    /// Load-balancing group across healthy nodes.
    LoadBalance,
    /// Chained proxy relay group.
    Relay,
    /// Built-in direct outbound.
    Direct,
    /// Built-in reject/drop outbound.
    Reject,
    /// Unrecognized upstream group kind preserved verbatim.
    Unknown(String),
}

/// An individual outbound proxy node or built-in outbound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyNode {
    /// Unique node name.
    pub name: String,
    /// Normalized protocol label (for example `"vless"`, `"trojan"`, `"ss"`).
    pub protocol: String,
    /// Whether UDP relay is enabled for this node.
    pub udp: bool,
    /// Most recent measured round-trip delay in milliseconds, if available.
    pub latest_delay_ms: Option<u16>,
    /// Whether the node is currently considered reachable/alive.
    pub alive: bool,
}

/// Kind of external kernel provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    /// External proxy node subscription provider.
    Proxy,
    /// External routing rule set provider.
    Rule,
}

/// Metadata summary for an external proxy provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyProviderInfo {
    /// Unique provider name.
    pub name: String,
    /// Transport vehicle type (for example `"HTTP"`, `"File"`).
    pub vehicle_type: String,
    /// Node names currently supplied by this provider.
    pub proxies: Vec<String>,
    /// Last update timestamp string reported by the kernel, if available.
    pub updated_at: Option<String>,
}

/// Metadata summary for an external rule provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProviderInfo {
    /// Unique rule provider name.
    pub name: String,
    /// Matching behavior (for example `"domain"`, `"ipcidr"`, `"classical"`).
    pub behavior: String,
    /// Transport vehicle type (for example `"HTTP"`, `"File"`).
    pub vehicle_type: String,
    /// Number of compiled rules contained in this provider.
    pub rule_count: usize,
    /// Last update timestamp string reported by the kernel, if available.
    pub updated_at: Option<String>,
}

/// A single active routing rule entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleEntry {
    /// Rule matcher type (for example `"DOMAIN-SUFFIX"`, `"GEOIP"`, `"MATCH"`).
    pub kind: String,
    /// Rule match expression or payload.
    pub payload: String,
    /// Target proxy or group name.
    pub proxy: String,
}

/// Snapshot of active routing rules and external rule providers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuleSnapshot {
    /// Ordered routing rules currently loaded in the kernel.
    pub rules: Vec<RuleEntry>,
    /// External rule providers registered in the kernel.
    pub providers: Vec<RuleProviderInfo>,
}

/// An active or recently observed network connection tracked by the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionInfo {
    /// Unique connection identifier assigned by the kernel.
    pub id: String,
    /// Target domain host or fallback address.
    pub host: String,
    /// Resolved destination IP address, if known.
    pub destination_ip: Option<String>,
    /// Destination port number, if known.
    pub destination_port: Option<u16>,
    /// Transport network (`"tcp"` / `"udp"`).
    pub network: String,
    /// Inbound listener or connection type label.
    pub conn_type: String,
    /// Matched rule type.
    pub rule: String,
    /// Matched rule payload.
    pub rule_payload: String,
    /// Resolution chain from outbound group down to leaf node.
    pub chains: Vec<String>,
    /// Cumulative uploaded bytes for this connection.
    pub upload_bytes: u64,
    /// Cumulative downloaded bytes for this connection.
    pub download_bytes: u64,
    /// Connection start time in Unix epoch milliseconds.
    pub start_unix_ms: u64,
}

/// Point-in-time aggregate upload/download throughput sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrafficSample {
    /// Current uplink throughput in bytes per second.
    pub up_bps: u64,
    /// Current downlink throughput in bytes per second.
    pub down_bps: u64,
}

/// Point-in-time kernel memory footprint sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemorySample {
    /// Resident or in-use kernel memory in bytes.
    pub inuse_bytes: u64,
}

/// Severity level for kernel log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Verbose debugging trace.
    Debug,
    /// Informational lifecycle message.
    Info,
    /// Recoverable warning condition.
    Warning,
    /// Operational or connection error.
    Error,
}

/// Structured log event emitted by the proxy kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Severity level of the log line.
    pub level: LogLevel,
    /// Formatted log payload text.
    pub payload: String,
    /// Event timestamp in Unix epoch milliseconds.
    pub timestamp_unix_ms: u64,
}

/// Telemetry stream channels supported by [`crate::ProxyKernel::open_telemetry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TelemetryKind {
    /// Live upload/download rate samples.
    Traffic,
    /// Live kernel memory usage samples.
    Memory,
    /// Live active connection snapshots.
    Connections,
    /// Live kernel log stream.
    Logs,
}

/// Unified telemetry event yielded by [`TelemetryStream`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryEvent {
    /// Uplink/downlink rate sample.
    Traffic(TrafficSample),
    /// Memory footprint sample.
    Memory(MemorySample),
    /// Active connections snapshot with cumulative totals.
    Connections {
        /// Cumulative uploaded bytes across tracked connections.
        upload_total: u64,
        /// Cumulative downloaded bytes across tracked connections.
        download_total: u64,
        /// Active connection records.
        connections: Vec<ConnectionInfo>,
    },
    /// Kernel log entry.
    Log(LogEntry),
}

/// Boxed asynchronous stream of [`TelemetryEvent`] updates.
pub type TelemetryStream = Pin<Box<dyn Stream<Item = Result<TelemetryEvent, KernelError>> + Send>>;

/// Diagnostic reported when kernel configuration preflight validation fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    /// 1-based line number in the validated config file, if reported.
    pub line: Option<usize>,
    /// 1-based column number in the validated config file, if reported.
    pub column: Option<usize>,
    /// Human-readable validation diagnostic message.
    pub message: String,
}

/// Errors produced by kernel supervision and control-plane operations.
#[derive(Debug, Error)]
pub enum KernelError {
    /// Spawning the kernel child process failed.
    #[error("failed to spawn kernel process: {0}")]
    ProcessSpawn(#[source] std::io::Error),
    /// A supervised kernel process is already active.
    #[error("kernel process is already running (pid {pid})")]
    AlreadyRunning {
        /// Process ID of the already-running kernel.
        pid: u32,
    },
    /// Operation requires an active kernel process, but none is running.
    #[error("kernel process is not running")]
    NotRunning,
    /// Privileged `verve-service` supervision backend is unavailable in this build.
    #[error("privileged service supervisor is not available")]
    ServiceUnavailable,
    /// Control-plane endpoint configuration or connection error.
    #[error("kernel control endpoint error: {0}")]
    ControlEndpoint(String),
    /// Kernel control-plane request or response failed protocol semantics.
    #[error("kernel protocol error: {0}")]
    Protocol(String),
    /// Operation exceeded the configured timeout.
    #[error("kernel operation timed out")]
    Timeout,
    /// Kernel configuration preflight check failed with diagnostics.
    #[error("kernel configuration validation failed with {} diagnostic(s)", .0.len())]
    ValidationFailed(Vec<ConfigDiagnostic>),
    /// Generic I/O error.
    #[error("kernel I/O error: {0}")]
    Io(#[from] std::io::Error),
}
