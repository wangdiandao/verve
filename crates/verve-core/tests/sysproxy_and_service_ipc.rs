//! Headless integration tests for system proxy crash self-healing, TUN/theme commands,
//! and `ProtocolServiceIpcClient` framing/error mapping.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use verve_compiler::CompilerPipeline;
use verve_config::ConfigManager;
use verve_core::{AppCoordinator, CoordinatorConfig, CoreError, ProtocolServiceIpcClient};
use verve_kernel::{
    ConfigDiagnostic, ControlEndpoint, DaemonIpcSupervisor, DirectProcessSupervisor,
    KernelCapabilities, KernelError, KernelSupervisor, ProviderKind, ProxyGroup, ProxyGroupKind,
    ProxyKernel, ProxyNode, ProxyTopology, RuleSnapshot, ServiceIpcClient, TelemetryKind,
    TelemetryStream,
};
use verve_protocol::{
    IpcRequest, IpcResponse, KernelServiceStatus, MAX_FRAME_LEN, PROTOCOL_VERSION,
    ProtocolErrorCode, ServiceCapabilities, decode_frame, encode_frame,
};
use verve_sys::{InMemorySystemProxyBackend, SystemProxySnapshot};

#[derive(Clone, Default)]
struct SimpleMockKernel;

impl ProxyKernel for SimpleMockKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities::default()
    }

    async fn validate_config(&self, _path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        Ok(())
    }

    async fn reload_config(&self, _path: &Path) -> Result<(), KernelError> {
        Ok(())
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "DIRECT".into(),
            ProxyNode {
                name: "DIRECT".into(),
                protocol: "direct".into(),
                udp: true,
                latest_delay_ms: Some(10),
                alive: true,
            },
        );
        Ok(ProxyTopology {
            groups: vec![ProxyGroup {
                name: "Proxy".into(),
                kind: ProxyGroupKind::Select,
                now: Some("DIRECT".into()),
                all: vec!["DIRECT".into()],
                hidden: false,
            }],
            nodes,
        })
    }

    async fn select_proxy(&self, _group: &str, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn test_delay(
        &self,
        _name: &str,
        _url: &str,
        _timeout: Duration,
    ) -> Result<u16, KernelError> {
        Ok(10)
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        Ok(RuleSnapshot::default())
    }

    async fn update_provider(&self, _kind: ProviderKind, _name: &str) -> Result<(), KernelError> {
        Ok(())
    }

    async fn close_connection(&self, _id: Option<&str>) -> Result<(), KernelError> {
        Ok(())
    }

    async fn open_telemetry(&self, _kind: TelemetryKind) -> Result<TelemetryStream, KernelError> {
        Ok(Box::pin(futures_util::stream::pending()))
    }
}

#[tokio::test]
async fn system_proxy_takeover_heals_after_simulated_crash_and_disables_on_shutdown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = SystemProxySnapshot {
        enable: true,
        host: "corp-proxy.internal".into(),
        port: 8080,
        bypass: "*.internal".into(),
    };
    let backend = InMemorySystemProxyBackend::new(original.clone());
    let shared_backend: Arc<dyn verve_sys::SystemProxyBackend> = Arc::new(backend.clone());

    let cfg_mgr_1 = ConfigManager::open(dir.path()).expect("config manager 1");
    let runtime_dir = cfg_mgr_1.runtime_dir();
    let handle_1 = AppCoordinator::spawn(
        cfg_mgr_1,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::new(SimpleMockKernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("sysproxy-heal-1"),
            system_proxy_backend: Some(Arc::clone(&shared_backend)),
            ..Default::default()
        },
    )
    .expect("spawn coordinator 1");

    // Keep a bus clone alive so dropping `handle_1` does NOT trigger graceful channel-close shutdown,
    // accurately simulating an unclean process crash that leaves `sysproxy_takeover.json` behind.
    let bus_keepalive = handle_1.bus().clone();
    bus_keepalive
        .set_system_proxy(true)
        .await
        .expect("set_system_proxy(true) must succeed");

    let active = backend.snapshot();
    assert!(active.enable);
    assert_eq!(active.host, "127.0.0.1");
    assert_eq!(active.port, 7897);
    assert!(handle_1.snapshot().runtime.system_proxy_enabled);
    assert!(handle_1.snapshot().settings.syncable.system_proxy_enabled);
    assert!(runtime_dir.join("sysproxy_takeover.json").exists());

    drop(handle_1);

    // Next startup must automatically invoke `heal_if_crashed()` and restore the original proxy.
    let cfg_mgr_2 = ConfigManager::open(dir.path()).expect("config manager 2");
    let mut handle_2 = AppCoordinator::spawn(
        cfg_mgr_2,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::new(SimpleMockKernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("sysproxy-heal-2"),
            system_proxy_backend: Some(Arc::clone(&shared_backend)),
            ..Default::default()
        },
    )
    .expect("spawn coordinator 2");

    assert_eq!(backend.snapshot(), original);
    assert!(!runtime_dir.join("sysproxy_takeover.json").exists());
    assert!(!handle_2.snapshot().runtime.system_proxy_enabled);
    assert!(!handle_2.snapshot().settings.syncable.system_proxy_enabled);

    // Cleanly stop coordinator 1 before testing clean shutdown on coordinator 2.
    bus_keepalive
        .shutdown()
        .await
        .expect("clean up coordinator 1");

    // Enabling and cleanly shutting down must also restore original proxy settings.
    handle_2
        .bus()
        .set_system_proxy(true)
        .await
        .expect("re-enable system proxy");
    assert_eq!(backend.snapshot().host, "127.0.0.1");

    handle_2.shutdown().await.expect("clean shutdown");
    assert_eq!(backend.snapshot(), original);
    assert!(!runtime_dir.join("sysproxy_takeover.json").exists());
}

#[tokio::test]
async fn tun_and_theme_commands_persist_compile_and_validate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_mgr = ConfigManager::open(dir.path()).expect("config manager");
    let runtime_dir = cfg_mgr.runtime_dir();
    let backend = Arc::new(InMemorySystemProxyBackend::default());

    let mut handle = AppCoordinator::spawn(
        cfg_mgr,
        CompilerPipeline::default(),
        KernelSupervisor::Direct(DirectProcessSupervisor::new()),
        Arc::new(SimpleMockKernel),
        CoordinatorConfig {
            control_endpoint: ControlEndpoint::default_for_session("tun-theme"),
            system_proxy_backend: Some(backend),
            ..Default::default()
        },
    )
    .expect("spawn coordinator");

    handle
        .bus()
        .import_local_profile(
            "default",
            "Default Profile",
            "mixed-port: 7890\nrules:\n  - MATCH,DIRECT\n",
            None,
        )
        .await
        .expect("import profile");
    handle
        .bus()
        .activate_profile("default")
        .await
        .expect("activate profile");

    handle
        .bus()
        .set_tun_enabled(true)
        .await
        .expect("enable TUN");
    assert!(handle.snapshot().runtime.tun_enabled);
    assert!(handle.snapshot().settings.syncable.tun_enabled);
    let active_yaml = std::fs::read_to_string(runtime_dir.join("active.yaml")).unwrap();
    assert!(active_yaml.contains("tun:"));
    assert!(active_yaml.contains("enable: true"));

    handle
        .bus()
        .set_theme_mode("Dark")
        .await
        .expect("set dark theme");
    assert_eq!(handle.snapshot().settings.syncable.theme_mode, "dark");

    let err = handle
        .bus()
        .set_theme_mode("sepia")
        .await
        .expect_err("invalid theme mode must fail");
    assert!(matches!(err, CoreError::Config(_)));

    assert_eq!(handle.snapshot().settings.syncable.locale, "zh-CN");
    handle
        .bus()
        .set_locale("en_us")
        .await
        .expect("set en-US locale");
    assert_eq!(handle.snapshot().settings.syncable.locale, "en-US");

    handle
        .bus()
        .set_locale("zh")
        .await
        .expect("set zh-CN locale");
    assert_eq!(handle.snapshot().settings.syncable.locale, "zh-CN");

    let locale_err = handle
        .bus()
        .set_locale("fr-FR")
        .await
        .expect_err("unsupported locale must fail");
    assert!(matches!(locale_err, CoreError::Config(_)));

    handle.shutdown().await.expect("shutdown");
}

async fn read_req<R: AsyncRead + Unpin>(reader: &mut R) -> Option<IpcRequest> {
    let mut hdr = [0u8; 4];
    if reader.read_exact(&mut hdr).await.is_err() {
        return None;
    }
    let len = u32::from_le_bytes(hdr) as usize;
    assert!(len <= MAX_FRAME_LEN);
    let mut frame = vec![0u8; 4 + len];
    frame[..4].copy_from_slice(&hdr);
    reader.read_exact(&mut frame[4..]).await.unwrap();
    Some(decode_frame(&frame).unwrap())
}

async fn write_resp<W: AsyncWrite + Unpin>(writer: &mut W, resp: &IpcResponse) {
    let bytes = encode_frame(resp).unwrap();
    writer.write_all(&bytes).await.unwrap();
    writer.flush().await.unwrap();
}

async fn handle_mock_ipc_conn<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) {
    while let Some(req) = read_req(&mut stream).await {
        let resp = match req {
            IpcRequest::Handshake { client_proto_ver } => IpcResponse::HandshakeAck {
                server_proto_ver: client_proto_ver.min(PROTOCOL_VERSION),
                capabilities: ServiceCapabilities {
                    supports_tun: true,
                    supports_restart: true,
                    supports_stderr_tail: true,
                },
                running_pid: None,
            },
            IpcRequest::StartKernel {
                bin_path,
                ctrl_endpoint,
                ..
            } => {
                if bin_path.contains("untrusted") {
                    IpcResponse::Error {
                        code: ProtocolErrorCode::UntrustedBinary,
                        message: "binary outside trusted dir".into(),
                    }
                } else {
                    IpcResponse::KernelStarted {
                        pid: 4242,
                        ctrl_endpoint,
                    }
                }
            }
            IpcRequest::QueryStatus => IpcResponse::Status(KernelServiceStatus {
                running: true,
                pid: Some(4242),
                kernel_id: Some("mihomo".into()),
                ctrl_endpoint: Some("pipe://ctrl".into()),
            }),
            IpcRequest::ReadStderrTail { max_lines } => IpcResponse::StderrTail {
                lines: (0..max_lines.min(2))
                    .map(|idx| format!("stderr-{idx}"))
                    .collect(),
            },
            IpcRequest::StopKernel => IpcResponse::Error {
                code: ProtocolErrorCode::NotRunning,
                message: "already stopped".into(),
            },
            IpcRequest::RestartKernel { .. } => IpcResponse::Stopped,
        };
        write_resp(&mut stream, &resp).await;
    }
}

#[tokio::test]
async fn protocol_service_ipc_client_round_trip_and_error_mapping() {
    let unique = format!(
        "verve-core-ipc-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    #[cfg(windows)]
    let (_dir, endpoint) = (None::<tempfile::TempDir>, format!(r"\\.\pipe\{unique}"));
    #[cfg(not(windows))]
    let (_dir, endpoint) = {
        let d = tempfile::tempdir().unwrap();
        let ep = d
            .path()
            .join(format!("{unique}.sock"))
            .to_string_lossy()
            .into_owned();
        (Some(d), ep)
    };

    let server_ep = endpoint.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let mut server = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&server_ep)
                .unwrap();
            let _ = ready_tx.send(());
            for _ in 0..5 {
                server.connect().await.unwrap();
                let conn = server;
                tokio::spawn(handle_mock_ipc_conn(conn));
                server = ServerOptions::new()
                    .first_pipe_instance(false)
                    .reject_remote_clients(true)
                    .create(&server_ep)
                    .unwrap();
            }
        }
        #[cfg(unix)]
        {
            let listener = tokio::net::UnixListener::bind(&server_ep).unwrap();
            let _ = ready_tx.send(());
            for _ in 0..5 {
                let (conn, _) = listener.accept().await.unwrap();
                tokio::spawn(handle_mock_ipc_conn(conn));
            }
        }
    });

    ready_rx.await.unwrap();

    let client: Arc<dyn ServiceIpcClient> = Arc::new(ProtocolServiceIpcClient::new(&endpoint));
    let mut supervisor = DaemonIpcSupervisor::new(Arc::clone(&client), "mihomo", "pipe://ctrl");

    // 1. Successful start
    let pid = supervisor
        .start(
            Path::new("/trusted/mihomo"),
            Path::new("/tmp/work"),
            Path::new("/tmp/active.yaml"),
        )
        .await
        .expect("start_remote must succeed");
    assert_eq!(pid, 4242);

    // 2. Query status
    let status = supervisor
        .refresh_status()
        .await
        .expect("query_remote_status must succeed");
    assert!(status.running);
    assert_eq!(status.pid, Some(4242));

    // 3. Stderr tail
    let tail = supervisor
        .fetch_stderr_tail(2)
        .await
        .expect("read_remote_stderr_tail must succeed");
    assert_eq!(tail, vec!["stderr-0", "stderr-1"]);

    // 4. Error mapping: UntrustedBinary -> KernelError::Protocol
    let err = client
        .start_remote(
            "mihomo",
            Path::new("/untrusted/mihomo"),
            Path::new("/tmp/work"),
            Path::new("/tmp/active.yaml"),
            "pipe://ctrl",
        )
        .await
        .expect_err("untrusted binary error must propagate");
    assert!(
        matches!(err, KernelError::Protocol(msg) if msg.contains("binary outside trusted dir"))
    );

    // 5. Error mapping: NotRunning -> KernelError::NotRunning
    let stop_err = supervisor
        .stop()
        .await
        .expect_err("NotRunning error must map to KernelError::NotRunning");
    assert!(matches!(stop_err, KernelError::NotRunning));

    server_task.await.unwrap();
}
