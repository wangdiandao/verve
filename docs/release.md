# Versioning and releases

English | [中文](release.zh.md)

<a id="version-policy"></a>

## Version progression policy

All workspace crates (`verve-protocol`, `verve-config`, `verve-kernel`, `verve-kernel-mihomo`, `verve-compiler`, `verve-sys`, `verve-sync`, `verve-core`, `verve-service`, `verve-ui-kit`, `verve-editor`, and `verve-app`) share a single version in the root `[workspace.package]`.

Verve does **not** track development order with abstract phases. Every milestone maps directly to a semantic version with clear progression rules:

1. **`0.0.x` — Pre-`0.1.0` milestones (`0.0.1`–`0.0.6`)**
   - `0.0.1`–`0.0.2` build the headless core, OS integration, privileged service, and native UI shell; **`0.0.3`** reaches standalone daily-usable **P0 core** parity (subscription import, Mihomo kernel control, proxy group selection & delay testing, system proxy & privileged TUN mode, native M3 UI, and window-decoupled tray residency); `0.0.4`–`0.0.6` complete P1–P2 capabilities and three-platform polish.
   - Each completed milestone increments `patch` (`0.0.1` → `0.0.2` → `0.0.3` → ... → `0.0.6`).
   - Internal traits, schemas, or IPC may evolve freely and no formal end-user installers are distributed yet.
2. **`0.1.0` — First public multi-platform release**
   - Locks `verve-config` forward migration guarantees, freezes the `verve-protocol` v1 IPC contract, and finalizes signed three-platform automated GitHub Releases.
3. **`0.y.z` (`0.1.0 <= v < 1.0.0`) — Public pre-1.0 iteration**
   - Increment `minor` (`y`, resetting `z` to `0`) for a new feature milestone, major capability addition, or breaking change to user configuration schema or `verve-protocol` IPC.
   - Increment `patch` (`z`) for backward-compatible bug fixes, platform adjustments, security/dependency patch updates, and UX refinements.
   - When a release combines multiple change types, apply the highest applicable bump.
   - Document every breaking change and migration step in [CHANGELOG.md](../CHANGELOG.md), including before `1.0.0`.
4. **`1.0.0+` — Stable releases ([SemVer 2.0.0](https://semver.org/))**
   - Increment `major` for incompatible configuration schema, `verve-protocol` IPC, or public extension contract changes.
   - Increment `minor` for backward-compatible features and public deprecations.
   - Increment `patch` for backward-compatible bug and security fixes.

<a id="versioned-roadmap"></a>

## Version-driven development roadmap

| Target Version | Milestone Title | Deliverable Scope | Exit Criteria |
| --- | --- | --- | --- |
| **`0.0.1`** | Workspace skeleton & headless kernel loop | Initialize Cargo Workspace DAG; implement `verve-protocol`, `verve-config` (dual-domain config, atomic writes, SQLite WAL metadata), `verve-kernel`, `verve-kernel-mihomo` (Named Pipe / UDS control plane & `mihomo -t` preflight), `verve-compiler` (YAML Merge pipeline), and `verve-core` (`AppCoordinator` + sliced `AppSnapshot`). | Headless integration tests load a real subscription YAML, compile `active.yaml`, start Mihomo, fetch proxy topology over Pipe/UDS, and switch active nodes. |
| **`0.0.2`** | Native UI foundation, OS integration & privileged service | Implement `verve-ui-kit` (M3 theme tokens, shared primitives, Canvas traffic chart), `verve-sys` (system proxy snapshot/restore/self-heal, tray backend, single instance, auto-launch), `verve-service` (minimal authenticated IPC supervisor + anti-zombie bindings), and `verve-app` (`AppShell`, `UiStore` slice projection, `TelemetryLease`, and `rust-i18n` `zh-CN` / `en-US` bilingual UI & tray localization). | Desktop shell launches cleanly with live `zh-CN` / `en-US` switching, closes the main window while releasing rendering resources and staying resident in the tray, and toggles system proxy or privileged TUN startup. |
| **`0.0.3`** | **First daily-usable milestone (P0 core)** | Deliver all P0 daily-essential views backed by live `verve-core` state: **Overview** (mode switch, system proxy/TUN controls, active profile summary, quick node switch, live traffic chart, copy shell proxy env), **Proxies** (collapsible groups, responsive node grid, single & group delay test, search/sort), **Profiles** (URL import, local YAML creation/import, activate, quota/expiry display, refresh, delete), and **Settings — Network & Core Appearance** (Bypass, TUN essentials, service install/uninstall, `mixed-port`, Allow LAN, IPv6, theme & accent). | Can replace existing desktop clients as a primary daily proxy client end-to-end on Windows, macOS, or Linux. |
| **`0.0.4`** | Native override workbench & Activity center (P1 part 1) | Add `verve-editor` (Rope + Tree-sitter YAML/JS highlighting, Mihomo static completions, inline compiler diagnostics) and `verve-compiler` QuickJS sandbox; enable Profile/Global Merge & Script editing, read-only runtime config preview, inline script console, scheduled subscription refresh; deliver **Activity** (Connections table + details drawer + right-click one-click Add Rule via Quick Rules stage, Rules + Rule Providers, real-time Logs) and Proxy Providers drawer. | Users can edit YAML/JS overrides with live preflight diagnostics and troubleshoot traffic, rules, and logs inside a single on-demand Activity center with zero background stream leakage. |
| **`0.0.5`** | Sync & complete Settings (P1 part 2) | Add `verve-sync` (`BackupBackend`, local encrypted archives, WebDAV sync & snapshot restore); complete all 4 Settings categories: DNS override (curated form + raw YAML dual-mode), GeoData update, unified delay, auto-close connections on switch, auto log cleanup, silent start, app directories, update check, and reset defaults. | Complete daily experience covering all P0 + P1 features in [Product design](design.md). |
| **`0.0.6`** | Power-user refinements & three-platform polish (P2) | Global hotkeys (4–5 core actions), on-demand exit IP lookup, opt-in external TCP controller (collapsed advanced section), closed connection history, sanitized diagnostic bundle export, and cross-platform performance tuning (cold start, 10k+ rules/connections virtual scrolling). | Native-grade responsiveness, memory discipline, and polish across Windows, macOS, and Linux. |
| **`0.1.0`** | **First public stable contract & multi-platform release** | Freeze `verve-config` forward migration guarantees, lock `verve-protocol` v1 IPC stability, and finalize signed three-platform automated GitHub Releases. | Full contract stability commitment and production three-platform installer distribution. |

<a id="changelog"></a>

## Changelog

Record user-visible changes under `Unreleased` in [CHANGELOG.md](../CHANGELOG.md) as PRs merge—new features, observable fixes, security or performance improvements, platform requirements, and configuration/IPC migrations. Exclude pure documentation, CI-only, or internal-only refactorings that have no user-visible impact. Group entries by area (`Core & Kernel`, `Compiler & Overrides`, `UI & UX`, `System & Service`, `Sync & Backup`, `Breaking & Migration`).

When cutting a release:

1. Reconcile `Unreleased` against the commit range since the previous tag.
2. Move entries under `## X.Y.Z - YYYY-MM-DD`, preserving an empty `## Unreleased` section at the top.
3. Include contributor credits and a `Full Changelog` GitHub compare link for `0.1.0` and later releases.

<a id="prepare-and-publish"></a>

## Prepare and publish

1. **Release PR**: Bump `[workspace.package].version` (and internal crate version pins), update `Cargo.lock`, finalize `CHANGELOG.md`, and verify version references in documentation.
2. **Automated checks & smoke verification**: Ensure all CI checks are green on `main` (headless `verve-core` tests, `verve-compiler` sandbox tests, `gpui_material::test` UI tests, Clippy, Rustdoc) and run platform smoke verification (tray residency, window close/reopen, profile compilation, node switch, system proxy/TUN recovery).
3. **Tag and GitHub Release**: Create an annotated `vX.Y.Z` tag on `main` and publish the GitHub Release (with platform artifacts starting at `0.1.0`).
