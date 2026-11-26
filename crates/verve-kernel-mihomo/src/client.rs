//! `MihomoKernel` control-plane client implementing [`verve_kernel::ProxyKernel`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use http::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use verve_kernel::{
    ConfigDiagnostic, ControlEndpoint, KernelCapabilities, KernelError, ProviderKind, ProxyKernel,
    ProxyMode, ProxyTopology, RuleSnapshot, TelemetryKind, TelemetryStream,
};

use crate::preflight;
use crate::schema::{
    DelayResponse, ErrorMessageResponse, PatchModeRequest, ProxiesResponse, ReloadConfigRequest,
    RuleProvidersResponse, RulesResponse, SelectProxyRequest, VersionResponse, into_rule_snapshot,
    parse_telemetry_frame,
};
use crate::transport::{
    HttpResponse, IpcStream, encode_segment, open_websocket, send_http_request,
};

/// Configuration for connecting to and validating a Mihomo proxy kernel instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MihomoKernelConfig {
    /// Filesystem path to the Mihomo executable used for `-t` preflight checks.
    pub binary_path: PathBuf,
    /// Working directory passed via `-d` during preflight checks.
    pub work_dir: PathBuf,
    /// Local Named Pipe or Unix Domain Socket endpoint for Mihomo's controller API.
    pub endpoint: ControlEndpoint,
    /// Optional Bearer secret configured on Mihomo's REST/WebSocket controller.
    pub secret: Option<String>,
    /// Per-request IPC connect and response timeout.
    pub request_timeout: Duration,
}

/// Mihomo control-plane adapter implementing [`ProxyKernel`].
#[derive(Debug, Clone)]
pub struct MihomoKernel {
    config: MihomoKernelConfig,
}

impl MihomoKernel {
    /// Creates a new [`MihomoKernel`] client from the supplied configuration.
    pub fn new(config: MihomoKernelConfig) -> Self {
        Self { config }
    }

    /// Returns a reference to the underlying kernel configuration.
    pub fn config(&self) -> &MihomoKernelConfig {
        &self.config
    }

    /// Queries `GET /version` and returns Mihomo's reported version string.
    pub async fn fetch_version(&self) -> Result<String, KernelError> {
        let resp: VersionResponse = self
            .request_json(
                Method::GET,
                "/version",
                None::<&()>,
                self.config.request_timeout,
            )
            .await?;
        Ok(resp.version)
    }

    /// Polls `GET /version` with short backoff until Mihomo responds or `timeout` elapses.
    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<String, KernelError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut backoff = Duration::from_millis(25);

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(KernelError::Timeout);
            }
            let remaining = deadline - now;
            let attempt_timeout = remaining.min(self.config.request_timeout);

            match tokio::time::timeout(attempt_timeout, self.fetch_version()).await {
                Ok(Ok(version)) => return Ok(version),
                Ok(Err(err)) => {
                    tracing::debug!(error = %err, "mihomo controller not ready yet");
                }
                Err(_) => {
                    tracing::debug!("mihomo readiness probe timed out");
                }
            }

            let now_after = tokio::time::Instant::now();
            if now_after >= deadline {
                return Err(KernelError::Timeout);
            }
            let sleep_for = backoff.min(deadline - now_after);
            tokio::time::sleep(sleep_for).await;
            backoff = (backoff * 2).min(Duration::from_millis(150));
        }
    }

    /// Switches Mihomo's global routing mode via `PATCH /configs`.
    pub async fn set_mode(&self, mode: ProxyMode) -> Result<(), KernelError> {
        let body = PatchModeRequest { mode };
        self.request_empty(
            Method::PATCH,
            "/configs",
            Some(&body),
            self.config.request_timeout,
        )
        .await
    }

    async fn request_raw<B: Serialize>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<HttpResponse, KernelError> {
        let payload = match body {
            Some(value) => Some(Bytes::from(serde_json::to_vec(value).map_err(|err| {
                KernelError::Protocol(format!("failed to serialize request body: {err}"))
            })?)),
            None => None,
        };

        send_http_request(
            &self.config.endpoint,
            self.config.secret.as_deref(),
            timeout,
            method,
            path_and_query,
            payload,
        )
        .await
    }

    async fn request_empty<B: Serialize>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<(), KernelError> {
        let resp = self
            .request_raw(method.clone(), path_and_query, body, timeout)
            .await?;
        if resp.status.is_success() {
            Ok(())
        } else {
            Err(api_status_error(&method, path_and_query, resp))
        }
    }

    async fn request_json<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path_and_query: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<R, KernelError> {
        let resp = self
            .request_raw(method.clone(), path_and_query, body, timeout)
            .await?;
        if !resp.status.is_success() {
            return Err(api_status_error(&method, path_and_query, resp));
        }

        serde_json::from_slice(&resp.body).map_err(|err| {
            KernelError::Protocol(format!(
                "failed to decode JSON response for {method} {path_and_query}: {err}"
            ))
        })
    }
}

fn api_status_error(method: &Method, path_and_query: &str, resp: HttpResponse) -> KernelError {
    let detail = serde_json::from_slice::<ErrorMessageResponse>(&resp.body)
        .ok()
        .and_then(|err| err.message)
        .filter(|msg| !msg.trim().is_empty())
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(&resp.body).trim().to_string();
            if text.is_empty() {
                resp.status
                    .canonical_reason()
                    .unwrap_or("unexpected HTTP status")
                    .to_string()
            } else {
                text
            }
        });

    KernelError::Protocol(format!(
        "mihomo API {method} {path_and_query} failed with {}: {detail}",
        resp.status
    ))
}

impl ProxyKernel for MihomoKernel {
    fn capabilities(&self) -> KernelCapabilities {
        KernelCapabilities {
            supports_tun: true,
            supports_traffic_stream: true,
            supports_memory_stream: true,
            supports_connections_stream: true,
            supports_logs_stream: true,
            supports_delay_test: true,
            supports_providers: true,
        }
    }

    async fn fetch_version(&self) -> Result<Option<String>, KernelError> {
        MihomoKernel::fetch_version(self).await.map(Some)
    }

    async fn validate_config(&self, path: &Path) -> Result<(), Vec<ConfigDiagnostic>> {
        preflight::run_preflight(
            &self.config.binary_path,
            &self.config.work_dir,
            path,
            self.config.request_timeout,
        )
        .await
    }

    async fn reload_config(&self, path: &Path) -> Result<(), KernelError> {
        let path_str = path.to_string_lossy();
        let body = ReloadConfigRequest {
            path: path_str.as_ref(),
        };
        self.request_empty(
            Method::PUT,
            "/configs?force=true",
            Some(&body),
            self.config.request_timeout,
        )
        .await
    }

    async fn fetch_proxies(&self) -> Result<ProxyTopology, KernelError> {
        let raw: ProxiesResponse = self
            .request_json(
                Method::GET,
                "/proxies",
                None::<&()>,
                self.config.request_timeout,
            )
            .await?;
        Ok(raw.into_topology())
    }

    async fn select_proxy(&self, group: &str, name: &str) -> Result<(), KernelError> {
        let path = format!("/proxies/{}", encode_segment(group));
        let body = SelectProxyRequest { name };
        self.request_empty(Method::PUT, &path, Some(&body), self.config.request_timeout)
            .await
    }

    async fn test_delay(
        &self,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<u16, KernelError> {
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let path = format!(
            "/proxies/{}/delay?url={}&timeout={timeout_ms}",
            encode_segment(name),
            encode_segment(url)
        );
        let http_timeout = self
            .config
            .request_timeout
            .max(timeout.saturating_add(Duration::from_secs(2)));

        let resp: DelayResponse = self
            .request_json(Method::GET, &path, None::<&()>, http_timeout)
            .await?;
        u16::try_from(resp.delay).map_err(|_| {
            KernelError::Protocol(format!(
                "measured delay {}ms for `{name}` exceeds u16::MAX",
                resp.delay
            ))
        })
    }

    async fn fetch_rules(&self) -> Result<RuleSnapshot, KernelError> {
        let rules_resp: RulesResponse = self
            .request_json(
                Method::GET,
                "/rules",
                None::<&()>,
                self.config.request_timeout,
            )
            .await?;

        let providers_raw = self
            .request_raw(
                Method::GET,
                "/providers/rules",
                None::<&()>,
                self.config.request_timeout,
            )
            .await;

        let providers_resp = match providers_raw {
            Ok(resp) if resp.status.is_success() => Some(
                serde_json::from_slice::<RuleProvidersResponse>(&resp.body).map_err(|err| {
                    KernelError::Protocol(format!(
                        "failed to decode JSON response for GET /providers/rules: {err}"
                    ))
                })?,
            ),
            Ok(resp) if resp.status == StatusCode::NOT_FOUND => None,
            Ok(resp) => return Err(api_status_error(&Method::GET, "/providers/rules", resp)),
            Err(err) => {
                tracing::debug!(error = %err, "best-effort GET /providers/rules failed");
                None
            }
        };

        Ok(into_rule_snapshot(rules_resp, providers_resp))
    }

    async fn update_provider(&self, kind: ProviderKind, name: &str) -> Result<(), KernelError> {
        let prefix = match kind {
            ProviderKind::Proxy => "/providers/proxies",
            ProviderKind::Rule => "/providers/rules",
        };
        let path = format!("{prefix}/{}", encode_segment(name));
        self.request_empty(Method::PUT, &path, None::<&()>, self.config.request_timeout)
            .await
    }

    async fn close_connection(&self, id: Option<&str>) -> Result<(), KernelError> {
        let path = match id {
            Some(conn_id) => format!("/connections/{}", encode_segment(conn_id)),
            None => "/connections".to_string(),
        };
        self.request_empty(
            Method::DELETE,
            &path,
            None::<&()>,
            self.config.request_timeout,
        )
        .await
    }

    async fn open_telemetry(&self, kind: TelemetryKind) -> Result<TelemetryStream, KernelError> {
        let path = match kind {
            TelemetryKind::Traffic => "/traffic",
            TelemetryKind::Memory => "/memory",
            TelemetryKind::Connections => "/connections",
            TelemetryKind::Logs => "/logs?level=info",
        };

        let ws = open_websocket(
            &self.config.endpoint,
            self.config.secret.as_deref(),
            self.config.request_timeout,
            path,
        )
        .await?;

        Ok(telemetry_stream_from_ws(kind, ws))
    }
}

fn telemetry_stream_from_ws(
    kind: TelemetryKind,
    ws: WebSocketStream<IpcStream>,
) -> TelemetryStream {
    enum WsState {
        Open(Box<WebSocketStream<IpcStream>>),
        Closed,
    }

    let stream =
        futures_util::stream::unfold(WsState::Open(Box::new(ws)), move |state| async move {
            let mut ws = match state {
                WsState::Open(ws) => ws,
                WsState::Closed => return None,
            };

            loop {
                match ws.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let parsed = parse_telemetry_frame(kind, text.as_str());
                        let next_state = if parsed.is_ok() {
                            WsState::Open(ws)
                        } else {
                            WsState::Closed
                        };
                        return Some((parsed, next_state));
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if let Ok(text) = std::str::from_utf8(&bytes)
                            && !text.trim().is_empty()
                        {
                            let parsed = parse_telemetry_frame(kind, text);
                            let next_state = if parsed.is_ok() {
                                WsState::Open(ws)
                            } else {
                                WsState::Closed
                            };
                            return Some((parsed, next_state));
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => return None,
                    Some(Err(err)) => {
                        return Some((
                            Err(KernelError::Protocol(format!(
                                "telemetry websocket stream error: {err}"
                            ))),
                            WsState::Closed,
                        ));
                    }
                }
            }
        });

    Box::pin(stream)
}
