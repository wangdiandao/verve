//! CLI argument parsing, OS service installation/uninstallation, and Windows service entrypoint.

use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceUninstallCtx,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::allowlist::SecurityPolicy;
use crate::ipc::{IpcError, ShutdownSignal, run_ipc_server};
use crate::supervisor::ServiceSupervisor;

/// Canonical OS service identifier used during registration and Windows SCM dispatch.
const SERVICE_LABEL: &str = "io.verve.service";

/// Windows SCM service name.
#[cfg(windows)]
const WINDOWS_SERVICE_NAME: &str = "verve-service";

/// Errors returned by `verve-service` CLI and OS service setup.
#[derive(Debug, Error)]
pub enum CliError {
    /// Invalid or missing CLI argument.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// Service manager registration error.
    #[error("service manager error: {0}")]
    ServiceManager(#[from] std::io::Error),
    /// IPC server runtime error.
    #[error("IPC server error: {0}")]
    Ipc(#[from] IpcError),
    /// Windows SCM dispatch error.
    #[cfg(windows)]
    #[error("Windows service error: {0}")]
    WindowsService(#[from] windows_service::Error),
}

/// Parsed CLI command for `verve-service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCommand {
    /// Print CLI usage and exit.
    Help,
    /// Run the foreground/daemon IPC supervisor loop.
    Daemon {
        /// Local Named Pipe or Unix Domain Socket endpoint.
        ipc_endpoint: String,
        /// Trusted directories allowed to host kernel binaries.
        trusted_dirs: Vec<PathBuf>,
        /// Optional SHA-256 hex allowlist.
        allowed_sha256_hex: Vec<String>,
    },
    /// Install `verve-service` into the native OS service manager.
    Install {
        /// Optional IPC endpoint passed to the installed service.
        ipc_endpoint: Option<String>,
        /// Trusted directories passed to the installed service.
        trusted_dirs: Vec<PathBuf>,
        /// Optional SHA-256 hex allowlist passed to the installed service.
        allowed_sha256_hex: Vec<String>,
    },
    /// Uninstall `verve-service` from the native OS service manager.
    Uninstall,
    /// Run under the Windows Service Control Manager (SCM).
    #[cfg(windows)]
    WindowsService {
        /// Local Named Pipe endpoint.
        ipc_endpoint: String,
        /// Trusted directories allowed to host kernel binaries.
        trusted_dirs: Vec<PathBuf>,
        /// Optional SHA-256 hex allowlist.
        allowed_sha256_hex: Vec<String>,
    },
}

/// Parses CLI arguments into a [`ServiceCommand`].
pub fn parse_args<I>(args: I) -> Result<ServiceCommand, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut iter = args.into_iter().skip(1);
    let mut mode: Option<&'static str> = None;
    let mut ipc_endpoint: Option<String> = None;
    let mut trusted_dirs: Vec<PathBuf> = Vec::new();
    let mut allowed_sha256_hex: Vec<String> = Vec::new();

    while let Some(arg) = iter.next() {
        let s = arg.to_string_lossy();
        match s.as_ref() {
            "-h" | "--help" => return Ok(ServiceCommand::Help),
            "--daemon" => set_mode(&mut mode, "daemon")?,
            "--install" => set_mode(&mut mode, "install")?,
            "--uninstall" => set_mode(&mut mode, "uninstall")?,
            "--windows-service" => set_mode(&mut mode, "windows-service")?,
            "--ipc-endpoint" => {
                let val = iter.next().ok_or_else(|| {
                    CliError::InvalidArgs("--ipc-endpoint requires a value".into())
                })?;
                ipc_endpoint = Some(val.to_string_lossy().into_owned());
            }
            "--trusted-dir" => {
                let val = iter.next().ok_or_else(|| {
                    CliError::InvalidArgs("--trusted-dir requires a value".into())
                })?;
                trusted_dirs.push(PathBuf::from(val));
            }
            "--allow-sha256" => {
                let val = iter.next().ok_or_else(|| {
                    CliError::InvalidArgs("--allow-sha256 requires a value".into())
                })?;
                allowed_sha256_hex.push(val.to_string_lossy().into_owned());
            }
            other => {
                return Err(CliError::InvalidArgs(format!(
                    "unrecognized argument '{other}'"
                )));
            }
        }
    }

    match mode {
        Some("daemon") => {
            let endpoint = ipc_endpoint.unwrap_or_else(default_service_endpoint);
            Ok(ServiceCommand::Daemon {
                ipc_endpoint: endpoint,
                trusted_dirs,
                allowed_sha256_hex,
            })
        }
        Some("install") => Ok(ServiceCommand::Install {
            ipc_endpoint,
            trusted_dirs,
            allowed_sha256_hex,
        }),
        Some("uninstall") => Ok(ServiceCommand::Uninstall),
        #[cfg(windows)]
        Some("windows-service") => Ok(ServiceCommand::WindowsService {
            ipc_endpoint: ipc_endpoint.unwrap_or_else(default_service_endpoint),
            trusted_dirs,
            allowed_sha256_hex,
        }),
        #[cfg(not(windows))]
        Some("windows-service") => Err(CliError::InvalidArgs(
            "--windows-service is only supported on Windows".into(),
        )),
        Some(other) => Err(CliError::InvalidArgs(format!("unknown mode '{other}'"))),
        None => Err(CliError::InvalidArgs(
            "expected one of --daemon, --install, --uninstall, or --windows-service".into(),
        )),
    }
}

fn set_mode(slot: &mut Option<&'static str>, next: &'static str) -> Result<(), CliError> {
    if let Some(prev) = *slot {
        return Err(CliError::InvalidArgs(format!(
            "conflicting mode flags '--{prev}' and '--{next}'"
        )));
    }
    *slot = Some(next);
    Ok(())
}

/// Returns the default platform IPC endpoint for `verve-service`.
pub fn default_service_endpoint() -> String {
    #[cfg(windows)]
    {
        r"\\.\pipe\verve-service".to_owned()
    }
    #[cfg(not(windows))]
    {
        "/run/verve/verve-service.sock".to_owned()
    }
}

/// Prints CLI usage instructions.
pub fn print_usage() {
    println!(
        "verve-service - minimal privileged kernel supervisor\n\n\
         USAGE:\n  \
           verve-service --daemon --ipc-endpoint <PIPE_OR_SOCK> --trusted-dir <DIR> [--allow-sha256 <HEX>]\n  \
           verve-service --install [--ipc-endpoint <PIPE_OR_SOCK>] [--trusted-dir <DIR> ...]\n  \
           verve-service --uninstall\n  \
           verve-service --windows-service [--ipc-endpoint <PIPE>] [--trusted-dir <DIR> ...]"
    );
}

/// Executes the parsed [`ServiceCommand`].
pub fn execute_command(cmd: ServiceCommand) -> Result<(), CliError> {
    match cmd {
        ServiceCommand::Help => {
            print_usage();
            Ok(())
        }
        ServiceCommand::Daemon {
            ipc_endpoint,
            trusted_dirs,
            allowed_sha256_hex,
        } => run_foreground_daemon(
            &ipc_endpoint,
            SecurityPolicy {
                trusted_dirs,
                allowed_sha256_hex,
            },
        ),
        ServiceCommand::Install {
            ipc_endpoint,
            trusted_dirs,
            allowed_sha256_hex,
        } => install_service(ipc_endpoint, trusted_dirs, allowed_sha256_hex),
        ServiceCommand::Uninstall => uninstall_service(),
        #[cfg(windows)]
        ServiceCommand::WindowsService {
            ipc_endpoint,
            trusted_dirs,
            allowed_sha256_hex,
        } => windows_scm::run(
            ipc_endpoint,
            SecurityPolicy {
                trusted_dirs,
                allowed_sha256_hex,
            },
        ),
    }
}

fn run_foreground_daemon(endpoint: &str, policy: SecurityPolicy) -> Result<(), CliError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let supervisor = Arc::new(Mutex::new(ServiceSupervisor::new(policy)));
        let shutdown = ShutdownSignal::new();

        let signal_shutdown = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("received shutdown signal, stopping verve-service");
                signal_shutdown.trigger();
            }
        });

        let result = run_ipc_server(endpoint, Arc::clone(&supervisor), shutdown).await;
        supervisor.lock().await.shutdown().await;
        result
    })?;

    Ok(())
}

fn install_service(
    ipc_endpoint: Option<String>,
    trusted_dirs: Vec<PathBuf>,
    allowed_sha256_hex: Vec<String>,
) -> Result<(), CliError> {
    let manager = <dyn ServiceManager>::native()?;
    let label = ServiceLabel::from_str(SERVICE_LABEL)?;
    let program = std::env::current_exe()?;

    let mut args: Vec<OsString> = Vec::new();
    #[cfg(windows)]
    args.push(OsString::from("--windows-service"));
    #[cfg(not(windows))]
    args.push(OsString::from("--daemon"));

    if let Some(endpoint) = ipc_endpoint {
        args.push(OsString::from("--ipc-endpoint"));
        args.push(OsString::from(endpoint));
    }
    for dir in trusted_dirs {
        args.push(OsString::from("--trusted-dir"));
        args.push(dir.into_os_string());
    }
    for hex in allowed_sha256_hex {
        args.push(OsString::from("--allow-sha256"));
        args.push(OsString::from(hex));
    }

    manager.install(ServiceInstallCtx {
        label,
        program,
        args,
        contents: None,
        username: None,
        working_directory: None,
        environment: None,
        autostart: true,
        restart_policy: RestartPolicy::default(),
    })?;

    Ok(())
}

fn uninstall_service() -> Result<(), CliError> {
    let manager = <dyn ServiceManager>::native()?;
    let label = ServiceLabel::from_str(SERVICE_LABEL)?;
    manager.uninstall(ServiceUninstallCtx { label })?;
    Ok(())
}

#[cfg(windows)]
mod windows_scm {
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex as StdMutex, OnceLock};
    use std::time::Duration;

    use tokio::sync::Mutex;
    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;

    use super::{CliError, WINDOWS_SERVICE_NAME};
    use crate::allowlist::SecurityPolicy;
    use crate::ipc::{ShutdownSignal, run_ipc_server};
    use crate::supervisor::ServiceSupervisor;

    static WINDOWS_SERVICE_CONFIG: OnceLock<StdMutex<Option<(String, SecurityPolicy)>>> =
        OnceLock::new();

    define_windows_service!(ffi_service_main, windows_service_main);

    pub(super) fn run(endpoint: String, policy: SecurityPolicy) -> Result<(), CliError> {
        let slot = WINDOWS_SERVICE_CONFIG.get_or_init(|| StdMutex::new(None));
        *slot.lock().expect("windows service config mutex poisoned") = Some((endpoint, policy));
        service_dispatcher::start(WINDOWS_SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    fn windows_service_main(_arguments: Vec<OsString>) {
        if let Err(err) = run_service_inner() {
            tracing::error!(error = %err, "Windows service exited with error");
        }
    }

    fn run_service_inner() -> Result<(), CliError> {
        let (endpoint, policy) = WINDOWS_SERVICE_CONFIG
            .get()
            .and_then(|slot| slot.lock().ok().and_then(|mut g| g.take()))
            .unwrap_or_else(|| (super::default_service_endpoint(), SecurityPolicy::default()));

        let shutdown = ShutdownSignal::new();
        let handler_shutdown = shutdown.clone();

        let status_handle =
            service_control_handler::register(WINDOWS_SERVICE_NAME, move |control_event| {
                match control_event {
                    ServiceControl::Stop | ServiceControl::Shutdown => {
                        handler_shutdown.trigger();
                        ServiceControlHandlerResult::NoError
                    }
                    ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                    _ => ServiceControlHandlerResult::NotImplemented,
                }
            })?;

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let server_result = rt.block_on(async move {
            let supervisor = Arc::new(Mutex::new(ServiceSupervisor::new(policy)));
            let res = run_ipc_server(&endpoint, Arc::clone(&supervisor), shutdown).await;
            supervisor.lock().await.shutdown().await;
            res
        });

        let exit_code = if server_result.is_ok() {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(1)
        };

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        server_result?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daemon_flags_and_rejects_conflicting_modes() {
        let args = [
            "verve-service",
            "--daemon",
            "--ipc-endpoint",
            "/tmp/verve.sock",
            "--trusted-dir",
            "/usr/local/bin",
            "--trusted-dir",
            "/opt/verve",
            "--allow-sha256",
            "abcd1234",
        ]
        .into_iter()
        .map(OsString::from);

        let cmd = parse_args(args).unwrap();
        assert_eq!(
            cmd,
            ServiceCommand::Daemon {
                ipc_endpoint: "/tmp/verve.sock".into(),
                trusted_dirs: vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/opt/verve")],
                allowed_sha256_hex: vec!["abcd1234".into()],
            }
        );

        let conflict = ["verve-service", "--install", "--uninstall"]
            .into_iter()
            .map(OsString::from);
        assert!(parse_args(conflict).is_err());
    }
}
