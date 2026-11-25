# Contributing to Verve

English | [中文](CONTRIBUTING.zh.md)

## Issues and pull requests

- Search existing issues and PRs before opening a new one. Bug reports should include a minimal reproduction, operating system and version, kernel version, and expected vs. observed behavior. Feature proposals should describe the concrete daily use case and explain how it fits within Verve's streamlined scope in the [product design](docs/design.md).
- Branch from the latest `main` using `<type>/<short-topic>` (for example `feat/mihomo-pipe`, `fix/sysproxy-restore`, `docs/release-policy`). Keep each PR focused on a single problem and link the corresponding issue. See [branch naming conventions](docs/development.md#contribution-workflow-and-branch-naming).

## Scope and architecture compliance

- Feature additions and UI changes must respect the [product design and anti-bloat principles](docs/design.md) and the [system architecture boundaries](docs/architecture.md):
  - Never add GPUI or UI dependencies to `verve-core` or lower crates.
  - Keep `verve-app` organized as independent feature slices (`shell` + `features/*`); do not introduce global god-structs or global modal enums.
  - Follow Material 3 Expressive patterns via `gpui-material`: inline controls for simple toggles, slide-over `SideSheet` for details/forms/editors, and modal dialogs only for destructive confirmations.
- Proposals to re-introduce features marked as `Drop` in [the feature audit](docs/design.md#part-1-complete-feature-audit-and-triage) require an issue explaining why they belong in the core client rather than an override script or optional extension.

## PR review and merge

- Follow the [coding conventions](docs/development.md#coding-conventions) and [testing conventions](docs/development.md#testing-conventions). Contributors are responsible for reviewing and verifying all submitted code, including AI-assisted work.
- When a change updates public crate traits, `verve-protocol` IPC frames, or `verve-config` schemas, document the exact signatures, compatibility impact, and migration path in [the changelog](CHANGELOG.md).
- Record reproducible verification commands, target platform, and results in the PR description. Include screenshots or recordings for visual UI changes.
- Record user-visible changes under `Unreleased` in [CHANGELOG.md](CHANGELOG.md). Pure documentation changes are excluded; see [changelog rules](docs/release.md#changelog).
- PRs merge after maintainer approval, resolution of review comments, and green required CI checks on the latest commit.

## Upstream issues and contributions

Report issues directly to the upstream project when behavior belongs outside Verve:

- Material 3 theming, components, and desktop window chrome belong in [`gpui-material`](https://github.com/wangdiandao/gpui-material).
- Reusable control behavior, focus, and IME handling belong in GPUI Kit's `gpui-base`.
- Windowing, GPU rendering, and platform events belong in Zed's GPUI.
- Proxy protocol routing and kernel behavior belong in Mihomo.

When an upstream bug impacts Verve, link the upstream issue or PR from a local tracking issue and document any temporary workaround along with its removal condition. See [upstream contributions](docs/development.md#upstream-contributions).
