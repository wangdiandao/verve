//! Single-instance lock guard and local primary-instance activation signaling.

use std::path::PathBuf;

use single_instance::SingleInstance;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::{OsEvent, OsEventBus, SysError};

/// Magic activation line sent by secondary launches over local IPC.
const ACTIVATE_PAYLOAD: &[u8] = b"ACTIVATE\n";

/// Cross-platform single-instance mutex/lock guard.
pub struct SingleInstanceGuard {
    _inner: SingleInstance,
}

impl std::fmt::Debug for SingleInstanceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleInstanceGuard")
            .finish_non_exhaustive()
    }
}

impl SingleInstanceGuard {
    /// Attempts to acquire the exclusive instance lock for `app_id`.
    ///
    /// Returns `Ok(Some(guard))` when this process is the primary instance, or
    /// `Ok(None)` when another instance already holds the lock.
    pub fn try_acquire(app_id: &str) -> Result<Option<Self>, SysError> {
        let lock_id = platform_lock_id(app_id);
        let instance = SingleInstance::new(&lock_id)
            .map_err(|err| SysError::SingleInstance(err.to_string()))?;
        if instance.is_single() {
            Ok(Some(Self { _inner: instance }))
        } else {
            Ok(None)
        }
    }
}

fn sanitize_slug(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "verve".to_owned()
    } else {
        sanitized
    }
}

fn platform_lock_id(app_id: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        let path = std::path::Path::new(app_id);
        if path.is_absolute() {
            app_id.to_owned()
        } else {
            std::env::temp_dir()
                .join(format!("{}.lock", sanitize_slug(app_id)))
                .to_string_lossy()
                .into_owned()
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        app_id.to_owned()
    }
}

/// Returns the default local activation endpoint path for `app_id`.
pub fn default_activation_endpoint(app_id: &str) -> String {
    let slug = sanitize_slug(app_id);
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\verve-activate-{slug}")
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
            .join(format!("verve-activate-{slug}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

/// Lightweight local IPC listener that emits [`OsEvent::ActivatePrimaryInstance`]
/// whenever a secondary instance connects.
#[derive(Debug)]
pub struct ActivationListener {
    shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(not(unix))]
    _endpoint: PathBuf,
}

impl ActivationListener {
    /// Binds a local activation listener at `endpoint` and forwards activations into `bus`.
    pub async fn start(endpoint: impl Into<String>, bus: OsEventBus) -> Result<Self, SysError> {
        let endpoint = endpoint.into();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        #[cfg(windows)]
        let task = start_windows_listener(endpoint.clone(), bus, shutdown_rx)?;

        #[cfg(unix)]
        let task = start_unix_listener(endpoint.clone(), bus, shutdown_rx)?;

        Ok(Self {
            shutdown_tx,
            task: Some(task),
            #[cfg(unix)]
            socket_path: PathBuf::from(endpoint),
            #[cfg(not(unix))]
            _endpoint: PathBuf::from(endpoint),
        })
    }

    /// Gracefully stops the activation listener and cleans up any local socket file.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for ActivationListener {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(windows)]
fn start_windows_listener(
    endpoint: String,
    bus: OsEventBus,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, SysError> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&endpoint)?;

    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                res = server.connect() => {
                    match res {
                        Ok(()) => {
                            bus.emit(OsEvent::ActivatePrimaryInstance);
                            drop(server);
                            match ServerOptions::new()
                                .first_pipe_instance(false)
                                .reject_remote_clients(true)
                                .create(&endpoint)
                            {
                                Ok(next) => server = next,
                                Err(err) => {
                                    tracing::warn!(error = %err, "failed to recreate activation named pipe");
                                    break;
                                }
                            }
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "activation named pipe accept error");
                            break;
                        }
                    }
                }
            }
        }
    }))
}

#[cfg(unix)]
fn start_unix_listener(
    endpoint: String,
    bus: OsEventBus,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, SysError> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    let path = std::path::Path::new(&endpoint);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(SysError::Io(err)),
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                res = listener.accept() => {
                    match res {
                        Ok((_stream, _addr)) => {
                            bus.emit(OsEvent::ActivatePrimaryInstance);
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "activation unix socket accept error");
                            break;
                        }
                    }
                }
            }
        }
    }))
}

/// Signals an already-running primary instance listening on `endpoint` to activate its window.
pub async fn notify_primary_instance(endpoint: &str) -> Result<(), SysError> {
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        const ERROR_PIPE_BUSY: i32 = 231;

        let mut attempts = 0u8;
        let mut client = loop {
            match ClientOptions::new().open(endpoint) {
                Ok(client) => break client,
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 10 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(err) => return Err(SysError::Io(err)),
            }
        };
        let _ = client.write_all(ACTIVATE_PAYLOAD).await;
        let _ = client.flush().await;
        Ok(())
    }

    #[cfg(unix)]
    {
        use tokio::net::UnixStream;

        let mut stream = UnixStream::connect(endpoint).await?;
        let _ = stream.write_all(ACTIVATE_PAYLOAD).await;
        let _ = stream.flush().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_instance_guard_rejects_second_acquire_and_releases_on_drop() {
        let unique = format!(
            "verve-test-instance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let first = SingleInstanceGuard::try_acquire(&unique)
            .expect("first acquire should not error")
            .expect("first acquire must succeed");
        let second =
            SingleInstanceGuard::try_acquire(&unique).expect("second acquire should not error");
        assert!(second.is_none(), "second acquire must be rejected");

        drop(first);

        let third = SingleInstanceGuard::try_acquire(&unique)
            .expect("re-acquire after drop should not error")
            .expect("re-acquire after drop must succeed");
        drop(third);
    }

    #[tokio::test]
    async fn activation_listener_emits_activate_primary_instance_event() {
        let unique = format!(
            "verve-test-activate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let endpoint = default_activation_endpoint(&unique);
        let bus = OsEventBus::default();
        let mut rx = bus.subscribe();

        let listener = ActivationListener::start(endpoint.clone(), bus)
            .await
            .expect("activation listener should start");

        notify_primary_instance(&endpoint)
            .await
            .expect("notifying primary instance should succeed");

        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("event should arrive before timeout")
            .expect("event channel should stay open");
        assert_eq!(ev, OsEvent::ActivatePrimaryInstance);

        listener.shutdown().await;
    }
}
