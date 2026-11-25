//! Local IPC transport (Named Pipe / Unix Domain Socket) and HTTP/1.1 + WebSocket helpers.

use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST};
use http::{HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use verve_kernel::{ControlEndpoint, KernelError};

#[cfg(windows)]
pub(crate) type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
pub(crate) type IpcStream = tokio::net::UnixStream;

/// Raw HTTP response collected from Mihomo's local control endpoint.
pub(crate) struct HttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) body: Bytes,
}

/// Percent-encodes a single URI path segment or query parameter value.
pub(crate) fn encode_segment(raw: &str) -> String {
    utf8_percent_encode(raw, NON_ALPHANUMERIC).to_string()
}

/// Normalizes a Windows Named Pipe name to `\\.\pipe\<name>`.
#[cfg(windows)]
pub(crate) fn normalize_pipe_name(pipe: &str) -> Result<String, KernelError> {
    let trimmed = pipe.trim();
    if trimmed.is_empty() {
        return Err(KernelError::ControlEndpoint(
            "named pipe path must not be empty".into(),
        ));
    }
    if trimmed.starts_with(r"\\.\pipe\") || trimmed.starts_with(r"\\?\pipe\") {
        Ok(trimmed.to_string())
    } else {
        let suffix = trimmed.trim_start_matches(['\\', '/']);
        if suffix.is_empty() {
            return Err(KernelError::ControlEndpoint(
                "named pipe name must not be empty".into(),
            ));
        }
        Ok(format!(r"\\.\pipe\{suffix}"))
    }
}

/// Connects to the configured local control IPC endpoint within `timeout`.
pub(crate) async fn connect_ipc(
    endpoint: &ControlEndpoint,
    timeout: Duration,
) -> Result<IpcStream, KernelError> {
    #[cfg(windows)]
    {
        match endpoint {
            ControlEndpoint::NamedPipe(pipe) => {
                let normalized = normalize_pipe_name(pipe)?;
                let deadline = tokio::time::Instant::now() + timeout;
                loop {
                    match tokio::net::windows::named_pipe::ClientOptions::new().open(&normalized) {
                        Ok(client) => return Ok(client),
                        Err(err)
                            if err.raw_os_error()
                                == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) =>
                        {
                            let now = tokio::time::Instant::now();
                            if now >= deadline {
                                return Err(KernelError::Timeout);
                            }
                            let backoff = Duration::from_millis(10).min(deadline - now);
                            tokio::time::sleep(backoff).await;
                        }
                        Err(err) => {
                            return Err(KernelError::ControlEndpoint(format!(
                                "failed to connect named pipe `{normalized}`: {err}"
                            )));
                        }
                    }
                }
            }
            ControlEndpoint::UnixSocket(path) => Err(KernelError::ControlEndpoint(format!(
                "unix domain socket endpoint `{}` is not supported on Windows",
                path.display()
            ))),
        }
    }

    #[cfg(unix)]
    {
        match endpoint {
            ControlEndpoint::UnixSocket(path) => {
                match tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path)).await {
                    Ok(Ok(stream)) => Ok(stream),
                    Ok(Err(err)) => Err(KernelError::ControlEndpoint(format!(
                        "failed to connect unix socket `{}`: {err}",
                        path.display()
                    ))),
                    Err(_) => Err(KernelError::Timeout),
                }
            }
            ControlEndpoint::NamedPipe(pipe) => Err(KernelError::ControlEndpoint(format!(
                "named pipe endpoint `{pipe}` is not supported on Unix"
            ))),
        }
    }
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// Sends a single HTTP/1.1 request over the local IPC endpoint and returns the full response.
pub(crate) async fn send_http_request(
    endpoint: &ControlEndpoint,
    secret: Option<&str>,
    timeout: Duration,
    method: Method,
    path_and_query: &str,
    body: Option<Bytes>,
) -> Result<HttpResponse, KernelError> {
    let fut = async {
        let stream = connect_ipc(endpoint, timeout).await?;
        let io = TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|err| KernelError::Protocol(format!("http1 handshake failed: {err}")))?;

        let _conn_guard = AbortOnDrop(Some(tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(error = %err, "mihomo HTTP/1.1 IPC connection terminated");
            }
        })));

        let mut builder = Request::builder()
            .method(method.clone())
            .uri(path_and_query)
            .header(HOST, "localhost")
            .header(CONNECTION, "close");

        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            let auth_value = HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|err| {
                KernelError::Protocol(format!("invalid authorization secret header: {err}"))
            })?;
            builder = builder.header(AUTHORIZATION, auth_value);
        }

        if body.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }

        let req = builder
            .body(Full::new(body.unwrap_or_default()))
            .map_err(|err| KernelError::Protocol(format!("failed to build HTTP request: {err}")))?;

        let res = sender.send_request(req).await.map_err(|err| {
            KernelError::Protocol(format!(
                "HTTP request {method} {path_and_query} failed: {err}"
            ))
        })?;

        let status = res.status();
        let bytes = res
            .into_body()
            .collect()
            .await
            .map_err(|err| {
                KernelError::Protocol(format!(
                    "failed to read response body for {method} {path_and_query}: {err}"
                ))
            })?
            .to_bytes();

        Ok(HttpResponse {
            status,
            body: bytes,
        })
    };

    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| KernelError::Timeout)?
}

/// Opens a WebSocket stream over a dedicated local IPC connection.
pub(crate) async fn open_websocket(
    endpoint: &ControlEndpoint,
    secret: Option<&str>,
    timeout: Duration,
    path_and_query: &str,
) -> Result<WebSocketStream<IpcStream>, KernelError> {
    let fut = async {
        let stream = connect_ipc(endpoint, timeout).await?;
        let url = format!("ws://localhost{path_and_query}");
        let mut request = url.into_client_request().map_err(|err| {
            KernelError::Protocol(format!("invalid websocket URI `{path_and_query}`: {err}"))
        })?;

        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            let auth_value = HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|err| {
                KernelError::Protocol(format!("invalid authorization secret header: {err}"))
            })?;
            request.headers_mut().insert(AUTHORIZATION, auth_value);
        }

        let (ws, _response) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|err| {
                KernelError::Protocol(format!(
                    "websocket handshake failed for `{path_and_query}`: {err}"
                ))
            })?;

        Ok(ws)
    };

    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| KernelError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_segment_escapes_spaces_slashes_and_unicode() {
        assert_eq!(encode_segment("Proxy Group/1"), "Proxy%20Group%2F1");
        assert_eq!(encode_segment("香港 01"), "%E9%A6%99%E6%B8%AF%2001");
        assert_eq!(
            encode_segment("https://cp.cloudflare.com/generate_204"),
            "https%3A%2F%2Fcp%2Ecloudflare%2Ecom%2Fgenerate%5F204"
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_pipe_name_handles_short_and_prefixed_names() {
        assert_eq!(
            normalize_pipe_name("verve-test").expect("valid pipe"),
            r"\\.\pipe\verve-test"
        );
        assert_eq!(
            normalize_pipe_name(r"\verve-test").expect("valid pipe"),
            r"\\.\pipe\verve-test"
        );
        assert_eq!(
            normalize_pipe_name(r"\\.\pipe\verve-test").expect("valid pipe"),
            r"\\.\pipe\verve-test"
        );
        assert!(normalize_pipe_name("   ").is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unix_socket_endpoint_rejected_gracefully_on_windows() {
        let err = connect_ipc(
            &ControlEndpoint::UnixSocket(std::path::PathBuf::from("/tmp/mihomo.sock")),
            Duration::from_millis(50),
        )
        .await
        .expect_err("unix socket must fail cleanly on windows");
        assert!(matches!(err, KernelError::ControlEndpoint(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn named_pipe_endpoint_rejected_gracefully_on_unix() {
        let err = connect_ipc(
            &ControlEndpoint::NamedPipe(r"\\.\pipe\mihomo".into()),
            Duration::from_millis(50),
        )
        .await
        .expect_err("named pipe must fail cleanly on unix");
        assert!(matches!(err, KernelError::ControlEndpoint(_)));
    }
}
