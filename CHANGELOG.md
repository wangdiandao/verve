# Changelog

See [versioning and releases](docs/release.md) for the version progression policy, milestone roadmap, and release process.

## Unreleased

### UI & UX

- Refined the `AppShell` navigation chrome: removed the redundant `NavigationRail` headline text, switched collapsed rail destinations to icon-only mode with larger 24dp icons, and removed the theme toggle button from the `TitleBar` (theme switching remains in `Settings`).
- Upgraded `gpui-material` to `0.4.1` (bringing Material ripple/state-layer feedback, pill-expand & color-interpolated active indicators, and native `NavigationRail::footer` support) and moved the `AppShell` sidebar telemetry pod into `NavigationRail::footer`.
- Unified the `AppShell` sidebar onto a single `NavigationRail` driven by `NavigationRailState` (`RailMode::Expanded` ↔ `RailMode::Collapsed`), enabling smooth sidebar width transitions and animated active-indicator crossfades across all navigation destinations.
- Added `rust-i18n` bilingual (`zh-CN` / `en-US`) localization across the `verve-app` shell, all 5 feature views (`Overview`, `Proxies`, `Profiles`, `Activity`, `Settings`), and OS system tray menus (`TrayMenuLabels`), with live zero-restart language switching and canonical `locale` persistence in `settings.toml`.

## 0.0.2 - 2026-11-25

### UI & UX

- Implemented the `verve-ui-kit` Material 3 Expressive design system wrapper with light/dark/system theme tokens, `SectionCard`, `HealthIndicator`, `LatencyBadge`, `MetricTile`, `EmptyState`, and GPU Canvas `TrafficChart` / `MiniSparkline` components.
- Built the `verve-app` desktop composition root featuring `AppShell` (40px client-drawn `TitleBar`, adaptive `NavigationDrawer`/`NavigationRail`, sidebar telemetry pod, and error `Snackbar`), `UiStore` selective domain projections, `CommandDispatcher`, scoped `SideSheetHost` / `ConfirmDialogHost`, and RAII `UiTelemetryLease` streaming across the 5 vertical feature slices (`Overview`, `Proxies`, `Profiles`, `Activity`, `Settings`).

### System & Service

- Implemented `verve-sys` platform adapters for OS system proxy takeover with atomic crash-recovery markers, cross-platform system tray residency (`NativeTray` / `LinuxKsniTray`), single-instance locking with primary-window activation IPC, login auto-launch, and host power/network `OsEventBus` listeners.
- Implemented the minimal privileged `verve-service` IPC daemon (`verve-protocol` v1) with peer-credential checks, trusted binary path/SHA-256 allowlist validation, and anti-orphan kernel supervision, wired to `AppCoordinator` via `ProtocolServiceIpcClient`, `set_system_proxy`, `set_tun_enabled`, and `set_theme_mode`.

## 0.0.1 - 2026-11-25

### Core & Kernel

- Established the 12-crate Cargo workspace with compile-time unidirectional DAG enforcement and zero-GPUI headless core boundaries (`verve-core`, `verve-kernel`, `verve-kernel-mihomo`, `verve-config`, `verve-protocol`).
- Implemented dual-domain configuration persistence (`SyncableConfig` in `settings.toml`, `LocalMachineConfig` in `local.toml`), deterministic schema migration, atomic `tempfile -> sync_all -> rename` writes, and SQLite WAL metadata storage (`state.db`).
- Added the `ProxyKernel` abstraction, static `KernelSupervisor` (`DirectProcessSupervisor` with bounded stderr ring buffer and Windows Job Object kill-on-close), and the `MihomoKernel` adapter over Windows Named Pipes and Unix Domain Sockets with `mihomo -t` preflight diagnostics.
- Implemented the single-writer CQRS `AppCoordinator`, `CommandBus`, lock-free sliced `AppSnapshot` / `SnapshotStore` with per-domain revisions, and reference-counted `TelemetryHub` / RAII `TelemetryLease` streaming.

### Compiler & Overrides

- Implemented `CompilerPipeline` (`verve-compiler`) supporting immutable multi-stage profile compilation: per-profile and global YAML deep Merge (`prepend-*` / `append-*` list directives), `ScriptEngine` stage hooks, `QuickRulesStage`, and authoritative `RuntimeInjectionStage`.
- Added preflight-gated atomic activation (`active.yaml` with `last_known_good.yaml` backup and rollback on validation failure), verified end-to-end against a live Mihomo process.
