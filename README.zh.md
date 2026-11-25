# Verve

[English](README.md) | 中文

Verve 是一款基于 **Rust** 与 **GPUI (`gpui-material`)** 构建的跨平台（Windows / macOS / Linux）原生桌面代理客户端。首期聚焦 **Mihomo** 内核并通过抽象边界保持低耦合，原生支持 **声明式 Merge** 与沙箱化 **JavaScript** 双配置覆写体系，采用克制、高密度的 Material 3 Expressive 原生界面。

> **当前状态**：`0.0.x` 早期开发中，将于 **`0.0.3`** 达到日常主力可用（P0 核心），并于 **`0.1.0`** 发布首个稳定契约与三端正式版。完整版本路线与递进规则见[版本与发布](docs/release.zh.md)。

## 设计目标

- **原生常驻与窗口解耦**：基于 GPUI 原生 GPU 渲染；关闭主窗口即释放全部窗口与渲染资源，后台核心与系统托盘零感常驻。
- **强模块化与低耦合**：编译期强制 Cargo Workspace 单向依赖 DAG、无头后端核心（`verve-core`）、UI 垂直功能切片（`verve-app`）以及显式扩展契约（`ProxyKernel`、`CompilerStage`、`ScriptEngine`、`BackupBackend`）。
- **精简日常体验**：剔除老客户端历史包袱，收敛为 5 个高频核心视图（概览、代理、配置、活动、设置），坚持 Material 3 内联控件与右侧抽屉交互，消灭多层弹窗套娃。
- **最小权限与安全隔离**：GUI 始终以普通用户权限运行；TUN 模式通过强认证本地 IPC 委托零业务逻辑的极简守护进程（`verve-service`）拉起经校验的内核。配置编译全程不可变预检并支持原子回滚。

## 版本路线概览

开发顺序严格以实际版本号驱动：

| 目标版本 | 交付范围 |
| --- | --- |
| `0.0.1` | Workspace 骨架与无头内核闭环（`verve-config`、`verve-kernel`、`verve-kernel-mihomo`、`verve-compiler` Merge 与预检、`verve-core`）。 |
| `0.0.2` | 原生 UI 底座（`verve-ui-kit`）、系统集成（`verve-sys`）、特权守护进程（`verve-service`）以及 `verve-app` 壳层、托盘与 `UiStore` 桥接。 |
| **`0.0.3`** | **首个达到日常主力可用的里程碑（P0 刚需核心）**：概览、代理、配置、网络与基础设置、系统代理与 TUN 接管，可独立承担日常代理上网。 |
| `0.0.4` | 原生 `verve-editor`（YAML/JS 高亮、静态补全与预检诊断）、QuickJS 覆写沙箱、活动中心（连接监控与右键一键加规则、规则、日志）与 Providers 管理。 |
| `0.0.5` | `verve-sync`（本地加密归档与 WebDAV 同步）、DNS 覆写双模式、GeoData 更新、多语言（中/英）与完整设置项。 |
| `0.0.6` | 全局快捷键、出口 IP 按需检测、可选外部 TCP 控制、脱敏诊断包导出及三端体验与性能深度打磨。 |
| **`0.1.0`** | **首个稳定契约与三端正式发布版**：锁定 `verve-config` 配置向前迁移保障、冻结 `verve-protocol` v1 IPC 稳定契约，并完善三端签名与自动化发布流水线。 |

## 项目文档

- [文档索引](docs/index.zh.md)
- [系统架构](docs/architecture.zh.md)
- [产品设计与功能盘点](docs/design.zh.md)
- [开发指南](docs/development.zh.md)
- [版本与发布](docs/release.zh.md)
- [贡献指南](CONTRIBUTING.zh.md)
- [变更日志](CHANGELOG.md)

## 许可协议

Verve 计划采用 **GPL-3.0-or-later** 开源许可协议。
