# 版本与发布

[English](release.md) | 中文

<a id="version-policy"></a>

## 版本号递进规则

Workspace 内全部内部 Crate（`verve-protocol`、`verve-config`、`verve-kernel`、`verve-kernel-mihomo`、`verve-compiler`、`verve-sys`、`verve-sync`、`verve-core`、`verve-service`、`verve-ui-kit`、`verve-editor` 与 `verve-app`）统一使用根清单 `[workspace.package]` 的语义化版本号。

Verve **不使用抽象的 Phase 划分开发顺序**，所有里程碑直接以实际版本号定义，并严格遵循以下递进规则：

1. **`0.0.x` — `0.1.0` 前期里程碑（`0.0.1`–`0.0.6`）**
   - `0.0.1`–`0.0.2` 完成无头核心闭环、系统接管、特权服务与原生 UI 壳层；**`0.0.3`** 达到可脱离其他客户端独立承担日常代理上网的 **P0 刚需核心**可用门槛（订阅导入与管理、Mihomo 内核运行控制、代理组选择与测速、系统代理与特权 TUN 接管、原生 M3 界面及关窗常驻托盘）；`0.0.4`–`0.0.6` 依次补齐 P1–P2 能力与三端深度打磨。
   - 每完成一个里程碑递增 `patch`（`0.0.1` → `0.0.2` → `0.0.3` → … → `0.0.6`）。
   - 允许内部 Trait、配置结构与 IPC 自由演进；暂不面向终端用户发布正式安装包。
2. **`0.1.0` — 首个稳定契约与三端正式发布版**
   - 锁定 `verve-config` 配置向前迁移保障、冻结 `verve-protocol` v1 IPC 稳定契约，并完善三端签名与自动化 GitHub Releases 发布。
3. **`0.y.z`（`0.1.0 <= v < 1.0.0`）— 公开迭代期**
   - 递增 `minor`（`y`，同时将 `z` 归零）：交付新的功能里程碑、核心能力扩展，或发生配置 Schema / `verve-protocol` IPC 破坏性变更。
   - 递增 `patch`（`z`）：向后兼容的缺陷修复、平台适配修正、依赖安全小版本更新与交互体验微调。
   - 同一版本混合多种变更时采用最高适用级别。
   - 所有破坏性变更（包括 `1.0.0` 之前）必须在 [CHANGELOG.md](../CHANGELOG.md) 中写明迁移步骤。
4. **`1.0.0+` — 稳定期（严格遵循 [SemVer 2.0.0](https://semver.org/lang/zh-CN/)）**
   - `major`：配置 Schema、`verve-protocol` IPC 或公开扩展契约的不兼容变更。
   - `minor`：向后兼容的新功能与公开弃用。
   - `patch`：向后兼容的缺陷修复与安全补丁。

<a id="versioned-roadmap"></a>

## 按版本号驱动的开发路线

| 目标版本 | 里程碑主题 | 交付范围 | 验收标准 |
| --- | --- | --- | --- |
| **`0.0.1`** | 工程骨架与无头内核闭环 | 建立 Cargo Workspace 单向依赖 DAG；落地 `verve-protocol`、`verve-config`（双域配置、原子落盘、SQLite WAL 元数据）、`verve-kernel`、`verve-kernel-mihomo`（Named Pipe / UDS 控制面与 `mihomo -t` 预检）、`verve-compiler`（YAML Merge 管线）与 `verve-core`（`AppCoordinator` + 分域 `AppSnapshot`）。 | 无头集成测试可加载真实订阅 YAML、编译 `active.yaml`、拉起 Mihomo、经管道拉取代理拓扑并切换节点。 |
| **`0.0.2`** | 原生 UI 底座、系统接管与特权服务 | 落地 `verve-ui-kit`（M3 主题 Token、基础复合件、Canvas 流量图）、`verve-sys`（系统代理接管前快照与异常自愈、托盘底层、单实例、开机自启）、`verve-service`（极简强认证 IPC 守护进程与防僵尸绑定），以及 `verve-app`（`AppShell`、`UiStore` 分域切片投影、`TelemetryLease`）。 | 桌面窗口正常启动，关闭窗口即释放渲染资源并常驻托盘，可通过系统代理或特权 TUN 模式成功拉起与托管内核。 |
| **`0.0.3`** | **首个达到日常主力可用的里程碑（P0 刚需核心）** | 交付全部基于真实 `verve-core` 状态的 P0 核心视图：**概览**（代理模式、系统代理/TUN 主控、当前配置摘要、出口快切、实时流量曲线、复制终端代理命令）、**代理**（分组折叠、自适应节点网格、单点与整组测速、搜索与排序）、**配置**（URL 一键导入、新建/导入本地 YAML、激活、流量与到期展示、刷新与删除）、**设置（网络与基础外观）**（Bypass、TUN 核心项、服务安装卸载、`mixed-port`、允许局域网、IPv6、浅深色与动态主题色）。 | 可在 Windows / macOS / Linux 上独立作为主力代理客户端稳定完成日常上网任务。 |
| **`0.0.4`** | 原生覆写工作台与活动排障中心（P1 上半） | 落地 `verve-editor`（Rope + Tree-sitter YAML/JS 高亮、Mihomo 静态补全、编译诊断行内提示）与 `verve-compiler` QuickJS 沙箱；配置页支持单配置/全局 Merge 与 JS Script 编辑、运行配置只读预览、内嵌脚本控制台、定时/全部刷新；交付**活动中心**（连接监控表 + 详情抽屉 + 右键一键加规则写入 Quick Rules 阶段、规则与 Rule Providers、实时日志）及代理页 Proxy Providers 抽屉。 | 用户可在原生编辑器内完成 YAML/JS 覆写并实时获得预检诊断，同时在按需开启的活动中心内一站式排查连接、规则与日志，离开页面零后台流空转。 |
| **`0.0.5`** | 数据同步、完整设置与多语言（P1 下半） | 落地 `verve-sync`（`BackupBackend`、本地加密归档、WebDAV 同步与历史恢复）；补齐设置页全部四分类：DNS 覆写（精简表单 + 原生 YAML 双模式）、GeoData 更新、统一延迟、切节点自动断连、自动日志清理、多语言（`rust-i18n` 中/英）、静默启动、应用目录、检查更新与恢复默认设置。 | 完整覆盖[产品设计](design.zh.md)中全部 P0 + P1 日常需求。 |
| **`0.0.6`** | 进阶能力与三端深度打磨（P2 增强） | 全局快捷键（4~5 核心动作）、出口公网 IP 按需检测、可选外部 TCP 控制（高级折叠区）、已关闭连接历史、脱敏诊断包导出，以及三端冷启动、关窗内存与万级规则/连接虚拟列表性能调优。 | Windows / macOS / Linux 三端交互流畅度、资源占用与原生细节达到标杆水准。 |
| **`0.1.0`** | **首个稳定契约与三端正式发布版** | 锁定 `verve-config` 配置向前迁移保障、冻结 `verve-protocol` v1 IPC 稳定契约、完善三端签名与自动化发布流水线。 | 兑现契约稳定性承诺并提供正式跨平台安装包分发。 |

<a id="changelog"></a>

## 变更日志

面向用户的变更在 PR 合并时记入 [CHANGELOG.md](../CHANGELOG.md) 的 `Unreleased`，包括新功能、可感知修复、安全与性能改进、平台要求变化以及配置/IPC 迁移说明。纯文档、仅 CI 调整或无外部影响的内部重构不写入变更日志。条目按模块分组（`Core & Kernel`、`Compiler & Overrides`、`UI & UX`、`System & Service`、`Sync & Backup`、`Breaking & Migration`）。

准备发版时：

1. 对照上一个发布 tag 至今的全部提交核对 `Unreleased`。
2. 将本次变更移至 `## X.Y.Z - YYYY-MM-DD` 标题下，并在顶部保留空的 `## Unreleased` 节。
3. 从 `0.1.0` 起，在版本节末尾附贡献者列表与 `Full Changelog` 对比链接，直接作为 GitHub Release 正文。

<a id="prepare-and-publish"></a>

## 准备与发布

1. **发布 PR**：更新 `[workspace.package].version`（及内部 crate 依赖版本）、刷新 `Cargo.lock`、整理 `CHANGELOG.md` 并同步文档中的版本说明。
2. **CI 与桌面冒烟检查**：确认 `main` 上全部 CI 检查通过（`verve-core` 无头测试、`verve-compiler` 沙箱测试、`gpui_material::test` 界面测试、Clippy、Rustdoc），并执行桌面冒烟检查（托盘常驻、关窗再打开、配置编译、节点切换、系统代理与 TUN 异常恢复）。
3. **打标与发布**：在 `main` 上创建带附注的 `vX.Y.Z` tag 并发布 GitHub Release（自 `0.1.0` 起附带跨平台构建产物）。
