//! Static process supervision strategies (`Direct` and `Service`) for proxy kernels.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::KernelError;

/// Default maximum number of stderr lines retained in memory per supervised kernel.
const DEFAULT_STDERR_CAPACITY: usize = 256;

/// Bounded ring buffer of trailing stderr lines.
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

    /// RAII handle to a Windows Job Object configured with `KILL_ON_JOB_CLOSE`.
    #[derive(Debug)]
    pub(super) struct KillOnCloseJob(HANDLE);

    // SAFETY: Windows Job Object handles are kernel objects safe to send/drop across threads.
    unsafe impl Send for KillOnCloseJob {}
    unsafe impl Sync for KillOnCloseJob {}

    impl KillOnCloseJob {
        pub(super) fn attach(process_handle: RawHandle) -> Result<Self, std::io::Error> {
            // SAFETY: null security attributes and unnamed job object are valid for CreateJobObjectW.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            // SAFETY: `info` is properly initialized and sized for JobObjectExtendedLimitInformation.
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

            // SAFETY: `job` is valid and `process_handle` comes from a live child process.
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
                // SAFETY: `self.0` is an owned Job Object handle created by `CreateJobObjectW`.
                unsafe { CloseHandle(self.0) };
            }
        }
    }
}

/// Process supervisor strategy selected via static enum dispatch.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum KernelSupervisor {
    /// Spawns and supervises the kernel directly as an unprivileged child process.
    Direct(DirectProcessSupervisor),
    /// Delegates privileged kernel supervision over local IPC to `verve-service`.
    Service(DaemonIpcSupervisor),
}

impl KernelSupervisor {
    /// Starts the kernel process with the given binary, working directory, and config path.
    pub async fn start(
        &mut self,
        bin_path: &Path,
        work_dir: &Path,
        config_path: &Path,
    ) -> Result<u32, KernelError> {
        match self {
            Self::Direct(sup) => sup.start(bin_path, work_dir, config_path).await,
            Self::Service(sup) => sup.start(bin_path, work_dir, config_path).await,
        }
    }

    /// Stops the currently running kernel process, if any.
    pub async fn stop(&mut self) -> Result<(), KernelError> {
        match self {
            Self::Direct(sup) => sup.stop().await,
            Self::Service(sup) => sup.stop().await,
        }
    }

    /// Returns the OS process ID if the kernel is currently running.
    pub fn pid(&self) -> Option<u32> {
        match self {
            Self::Direct(sup) => sup.pid(),
            Self::Service(sup) => sup.pid(),
        }
    }

    /// Returns `true` if a supervised kernel process is still alive.
    pub fn is_running(&self) -> bool {
        match self {
            Self::Direct(sup) => sup.is_running(),
            Self::Service(sup) => sup.is_running(),
        }
    }

    /// Returns up to `max_lines` trailing stderr lines ordered oldest-to-newest.
    pub fn stderr_tail(&self, max_lines: usize) -> Vec<String> {
        match self {
            Self::Direct(sup) => sup.stderr_tail(max_lines),
            Self::Service(sup) => sup.stderr_tail(max_lines),
        }
    }
}

/// Direct unprivileged child-process supervisor with bounded stderr capture and anti-orphan cleanup.
#[derive(Debug)]
pub struct DirectProcessSupervisor {
    child: Mutex<Option<Child>>,
    stderr_ring: Arc<Mutex<StderrRingBuffer>>,
    stderr_task: Option<JoinHandle<()>>,
    #[cfg(windows)]
    job: Option<windows_job::KillOnCloseJob>,
}

impl Default for DirectProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectProcessSupervisor {
    /// Creates a new direct process supervisor with the default stderr ring buffer capacity.
    pub fn new() -> Self {
        Self::with_stderr_capacity(DEFAULT_STDERR_CAPACITY)
    }

    /// Creates a new direct process supervisor retaining at most `stderr_capacity` stderr lines.
    pub fn with_stderr_capacity(stderr_capacity: usize) -> Self {
        Self {
            child: Mutex::new(None),
            stderr_ring: Arc::new(Mutex::new(StderrRingBuffer::new(stderr_capacity))),
            stderr_task: None,
            #[cfg(windows)]
            job: None,
        }
    }

    /// Spawns `bin_path` with standard kernel flags (`-d <work_dir> -f <config_path>`).
    pub async fn start(
        &mut self,
        bin_path: &Path,
        work_dir: &Path,
        config_path: &Path,
    ) -> Result<u32, KernelError> {
        let args = [
            OsStr::new("-d"),
            work_dir.as_os_str(),
            OsStr::new("-f"),
            config_path.as_os_str(),
        ];
        self.start_with_args(bin_path, work_dir, args).await
    }

    /// Spawns `bin_path` in `work_dir` with explicit command-line arguments.
    pub async fn start_with_args<I, S>(
        &mut self,
        bin_path: &Path,
        work_dir: &Path,
        args: I,
    ) -> Result<u32, KernelError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        if let Some(pid) = self.pid() {
            return Err(KernelError::AlreadyRunning { pid });
        }

        self.cleanup_finished_tasks().await;
        if let Ok(mut ring) = self.stderr_ring.lock() {
            ring.lines.clear();
        }

        let mut cmd = Command::new(bin_path);
        cmd.args(args)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(KernelError::ProcessSpawn)?;
        let pid = child.id().ok_or_else(|| {
            KernelError::ProcessSpawn(std::io::Error::other("spawned child process has no PID"))
        })?;

        #[cfg(windows)]
        {
            self.job = child.raw_handle().and_then(|handle| {
                match windows_job::KillOnCloseJob::attach(handle) {
                    Ok(job) => Some(job),
                    Err(err) => {
                        tracing::warn!(
                            pid,
                            error = %err,
                            "failed to attach child process to Windows kill-on-close job object"
                        );
                        None
                    }
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let ring = Arc::clone(&self.stderr_ring);
            self.stderr_task = Some(tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut guard) = ring.lock() {
                        guard.push_line(line);
                    }
                }
            }));
        }

        *self.child.get_mut().expect("child mutex poisoned") = Some(child);
        Ok(pid)
    }

    /// Stops the supervised child process and waits for it and its stderr reader to exit.
    pub async fn stop(&mut self) -> Result<(), KernelError> {
        let maybe_child = self.child.get_mut().expect("child mutex poisoned").take();
        if let Some(mut child) = maybe_child
            && child.try_wait()?.is_none()
        {
            if let Err(err) = child.kill().await
                && err.kind() != std::io::ErrorKind::InvalidInput
            {
                return Err(KernelError::Io(err));
            }
            let _ = child.wait().await?;
        }

        #[cfg(windows)]
        {
            // Closing the kill-on-close Job Object before awaiting stderr EOF terminates any
            // descendant processes that may have inherited the stderr pipe handle.
            self.job = None;
        }
        self.cleanup_finished_tasks().await;
        Ok(())
    }

    /// Returns the OS process ID if the child process is still alive.
    pub fn pid(&self) -> Option<u32> {
        let mut guard = self.child.lock().expect("child mutex poisoned");
        let exited = {
            let child = guard.as_mut()?;
            matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        };
        if exited {
            *guard = None;
            return None;
        }
        guard.as_ref().and_then(Child::id)
    }

    /// Returns `true` if the child process is still alive.
    pub fn is_running(&self) -> bool {
        self.pid().is_some()
    }

    /// Returns up to `max_lines` trailing stderr lines ordered oldest-to-newest.
    pub fn stderr_tail(&self, max_lines: usize) -> Vec<String> {
        self.stderr_ring
            .lock()
            .expect("stderr ring mutex poisoned")
            .tail(max_lines)
    }

    async fn cleanup_finished_tasks(&mut self) {
        if let Some(mut task) = self.stderr_task.take()
            && tokio::time::timeout(std::time::Duration::from_millis(250), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for DirectProcessSupervisor {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

/// Kernel process status snapshot reported by a remote [`ServiceIpcClient`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteKernelStatus {
    /// Whether a supervised kernel process is currently alive.
    pub running: bool,
    /// Operating system process ID when `running` is `true`.
    pub pid: Option<u32>,
    /// Active control endpoint bound by the supervised kernel, if any.
    pub ctrl_endpoint: Option<String>,
}

/// Dyn-compatible IPC client contract for delegating kernel lifecycle to `verve-service`.
///
/// Defined in `verve-kernel` without pulling `verve-protocol` into the lower crate DAG.
pub trait ServiceIpcClient: Send + Sync + 'static {
    /// Requests the remote privileged daemon to validate and spawn the kernel process.
    fn start_remote<'a>(
        &'a self,
        kernel_id: &'a str,
        bin_path: &'a Path,
        work_dir: &'a Path,
        config_path: &'a Path,
        ctrl_endpoint: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, KernelError>> + Send + 'a>>;

    /// Requests the remote privileged daemon to stop the supervised kernel process.
    fn stop_remote<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>>;

    /// Queries the current remote kernel supervision status.
    fn query_remote_status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteKernelStatus, KernelError>> + Send + 'a>>;

    /// Fetches up to `max_lines` trailing stderr lines from the remote supervised kernel.
    fn read_remote_stderr_tail<'a>(
        &'a self,
        max_lines: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, KernelError>> + Send + 'a>>;
}

/// Supervisor that delegates privileged kernel execution over local IPC to `verve-service`.
pub struct DaemonIpcSupervisor {
    client: Option<Arc<dyn ServiceIpcClient>>,
    kernel_id: String,
    ctrl_endpoint: String,
    running: bool,
    pid: Option<u32>,
    cached_stderr: Mutex<Vec<String>>,
}

impl fmt::Debug for DaemonIpcSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonIpcSupervisor")
            .field("configured", &self.client.is_some())
            .field("kernel_id", &self.kernel_id)
            .field("ctrl_endpoint", &self.ctrl_endpoint)
            .field("running", &self.running)
            .field("pid", &self.pid)
            .finish()
    }
}

impl Default for DaemonIpcSupervisor {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl DaemonIpcSupervisor {
    /// Creates an unconfigured daemon supervisor whose operations return [`KernelError::ServiceUnavailable`].
    pub fn unconfigured() -> Self {
        Self {
            client: None,
            kernel_id: String::from("mihomo"),
            ctrl_endpoint: String::new(),
            running: false,
            pid: None,
            cached_stderr: Mutex::new(Vec::new()),
        }
    }

    /// Creates a configured daemon IPC supervisor backed by `client`.
    pub fn new(
        client: Arc<dyn ServiceIpcClient>,
        kernel_id: impl Into<String>,
        ctrl_endpoint: impl Into<String>,
    ) -> Self {
        Self {
            client: Some(client),
            kernel_id: kernel_id.into(),
            ctrl_endpoint: ctrl_endpoint.into(),
            running: false,
            pid: None,
            cached_stderr: Mutex::new(Vec::new()),
        }
    }

    /// Returns the logical kernel identifier passed to the remote service.
    pub fn kernel_id(&self) -> &str {
        &self.kernel_id
    }

    /// Returns the control endpoint string configured for this supervisor, if non-empty.
    pub fn ctrl_endpoint(&self) -> Option<&str> {
        (!self.ctrl_endpoint.is_empty()).then_some(self.ctrl_endpoint.as_str())
    }

    /// Delegates kernel startup to the remote `verve-service` over IPC.
    pub async fn start(
        &mut self,
        bin_path: &Path,
        work_dir: &Path,
        config_path: &Path,
    ) -> Result<u32, KernelError> {
        let client = self
            .client
            .as_ref()
            .ok_or(KernelError::ServiceUnavailable)?;
        let pid = client
            .start_remote(
                &self.kernel_id,
                bin_path,
                work_dir,
                config_path,
                &self.ctrl_endpoint,
            )
            .await?;
        self.running = true;
        self.pid = Some(pid);
        if let Ok(mut guard) = self.cached_stderr.lock() {
            guard.clear();
        }
        Ok(pid)
    }

    /// Delegates stopping the remote kernel process to `verve-service`.
    pub async fn stop(&mut self) -> Result<(), KernelError> {
        let client = self
            .client
            .as_ref()
            .ok_or(KernelError::ServiceUnavailable)?;
        client.stop_remote().await?;
        self.running = false;
        self.pid = None;
        Ok(())
    }

    /// Refreshes cached running/PID status from the remote daemon.
    pub async fn refresh_status(&mut self) -> Result<RemoteKernelStatus, KernelError> {
        let client = self
            .client
            .as_ref()
            .ok_or(KernelError::ServiceUnavailable)?;
        let status = client.query_remote_status().await?;
        self.running = status.running;
        self.pid = status.pid;
        if let Some(endpoint) = &status.ctrl_endpoint {
            self.ctrl_endpoint = endpoint.clone();
        }
        Ok(status)
    }

    /// Fetches up to `max_lines` trailing stderr lines from the remote daemon and updates cache.
    pub async fn fetch_stderr_tail(&self, max_lines: usize) -> Result<Vec<String>, KernelError> {
        let client = self
            .client
            .as_ref()
            .ok_or(KernelError::ServiceUnavailable)?;
        let lines = client.read_remote_stderr_tail(max_lines).await?;
        if let Ok(mut guard) = self.cached_stderr.lock() {
            *guard = lines.clone();
        }
        Ok(lines)
    }

    /// Returns the cached OS process ID if the remote kernel is thought to be running.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Returns the cached running state from the last `start`, `stop`, or `refresh_status`.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Returns up to `max_lines` cached trailing stderr lines fetched via [`Self::fetch_stderr_tail`].
    pub fn stderr_tail(&self, max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }
        let guard = self
            .cached_stderr
            .lock()
            .expect("cached stderr mutex poisoned");
        let skip = guard.len().saturating_sub(max_lines);
        guard.iter().skip(skip).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_ring_buffer_keeps_bounded_tail() {
        let mut ring = StderrRingBuffer::new(3);
        for idx in 1..=5 {
            ring.push_line(format!("line-{idx}"));
        }

        assert_eq!(ring.tail(10), vec!["line-3", "line-4", "line-5"]);
        assert_eq!(ring.tail(2), vec!["line-4", "line-5"]);
        assert!(ring.tail(0).is_empty());

        let mut zero_cap = StderrRingBuffer::new(0);
        zero_cap.push_line("ignored".into());
        assert!(zero_cap.tail(5).is_empty());
    }

    #[tokio::test]
    async fn direct_supervisor_spawns_captures_stderr_and_stops_cleanly() {
        let work_dir = std::env::temp_dir();
        let mut supervisor = DirectProcessSupervisor::with_stderr_capacity(2);

        #[cfg(windows)]
        let (bin, args): (&str, Vec<&str>) = (
            "cmd",
            vec![
                "/C",
                "echo err-1 1>&2 & echo err-2 1>&2 & echo err-3 1>&2 & ping -n 6 127.0.0.1 >NUL",
            ],
        );

        #[cfg(not(windows))]
        let (bin, args): (&str, Vec<&str>) = (
            "sh",
            vec![
                "-c",
                "echo err-1 >&2; echo err-2 >&2; echo err-3 >&2; sleep 5",
            ],
        );

        let pid = supervisor
            .start_with_args(Path::new(bin), &work_dir, args)
            .await
            .expect("short-lived command should spawn");
        assert!(pid > 0);
        assert!(supervisor.is_running());
        assert_eq!(supervisor.pid(), Some(pid));

        // Wait briefly for the spawned shell to flush stderr lines.
        for _ in 0..40 {
            if supervisor.stderr_tail(2).len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        supervisor.stop().await.expect("stop should succeed");
        assert!(!supervisor.is_running());
        assert_eq!(supervisor.pid(), None);

        let tail = supervisor
            .stderr_tail(2)
            .into_iter()
            .map(|line| line.trim().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(tail, vec!["err-2", "err-3"]);
    }

    #[tokio::test]
    async fn daemon_supervisor_unconfigured_returns_service_unavailable() {
        let mut supervisor = KernelSupervisor::Service(DaemonIpcSupervisor::unconfigured());
        assert!(!supervisor.is_running());
        assert_eq!(supervisor.pid(), None);
        assert!(supervisor.stderr_tail(10).is_empty());

        let err = supervisor
            .start(
                Path::new("mihomo"),
                Path::new("."),
                Path::new("config.yaml"),
            )
            .await
            .expect_err("unconfigured service supervisor must report unavailable");
        assert!(matches!(err, KernelError::ServiceUnavailable));

        let stop_err = supervisor
            .stop()
            .await
            .expect_err("unconfigured service supervisor must report unavailable on stop");
        assert!(matches!(stop_err, KernelError::ServiceUnavailable));
    }

    struct MockServiceIpcClient {
        state: Mutex<RemoteKernelStatus>,
        stderr: Mutex<Vec<String>>,
    }

    impl MockServiceIpcClient {
        fn new(stderr_lines: Vec<String>) -> Self {
            Self {
                state: Mutex::new(RemoteKernelStatus::default()),
                stderr: Mutex::new(stderr_lines),
            }
        }
    }

    impl ServiceIpcClient for MockServiceIpcClient {
        fn start_remote<'a>(
            &'a self,
            _kernel_id: &'a str,
            _bin_path: &'a Path,
            _work_dir: &'a Path,
            _config_path: &'a Path,
            ctrl_endpoint: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<u32, KernelError>> + Send + 'a>> {
            Box::pin(async move {
                let mut guard = self.state.lock().unwrap();
                if guard.running {
                    return Err(KernelError::AlreadyRunning {
                        pid: guard.pid.unwrap_or(0),
                    });
                }
                guard.running = true;
                guard.pid = Some(9527);
                guard.ctrl_endpoint = Some(ctrl_endpoint.to_owned());
                Ok(9527)
            })
        }

        fn stop_remote<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<(), KernelError>> + Send + 'a>> {
            Box::pin(async move {
                let mut guard = self.state.lock().unwrap();
                if !guard.running {
                    return Err(KernelError::NotRunning);
                }
                guard.running = false;
                guard.pid = None;
                Ok(())
            })
        }

        fn query_remote_status<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<RemoteKernelStatus, KernelError>> + Send + 'a>>
        {
            Box::pin(async move { Ok(self.state.lock().unwrap().clone()) })
        }

        fn read_remote_stderr_tail<'a>(
            &'a self,
            max_lines: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, KernelError>> + Send + 'a>> {
            Box::pin(async move {
                let guard = self.stderr.lock().unwrap();
                let skip = guard.len().saturating_sub(max_lines);
                Ok(guard.iter().skip(skip).cloned().collect())
            })
        }
    }

    #[tokio::test]
    async fn daemon_supervisor_delegates_lifecycle_and_status_to_ipc_client() {
        let client = Arc::new(MockServiceIpcClient::new(vec![
            "boot".into(),
            "ready".into(),
            "serving".into(),
        ]));
        let mut sup = DaemonIpcSupervisor::new(client.clone(), "mihomo", r"\\.\pipe\verve-test");

        assert!(!sup.is_running());
        assert_eq!(sup.pid(), None);
        assert_eq!(sup.kernel_id(), "mihomo");
        assert_eq!(sup.ctrl_endpoint(), Some(r"\\.\pipe\verve-test"));

        let pid = sup
            .start(
                Path::new("mihomo"),
                Path::new("."),
                Path::new("config.yaml"),
            )
            .await
            .expect("mock remote start should succeed");
        assert_eq!(pid, 9527);
        assert!(sup.is_running());
        assert_eq!(sup.pid(), Some(9527));

        let status = sup
            .refresh_status()
            .await
            .expect("refresh_status should succeed");
        assert_eq!(
            status,
            RemoteKernelStatus {
                running: true,
                pid: Some(9527),
                ctrl_endpoint: Some(r"\\.\pipe\verve-test".into()),
            }
        );

        let tail = sup
            .fetch_stderr_tail(2)
            .await
            .expect("fetch_stderr_tail should succeed");
        assert_eq!(tail, vec!["ready", "serving"]);
        assert_eq!(sup.stderr_tail(1), vec!["serving"]);

        sup.stop().await.expect("mock remote stop should succeed");
        assert!(!sup.is_running());
        assert_eq!(sup.pid(), None);
    }
}
