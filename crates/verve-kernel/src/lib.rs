//! Kernel-agnostic proxy domain model, [`ProxyKernel`] abstraction, and [`KernelSupervisor`].

mod model;
mod supervisor;

use std::future::Future;
use std::path::Path;
use std::time::Duration;

pub use model::{
    ConfigDiagnostic, ConnectionInfo, ControlEndpoint, KernelCapabilities, KernelError, LogEntry,
    LogLevel, MemorySample, ProviderKind, ProxyGroup, ProxyGroupKind, ProxyMode, ProxyNode,
    ProxyProviderInfo, ProxyTopology, RuleEntry, RuleProviderInfo, RuleSnapshot, TelemetryEvent,
    TelemetryKind, TelemetryStream, TrafficSample,
};
pub use supervisor::{
    DaemonIpcSupervisor, DirectProcessSupervisor, KernelSupervisor, RemoteKernelStatus,
    ServiceIpcClient,
};

/// Abstract control-plane contract implemented by concrete proxy kernel adapters.
pub trait ProxyKernel: Send + Sync + 'static {
    /// Returns the static capability flags supported by this kernel adapter.
    fn capabilities(&self) -> KernelCapabilities;

    /// Runs kernel-level configuration preflight validation on `path`.
    fn validate_config(
        &self,
        path: &Path,
    ) -> impl Future<Output = Result<(), Vec<ConfigDiagnostic>>> + Send;

    /// Hot-reloads the running kernel configuration from `path`.
    fn reload_config(&self, path: &Path) -> impl Future<Output = Result<(), KernelError>> + Send;

    /// Fetches the current proxy group and outbound node topology.
    fn fetch_proxies(&self) -> impl Future<Output = Result<ProxyTopology, KernelError>> + Send;

    /// Selects outbound `name` inside selector group `group`.
    fn select_proxy(
        &self,
        group: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), KernelError>> + Send;

    /// Measures round-trip latency (in milliseconds) of proxy `name` against `url`.
    fn test_delay(
        &self,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<u16, KernelError>> + Send;

    /// Fetches active routing rules and external rule provider summaries.
    fn fetch_rules(&self) -> impl Future<Output = Result<RuleSnapshot, KernelError>> + Send;

    /// Triggers an update of an external proxy or rule provider.
    fn update_provider(
        &self,
        kind: ProviderKind,
        name: &str,
    ) -> impl Future<Output = Result<(), KernelError>> + Send;

    /// Closes a specific connection by `id`, or all active connections when `None`.
    fn close_connection(
        &self,
        id: Option<&str>,
    ) -> impl Future<Output = Result<(), KernelError>> + Send;

    /// Opens a live telemetry stream for the requested [`TelemetryKind`].
    fn open_telemetry(
        &self,
        kind: TelemetryKind,
    ) -> impl Future<Output = Result<TelemetryStream, KernelError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn domain_types_serialize_and_round_trip() {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "tokyo-01".into(),
            ProxyNode {
                name: "tokyo-01".into(),
                protocol: "vless".into(),
                udp: true,
                latest_delay_ms: Some(42),
                alive: true,
            },
        );

        let topology = ProxyTopology {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("tokyo-01".into()),
                all: vec!["tokyo-01".into(), "DIRECT".into()],
                hidden: false,
            }],
            nodes,
        };

        let json = serde_json::to_string(&topology).expect("topology serialization must succeed");
        let decoded: ProxyTopology =
            serde_json::from_str(&json).expect("topology deserialization must succeed");
        assert_eq!(decoded, topology);

        let event = TelemetryEvent::Connections {
            upload_total: 1024,
            download_total: 4096,
            connections: vec![ConnectionInfo {
                id: "conn-1".into(),
                host: "example.com".into(),
                destination_ip: Some("93.184.216.34".into()),
                destination_port: Some(443),
                network: "tcp".into(),
                conn_type: "HTTP".into(),
                rule: "DOMAIN-SUFFIX".into(),
                rule_payload: "example.com".into(),
                chains: vec!["Proxy".into(), "tokyo-01".into()],
                upload_bytes: 512,
                download_bytes: 2048,
                start_unix_ms: 1_700_000_000_000,
            }],
        };

        let event_json = serde_json::to_string(&event).expect("event serialization must succeed");
        let decoded_event: TelemetryEvent =
            serde_json::from_str(&event_json).expect("event deserialization must succeed");
        assert_eq!(decoded_event, event);
    }

    #[test]
    fn control_endpoint_default_sanitizes_session_tag() {
        let endpoint = ControlEndpoint::default_for_session("dev session/1");
        let rendered = endpoint.as_str_lossy();
        assert!(rendered.contains("verve-mihomo-dev-session-1"));

        #[cfg(windows)]
        assert!(matches!(endpoint, ControlEndpoint::NamedPipe(_)));

        #[cfg(not(windows))]
        assert!(matches!(endpoint, ControlEndpoint::UnixSocket(_)));
    }
}
