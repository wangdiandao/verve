//! Anti-zombie kernel process supervisor and IPC request dispatcher.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use verve_protocol::{
    IpcRequest, IpcResponse, KernelServiceStatus, PROTOCOL_VERSION, ProtocolErrorCode,
    ServiceCapabilities, check_protocol_version,
};

use crate::allowlist::{SecurityPolicy, ValidatedStartArgs};

/// Default bounded capacity of retained stderr lines.
const STDERR_RING_CAPACITY: usize = 256;

#[derive(Debug)]
struct StderrRingBuffer {
    capacity: usize,
    lines: VecDeque<String>,
}

impl StderrRingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lines: VecDeque::with_capacity(capacity.min(1024)),
        }
    }

    fn push_line(&mut self, line: String) {
        if self.capacity == 0 {
            return;
        }
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn tail(&self, max_lines: usize) -> Vec<String> {
        if max_lines == 0 || self.lines.is_empty() {
            return Vec::new();
        }
        let skip = self.lines.len().saturating_sub(max_lines);
        self.lines.iter().skip(skip).cloned().collect()
    }
}

#[cfg(windows)]
mod windows_job {
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    #[derive(Debug)]
    pub(super) struct KillOnCloseJob(HANDLE);

    // SAFETY: Windows Job Object handles are kernel objects safe to send/drop across threads.
    unsafe impl Send for KillOnCloseJob {}
    unsafe impl Sync for KillOnCloseJob {}

    impl KillOnCloseJob {
        pub(super) fn attach(process_handle: RawHandle) -> Result<Self, std::io::Error> {
            // SAFETY: Null security attributes and unnamed job object are valid for CreateJobObjectW.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            // SAFETY: `info` is properly initialized for `JobObjectExtendedLimitInformation`.
            let set_ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .expect("job info size fits in u32"),
                )
            };
            if set_ok == 0 {
                let err = std::io::Error::last_os_error();
                unsafe { CloseHandle(job) };
                return Err(err);
            }

            // SAFETY: `job` is valid and `process_handle` belongs to the newly spawned child.
            let assign_ok = unsafe { AssignProcessToJobObject(job, process_handle as HANDLE) };
            if assign_ok == 0 {
                let err = std::io::Error::last_os_error();
                unsafe { CloseHandle(job) };
                return Err(err);
            }

            Ok(Self(job))
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is an owned Job Object handle from `CreateJobObjectW`.
                unsafe { CloseHandle(self.0) };
            }
        }
    }
}

#[derive(Debug)]
struct SupervisedChild {
    kernel_id: String,
    bin_path: PathBuf,
    work_dir: PathBuf,
    config_path: PathBuf,
    ctrl_endpoint: String,
    pid: u32,
    child: Child,
    stderr_task: Option<JoinHandle<()>>,
    #[cfg(windows)]
    job: Option<windows_job::KillOnCloseJob>,
}

/// Privileged single-kernel process supervisor serving [`IpcRequest`] primitives.
#[derive(Debug)]
pub struct ServiceSupervisor {
    policy: SecurityPolicy,
    active: Option<SupervisedChild>,
    stderr_ring: Arc<Mutex<StderrRingBuffer>>,
}

impl ServiceSupervisor {
    /// Creates a new supervisor enforcing `policy`.
    pub fn new(policy: SecurityPolicy) -> Self {
        Self {
            policy,
            active: None,
            stderr_ring: Arc::new(Mutex::new(StderrRingBuffer::new(STDERR_RING_CAPACITY))),
        }
    }

    /// Handles a single decoded [`IpcRequest`] and produces an [`IpcResponse`].
    pub async fn handle_request(&mut self, request: IpcRequest) -> IpcResponse {
        match request {
            IpcRequest::Handshake { client_proto_ver } => self.handle_handshake(client_proto_ver),
            IpcRequest::StartKernel {
                kernel_id,
                bin_path,
                config_path,
                work_dir,
                ctrl_endpoint,
            } => {
                self.handle_start(
                    &kernel_id,
                    &bin_path,
                    &work_dir,
                    &config_path,
                    &ctrl_endpoint,
                )
                .await
            }
            IpcRequest::StopKernel => self.handle_stop().await,
            IpcRequest::RestartKernel { config_path } => {
                self.handle_restart(config_path.as_deref()).await
            }
            IpcRequest::QueryStatus => self.handle_query_status(),
            IpcRequest::ReadStderrTail { max_lines } => self.handle_read_stderr(max_lines),
        }
    }

    /// Gracefully stops any running supervised child process during daemon shutdown.
    pub async fn shutdown(&mut self) {
        if let Some(child) = self.active.take() {
            stop_child(child).await;
        }
    }

    fn reap_if_exited(&mut self) {
        let exited = match self.active.as_mut() {
            Some(entry) => matches!(entry.child.try_wait(), Ok(Some(_)) | Err(_)),
            None => false,
        };
        if exited && let Some(mut entry) = self.active.take() {
            #[cfg(windows)]
            {
                entry.job = None;
            }
            if let Some(task) = entry.stderr_task.take() {
                task.abort();
            }
        }
    }

    fn running_pid(&mut self) -> Option<u32> {
        self.reap_if_exited();
        self.active.as_ref().map(|entry| entry.pid)
    }

    fn handle_handshake(&mut self, client_proto_ver: u16) -> IpcResponse {
        if let Err(err) = check_protocol_version(client_proto_ver) {
            return IpcResponse::Error {
                code: ProtocolErrorCode::UnsupportedVersion,
                message: err.to_string(),
            };
        }

        IpcResponse::HandshakeAck {
            server_proto_ver: PROTOCOL_VERSION,
            capabilities: ServiceCapabilities {
                supports_tun: true,
                supports_restart: true,
                supports_stderr_tail: true,
            },
            running_pid: self.running_pid(),
        }
    }

    async fn handle_start(
        &mut self,
        kernel_id: &str,
        bin_path: &str,
        work_dir: &str,
        config_path: &str,
        ctrl_endpoint: &str,
    ) -> IpcResponse {
        if let Some(pid) = self.running_pid() {
            return IpcResponse::Error {
                code: ProtocolErrorCode::AlreadyRunning,
                message: format!("kernel process is already running (pid {pid})"),
            };
        }

        let validated = match self.policy.validate_start(
            kernel_id,
            bin_path,
            work_dir,
            config_path,
            ctrl_endpoint,
        ) {
            Ok(args) => args,
            Err(err) => {
                return IpcResponse::Error {
                    code: err.code,
                    message: err.message,
                };
            }
        };

        match self.spawn_validated(validated) {
            Ok((pid, endpoint)) => IpcResponse::KernelStarted {
                pid,
                ctrl_endpoint: endpoint,
            },
            Err(message) => IpcResponse::Error {
                code: ProtocolErrorCode::SpawnFailed,
                message,
            },
        }
    }

    async fn handle_stop(&mut self) -> IpcResponse {
        self.reap_if_exited();
        let Some(entry) = self.active.take() else {
            return IpcResponse::Error {
                code: ProtocolErrorCode::NotRunning,
                message: "no supervised kernel process is running".into(),
            };
        };

        stop_child(entry).await;
        IpcResponse::Stopped
    }

    async fn handle_restart(&mut self, next_config_path: Option<&str>) -> IpcResponse {
        self.reap_if_exited();
        let Some(current) = self.active.as_ref() else {
            return IpcResponse::Error {
                code: ProtocolErrorCode::NotRunning,
                message: "no supervised kernel process is running".into(),
            };
        };

        let kernel_id = current.kernel_id.clone();
        let bin_path = current.bin_path.clone();
        let work_dir = current.work_dir.clone();
        let ctrl_endpoint = current.ctrl_endpoint.clone();
        let target_config = match next_config_path {
            Some(path) => match self.policy.validate_config_path(path) {
                Ok(validated) => validated,
                Err(err) => {
                    return IpcResponse::Error {
                        code: err.code,
                        message: err.message,
                    };
                }
            },
            None => current.config_path.clone(),
        };

        // Re-validate before stopping so an invalid restart request keeps the active kernel online.
        let validated = match self.policy.validate_start(
            &kernel_id,
            &bin_path.to_string_lossy(),
            &work_dir.to_string_lossy(),
            &target_config.to_string_lossy(),
            &ctrl_endpoint,
        ) {
            Ok(args) => args,
            Err(err) => {
                return IpcResponse::Error {
                    code: err.code,
                    message: err.message,
                };
            }
        };

        if let Some(entry) = self.active.take() {
            stop_child(entry).await;
        }

        match self.spawn_validated(validated) {
            Ok((pid, endpoint)) => IpcResponse::KernelStarted {
                pid,
                ctrl_endpoint: endpoint,
            },
            Err(message) => IpcResponse::Error {
                code: ProtocolErrorCode::SpawnFailed,
                message,
            },
        }
    }

    fn handle_query_status(&mut self) -> IpcResponse {
        self.reap_if_exited();
        match &self.active {
            Some(entry) => IpcResponse::Status(KernelServiceStatus {
                running: true,
                pid: Some(entry.pid),
                kernel_id: Some(entry.kernel_id.clone()),
                ctrl_endpoint: Some(entry.ctrl_endpoint.clone()),
            }),
            None => IpcResponse::Status(KernelServiceStatus::default()),
        }
    }

    fn handle_read_stderr(&self, max_lines: u16) -> IpcResponse {
        let lines = self
            .stderr_ring
            .lock()
            .expect("stderr ring mutex poisoned")
            .tail(max_lines as usize);
        IpcResponse::StderrTail { lines }
    }

    fn spawn_validated(&mut self, args: ValidatedStartArgs) -> Result<(u32, String), String> {
        if let Ok(mut ring) = self.stderr_ring.lock() {
            ring.lines.clear();
        }

        let mut cmd = build_kernel_command(&args.bin_path, &args.work_dir, &args.config_path);
        let mut child = cmd
            .spawn()
            .map_err(|err| format!("failed to spawn kernel process: {err}"))?;

        let pid = child
            .id()
            .ok_or_else(|| "spawned kernel process has no PID".to_owned())?;

        #[cfg(windows)]
        let job = child.raw_handle().and_then(|handle| {
            match windows_job::KillOnCloseJob::attach(handle) {
                Ok(job) => Some(job),
                Err(err) => {
                    tracing::warn!(
                        pid,
                        error = %err,
                        "failed to attach kernel child to Windows kill-on-close Job Object"
                    );
                    None
                }
            }
        });

        let stderr_task = child.stderr.take().map(|stderr| {
            let ring = Arc::clone(&self.stderr_ring);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut guard) = ring.lock() {
                        guard.push_line(line);
                    }
                }
            })
        });

        let endpoint = args.ctrl_endpoint.clone();
        self.active = Some(SupervisedChild {
            kernel_id: args.kernel_id,
            bin_path: args.bin_path,
            work_dir: args.work_dir,
            config_path: args.config_path,
            ctrl_endpoint: args.ctrl_endpoint,
            pid,
            child,
            stderr_task,
            #[cfg(windows)]
            job,
        });

        Ok((pid, endpoint))
    }
}

impl Drop for ServiceSupervisor {
    fn drop(&mut self) {
        if let Some(mut entry) = self.active.take() {
            let _ = entry.child.start_kill();
            if let Some(task) = entry.stderr_task.take() {
                task.abort();
            }
        }
    }
}

fn build_kernel_command(bin_path: &Path, work_dir: &Path, config_path: &Path) -> Command {
    let mut cmd = Command::new(bin_path);
    // Fixed argument template: `<bin_path> -d <work_dir> -f <config_path>`
    cmd.arg("-d")
        .arg(work_dir)
        .arg("-f")
        .arg(config_path)
        .current_dir(work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGTERM)` is async-signal-safe and ensures the kernel
        // child receives SIGTERM immediately if `verve-service` terminates unexpectedly.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    cmd
}

async fn stop_child(mut entry: SupervisedChild) {
    if matches!(entry.child.try_wait(), Ok(None)) {
        let _ = entry.child.kill().await;
        let _ = entry.child.wait().await;
    }
    #[cfg(windows)]
    {
        // Drop Job Object before awaiting stderr EOF so inherited pipe handles are closed.
        entry.job = None;
    }
    if let Some(mut task) = entry.stderr_task.take()
        && tokio::time::timeout(std::time::Duration::from_millis(250), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handshake_validates_protocol_version() {
        let mut sup = ServiceSupervisor::new(SecurityPolicy::default());

        let ack = sup
            .handle_request(IpcRequest::Handshake {
                client_proto_ver: PROTOCOL_VERSION,
            })
            .await;
        assert!(matches!(
            ack,
            IpcResponse::HandshakeAck {
                server_proto_ver: PROTOCOL_VERSION,
                running_pid: None,
                ..
            }
        ));

        let err = sup
            .handle_request(IpcRequest::Handshake {
                client_proto_ver: PROTOCOL_VERSION + 1,
            })
            .await;
        assert!(matches!(
            err,
            IpcResponse::Error {
                code: ProtocolErrorCode::UnsupportedVersion,
                ..
            }
        ));
    }
}
