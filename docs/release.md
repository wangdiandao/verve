# Versioning and releases

English | [中文](release.zh.md)

<a id="version-policy"></a>

## Version progression policy

All workspace crates (`verve-protocol`, `verve-config`, `verve-kernel`, `verve-kernel-mihomo`, `verve-compiler`, `verve-sys`, `verve-sync`, `verve-core`, `verve-service`, `verve-ui-kit`, `verve-editor`, and `verve-app`) share a single version in the root `[workspace.package]`.

Verve does **not** track development order with abstract phases. Every milestone maps directly to a semantic version with clear progression rules:

1. **`0.0.x` — Pre-usable foundation milestones (`0.0.1`, `0.0.2`, ...)**
   - Used while bootstrapping the workspace, headless backend core, system integration, and UI shell before the application is ready for daily proxy use.
   - Each completed internal milestone increments `patch` (`0.0.1` → `0.0.2`).
   - Breaking internal trait, schema, or IPC changes are allowed freely; no end-user installer releases are published.
2. **`0.1.0` — First daily-usable release**
   - Marks the threshold where Verve can serve as a standalone daily desktop proxy client covering all **P0 core requirements** (subscription import, Mihomo kernel control, proxy group selection & delay testing, system proxy & privileged TUN mode, native M3 UI, and window-decoupled tray residency).
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
| **`0.0.2`** | Native UI foundation, OS integration & privileged service | Implement `verve-ui-kit` (M3 theme tokens, shared primitives, Canvas traffic chart), `verve-sys` (system proxy snapshot/restore/self-heal, tray backend, single instance, auto-launch), `verve-service` (minimal authenticated IPC supervisor + anti-zombie bindings), and `verve-app` (`AppShell`, `UiStore` slice projection, `TelemetryLease`). | Desktop shell launches cleanly, closes the main window while releasing rendering resources and staying resident in the tray, and toggles system proxy or privileged TUN startup. |
| **`0.1.0`** | **First daily-usable release (P0 core)** | Deliver all P0 daily-essential views backed by live `verve-core` state: **Overview** (mode switch, system proxy/TUN controls, active profile summary, quick node switch, live traffic chart, copy shell proxy env), **Proxies** (collapsible groups, responsive node grid, single & group delay test, search/sort), **Profiles** (URL import, local YAML creation/import, activate, quota/expiry display, refresh, delete), and **Settings — Network & Core Appearance** (Bypass, TUN essentials, service install/uninstall, `mixed-port`, Allow LAN, IPv6, theme & accent). | Can replace existing desktop clients as a primary daily proxy client end-to-end on Windows, macOS, or Linux. |
| **`0.2.0`** | Native override workbench & Activity center (P1 part 1) | Add `verve-editor` (Rope + Tree-sitter YAML/JS highlighting, Mihomo static completions, inline compiler diagnostics) and `verve-compiler` QuickJS sandbox; enable Profile/Global Merge & Script editing, read-only runtime config preview, inline script console, scheduled subscription refresh; deliver **Activity** (Connections table + details drawer + right-click one-click Add Rule via Quick Rules stage, Rules + Rule Providers, real-time Logs) and Proxy Providers drawer. | Users can edit YAML/JS overrides with live preflight diagnostics and troubleshoot traffic, rules, and logs inside a single on-demand Activity center with zero background stream leakage. |
| **`0.3.0`** | Sync, complete Settings & i18n (P1 part 2) | Add `verve-sync` (`BackupBackend`, local encrypted archives, WebDAV sync & snapshot restore); complete all 4 Settings categories: DNS override (curated form + raw YAML dual-mode), GeoData update, unified delay, auto-close connections on switch, auto log cleanup, bilingual UI (`rust-i18n` `zh-CN` / `en-US`), silent start, app directories, update check, and reset defaults. | Complete daily experience covering all P0 + P1 features in [Product design](design.md). |
| **`0.4.0`** | Power-user refinements & three-platform polish (P2) | Global hotkeys (4–5 core actions), on-demand exit IP lookup, opt-in external TCP controller (collapsed advanced section), closed connection history, sanitized diagnostic bundle export, and cross-platform performance tuning (cold start, 10k+ rules/connections virtual scrolling). | Native-grade responsiveness, memory discipline, and polish across Windows, macOS, and Linux. |
| **`1.0.0`** | Stable contract release | Freeze `verve-config` migration policy, lock `verve-protocol` v1 IPC stability, and finalize signed three-platform automated GitHub Releases. | Full SemVer 2.0.0 commitment and production installer distribution. |

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
