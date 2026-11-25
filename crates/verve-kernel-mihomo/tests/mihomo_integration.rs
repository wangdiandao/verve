//! Integration tests for `verve-kernel-mihomo` against mock IPC servers and real Mihomo.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};
use verve_kernel::{
    ControlEndpoint, DirectProcessSupervisor, LogLevel, ProviderKind, ProxyGroupKind, ProxyKernel,
    ProxyMode, TelemetryEvent, TelemetryKind,
};
use verve_kernel_mihomo::{MihomoKernel, MihomoKernelConfig, discover_mihomo_binary};

static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

struct TestTempDir {
    path: PathBuf,
    extra_files: Vec<PathBuf>,
}

impl TestTempDir {
    fn new(prefix: &str) -> Self {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "verve-mihomo-it-{prefix}-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self {
            path,
            extra_files: Vec::new(),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn track_cleanup(&mut self, path: PathBuf) {
        self.extra_files.push(path);
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        for path in &self.extra_files {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique_endpoint(prefix: &str) -> ControlEndpoint {
    let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    ControlEndpoint::default_for_session(&format!("{prefix}-{}-{seq}", std::process::id()))
}

#[derive(Debug, Default)]
struct MockState {
    selected_proxy: String,
    mode: String,
    reloaded_paths: Vec<String>,
    updated_providers: Vec<(String, String)>,
    closed_connections: Vec<Option<String>>,
    include_rule_providers: bool,
    requests: Vec<(String, String, Option<String>)>,
}

struct MockServerHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    #[cfg(unix)]
    socket_path: PathBuf,
}

impl Drop for MockServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

async fn handle_http_request(
    req: Request<Incoming>,
    state: Arc<Mutex<MockState>>,
    expected_secret: &'static str,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let method = req.method().as_str().to_string();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let auth = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    let host_ok = req
        .headers()
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        == Some("localhost");
    if !host_ok {
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from_static(b"missing Host: localhost")))
            .expect("response"));
    }

    if auth.as_deref() != Some(&format!("Bearer {expected_secret}")) {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Full::new(Bytes::from_static(
                br#"{"message":"unauthorized"}"#,
            )))
            .expect("response"));
    }

    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let body_str = String::from_utf8_lossy(&body_bytes).to_string();

    {
        let mut guard = state.lock().expect("lock mock state");
        guard
            .requests
            .push((method.clone(), path_and_query.clone(), auth));
    }

    let (status, json_body): (StatusCode, String) = match (method.as_str(), path_and_query.as_str())
    {
        ("GET", "/version") => (StatusCode::OK, r#"{"meta":true,"version":"v1.19.33"}"#.into()),
        ("PATCH", "/configs") => {
            let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();
            if let Some(mode) = val.get("mode").and_then(|v| v.as_str()) {
                state.lock().expect("lock").mode = mode.to_string();
            }
            (StatusCode::NO_CONTENT, String::new())
        }
        ("PUT", "/configs?force=true") => {
            let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();
            if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
                state.lock().expect("lock").reloaded_paths.push(path.into());
            }
            (StatusCode::NO_CONTENT, String::new())
        }
        ("GET", "/proxies") => {
            let selected = state.lock().expect("lock").selected_proxy.clone();
            let payload = serde_json::json!({
                "proxies": {
                    "GLOBAL": {
                        "name": "GLOBAL",
                        "type": "Selector",
                        "now": "Proxy 节点",
                        "all": ["Proxy 节点", "DIRECT"]
                    },
                    "Proxy 节点": {
                        "name": "Proxy 节点",
                        "type": "Selector",
                        "now": selected,
                        "all": ["香港 01/VIP", "tokyo-01", "DIRECT"]
                    },
                    "香港 01/VIP": {
                        "name": "香港 01/VIP",
                        "type": "VLESS",
                        "udp": true,
                        "history": [{"delay": 64}]
                    },
                    "tokyo-01": {
                        "name": "tokyo-01",
                        "type": "Trojan",
                        "udp": false,
                        "alive": false,
                        "history": [{"delay": 95}, {"delay": 0}]
                    },
                    "DIRECT": {
                        "name": "DIRECT",
                        "type": "Direct",
                        "udp": true
                    }
                }
            });
            (StatusCode::OK, payload.to_string())
        }
        ("PUT", "/proxies/Proxy%20%E8%8A%82%E7%82%B9") => {
            let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();
            if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                state.lock().expect("lock").selected_proxy = name.to_string();
            }
            (StatusCode::NO_CONTENT, String::new())
        }
        ("GET", pq) if pq.starts_with("/proxies/%E9%A6%99%E6%B8%AF%2001%2FVIP/delay?") => {
            assert!(pq.contains("url=https%3A%2F%2Fcp%2Ecloudflare%2Ecom%2Fgenerate%5F204"));
            assert!(pq.contains("timeout=2500"));
            (StatusCode::OK, r#"{"delay":37}"#.into())
        }
        ("GET", "/rules") => (
            StatusCode::OK,
            r#"{"rules":[{"type":"DOMAIN-SUFFIX","payload":"example.com","proxy":"Proxy 节点"},{"type":"MATCH","payload":"","proxy":"DIRECT"}]}"#.into(),
        ),
        ("GET", "/providers/rules") => {
            if state.lock().expect("lock").include_rule_providers {
                (
                    StatusCode::OK,
                    r#"{"providers":{"ads":{"name":"ads","behavior":"domain","vehicleType":"HTTP","ruleCount":512,"updatedAt":"2026-11-25T10:00:00Z"}}}"#.into(),
                )
            } else {
                (StatusCode::NOT_FOUND, r#"{"message":"not found"}"#.into())
            }
        }
        ("PUT", "/providers/proxies/sub%20one") => {
            state
                .lock()
                .expect("lock")
                .updated_providers
                .push(("proxy".into(), "sub one".into()));
            (StatusCode::NO_CONTENT, String::new())
        }
        ("PUT", "/providers/rules/ads%2Flist") => {
            state
                .lock()
                .expect("lock")
                .updated_providers
                .push(("rule".into(), "ads/list".into()));
            (StatusCode::NO_CONTENT, String::new())
        }
        ("DELETE", "/connections") => {
            state.lock().expect("lock").closed_connections.push(None);
            (StatusCode::NO_CONTENT, String::new())
        }
        ("DELETE", "/connections/conn%201%2Fabc") => {
            state
                .lock()
                .expect("lock")
                .closed_connections
                .push(Some("conn 1/abc".into()));
            (StatusCode::NO_CONTENT, String::new())
        }
        _ => (
            StatusCode::NOT_FOUND,
            format!(r#"{{"message":"unhandled route {method} {path_and_query} body={body_str}"}}"#),
        ),
    };

    Ok(Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(json_body)))
        .expect("build mock HTTP response"))
}

async fn spawn_mock_http_server(
    endpoint: &ControlEndpoint,
    state: Arc<Mutex<MockState>>,
    secret: &'static str,
) -> MockServerHandle {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    #[cfg(windows)]
    let task = {
        let pipe = match endpoint {
            ControlEndpoint::NamedPipe(pipe) => pipe.clone(),
            ControlEndpoint::UnixSocket(_) => panic!("expected named pipe on Windows"),
        };
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .expect("create first mock pipe instance");

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = server.connect() => {
                        if res.is_err() {
                            break;
                        }
                        let stream = server;
                        server = match tokio::net::windows::named_pipe::ServerOptions::new().create(&pipe) {
                            Ok(next) => next,
                            Err(_) => break,
                        };
                        let st = Arc::clone(&state);
                        tokio::spawn(async move {
                            let svc = service_fn(move |req| {
                                handle_http_request(req, Arc::clone(&st), secret)
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), svc)
                                .await;
                        });
                    }
                }
            }
        })
    };

    #[cfg(unix)]
    let (task, socket_path) = {
        let path = match endpoint {
            ControlEndpoint::UnixSocket(path) => path.clone(),
            ControlEndpoint::NamedPipe(_) => panic!("expected unix socket on Unix"),
        };
        let _ = fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("bind mock unix socket");
        let path_for_handle = path.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = listener.accept() => {
                        let Ok((stream, _)) = res else { break };
                        let st = Arc::clone(&state);
                        tokio::spawn(async move {
                            let svc = service_fn(move |req| {
                                handle_http_request(req, Arc::clone(&st), secret)
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), svc)
                                .await;
                        });
                    }
                }
            }
        });
        (task, path_for_handle)
    };

    MockServerHandle {
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        #[cfg(unix)]
        socket_path,
    }
}

// `tungstenite::handshake::server::Callback` fixes `Err` to `http::Response<Option<String>>`.
#[allow(clippy::result_large_err)]
async fn handle_mock_ws<S>(stream: S, expected_secret: &'static str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let seen_path = Arc::new(Mutex::new(String::new()));
    let seen_path_cb = Arc::clone(&seen_path);

    let callback = |req: &WsRequest, response: WsResponse| {
        let auth = req
            .headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if auth != Some(&format!("Bearer {expected_secret}")) {
            let mut reject: Response<Option<String>> =
                Response::new(Some("unauthorized".to_string()));
            *reject.status_mut() = StatusCode::UNAUTHORIZED;
            return Err(reject);
        }
        let pq = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| req.uri().path().to_string());
        *seen_path_cb.lock().expect("lock path") = pq;
        Ok(response)
    };

    let Ok(mut ws) = accept_hdr_async(stream, callback).await else {
        return;
    };

    let path = seen_path.lock().expect("lock path").clone();
    match path.as_str() {
        "/traffic" => {
            let _ = ws.send(Message::text(r#"{"up":1024,"down":4096}"#)).await;
            let _ = ws.send(Message::text(r#"{"up":2048,"down":8192}"#)).await;
        }
        "/memory" => {
            let _ = ws
                .send(Message::text(r#"{"inuse":33554432,"oslimit":0}"#))
                .await;
        }
        "/connections" => {
            let _ = ws
                .send(Message::text(
                    r#"{"uploadTotal":10,"downloadTotal":20,"connections":null}"#,
                ))
                .await;
        }
        "/logs?level=info" => {
            let _ = ws
                .send(Message::text(
                    r#"{"type":"warning","payload":"[DNS] fallback triggered"}"#,
                ))
                .await;
        }
        _ => {}
    }
    let _ = ws.close(None).await;
}

async fn spawn_mock_ws_server(
    endpoint: &ControlEndpoint,
    secret: &'static str,
) -> MockServerHandle {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    #[cfg(windows)]
    let task = {
        let pipe = match endpoint {
            ControlEndpoint::NamedPipe(pipe) => pipe.clone(),
            ControlEndpoint::UnixSocket(_) => panic!("expected named pipe on Windows"),
        };
        let mut server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .expect("create first mock ws pipe instance");

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = server.connect() => {
                        if res.is_err() {
                            break;
                        }
                        let stream = server;
                        server = match tokio::net::windows::named_pipe::ServerOptions::new().create(&pipe) {
                            Ok(next) => next,
                            Err(_) => break,
                        };
                        tokio::spawn(handle_mock_ws(stream, secret));
                    }
                }
            }
        })
    };

    #[cfg(unix)]
    let (task, socket_path) = {
        let path = match endpoint {
            ControlEndpoint::UnixSocket(path) => path.clone(),
            ControlEndpoint::NamedPipe(_) => panic!("expected unix socket on Unix"),
        };
        let _ = fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("bind mock ws unix socket");
        let path_for_handle = path.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = listener.accept() => {
                        let Ok((stream, _)) = res else { break };
                        tokio::spawn(handle_mock_ws(stream, secret));
                    }
                }
            }
        });
        (task, path_for_handle)
    };

    MockServerHandle {
        shutdown_tx: Some(shutdown_tx),
        task: Some(task),
        #[cfg(unix)]
        socket_path,
    }
}

#[tokio::test]
async fn mock_ipc_rest_control_plane_round_trip() {
    let endpoint = unique_endpoint("mock-rest");
    let state = Arc::new(Mutex::new(MockState {
        selected_proxy: "香港 01/VIP".into(),
        include_rule_providers: true,
        ..Default::default()
    }));
    let _server = spawn_mock_http_server(&endpoint, Arc::clone(&state), "test-secret").await;

    let kernel = MihomoKernel::new(MihomoKernelConfig {
        binary_path: PathBuf::from("mihomo"),
        work_dir: std::env::temp_dir(),
        endpoint,
        secret: Some("test-secret".into()),
        request_timeout: Duration::from_secs(3),
    });

    let caps = kernel.capabilities();
    assert!(caps.supports_tun);
    assert!(caps.supports_traffic_stream);
    assert!(caps.supports_delay_test);
    assert!(caps.supports_providers);

    let version = kernel
        .wait_until_ready(Duration::from_secs(2))
        .await
        .expect("wait_until_ready succeeds");
    assert_eq!(version, "v1.19.33");

    kernel
        .set_mode(ProxyMode::Global)
        .await
        .expect("set_mode succeeds");
    assert_eq!(state.lock().expect("lock").mode, "global");

    let cfg_path = Path::new("/tmp/verve-active.yaml");
    kernel
        .reload_config(cfg_path)
        .await
        .expect("reload_config succeeds");
    assert_eq!(
        state.lock().expect("lock").reloaded_paths,
        vec![cfg_path.to_string_lossy().to_string()]
    );

    let topology = kernel
        .fetch_proxies()
        .await
        .expect("fetch_proxies succeeds");
    let group_names: Vec<&str> = topology.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(group_names, vec!["Proxy 节点", "GLOBAL"]);
    assert_eq!(topology.groups[0].kind, ProxyGroupKind::Select);
    assert_eq!(topology.groups[0].now.as_deref(), Some("香港 01/VIP"));
    assert_eq!(topology.nodes["香港 01/VIP"].latest_delay_ms, Some(64));
    assert_eq!(topology.nodes["tokyo-01"].latest_delay_ms, Some(95));
    assert!(!topology.nodes["tokyo-01"].alive);

    kernel
        .select_proxy("Proxy 节点", "tokyo-01")
        .await
        .expect("select_proxy succeeds");
    assert_eq!(state.lock().expect("lock").selected_proxy, "tokyo-01");

    let delay = kernel
        .test_delay(
            "香港 01/VIP",
            "https://cp.cloudflare.com/generate_204",
            Duration::from_millis(2500),
        )
        .await
        .expect("test_delay succeeds");
    assert_eq!(delay, 37);

    let rules = kernel.fetch_rules().await.expect("fetch_rules succeeds");
    assert_eq!(rules.rules.len(), 2);
    assert_eq!(rules.providers.len(), 1);
    assert_eq!(rules.providers[0].name, "ads");
    assert_eq!(rules.providers[0].rule_count, 512);

    // Verify 404 fallback on /providers/rules.
    state.lock().expect("lock").include_rule_providers = false;
    let rules_without_providers = kernel
        .fetch_rules()
        .await
        .expect("fetch_rules falls back cleanly on 404");
    assert_eq!(rules_without_providers.rules.len(), 2);
    assert!(rules_without_providers.providers.is_empty());

    kernel
        .update_provider(ProviderKind::Proxy, "sub one")
        .await
        .expect("update proxy provider succeeds");
    kernel
        .update_provider(ProviderKind::Rule, "ads/list")
        .await
        .expect("update rule provider succeeds");
    assert_eq!(
        state.lock().expect("lock").updated_providers,
        vec![
            ("proxy".into(), "sub one".into()),
            ("rule".into(), "ads/list".into()),
        ]
    );

    kernel
        .close_connection(Some("conn 1/abc"))
        .await
        .expect("close single connection succeeds");
    kernel
        .close_connection(None)
        .await
        .expect("close all connections succeeds");
    assert_eq!(
        state.lock().expect("lock").closed_connections,
        vec![Some("conn 1/abc".into()), None]
    );
}

#[tokio::test]
async fn mock_ipc_telemetry_websocket_streams_events() {
    let endpoint = unique_endpoint("mock-ws");
    let _server = spawn_mock_ws_server(&endpoint, "ws-secret").await;

    let kernel = MihomoKernel::new(MihomoKernelConfig {
        binary_path: PathBuf::from("mihomo"),
        work_dir: std::env::temp_dir(),
        endpoint,
        secret: Some("ws-secret".into()),
        request_timeout: Duration::from_secs(3),
    });

    // Traffic WebSocket frames
    let mut traffic = kernel
        .open_telemetry(TelemetryKind::Traffic)
        .await
        .expect("open traffic stream");
    let first = traffic
        .next()
        .await
        .expect("first traffic item")
        .expect("valid traffic sample");
    assert_eq!(
        first,
        TelemetryEvent::Traffic(verve_kernel::TrafficSample {
            up_bps: 1024,
            down_bps: 4096
        })
    );
    let second = traffic
        .next()
        .await
        .expect("second traffic item")
        .expect("valid traffic sample");
    assert_eq!(
        second,
        TelemetryEvent::Traffic(verve_kernel::TrafficSample {
            up_bps: 2048,
            down_bps: 8192
        })
    );
    assert!(traffic.next().await.is_none());

    // Memory WebSocket frame
    let mut memory = kernel
        .open_telemetry(TelemetryKind::Memory)
        .await
        .expect("open memory stream");
    let mem_ev = memory
        .next()
        .await
        .expect("memory event")
        .expect("valid memory sample");
    assert_eq!(
        mem_ev,
        TelemetryEvent::Memory(verve_kernel::MemorySample {
            inuse_bytes: 33_554_432
        })
    );

    // Connections WebSocket frame (including null connections array)
    let mut conns = kernel
        .open_telemetry(TelemetryKind::Connections)
        .await
        .expect("open connections stream");
    let conn_ev = conns
        .next()
        .await
        .expect("connections event")
        .expect("valid connections sample");
    assert_eq!(
        conn_ev,
        TelemetryEvent::Connections {
            upload_total: 10,
            download_total: 20,
            connections: Vec::new()
        }
    );

    // Logs WebSocket frame
    let mut logs = kernel
        .open_telemetry(TelemetryKind::Logs)
        .await
        .expect("open logs stream");
    let log_ev = logs
        .next()
        .await
        .expect("log event")
        .expect("valid log entry");
    match log_ev {
        TelemetryEvent::Log(entry) => {
            assert_eq!(entry.level, LogLevel::Warning);
            assert_eq!(entry.payload, "[DNS] fallback triggered");
        }
        other => panic!("expected log event, got {other:?}"),
    }
}

#[tokio::test]
async fn real_mihomo_preflight_and_control_plane_end_to_end() {
    if std::env::var_os("VERVE_MIHOMO_BIN").is_some() {
        assert!(
            discover_mihomo_binary(None).is_some(),
            "discover_mihomo_binary(None) must locate VERVE_MIHOMO_BIN"
        );
    }

    let Some(binary_path) = discover_mihomo_binary(None) else {
        eprintln!("skipping real mihomo integration test: no mihomo binary discovered");
        return;
    };

    let mut temp_dir = TestTempDir::new("real-mihomo");
    let endpoint = unique_endpoint("real-mihomo");
    let controller_yaml_line = match &endpoint {
        ControlEndpoint::NamedPipe(pipe) => format!("external-controller-pipe: {pipe}"),
        ControlEndpoint::UnixSocket(sock) => {
            temp_dir.track_cleanup(sock.clone());
            format!("external-controller-unix: {}", sock.display())
        }
    };

    let valid_cfg = temp_dir.path().join("valid.yaml");
    let valid_yaml = format!(
        r#"mixed-port: 0
mode: rule
secret: "verve-it-secret"
{controller_yaml_line}
proxy-groups:
  - name: Proxy
    type: select
    proxies:
      - DIRECT
      - REJECT
rules:
  - MATCH,Proxy
"#
    );
    fs::write(&valid_cfg, valid_yaml).expect("write valid.yaml");

    let broken_cfg = temp_dir.path().join("broken.yaml");
    fs::write(&broken_cfg, "mixed-port: \"not-a-number\"\nmode: rule\n")
        .expect("write broken.yaml");

    let kernel = MihomoKernel::new(MihomoKernelConfig {
        binary_path: binary_path.clone(),
        work_dir: temp_dir.path().to_path_buf(),
        endpoint: endpoint.clone(),
        secret: Some("verve-it-secret".into()),
        request_timeout: Duration::from_secs(5),
    });

    // 1. Preflight passes on valid config and emits structured diagnostics on broken config.
    kernel
        .validate_config(&valid_cfg)
        .await
        .expect("valid Mihomo config should pass -t preflight");

    let diagnostics = kernel
        .validate_config(&broken_cfg)
        .await
        .expect_err("broken Mihomo config should fail -t preflight");
    assert!(!diagnostics.is_empty());
    assert!(
        diagnostics.iter().any(|d| d.line == Some(1)),
        "expected line 1 diagnostic, got: {diagnostics:?}"
    );

    // 2. Spawn real Mihomo via DirectProcessSupervisor and verify IPC control plane.
    let mut supervisor = DirectProcessSupervisor::new();
    let pid = supervisor
        .start(&binary_path, temp_dir.path(), &valid_cfg)
        .await
        .expect("real mihomo should spawn");
    assert!(pid > 0);

    let test_result = async {
        let version = kernel
            .wait_until_ready(Duration::from_secs(8))
            .await
            .map_err(|err| format!("wait_until_ready failed: {err}"))?;
        assert!(!version.trim().is_empty());

        let initial_topology = kernel
            .fetch_proxies()
            .await
            .map_err(|err| format!("fetch_proxies failed: {err}"))?;
        let proxy_group = initial_topology
            .groups
            .iter()
            .find(|g| g.name == "Proxy")
            .ok_or_else(|| "missing Proxy group in real Mihomo response".to_string())?;
        assert_eq!(proxy_group.kind, ProxyGroupKind::Select);
        assert_eq!(proxy_group.all, vec!["DIRECT", "REJECT"]);

        kernel
            .select_proxy("Proxy", "REJECT")
            .await
            .map_err(|err| format!("select_proxy failed: {err}"))?;

        let updated_topology = kernel
            .fetch_proxies()
            .await
            .map_err(|err| format!("fetch_proxies after select failed: {err}"))?;
        let updated_group = updated_topology
            .groups
            .iter()
            .find(|g| g.name == "Proxy")
            .ok_or_else(|| "missing Proxy group after select".to_string())?;
        assert_eq!(updated_group.now.as_deref(), Some("REJECT"));

        let rules = kernel
            .fetch_rules()
            .await
            .map_err(|err| format!("fetch_rules failed: {err}"))?;
        assert!(!rules.rules.is_empty());

        kernel
            .set_mode(ProxyMode::Direct)
            .await
            .map_err(|err| format!("set_mode failed: {err}"))?;

        let mut traffic = kernel
            .open_telemetry(TelemetryKind::Traffic)
            .await
            .map_err(|err| format!("open_telemetry failed: {err}"))?;
        let traffic_ev = tokio::time::timeout(Duration::from_secs(3), traffic.next())
            .await
            .map_err(|_| "timed out waiting for real Mihomo traffic sample".to_string())?
            .ok_or_else(|| "traffic stream ended unexpectedly".to_string())?
            .map_err(|err| format!("traffic stream error: {err}"))?;
        assert!(matches!(traffic_ev, TelemetryEvent::Traffic(_)));

        Ok::<(), String>(())
    }
    .await;

    let stop_res = supervisor.stop().await;
    if let Err(err) = test_result {
        let stderr = supervisor.stderr_tail(20);
        panic!("{err}; stderr tail: {stderr:?}");
    }
    stop_res.expect("stop real mihomo supervisor cleanly");
    assert!(!supervisor.is_running());
}
