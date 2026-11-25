# Verve 贡献指南

[English](CONTRIBUTING.md) | 中文

## Issue 与 PR

- 提交前先搜索现有 Issue 和 PR。缺陷报告须包含最小复现步骤、操作系统与版本、内核版本以及预期与实际行为。功能提议须说明具体的日常使用场景，并对照[产品设计](docs/design.zh.md)说明其与精简定位的契合度。
- 从最新 `main` 创建分支，使用 `<类型>/<简短主题>` 命名（如 `feat/mihomo-pipe`、`fix/sysproxy-restore`、`docs/release-policy`）。每个 PR 只聚焦一个问题并关联对应 Issue。详见[分支命名约定](docs/development.zh.md#contribution-workflow-and-branch-naming)。

## 范围与架构合规

- 功能新增与界面调整必须遵守[产品设计与精简原则](docs/design.zh.md)以及[系统架构边界](docs/architecture.zh.md)：
  - 严禁在 `verve-core` 及更底层 crate 中引入任何 GPUI 或 UI 依赖。
  - `verve-app` 保持独立垂直功能切片（`shell` + `features/*`），禁止引入全局上帝状态或全局弹窗大枚举。
  - 基于 `gpui-material` 遵循 Material 3 Expressive 交互层级：简单设置内联修改、详情/表单/代码编辑使用右侧 `SideSheet` 抽屉、仅破坏性确认使用模态对话框。
- 若希望重新引入[全量功能盘点](docs/design.zh.md#第一部分现有界面全量功能盘点与去留决策)中标记为 `Drop` 的历史项，须先提交 Issue 说明为何应进入核心客户端而非通过覆写脚本或可选扩展实现。

## PR 评审与合并

- 遵守[编码规范](docs/development.zh.md#coding-conventions)与[测试规范](docs/development.zh.md#testing-conventions)。贡献者须对所提交代码负责，包括对 AI 辅助编写的代码进行审查与验证。
- 若改动涉及公开 Trait 契约、`verve-protocol` IPC 帧或 `verve-config` 配置 Schema，须在 PR 说明中列出签名变更与兼容性影响，并在[变更日志](CHANGELOG.md)记录迁移步骤。
- 在 PR 说明中记录可复现的验证命令、测试环境与结果；涉及界面视觉改动时附截图或录屏。
- 面向用户的变更在合并前写入 [CHANGELOG.md](CHANGELOG.md) 的 `Unreleased` 下；纯文档改动不写入变更日志，详见[变更日志规则](docs/release.zh.md#changelog)。
- PR 经维护者批准、所有评审意见解决且最新提交通过全部必选 CI 检查后合并。

## 向上游提交 Issue 与贡献

超出 Verve 职责范围的问题，应直接向对应上游项目提交 Issue 或修复：

- Material 3 主题、组件与桌面标题栏归属 [`gpui-material`](https://github.com/wangdiandao/gpui-material)。
- 通用控件行为、焦点与输入法基础能力归属 GPUI Kit 的 `gpui-base`。
- 窗口管理、GPU 渲染与平台底层事件归属 Zed 的 GPUI。
- 代理协议转发与内核路由行为归属 Mihomo。

上游问题影响 Verve 时，在本地跟踪 Issue 中关联上游链接，并记录临时适配方案及移除条件。详见[上游贡献](docs/development.zh.md#upstream-contributions)。
