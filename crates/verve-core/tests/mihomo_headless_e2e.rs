//! 0.0.1 Exit Criteria: Full headless E2E loop against a real Mihomo binary over Named Pipe / UDS.

use std::net::TcpListener;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

use verve_config::{ConfigManager, SyncableConfig};
use verve_core::MihomoCoordinatorBuilder;
use verve_kernel::{ControlEndpoint, ProxyGroupKind, ProxyKernel, ProxyMode, TelemetryKind};
use verve_kernel_mihomo::discover_mihomo_binary;

struct SocketCleanupGuard {
    #[cfg(unix)]
    path: PathBuf,
}

impl Drop for SocketCleanupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn reserve_ephemeral_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn unique_e2e_endpoint() -> (ControlEndpoint, SocketCleanupGuard) {
    #[cfg(windows)]
    {
        let endpoint = ControlEndpoint::default_for_session(&format!("e2e-{}", std::process::id()));
        (endpoint, SocketCleanupGuard {})
    }

    #[cfg(unix)]
    {
        // Keep Unix socket path short (< 104 bytes `sun_path` limit on Linux/macOS).
        let path = PathBuf::from(format!("/tmp/verve-e2e-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        (
            ControlEndpoint::UnixSocket(path.clone()),
            SocketCleanupGuard { path },
        )
    }
}

#[tokio::test]
async fn mihomo_headless_e2e_import_compile_start_select_persist_and_telemetry() {
    let binary_path =
        discover_mihomo_binary(None).expect("mihomo binary must be available for 0.0.1 E2E test");

    let temp_dir = tempfile::tempdir().expect("create temp workspace");
    let mixed_port = reserve_ephemeral_port();
    let (endpoint, _sock_guard) = unique_e2e_endpoint();

    // Seed settings.toml with our reserved ephemeral mixed_port.
    {
        let manager = ConfigManager::open(temp_dir.path()).expect("open config manager");
        let syncable = SyncableConfig {
            mixed_port,
            ..Default::default()
        };
        manager
            .save_syncable(&syncable)
            .expect("persist initial settings.toml");
    }

    let mut handle = MihomoCoordinatorBuilder::new(temp_dir.path())
        .binary_path(binary_path)
        .control_endpoint(endpoint.clone())
        .startup_timeout(Duration::from_secs(10))
        .request_timeout(Duration::from_secs(5))
        .spawn()
        .expect("spawn Mihomo coordinator");

    assert_eq!(
        handle.kernel().config().request_timeout,
        Duration::from_secs(5)
    );
    assert_eq!(
        handle.kernel().config().preflight_timeout,
        verve_core::kernel_mihomo::DEFAULT_PREFLIGHT_TIMEOUT
    );

    let raw_subscription_yaml = r#"
proxies:
  - name: hk-01
    type: ss
    server: 198.51.100.1
    port: 8388
    cipher: aes-128-gcm
    password: test-password
    udp: true
  - name: jp-01
    type: ss
    server: 198.51.100.2
    port: 8388
    cipher: aes-128-gcm
    password: test-password
    udp: true
proxy-groups:
  - name: Proxy
    type: select
    proxies:
      - hk-01
      - jp-01
      - Auto
      - DIRECT
  - name: Auto
    type: url-test
    proxies:
      - hk-01
      - jp-01
    url: http://www.gstatic.com/generate_204
    interval: 300
rules:
  - DOMAIN-SUFFIX,local,DIRECT
  - MATCH,Proxy
"#;

    let merge_yaml = r#"
append-proxies:
  - name: sg-01
    type: ss
    server: 198.51.100.3
    port: 8388
    cipher: aes-128-gcm
    password: test-password
    udp: true
prepend-rules:
  - DOMAIN,override.example,DIRECT
"#;

    // 1. Import profile + merge overlay.
    let entry = handle
        .bus()
        .import_local_profile(
            "e2e-sub",
            "E2E Subscription",
            raw_subscription_yaml,
            Some(merge_yaml.to_owned()),
        )
        .await
        .expect("import local profile");
    assert_eq!(entry.id, "e2e-sub");

    // 2. Activate profile -> compiles active.yaml and runs `mihomo -t` preflight.
    let committed = handle
        .bus()
        .activate_profile("e2e-sub")
        .await
        .expect("activate_profile must pass mihomo -t preflight");
    assert!(committed.active_path.is_file());
    let active_yaml = std::fs::read_to_string(&committed.active_path).expect("read active.yaml");

    match &endpoint {
        ControlEndpoint::NamedPipe(pipe) => {
            assert!(
                active_yaml.contains("external-controller-pipe") && active_yaml.contains(pipe),
                "expected external-controller-pipe `{pipe}` in active.yaml:\n{active_yaml}"
            );
        }
        ControlEndpoint::UnixSocket(sock) => {
            let sock_str = sock.to_string_lossy();
            assert!(
                active_yaml.contains("external-controller-unix")
                    && active_yaml.contains(sock_str.as_ref()),
                "expected external-controller-unix `{sock_str}` in active.yaml:\n{active_yaml}"
            );
        }
    }
    assert!(active_yaml.contains("sg-01"));
    assert!(active_yaml.contains("DOMAIN,override.example,DIRECT"));

    // 3. Start real Mihomo process and verify control plane + initial topology.
    let pid = handle
        .bus()
        .start_kernel()
        .await
        .expect("start_kernel must spawn real Mihomo and connect");
    assert!(pid > 0);

    let snap_after_start = handle.snapshot();
    assert!(snap_after_start.runtime.kernel_running);
    assert_eq!(snap_after_start.runtime.kernel_pid, Some(pid));

    let proxy_group = snap_after_start
        .proxies
        .groups
        .iter()
        .find(|g| g.name == "Proxy")
        .expect("Proxy group must exist in snapshot");
    assert_eq!(proxy_group.kind, ProxyGroupKind::Select);
    assert!(proxy_group.all.iter().any(|n| n == "hk-01"));
    assert!(proxy_group.all.iter().any(|n| n == "jp-01"));
    assert!(proxy_group.all.iter().any(|n| n == "DIRECT"));
    assert_eq!(proxy_group.now.as_deref(), Some("hk-01"));
    assert!(snap_after_start.proxies.nodes.contains_key("sg-01"));

    let rev_before_select = snap_after_start.proxies.revision;

    // 4. Switch proxy selection and verify snapshot, live Mihomo state, and SQLite persistence.
    handle
        .bus()
        .select_proxy("Proxy", "jp-01")
        .await
        .expect("select_proxy must switch to jp-01");

    let snap_after_select = handle.snapshot();
    assert!(snap_after_select.proxies.revision > rev_before_select);
    let updated_group = snap_after_select
        .proxies
        .groups
        .iter()
        .find(|g| g.name == "Proxy")
        .expect("Proxy group must remain present");
    assert_eq!(updated_group.now.as_deref(), Some("jp-01"));

    let live_topology = handle
        .kernel()
        .fetch_proxies()
        .await
        .expect("direct fetch_proxies over Named Pipe / UDS");
    let live_group = live_topology
        .groups
        .iter()
        .find(|g| g.name == "Proxy")
        .expect("Proxy group in live topology");
    assert_eq!(live_group.now.as_deref(), Some("jp-01"));

    {
        let reopened_store = ConfigManager::open(temp_dir.path())
            .expect("reopen config manager")
            .open_state_store()
            .expect("reopen state.db");
        let persisted = reopened_store
            .get_proxy_selections("e2e-sub")
            .expect("query persisted proxy selections");
        assert_eq!(persisted.get("Proxy").map(String::as_str), Some("jp-01"));
    }

    // 5. Switch proxy mode while kernel is running -> recompiles, reloads Mihomo, and preserves
    //    remembered proxy selections.
    handle
        .bus()
        .set_proxy_mode(ProxyMode::Global)
        .await
        .expect("set_proxy_mode must recompile and reload running Mihomo");
    let snap_after_mode = handle.snapshot();
    assert_eq!(snap_after_mode.runtime.proxy_mode, ProxyMode::Global);
    let reloaded_group = snap_after_mode
        .proxies
        .groups
        .iter()
        .find(|g| g.name == "Proxy")
        .expect("Proxy group after live reload");
    assert_eq!(reloaded_group.now.as_deref(), Some("jp-01"));

    // 6. Acquire live WebSocket traffic telemetry lease and verify at least one sample arrives.
    let lease = handle.telemetry().acquire(TelemetryKind::Traffic);
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if !handle.snapshot().telemetry.traffic_history.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("real Mihomo WebSocket /traffic stream must emit at least one sample");

    drop(lease);

    // 7. Clean shutdown terminates Mihomo process and resets runtime slice.
    handle.shutdown().await.expect("clean coordinator shutdown");
    let final_snap = handle.snapshot();
    assert!(!final_snap.runtime.kernel_running);
    assert_eq!(final_snap.runtime.kernel_pid, None);
}
