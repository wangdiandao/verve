//! Application bootstrap: CLI/env resolution, single-instance lock, Tokio + coordinator
//! startup, GPUI `QuitMode::Explicit` lifecycle, and tray registration.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use directories::ProjectDirs;
use gpui::{App, AppContext, QuitMode, WindowAppearance};
use gpui_material::MaterialRoot;
use tracing_subscriber::EnvFilter;
use verve_core::compiler::CompilerPipeline;
use verve_core::config::ConfigManager;
use verve_core::kernel::{ControlEndpoint, DirectProcessSupervisor, KernelSupervisor};
use verve_core::kernel_mihomo::{MihomoKernel, MihomoKernelConfig};
use verve_core::sys::{
    ActivationListener, OsEvent, OsEventBus, SingleInstanceGuard, create_platform_tray,
    default_activation_endpoint, notify_primary_instance,
};
use verve_core::{
    AppCoordinator, CommandBus, CoordinatorConfig, CoreError, MihomoCoordinatorBuilder,
    SnapshotStore,
};
use verve_ui_kit::{AppearanceMode, DEFAULT_SEED_ARGB, build_material_theme, init_ui_kit};

use crate::bridge::{CommandDispatcher, TelemetrySource, UiStore};
use crate::shell::{AppShell, TrayController, default_main_window_options};

const APP_ID: &str = "io.github.wangdiandao.verve";

/// Resolves the application data directory from `--data-dir`, `VERVE_DATA_DIR`, or platform defaults.
pub fn resolve_data_dir_from_args<I, S>(args: I) -> PathBuf
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter().skip(1);
    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        if let Some(val) = s.strip_prefix("--data-dir=") {
            if !val.trim().is_empty() {
                return PathBuf::from(val);
            }
        } else if s == "--data-dir"
            && let Some(next) = iter.next()
        {
            return PathBuf::from(next.as_ref());
        }
    }

    if let Some(env_dir) = std::env::var_os("VERVE_DATA_DIR")
        && !env_dir.is_empty()
    {
        return PathBuf::from(env_dir);
    }

    ProjectDirs::from("io.github.wangdiandao", "Verve", "Verve").map_or_else(
        || std::env::temp_dir().join("verve"),
        |dirs| dirs.data_local_dir().to_path_buf(),
    )
}

struct BootBackend {
    bus: CommandBus,
    snapshots: Arc<SnapshotStore>,
    telemetry: Arc<dyn TelemetrySource>,
    shutdown_fn: Box<dyn FnOnce(&tokio::runtime::Runtime) + Send>,
}

fn spawn_backend(data_dir: &PathBuf) -> Result<BootBackend, CoreError> {
    match MihomoCoordinatorBuilder::new(data_dir).spawn() {
        Ok(mut handle) => {
            let bus = handle.bus().clone();
            let snapshots = Arc::clone(handle.snapshots());
            let telemetry: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
            let shutdown_fn = Box::new(move |rt: &tokio::runtime::Runtime| {
                rt.block_on(async move {
                    let _ = handle.shutdown().await;
                });
            });
            Ok(BootBackend {
                bus,
                snapshots,
                telemetry,
                shutdown_fn,
            })
        }
        Err(CoreError::KernelBinaryNotFound) => {
            tracing::warn!(
                "mihomo binary not found; starting UI shell in idle mode until kernel binary is configured"
            );
            let config_manager = ConfigManager::open(data_dir)?;
            let endpoint = ControlEndpoint::default_for_session("default");
            let kernel = Arc::new(MihomoKernel::new(MihomoKernelConfig {
                binary_path: PathBuf::from("mihomo"),
                work_dir: config_manager.runtime_dir(),
                endpoint: endpoint.clone(),
                secret: None,
                request_timeout: std::time::Duration::from_secs(5),
            }));
            let mut handle = AppCoordinator::spawn(
                config_manager,
                CompilerPipeline::default(),
                KernelSupervisor::Direct(DirectProcessSupervisor::new()),
                kernel,
                CoordinatorConfig {
                    control_endpoint: endpoint,
                    ..Default::default()
                },
            )?;
            let bus = handle.bus().clone();
            let snapshots = Arc::clone(handle.snapshots());
            let telemetry: Arc<dyn TelemetrySource> = Arc::new(handle.telemetry().clone());
            let shutdown_fn = Box::new(move |rt: &tokio::runtime::Runtime| {
                rt.block_on(async move {
                    let _ = handle.shutdown().await;
                });
            });
            Ok(BootBackend {
                bus,
                snapshots,
                telemetry,
                shutdown_fn,
            })
        }
        Err(err) => Err(err),
    }
}

/// Launches the desktop Verve application with single-instance lock, Tokio core, GPUI shell, and tray.
pub fn run_desktop_app() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    let data_dir = resolve_data_dir_from_args(std::env::args().collect::<Vec<_>>());
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");

    let activation_endpoint = default_activation_endpoint(APP_ID);
    let _instance_guard = match SingleInstanceGuard::try_acquire(APP_ID) {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            tokio_rt.block_on(async {
                let _ = notify_primary_instance(&activation_endpoint).await;
            });
            return;
        }
        Err(err) => {
            tracing::warn!(error = %err, "single-instance lock failed; continuing");
            None
        }
    };

    let os_events = OsEventBus::default();
    let mut os_rx = os_events.subscribe();
    let activation_listener = tokio_rt.block_on(async {
        match ActivationListener::start(&activation_endpoint, os_events.clone()).await {
            Ok(listener) => Some(listener),
            Err(err) => {
                tracing::warn!(error = %err, "failed to start activation listener");
                None
            }
        }
    });

    let backend = tokio_rt
        .block_on(async { spawn_backend(&data_dir) })
        .expect("failed to initialize Verve backend coordinator");

    let bus = backend.bus;
    let snapshots = backend.snapshots;
    let telemetry = backend.telemetry;
    let tokio_handle = tokio_rt.handle().clone();

    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            let initial_theme_str = snapshots.snapshot().settings.syncable.theme_mode.clone();
            let mode = AppearanceMode::from_config_str(&initial_theme_str)
                .unwrap_or(AppearanceMode::System);
            let system_dark = matches!(
                cx.window_appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            );
            init_ui_kit(
                build_material_theme(mode, Some(DEFAULT_SEED_ARGB), system_dark),
                cx,
            );

            let store = cx.new(|cx| UiStore::new(snapshots, telemetry, tokio_handle.clone(), cx));
            let commands = CommandDispatcher::new(bus, tokio_handle);

            let (tray_tx, tray_rx) = tokio::sync::mpsc::unbounded_channel();
            let tray_backend = create_platform_tray(tray_tx);

            let open_store = store.clone();
            let open_commands = commands.clone();
            let open_window = Rc::new(move |cx: &mut App| {
                let opts = default_main_window_options(cx);
                let store = open_store.clone();
                let commands = open_commands.clone();
                MaterialRoot::open_window(opts, cx, move |window, cx| {
                    cx.new(|cx| AppShell::new(store, commands, window, cx))
                })
            });

            let tray = cx.new(|cx| {
                TrayController::new(
                    tray_backend,
                    tray_rx,
                    store,
                    commands,
                    open_window,
                    None,
                    cx,
                )
            });

            tray.update(cx, |ctrl, cx| {
                if let Err(err) = ctrl.show_or_focus_window(cx) {
                    tracing::error!(error = %err, "failed to open initial AppShell window");
                }
            });

            let weak_tray = tray.downgrade();
            cx.spawn(async move |cx| {
                while let Ok(ev) = os_rx.recv().await {
                    if ev == OsEvent::ActivatePrimaryInstance
                        && weak_tray
                            .update(cx, |ctrl, cx| {
                                let _ = ctrl.show_or_focus_window(cx);
                            })
                            .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        });

    if let Some(listener) = activation_listener {
        tokio_rt.block_on(listener.shutdown());
    }
    (backend.shutdown_fn)(&tokio_rt);
}
