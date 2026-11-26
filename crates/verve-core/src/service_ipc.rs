//! Length-prefixed `verve-protocol` IPC client implementing [`verve_kernel::ServiceIpcClient`].

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use verve_kernel::{KernelError, RemoteKernelStatus, ServiceIpcClient};
use verve_protocol::{
    IpcRequest, IpcResponse, MAX_FRAME_LEN, PROTOCOL_VERSION, ProtocolError, ProtocolErrorCode,
    decode_frame, encode_frame,
};

use crate::error::CoreError;

const FRAME_HEADER_LEN: usize = std::mem::size_of::<u32>();

/// Returns the platform-native default IPC endpoint for `verve-service`.
pub fn default_service_ipc_endpoint() -> &'static str {
    #[cfg(windows)]
    {
        r"\\.\pipe\verve-service"
    }
    #[cfg(not(windows))]
    {
        "/run/verve/verve-service.sock"
    }
}

/// Async abstraction for installing and uninstalling the privileged `verve-service` OS daemon.
pub trait ServiceControlBackend: Send + Sync {
    /// Installs `verve-service` with the supplied trusted binary directories.
    fn install_service<'a>(
        &'a self,
        trusted_dirs: &'a [PathBuf],
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>>;

    /// Uninstalls `verve-service` from the host OS service manager.
    fn uninstall_service<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>>;
}

/// Production [`ServiceControlBackend`] invoking the sibling `verve-service` executable.
#[derive(Debug, Clone, Default)]
pub struct OsServiceControlBackend;

impl OsServiceControlBackend {
    fn locate_service_binary() -> PathBuf {
        let bin_name = if cfg!(windows) {
            "verve-service.exe"
        } else {
            "verve-service"
        };

        if let Ok(current) = std::env::current_exe()
            && let Some(dir) = current.parent()
        {
            let sibling = dir.join(bin_name);
            if sibling.is_file() {
                return sibling;
            }
        }

        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let candidate = dir.join(bin_name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }

        PathBuf::from(bin_name)
    }

    async fn run_service_cli(args: Vec<std::ffi::OsString>) -> Result<(), CoreError> {
        let bin = Self::locate_service_binary();
        let output = tokio::process::Command::new(&bin)
            .args(&args)
            .output()
            .await
            .map_err(|err| {
                CoreError::Kernel(KernelError::Protocol(format!(
                    "failed to launch `{}`: {err}",
                    bin.display()
                )))
            })?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit status {}", output.status)
            };
            Err(CoreError::Kernel(KernelError::Protocol(format!(
                "verve-service command failed: {detail}"
            ))))
        }
    }
}

impl ServiceControlBackend for OsServiceControlBackend {
    fn install_service<'a>(
        &'a self,
        trusted_dirs: &'a [PathBuf],
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        Box::pin(async move {
            let mut args = vec![std::ffi::OsString::from("--install")];
            for dir in trusted_dirs {
                args.push(std::ffi::OsString::from("--trusted-dir"));
                args.push(dir.as_os_str().to_owned());
            }
            Self::run_service_cli(args).await
        })
    }

    fn uninstall_service<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
        Box::pin(async move {
            Self::run_service_cli(vec![std::ffi::OsString::from("--uninstall")]).await
        })
    }
}

/// Local IPC client speaking length-prefixed `verve-protocol` v1 frames to `verve-service`.
#[derive(Debug, Clone)]
pub struct ProtocolServiceIpcClient {
    endpoint: String,
}

impl ProtocolServiceIpcClient {
    /// Creates a new IPC client targeting `endpoint` (Named Pipe on Windows, Unix socket on Unix).
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    /// Returns the configured IPC endpoint path.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn execute(&self, request: &IpcRequest) -> Result<IpcResponse, KernelError> {
        let mut stream = connect_endpoint(&self.endpoint).await?;

        let handshake = IpcRequest::Handshake {
            client_proto_ver: PROTOCOL_VERSION,
        };
        write_frame(&mut stream, &handshake).await?;
        let ack = read_frame(&mut stream).await?;
        match ack {
            IpcResponse::HandshakeAck {
                server_proto_ver, ..
            } => {
                if server_proto_ver != PROTOCOL_VERSION {
                    return Err(KernelError::Protocol(format!(
                        "service protocol version mismatch: server {server_proto_ver}, client {PROTOCOL_VERSION}"
                    )));
                }
            }
            IpcResponse::Error { code, message } => return Err(map_protocol_error(code, message)),
            other => {
                return Err(KernelError::Protocol(format!(
                    "unexpected handshake response from service: {other:?}"
                )));
            }
        }

        write_frame(&mut stream, request).await?;
        let response = read_frame(&mut stream).await?;
        match response {
            IpcResponse::Error { code, message } => Err(map_protocol_error(code, message)),
            other => Ok(other),
        }
    }
}

impl ServiceIpcClient for ProtocolServiceIpcClient {
    fn start_remote<'a>(
        &'a self,
        kernel_id: &'a str,
        bin_path: &'a Path,
        work_dir: &'a Path,
        config_path: &'a Path,
        ctrl_endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            let req = IpcRequest::StartKernel {
                kernel_id: kernel_id.to_owned(),
                bin_path: bin_path.to_string_lossy().into_owned(),
                config_path: config_path.to_string_lossy().into_owned(),
                work_dir: work_dir.to_string_lossy().into_owned(),
                ctrl_endpoint: ctrl_endpoint.to_owned(),
            };
            match self.execute(&req).await? {
                IpcResponse::KernelStarted { pid, .. } => Ok(pid),
                other => Err(KernelError::Protocol(format!(
                    "expected KernelStarted response, got {other:?}"
                ))),
            }
        })
    }

    fn stop_remote<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
        Box::pin(async move {
            match self.execute(&IpcRequest::StopKernel).await? {
                IpcResponse::Stopped => Ok(()),
                other => Err(KernelError::Protocol(format!(
                    "expected Stopped response, got {other:?}"
                ))),
            }
        })
    }

    fn query_remote_status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteKernelStatus, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            match self.execute(&IpcRequest::QueryStatus).await? {
                IpcResponse::Status(status) => Ok(RemoteKernelStatus {
                    running: status.running,
                    pid: status.pid,
                    ctrl_endpoint: status.ctrl_endpoint,
                }),
                other => Err(KernelError::Protocol(format!(
                    "expected Status response, got {other:?}"
                ))),
            }
        })
    }

    fn read_remote_stderr_tail<'a>(
        &'a self,
        max_lines: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, KernelError>> + Send + 'a>> {
        Box::pin(async move {
            let max_lines_u16 = u16::try_from(max_lines).unwrap_or(u16::MAX);
            let req = IpcRequest::ReadStderrTail {
                max_lines: max_lines_u16,
            };
            match self.execute(&req).await? {
                IpcResponse::StderrTail { lines } => Ok(lines),
                other => Err(KernelError::Protocol(format!(
                    "expected StderrTail response, got {other:?}"
                ))),
            }
        })
    }
}

#[cfg(windows)]
type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
type IpcStream = tokio::net::UnixStream;

async fn connect_endpoint(endpoint: &str) -> Result<IpcStream, KernelError> {
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        const ERROR_PIPE_BUSY: i32 = 231;

        let mut attempts = 0u8;
        loop {
            match ClientOptions::new().open(endpoint) {
                Ok(client) => return Ok(client),
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 10 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(err) => return Err(KernelError::Io(err)),
            }
        }
    }

    #[cfg(unix)]
    {
        Ok(tokio::net::UnixStream::connect(endpoint).await?)
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request: &IpcRequest,
) -> Result<(), KernelError> {
    let bytes = encode_frame(request).map_err(map_codec_error)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<IpcResponse, KernelError> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    let mut read_so_far = 0usize;
    while read_so_far < FRAME_HEADER_LEN {
        let n = reader.read(&mut header[read_so_far..]).await?;
        if n == 0 {
            if read_so_far == 0 {
                return Err(KernelError::Protocol(
                    "IPC server closed connection before sending response".into(),
                ));
            }
            return Err(KernelError::Protocol(
                ProtocolError::TruncatedFrame {
                    expected: FRAME_HEADER_LEN,
                    actual: read_so_far,
                }
                .to_string(),
            ));
        }
        read_so_far += n;
    }

    let payload_len = u32::from_le_bytes(header) as usize;
    if payload_len > MAX_FRAME_LEN {
        return Err(KernelError::Protocol(
            ProtocolError::FrameTooLarge {
                len: payload_len,
                max: MAX_FRAME_LEN,
            }
            .to_string(),
        ));
    }

    let mut frame = vec![0u8; FRAME_HEADER_LEN + payload_len];
    frame[..FRAME_HEADER_LEN].copy_from_slice(&header);
    reader.read_exact(&mut frame[FRAME_HEADER_LEN..]).await?;

    decode_frame(&frame).map_err(map_codec_error)
}

fn map_codec_error(err: ProtocolError) -> KernelError {
    KernelError::Protocol(err.to_string())
}

fn map_protocol_error(code: ProtocolErrorCode, message: String) -> KernelError {
    match code {
        ProtocolErrorCode::NotRunning => KernelError::NotRunning,
        _ => KernelError::Protocol(format!("{code:?}: {message}")),
    }
}
