# Development guide

English | [中文](development.zh.md)

<a id="contribution-workflow-and-branch-naming"></a>

## Contribution workflow and branch naming

- See [Contributing](../CONTRIBUTING.md) for issue guidelines, scope compliance, and PR review rules.
- Always branch from the latest `main` using lowercase kebab-case `<type>/<short-topic>` (for example `feat/config-compiler`, `fix/tun-dns-hijack`, `docs/architecture-update`), and reference the tracking issue in the PR description.
- Allowed prefixes: `feat/`, `fix/`, `docs/`, `test/`, `refactor/`, `ci/`, `chore/`, `release/`.

<a id="workspace-and-module-organization"></a>

## Workspace and module organization

Verve enforces a unidirectional Cargo Workspace DAG documented in [System architecture](architecture.md#workspace-module-layout-and-dependency-dag):

- **Headless core & adapters (`crates/verve-{protocol,config,kernel,kernel-mihomo,compiler,sys,sync,core}`)** contain all domain logic, persistence, compilation, IPC, and OS integration with zero GPUI dependencies.
- **Privileged supervisor (`crates/verve-service`)** depends only on `verve-protocol` and OS primitives—never pull configuration parsers, JS engines, or business logic into it.
- **UI foundation (`crates/verve-ui-kit`, `crates/verve-editor`)** wrap `gpui-material` tokens/primitives and the native YAML/JS editor without owning domain state.
- **Desktop app (`crates/verve-app`)** uses a strict **Shell + Feature Slices** layout (`shell/` + `features/{overview,proxies,profiles,activity,settings}/`).

<a id="coding-conventions"></a>

## Coding conventions

- **Consult docs first**: Locate active specifications from [Documentation index](index.md). Update matching English (`*.md`) and Chinese (`*.zh.md`) docs in the same PR when architecture, contracts, or user-visible behavior change.
- **Respect crate and slice boundaries**:
  - `verve-core` and lower crates must remain 100% headless.
  - Each UI feature module owns its own GPUI `Entity`, `TextFieldState`, `ScrollHandle`, and scoped `SideSheet` / confirmation views. Never add cross-page mutable fields to the root window struct or introduce a monolithic modal enum.
  - High-frequency telemetry streams must be bound to RAII `TelemetryLease` guards owned by the subscribing feature entity.
- **No mock-only UI sprawl**: Every control rendered in `verve-app` must bind to a real `AppCommand` and real `AppSnapshot` slice defined in `verve-core`.
- **Atomic persistence**: All disk writes for configuration and compiled profiles must go through same-directory `tempfile -> sync_all -> atomic rename`.
- **Idiomatic Rust**: Prefer narrow module visibility (`pub(super)` / `pub(crate)`), explicit domain error enums (`thiserror`) at library boundaries, and static dispatch / closed enums over unnecessary trait objects.

<a id="testing-conventions"></a>

## Testing conventions

Select the lowest effective test tier for the behavior being verified:

| Tier | Mechanism & Location | Coverage |
| --- | --- | --- |
| Pure unit tests | Standard `#[test]` inside `crates/` | Dual-domain config serialization/migration, YAML Merge & QuickJS sandbox limits, `verve-protocol` frame round-trips, ring buffers, and pure formatters. |
| Headless integration tests | `crates/verve-core/tests/` | End-to-end `AppCoordinator` command handling, `CompilerPipeline` preflight + rollback, `TelemetryHub` lease ref-counting, and mock `ProxyKernel` / `BackupBackend` orchestration without a GUI window. |
| Headless UI tests | `#[gpui::test]` using `gpui_material::test` | Feature slice rendering, `UiStore` selective slice refresh, side-sheet lifecycle, keyboard focus, and `verve-editor` diagnostics/completions. |
| Desktop smoke & OS integration | Manual / platform runner verification | Native tray behavior (`tray-icon` / `ksni`), window close/reopen GPU release, system proxy snapshot/restore self-heal, privileged `verve-service` TUN handoff, and system IME input. |

Testing guidelines:

- Prefer plain `#[test]` whenever GPUI `App` / `Window` is not required.
- Use fake/in-memory implementations of `ProxyKernel`, `ScriptEngine`, and `BackupBackend` to test failure recovery, cancellation, and rollback deterministically.
- Keep tests high-signal and clean up obsolete fixtures whenever behavior changes.

<a id="upstream-contributions"></a>

## Upstream contributions

- When encountering defects in `gpui-material`, `gpui-base`, GPUI, or Mihomo, isolate a minimal reproduction against the pinned upstream version first.
- Follow the upstream repository's contribution and licensing rules, keeping patches minimal and focused.
- Link upstream issues or PRs in a local Verve tracking issue and record the removal condition for any temporary adapter.

<a id="development-environment"></a>

## Development environment

Install [Rustup](https://rustup.rs/) matching the workspace toolchain pinned in `rust-toolchain.toml` (once initialized in `0.0.1`).

| Platform | Prerequisites |
| --- | --- |
| Windows 11 x64 | Visual Studio Build Tools ("Desktop development with C++" workload: MSVC x64 + Windows SDK). |
| macOS (Apple Silicon / x64) | Xcode Command Line Tools. |
| Linux (Ubuntu 24.04 x64) | Standard Rust/C build essentials, Wayland/X11 development headers required by GPUI, and `dbus` / `libcap` tooling for tray/TUN checks. |

When working across parallel Git worktrees, point `CARGO_TARGET_DIR` to the main checkout's `target` directory to reuse incremental build artifacts.

<a id="local-checks"></a>

## Local checks

Before opening or updating a PR, run the checks matching your change scope (active once the Rust workspace is initialized in `0.0.1`):

```powershell
git diff --check origin/main
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
$env:RUSTDOCFLAGS = '-D warnings'; cargo doc --locked --workspace --no-deps --all-features
cargo test --locked --workspace --all-features
```

Targeted headless checks during backend iteration:

```powershell
cargo test --locked -p verve-config -p verve-compiler -p verve-core
```
