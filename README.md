# Verve

English | [中文](README.zh.md)

Verve is a cross-platform (Windows / macOS / Linux) native desktop proxy client built with **Rust** and **GPUI (`gpui-material`)**. It targets the **Mihomo** kernel first with decoupled abstractions, native **YAML Merge** and sandboxed **JavaScript** configuration overrides, and a clean Material 3 Expressive interface.

> **Status:** Early pre-release development (`0.0.x` in progress), reaching daily-usable P0 parity at **`0.0.3`** and the first stable multi-platform release at **`0.1.0`**. See [Versioning and releases](docs/release.md) for the full versioned roadmap.

## Design goals

- **Native and window-decoupled.** GPU-rendered via GPUI; closing the main window releases all window and rendering resources while the headless core and system tray remain resident.
- **Modular and decoupled.** Strict compile-time Cargo Workspace DAG, headless backend core (`verve-core`), feature-sliced UI modules (`verve-app`), and explicit extension traits (`ProxyKernel`, `CompilerStage`, `ScriptEngine`, `BackupBackend`).
- **Streamlined daily UX.** Five focused views (`Overview`, `Proxies`, `Profiles`, `Activity`, `Settings`) following Material 3 inline controls and slide-over side sheets—no legacy modal sprawl.
- **Least privilege and safety.** The GUI runs unprivileged; TUN mode delegates kernel spawning via authenticated local IPC to a minimal privileged supervisor (`verve-service`). Configuration compilation runs in an immutable preflight pipeline with atomic rollback.

## Versioned roadmap overview

Development proceeds strictly by semantic milestones rather than abstract phases:

| Target Version | Scope |
| --- | --- |
| `0.0.1` | Workspace skeleton and headless kernel loop (`verve-config`, `verve-kernel`, `verve-kernel-mihomo`, `verve-compiler` Merge + preflight, `verve-core`). |
| `0.0.2` | Native UI kit (`verve-ui-kit`), OS integration (`verve-sys`), privileged supervisor (`verve-service`), and `verve-app` shell + tray + `UiStore` bridge + `rust-i18n` bilingual localization (`zh-CN` / `en-US`). |
| **`0.0.3`** | **First daily-usable milestone (P0 core):** Overview, Proxies, Profiles, essential Network & Appearance Settings, system proxy, and TUN mode. |
| `0.0.4` | Native `verve-editor` (YAML/JS + diagnostics), QuickJS override sandbox, Activity center (Connections + one-click Add Rule, Rules, Logs), and Proxy/Rule Providers. |
| `0.0.5` | `verve-sync` (local encrypted archive & WebDAV), DNS override dual-mode, GeoData updates, and full Settings coverage. |
| `0.0.6` | Global hotkeys, on-demand exit IP lookup, external TCP controller (opt-in), sanitized diagnostics export, and cross-platform polish. |
| **`0.1.0`** | **First public stable contract & multi-platform release:** Freeze `verve-config` forward migration guarantees, lock `verve-protocol` v1 IPC stability, and finalize signed three-platform automated GitHub Releases. |

## Project documentation

- [Documentation index](docs/index.md)
- [System architecture](docs/architecture.md)
- [Product design and feature audit](docs/design.md)
- [Development guide](docs/development.md)
- [Versioning and releases](docs/release.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## License

Verve is planned under **GPL-3.0-or-later**.
