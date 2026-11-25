# Changelog

See [versioning and releases](docs/release.md) for the version progression policy, milestone roadmap, and release process.

## Unreleased

## 0.0.1 - 2026-11-25

### Core & Kernel

- Established the 12-crate Cargo workspace with compile-time unidirectional DAG enforcement and zero-GPUI headless core boundaries (`verve-core`, `verve-kernel`, `verve-kernel-mihomo`, `verve-config`, `verve-protocol`).
- Implemented dual-domain configuration persistence (`SyncableConfig` in `settings.toml`, `LocalMachineConfig` in `local.toml`), deterministic schema migration, atomic `tempfile -> sync_all -> rename` writes, and SQLite WAL metadata storage (`state.db`).
- Added the `ProxyKernel` abstraction, static `KernelSupervisor` (`DirectProcessSupervisor` with bounded stderr ring buffer and Windows Job Object kill-on-close), and the `MihomoKernel` adapter over Windows Named Pipes and Unix Domain Sockets with `mihomo -t` preflight diagnostics.
- Implemented the single-writer CQRS `AppCoordinator`, `CommandBus`, lock-free sliced `AppSnapshot` / `SnapshotStore` with per-domain revisions, and reference-counted `TelemetryHub` / RAII `TelemetryLease` streaming.

### Compiler & Overrides

- Implemented `CompilerPipeline` (`verve-compiler`) supporting immutable multi-stage profile compilation: per-profile and global YAML deep Merge (`prepend-*` / `append-*` list directives), `ScriptEngine` stage hooks, `QuickRulesStage`, and authoritative `RuntimeInjectionStage`.
- Added preflight-gated atomic activation (`active.yaml` with `last_known_good.yaml` backup and rollback on validation failure), verified end-to-end against a live Mihomo process.
