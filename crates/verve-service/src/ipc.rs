//! Authenticated local IPC transport server and client helpers for `verve-service`.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, watch};
use verve_protocol::{
    IpcRequest, IpcResponse, MAX_FRAME_LEN, ProtocolError, ProtocolErrorCode, decode_frame,
    encode_frame,
};

use crate::supervisor::ServiceSupervisor;

/// Length in bytes of the 4-byte little-endian frame header.
const FRAME_HEADER_LEN: usize = std::mem::size_of::<u32>();

/// Cooperative shutdown signal shared with the IPC accept loop and active connection handlers.
#[derive(Debug, Clone)]
pub struct ShutdownSignal {
    tx: watch::Sender<bool>,
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownSignal {
    /// Creates a new unsignaled [`ShutdownSignal`].
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx }
    }

    /// Triggers shutdown for all tasks observing this signal.
    pub fn trigger(&self) {
        let _ = self.tx.send(true);
    }

    /// Creates a receiver for awaiting shutdown.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

/// Errors produced by the local IPC transport layer.
#[derive(Debug, Error)]
pub enum IpcError {
    /// Framing or serialization error from `verve-protocol`.
    #[error("IPC protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// Underlying I/O error.
    #[error("IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Reads a single length-prefixed frame from `reader`, returning `Ok(None)` on clean EOF.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, IpcError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; FRAME_HEADER_LEN];
    let mut read_so_far = 0usize;
    while read_so_far < FRAME_HEADER_LEN {
        let n = reader.read(&mut header[read_so_far..]).await?;
        if n == 0 {
            if read_so_far == 0 {
                return Ok(None);
            }
            return Err(IpcError::Protocol(ProtocolError::TruncatedFrame {
                expected: FRAME_HEADER_LEN,
                actual: read_so_far,
            }));
        }
        read_so_far += n;
    }

    let payload_len = u32::from_le_bytes(header) as usize;
    if payload_len > MAX_FRAME_LEN {
        return Err(IpcError::Protocol(ProtocolError::FrameTooLarge {
            len: payload_len,
            max: MAX_FRAME_LEN,
        }));
    }

    let mut frame = vec![0u8; FRAME_HEADER_LEN + payload_len];
    frame[..FRAME_HEADER_LEN].copy_from_slice(&header);
    reader.read_exact(&mut frame[FRAME_HEADER_LEN..]).await?;

    Ok(Some(decode_frame(&frame)?))
}

/// Encodes and writes a single length-prefixed frame to `writer`.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), IpcError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = encode_frame(message)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn handle_client_stream<S>(
    mut stream: S,
    supervisor: Arc<Mutex<ServiceSupervisor>>,
    mut shutdown_rx: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let maybe_req = tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            res = read_frame::<S, IpcRequest>(&mut stream) => res,
        };

        let request = match maybe_req {
            Ok(Some(req)) => req,
            Ok(None) => break,
            Err(err) => {
                let err_resp = IpcResponse::Error {
                    code: ProtocolErrorCode::InvalidArgument,
                    message: err.to_string(),
                };
                let _ = write_frame(&mut stream, &err_resp).await;
                break;
            }
        };

        let response = {
            let mut guard = supervisor.lock().await;
            guard.handle_request(request).await
        };

        if let Err(err) = write_frame(&mut stream, &response).await {
            tracing::debug!(error = %err, "failed to write IPC response to client");
            break;
        }
    }
}

/// Runs the authenticated local IPC server loop at `endpoint` until `shutdown` is triggered.
pub async fn run_ipc_server(
    endpoint: &str,
    supervisor: Arc<Mutex<ServiceSupervisor>>,
    shutdown: ShutdownSignal,
) -> Result<(), IpcError> {
    #[cfg(windows)]
    {
        run_windows_server(endpoint, supervisor, shutdown).await
    }
    #[cfg(unix)]
    {
        run_unix_server(endpoint, supervisor, shutdown).await
    }
}

#[cfg(windows)]
async fn run_windows_server(
    endpoint: &str,
    supervisor: Arc<Mutex<ServiceSupervisor>>,
    shutdown: ShutdownSignal,
) -> Result<(), IpcError> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(endpoint)?;
    let mut shutdown_rx = shutdown.subscribe();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            res = server.connect() => {
                res?;
                let connected = server;
                let sup = Arc::clone(&supervisor);
                let conn_shutdown = shutdown.subscribe();
                tokio::spawn(async move {
                    handle_client_stream(connected, sup, conn_shutdown).await;
                });

                server = ServerOptions::new()
                    .first_pipe_instance(false)
                    .reject_remote_clients(true)
                    .create(endpoint)?;
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
struct UnixSocketCleanup(std::path::PathBuf);

#[cfg(unix)]
impl Drop for UnixSocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
async fn run_unix_server(
    endpoint: &str,
    supervisor: Arc<Mutex<ServiceSupervisor>>,
    shutdown: ShutdownSignal,
) -> Result<(), IpcError> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    let path = std::path::Path::new(endpoint);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(IpcError::Io(err)),
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let _cleanup = UnixSocketCleanup(path.to_path_buf());
    let mut shutdown_rx = shutdown.subscribe();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            res = listener.accept() => {
                let (mut stream, _addr) = res?;
                let authorized = match stream.peer_cred() {
                    Ok(cred) => {
                        // SAFETY: `geteuid()` is always safe to call and returns the process effective UID.
                        let euid = unsafe { libc::geteuid() };
                        cred.uid() == 0 || cred.uid() == euid
                    }
                    Err(_) => false,
                };

                if !authorized {
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(250),
                            read_frame::<_, IpcRequest>(&mut stream),
                        )
                        .await;
                        let resp = IpcResponse::Error {
                            code: ProtocolErrorCode::Unauthorized,
                            message: "peer credentials rejected".into(),
                        };
                        let _ = write_frame(&mut stream, &resp).await;
                    });
                    continue;
                }

                let sup = Arc::clone(&supervisor);
                let conn_shutdown = shutdown.subscribe();
                tokio::spawn(async move {
                    handle_client_stream(stream, sup, conn_shutdown).await;
                });
            }
        }
    }

    Ok(())
}

/// Connects to a local `verve-service` IPC endpoint for issuing framed requests.
#[cfg(test)]
pub(crate) struct LocalIpcClient {
    #[cfg(windows)]
    stream: tokio::net::windows::named_pipe::NamedPipeClient,
    #[cfg(unix)]
    stream: tokio::net::UnixStream,
}

#[cfg(test)]
impl LocalIpcClient {
    /// Connects to `endpoint` (Named Pipe on Windows, Unix Domain Socket on Unix).
    pub async fn connect(endpoint: &str) -> Result<Self, IpcError> {
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ClientOptions;
            const ERROR_PIPE_BUSY: i32 = 231;

            let mut attempts = 0u8;
            let stream = loop {
                match ClientOptions::new().open(endpoint) {
                    Ok(client) => break client,
                    Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 10 => {
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    Err(err) => return Err(IpcError::Io(err)),
                }
            };
            Ok(Self { stream })
        }
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(endpoint).await?;
            Ok(Self { stream })
        }
    }

    /// Sends a single [`IpcRequest`] and awaits the corresponding [`IpcResponse`].
    pub async fn call(&mut self, request: &IpcRequest) -> Result<IpcResponse, IpcError> {
        write_frame(&mut self.stream, request).await?;
        read_frame(&mut self.stream).await?.ok_or_else(|| {
            IpcError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "IPC server closed connection",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;
    use verve_protocol::PROTOCOL_VERSION;

    use super::*;
    use crate::allowlist::SecurityPolicy;

    fn unique_test_endpoint(name: &str) -> (Option<TempDir>, String) {
        #[cfg(windows)]
        {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            (
                None,
                format!(
                    r"\\.\pipe\verve-svc-test-{name}-{}-{nanos}",
                    std::process::id()
                ),
            )
        }
        #[cfg(not(windows))]
        {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(format!("{name}.sock"));
            (Some(dir), path.to_string_lossy().into_owned())
        }
    }

    #[tokio::test]
    async fn live_ipc_round_trip_handshake_start_status_stderr_stop() {
        let (_sock_dir, endpoint) = unique_test_endpoint("roundtrip");
        let trusted_dir = TempDir::new().unwrap();
        let work_dir = TempDir::new().unwrap();

        let cfg_path = work_dir.path().join("config.yaml");
        std::fs::write(&cfg_path, b"mixed-port: 7890\n").unwrap();
        let next_cfg_path = work_dir.path().join("next.yaml");
        std::fs::write(&next_cfg_path, b"mixed-port: 7891\n").unwrap();

        let dummy_src = work_dir.path().join("dummy_kernel.rs");
        std::fs::write(
            &dummy_src,
            r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    eprintln!("kernel-boot args={} cfg={}", args.len(), args.get(4).map(String::as_str).unwrap_or(""));
    eprintln!("kernel-ready");
    std::thread::sleep(std::time::Duration::from_secs(30));
}
"#,
        )
        .unwrap();

        #[cfg(windows)]
        let bin_name = "mihomo-dummy.exe";
        #[cfg(not(windows))]
        let bin_name = "mihomo-dummy";
        let bin_path = trusted_dir.path().join(bin_name);

        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let compile = std::process::Command::new(rustc)
            .arg(&dummy_src)
            .arg("-o")
            .arg(&bin_path)
            .output()
            .expect("rustc must compile dummy test kernel");
        assert!(
            compile.status.success(),
            "failed to compile dummy kernel: {}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let sha256 = crate::allowlist::compute_sha256_hex(&bin_path).unwrap();
        let policy = SecurityPolicy {
            trusted_dirs: vec![PathBuf::from(trusted_dir.path())],
            allowed_sha256_hex: vec![sha256],
        };

        let supervisor = Arc::new(Mutex::new(ServiceSupervisor::new(policy)));
        let shutdown = ShutdownSignal::new();

        let server_sup = Arc::clone(&supervisor);
        let server_shutdown = shutdown.clone();
        let server_ep = endpoint.clone();
        let server_task = tokio::spawn(async move {
            run_ipc_server(&server_ep, server_sup, server_shutdown)
                .await
                .unwrap();
        });

        // Wait briefly for server socket/pipe creation.
        let mut client = {
            let mut connected = None;
            for _ in 0..40 {
                if let Ok(c) = LocalIpcClient::connect(&endpoint).await {
                    connected = Some(c);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            connected.expect("IPC client should connect to live server")
        };

        // 1. Handshake
        let ack = client
            .call(&IpcRequest::Handshake {
                client_proto_ver: PROTOCOL_VERSION,
            })
            .await
            .unwrap();
        assert!(matches!(
            ack,
            IpcResponse::HandshakeAck {
                server_proto_ver: PROTOCOL_VERSION,
                running_pid: None,
                ..
            }
        ));

        // 2. StartKernel
        let started = client
            .call(&IpcRequest::StartKernel {
                kernel_id: "mihomo".into(),
                bin_path: bin_path.to_string_lossy().into_owned(),
                config_path: cfg_path.to_string_lossy().into_owned(),
                work_dir: work_dir.path().to_string_lossy().into_owned(),
                ctrl_endpoint: "pipe://mihomo-ctrl".into(),
            })
            .await
            .unwrap();
        let pid = match started {
            IpcResponse::KernelStarted { pid, ctrl_endpoint } => {
                assert_eq!(ctrl_endpoint, "pipe://mihomo-ctrl");
                pid
            }
            other => panic!("expected KernelStarted, got {other:?}"),
        };

        // 3. QueryStatus
        let status = client.call(&IpcRequest::QueryStatus).await.unwrap();
        assert!(matches!(
            status,
            IpcResponse::Status(ref st) if st.running && st.pid == Some(pid)
        ));

        // 4. ReadStderrTail (poll briefly until stderr lines are captured)
        let mut lines = Vec::new();
        for _ in 0..40 {
            if let IpcResponse::StderrTail { lines: tail } = client
                .call(&IpcRequest::ReadStderrTail { max_lines: 10 })
                .await
                .unwrap()
                && tail.len() >= 2
            {
                lines = tail;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(lines.iter().any(|l| l.contains("kernel-boot args=5")));
        assert!(lines.iter().any(|l| l.contains("kernel-ready")));

        // 5. RestartKernel with invalid config preserves active process, then valid config restarts.
        let missing_cfg = work_dir.path().join("missing.yaml");
        let restart_err = client
            .call(&IpcRequest::RestartKernel {
                config_path: Some(missing_cfg.to_string_lossy().into_owned()),
            })
            .await
            .unwrap();
        assert!(matches!(
            restart_err,
            IpcResponse::Error {
                code: ProtocolErrorCode::InvalidArgument,
                ..
            }
        ));

        let restarted = client
            .call(&IpcRequest::RestartKernel {
                config_path: Some(next_cfg_path.to_string_lossy().into_owned()),
            })
            .await
            .unwrap();
        let restarted_pid = match restarted {
            IpcResponse::KernelStarted { pid: new_pid, .. } => new_pid,
            other => panic!("expected KernelStarted on restart, got {other:?}"),
        };

        let mut restarted_lines = Vec::new();
        for _ in 0..40 {
            if let IpcResponse::StderrTail { lines: tail } = client
                .call(&IpcRequest::ReadStderrTail { max_lines: 10 })
                .await
                .unwrap()
                && tail.iter().any(|l| l.contains("next.yaml"))
            {
                restarted_lines = tail;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            restarted_lines.iter().any(|l| l.contains("next.yaml")),
            "expected restarted kernel stderr to mention next.yaml, got {restarted_lines:?}"
        );

        let status_restarted = client.call(&IpcRequest::QueryStatus).await.unwrap();
        assert!(matches!(
            status_restarted,
            IpcResponse::Status(ref st) if st.running && st.pid == Some(restarted_pid)
        ));

        // 6. StopKernel
        let stopped = client.call(&IpcRequest::StopKernel).await.unwrap();
        assert_eq!(stopped, IpcResponse::Stopped);

        let status_after = client.call(&IpcRequest::QueryStatus).await.unwrap();
        assert!(matches!(
            status_after,
            IpcResponse::Status(ref st) if !st.running && st.pid.is_none()
        ));

        shutdown.trigger();
        supervisor.lock().await.shutdown().await;
        server_task.await.unwrap();
    }
}
