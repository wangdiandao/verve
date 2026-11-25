# 开发指南

[English](development.md) | 中文

<a id="contribution-workflow-and-branch-naming"></a>

## 贡献流程与分支命名

- Issue 提议、范围合规与评审要求见[贡献指南](../CONTRIBUTING.zh.md)。
- 从最新 `main` 创建分支，使用小写英文与连字符 `<类型>/<简短主题>`（如 `feat/config-compiler`、`fix/tun-dns-hijack`、`docs/architecture-update`），并在 PR 说明中关联对应 Issue。
- 前缀按改动目的选用：`feat/`、`fix/`、`docs/`、`test/`、`refactor/`、`ci/`、`chore/`、`release/`。

<a id="workspace-and-module-organization"></a>

## Workspace 与模块组织

Verve 通过多 Crate Workspace 强制单向依赖 DAG，完整拓扑见[系统架构](architecture.zh.md#3-workspace-模块划分与依赖拓扑)：

- **无头核心与适配层（`crates/verve-{protocol,config,kernel,kernel-mihomo,compiler,sys,sync,core}`）**：承载全部代理领域逻辑、持久化、配置编译、IPC 与系统集成，零 GPUI 依赖。
- **特权守护进程（`crates/verve-service`）**：仅依赖 `verve-protocol` 与 OS 原语，严禁引入配置解析、JS 引擎或业务逻辑。
- **UI 底座（`crates/verve-ui-kit`、`crates/verve-editor`）**：封装 `gpui-material` 主题/复合件与原生 YAML/JS 编辑器，不持有业务领域状态。
- **桌面主程序（`crates/verve-app`）**：严格采用 **Shell + 垂直功能切片** 结构（`shell/` + `features/{overview,proxies,profiles,activity,settings}/`）。

<a id="coding-conventions"></a>

## 编码规范

- **文档先行**：从[文档索引](index.zh.md)定位当前规格。涉及架构、契约或用户可见行为变更时，在同一个 PR 中同步更新中英双语文档（`*.md` 与 `*.zh.md`）。
- **严守模块与切片边界**：
  - `verve-core` 及更底层 crate 保持 100% 无头可测。
  - 每个 UI 功能模块独占自身的 GPUI `Entity`、`TextFieldState`、`ScrollHandle` 及局部 `SideSheet`/确认视图；禁止向根窗口塞入跨页字段或维护全局弹窗大枚举。
  - 高频遥测流必须绑定由订阅视图持有的 RAII `TelemetryLease`，卸载或关窗时自动回收。
- **禁止纯假数据 UI 堆砌**：`verve-app` 中呈现的每个控件必须对应 `verve-core` 中真实存在的 `AppCommand` 与 `AppSnapshot` 分域切片。
- **原子落盘**：配置文件与编译产物写入统一经过同目录 `tempfile -> sync_all -> atomic rename`。
- **Rust 工程约定**：收窄内部可见性（`pub(super)` / `pub(crate)`），库边界使用 `thiserror` 明确领域错误类型，优先静态分发与封闭枚举，避免无必要的动态虚表。

<a id="testing-conventions"></a>

## 测试规范

按需验证的行为选择最低有效测试层级：

| 层级 | 机制与位置 | 覆盖范围 |
| --- | --- | --- |
| 纯逻辑单元测试 | `crates/` 下的普通 `#[test]` | 双域配置序列化与版本迁移、YAML Merge 与 QuickJS 沙箱配额/超时、`verve-protocol` 帧编解码、环形缓冲区与纯格式化器。 |
| 无头核心集成测试 | `crates/verve-core/tests/` | 无需 GUI 窗口端到端验证 `AppCoordinator` 命令串行化、`CompilerPipeline` 预检与回滚、`TelemetryHub` 租约引用计数，以及基于 Mock `ProxyKernel` / `BackupBackend` 的编排流程。 |
| 无头 UI 集成测试 | 基于 `gpui_material::test` 的 `#[gpui::test]` | 功能切片渲染、`UiStore` 分域增量刷新、抽屉开关与焦点恢复、`verve-editor` 诊断与补全闭环。 |
| 桌面冒烟与系统集成 | 手工 / 平台环境验证 | 原生托盘（`tray-icon` / `ksni`）、关窗释放渲染资源与重开恢复、系统代理快照/异常自愈、`verve-service` 特权 TUN 接管及系统 IME 输入法行为。 |

测试编写约定：

- 不依赖 GPUI `App` / `Window` 时优先使用普通 `#[test]`。
- 通过内存/Mock 版 `ProxyKernel`、`ScriptEngine`、`BackupBackend` 确定性验证失败回滚、任务取消与边界错误路径。
- 保持高信噪比，行为演进时及时清理过时测试夹具。

<a id="upstream-contributions"></a>

## 上游贡献

- 确认 `gpui-material`、`gpui-base`、GPUI 或 Mihomo 存在缺陷时，先基于当前锁定版本构造最小复现。
- 遵循对应上游仓库的贡献、测试与许可规则，保持补丁最小化。
- 在本地跟踪 Issue 中关联上游 Issue/PR，并记录临时适配逻辑的移除条件。

<a id="development-environment"></a>

## 开发环境

安装 [Rustup](https://rustup.rs/) 并与 `rust-toolchain.toml`（于 `0.0.1` 初始化）中锁定的工具链版本保持一致。

| 平台 | 构建依赖 |
| --- | --- |
| Windows 11 x64 | Visual Studio Build Tools（「使用 C++ 的桌面开发」工作负载，含 MSVC x64 与 Windows SDK）。 |
| macOS（Apple Silicon / x64） | Xcode 命令行工具。 |
| Linux（Ubuntu 24.04 x64） | 标准 Rust/C 编译工具链、GPUI 所需的 Wayland/X11 开发头文件，以及托盘/TUN 所需的 `dbus` / `libcap` 环境。 |

在并行 Git worktree 中开发时，将 `CARGO_TARGET_DIR` 指向主检出的 `target` 目录以复用增量构建缓存。

<a id="local-checks"></a>

## 本地检查

提交或更新 PR 前，按改动范围运行检查（Rust Workspace 于 `0.0.1` 初始化后启用）：

```powershell
git diff --check origin/main
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
$env:RUSTDOCFLAGS = '-D warnings'; cargo doc --locked --workspace --no-deps --all-features
cargo test --locked --workspace --all-features
```

后端迭代期间的定向无头测试：

```powershell
cargo test --locked -p verve-config -p verve-compiler -p verve-core
```
