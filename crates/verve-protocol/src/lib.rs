//! Versioned local IPC protocol contract shared between `verve-app` and `verve-service`.
//!
//! Messages are framed with a 4-byte little-endian `u32` byte-length prefix followed
//! by a `postcard`-encoded payload bounded by [`MAX_FRAME_LEN`].

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current wire protocol version spoken by `verve-app` and `verve-service`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Maximum permitted payload length (in bytes) for a single IPC frame.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Length in bytes of the fixed little-endian frame header.
const FRAME_HEADER_LEN: usize = std::mem::size_of::<u32>();

/// Requests sent by `verve-app` to the privileged `verve-service` daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcRequest {
    /// Negotiates protocol compatibility and retrieves daemon capabilities.
    Handshake {
        /// Protocol version supported by the connecting client.
        client_proto_ver: u16,
    },
    /// Validates and spawns a proxy kernel child process.
    StartKernel {
        /// Logical kernel identifier (for example `"mihomo"`).
        kernel_id: String,
        /// Absolute path to the kernel binary inside a trusted directory.
        bin_path: String,
        /// Absolute path to the compiled runtime configuration file.
        config_path: String,
        /// Working directory passed to the kernel process.
        work_dir: String,
        /// Control plane endpoint (Named Pipe or Unix Domain Socket path).
        ctrl_endpoint: String,
    },
    /// Stops the currently supervised kernel process, if any.
    StopKernel,
    /// Restarts the supervised kernel process, optionally switching config path.
    RestartKernel {
        /// Optional replacement configuration path for the restarted kernel.
        config_path: Option<String>,
    },
    /// Queries current kernel process supervision status.
    QueryStatus,
    /// Reads up to `max_lines` recent stderr lines captured from the kernel process.
    ReadStderrTail {
        /// Maximum number of trailing lines to return.
        max_lines: u16,
    },
}

/// Responses sent by `verve-service` back to `verve-app`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcResponse {
    /// Confirms protocol compatibility and advertises daemon capabilities.
    HandshakeAck {
        /// Protocol version supported by the daemon.
        server_proto_ver: u16,
        /// Capabilities advertised by this service build.
        capabilities: ServiceCapabilities,
        /// PID of an already-running supervised kernel process, if present.
        running_pid: Option<u32>,
    },
    /// Confirms that the supervised kernel process was started.
    KernelStarted {
        /// Operating system process ID of the started kernel.
        pid: u32,
        /// Active control endpoint bound by the kernel.
        ctrl_endpoint: String,
    },
    /// Confirms that the supervised kernel process was stopped.
    Stopped,
    /// Reports current supervision state in response to [`IpcRequest::QueryStatus`].
    Status(KernelServiceStatus),
    /// Returns captured trailing stderr lines from the supervised kernel.
    StderrTail {
        /// Captured lines ordered oldest-to-newest.
        lines: Vec<String>,
    },
    /// Reports a structured remote execution or validation failure.
    Error {
        /// Machine-readable protocol error classification.
        code: ProtocolErrorCode,
        /// Human-readable diagnostic message.
        message: String,
    },
}

/// Capabilities advertised by `verve-service` during [`IpcRequest::Handshake`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServiceCapabilities {
    /// Whether the service can supervise kernels configured for TUN mode.
    pub supports_tun: bool,
    /// Whether the service supports in-place kernel restart commands.
    pub supports_restart: bool,
    /// Whether the service retains a bounded kernel stderr ring buffer.
    pub supports_stderr_tail: bool,
}

/// Current status of the kernel process supervised by `verve-service`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KernelServiceStatus {
    /// Whether a supervised kernel process is currently alive.
    pub running: bool,
    /// Operating system process ID when `running` is `true`.
    pub pid: Option<u32>,
    /// Active logical kernel identifier when running.
    pub kernel_id: Option<String>,
    /// Active control endpoint when running.
    pub ctrl_endpoint: Option<String>,
}

/// Stable error codes returned in [`IpcResponse::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolErrorCode {
    /// Client and server protocol versions are incompatible.
    UnsupportedVersion,
    /// Peer credentials failed OS-level authentication checks.
    Unauthorized,
    /// Request arguments were invalid or failed path/argument allowlist validation.
    InvalidArgument,
    /// Target kernel binary failed trusted path or SHA-256 verification.
    UntrustedBinary,
    /// A supervised kernel process is already running.
    AlreadyRunning,
    /// No supervised kernel process is currently running.
    NotRunning,
    /// Spawning or supervising the kernel process failed.
    SpawnFailed,
    /// Unexpected internal service error.
    Internal,
}

/// Errors produced while validating versions or framing IPC messages.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Client protocol version does not match [`PROTOCOL_VERSION`].
    #[error("unsupported protocol version {client} (expected {server})")]
    VersionMismatch {
        /// Protocol version presented by the client.
        client: u16,
        /// Protocol version required by the server.
        server: u16,
    },
    /// Encoded or declared payload length exceeds [`MAX_FRAME_LEN`].
    #[error("IPC frame size {len} bytes exceeds maximum {max} bytes")]
    FrameTooLarge {
        /// Actual or declared payload length in bytes.
        len: usize,
        /// Maximum allowed payload length in bytes.
        max: usize,
    },
    /// Input buffer ended before a complete frame header or payload arrived.
    #[error("truncated IPC frame: expected at least {expected} bytes, got {actual}")]
    TruncatedFrame {
        /// Minimum byte count required for the header or declared payload.
        expected: usize,
        /// Actual byte count present in the buffer.
        actual: usize,
    },
    /// Input buffer contained unexpected bytes after the declared frame payload.
    #[error("unexpected {trailing} trailing bytes after IPC frame")]
    TrailingBytes {
        /// Number of unconsumed trailing bytes.
        trailing: usize,
    },
    /// `postcard` serialization or deserialization failed.
    #[error("IPC codec error: {0}")]
    Codec(#[from] postcard::Error),
}

/// Validates that `client_proto_ver` matches [`PROTOCOL_VERSION`].
pub fn check_protocol_version(client_proto_ver: u16) -> Result<(), ProtocolError> {
    if client_proto_ver == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch {
            client: client_proto_ver,
            server: PROTOCOL_VERSION,
        })
    }
}

/// Encodes `message` as a length-prefixed `postcard` frame.
pub fn encode_frame<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolError> {
    let payload = postcard::to_allocvec(message)?;
    if payload.len() > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge {
            len: payload.len(),
            max: MAX_FRAME_LEN,
        });
    }

    let len_u32 = u32::try_from(payload.len()).map_err(|_| ProtocolError::FrameTooLarge {
        len: payload.len(),
        max: MAX_FRAME_LEN,
    })?;

    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&len_u32.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes a complete single length-prefixed `postcard` frame from `buf`.
pub fn decode_frame<T: DeserializeOwned>(buf: &[u8]) -> Result<T, ProtocolError> {
    if buf.len() < FRAME_HEADER_LEN {
        return Err(ProtocolError::TruncatedFrame {
            expected: FRAME_HEADER_LEN,
            actual: buf.len(),
        });
    }

    let header: [u8; FRAME_HEADER_LEN] = buf[..FRAME_HEADER_LEN]
        .try_into()
        .expect("slice length checked to equal FRAME_HEADER_LEN");
    let payload_len = u32::from_le_bytes(header) as usize;

    if payload_len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge {
            len: payload_len,
            max: MAX_FRAME_LEN,
        });
    }

    let total_len = FRAME_HEADER_LEN + payload_len;
    if buf.len() < total_len {
        return Err(ProtocolError::TruncatedFrame {
            expected: total_len,
            actual: buf.len(),
        });
    }
    if buf.len() > total_len {
        return Err(ProtocolError::TrailingBytes {
            trailing: buf.len() - total_len,
        });
    }

    Ok(postcard::from_bytes(&buf[FRAME_HEADER_LEN..total_len])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_six_request_primitives_and_responses() {
        let requests = [
            IpcRequest::Handshake {
                client_proto_ver: PROTOCOL_VERSION,
            },
            IpcRequest::StartKernel {
                kernel_id: "mihomo".into(),
                bin_path: "/usr/local/bin/mihomo".into(),
                config_path: "/var/lib/verve/active.yaml".into(),
                work_dir: "/var/lib/verve".into(),
                ctrl_endpoint: "/run/verve/mihomo.sock".into(),
            },
            IpcRequest::StopKernel,
            IpcRequest::RestartKernel {
                config_path: Some("/var/lib/verve/next.yaml".into()),
            },
            IpcRequest::QueryStatus,
            IpcRequest::ReadStderrTail { max_lines: 128 },
        ];

        for req in &requests {
            let frame = encode_frame(req).expect("request encoding must succeed");
            let decoded: IpcRequest = decode_frame(&frame).expect("request decoding must succeed");
            assert_eq!(&decoded, req);
        }

        let responses = [
            IpcResponse::HandshakeAck {
                server_proto_ver: PROTOCOL_VERSION,
                capabilities: ServiceCapabilities {
                    supports_tun: true,
                    supports_restart: true,
                    supports_stderr_tail: true,
                },
                running_pid: Some(4242),
            },
            IpcResponse::KernelStarted {
                pid: 4242,
                ctrl_endpoint: r"\\.\pipe\verve-mihomo".into(),
            },
            IpcResponse::Stopped,
            IpcResponse::Status(KernelServiceStatus {
                running: true,
                pid: Some(4242),
                kernel_id: Some("mihomo".into()),
                ctrl_endpoint: Some(r"\\.\pipe\verve-mihomo".into()),
            }),
            IpcResponse::StderrTail {
                lines: vec!["line 1".into(), "line 2".into()],
            },
            IpcResponse::Error {
                code: ProtocolErrorCode::UntrustedBinary,
                message: "hash mismatch".into(),
            },
        ];

        for resp in &responses {
            let frame = encode_frame(resp).expect("response encoding must succeed");
            let decoded: IpcResponse =
                decode_frame(&frame).expect("response decoding must succeed");
            assert_eq!(&decoded, resp);
        }
    }

    #[test]
    fn protocol_version_check_accepts_current_and_rejects_mismatch() {
        assert!(check_protocol_version(PROTOCOL_VERSION).is_ok());

        let err = check_protocol_version(PROTOCOL_VERSION + 1)
            .expect_err("mismatched protocol version must fail");
        assert!(matches!(
            err,
            ProtocolError::VersionMismatch {
                client,
                server: PROTOCOL_VERSION
            } if client == PROTOCOL_VERSION + 1
        ));
    }

    #[test]
    fn decode_rejects_truncated_buffers_and_trailing_bytes() {
        assert!(matches!(
            decode_frame::<IpcRequest>(&[1, 2]),
            Err(ProtocolError::TruncatedFrame {
                expected: 4,
                actual: 2
            })
        ));

        let mut frame = encode_frame(&IpcRequest::QueryStatus).unwrap();
        let truncated = &frame[..frame.len() - 1];
        assert!(matches!(
            decode_frame::<IpcRequest>(truncated),
            Err(ProtocolError::TruncatedFrame { .. })
        ));

        frame.push(0xAA);
        assert!(matches!(
            decode_frame::<IpcRequest>(&frame),
            Err(ProtocolError::TrailingBytes { trailing: 1 })
        ));
    }

    #[test]
    fn codec_rejects_oversized_frames_on_encode_and_decode() {
        let oversized = IpcRequest::StartKernel {
            kernel_id: "mihomo".into(),
            bin_path: "a".repeat(MAX_FRAME_LEN + 1),
            config_path: "cfg.yaml".into(),
            work_dir: "work".into(),
            ctrl_endpoint: "pipe".into(),
        };
        assert!(matches!(
            encode_frame(&oversized),
            Err(ProtocolError::FrameTooLarge {
                max: MAX_FRAME_LEN,
                ..
            })
        ));

        let declared_len = u32::try_from(MAX_FRAME_LEN + 1).unwrap();
        let mut bad_frame = Vec::from(declared_len.to_le_bytes());
        bad_frame.extend_from_slice(&[0u8; 8]);
        assert!(matches!(
            decode_frame::<IpcRequest>(&bad_frame),
            Err(ProtocolError::FrameTooLarge {
                len,
                max: MAX_FRAME_LEN
            }) if len == MAX_FRAME_LEN + 1
        ));
    }
}
