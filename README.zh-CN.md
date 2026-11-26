<div align="center">

<img src="assets/logo.svg" alt="ClamGuard" width="80" height="80">

# ClamGuard (mac-clamshell-mcp)

**让合盖跑编译与 AI Agent 的 MacBook 保持凉快、安静且行为可预期。**

[主页](https://clamguard.eatingdream.dev) · [English](README.md) | 简体中文 · [Releases](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases) · [AI 指南](docs/ai-guide.md) · [`llms.txt`](llms.txt)

[![CI](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-1A1916?logo=rust&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-1A1916?logo=apple&logoColor=white)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

<img src="docs/social-preview.png" alt="ClamGuard 面板预览" width="860">

</div>

## 它是什么

一个原生 Rust 编写的 macOS MCP 服务、SEP-1865 实时控制面板与命令行包裹器：实时监测开合盖、供电和热压力，自动限流重型构建并守护合盖显示状态。

它**不是**风扇调速工具、内核扩展或全局 `pmset` 修改器——全程无需 `sudo`，不改写风扇曲线，也不干扰其他应用。

## 核心能力

- **外屏友好的合盖熄屏守护**：未接外屏时，合盖通过 `pmset displaysleepnow` 休眠内屏并在开盖后恢复原亮度；连接外屏时保持外屏工作不触发 `displaysleepnow`，由 macOS 合盖机制管理内屏离线，并分别如实报告「已合盖」「内屏离线」与「已确认熄屏」状态，安全维护开盖恢复亮度目标。
- **自动限流降温**：结合开合盖、电池/插电与 macOS 热压力（兼容 Apple Silicon 与 Intel），自动切换 `normal`、`utility`、`eco`、`strict` 档位并约束 CPU QoS（`taskpolicy` / `nice`）与并发上限。
- **开盖凉爽时零打扰**：开盖且温控正常时钩子完全静默；仅在进入限流档位后才提示 Agent 包裹重任务。
- **只读 AppleSMC 风扇遥测**：零写入读取实时风扇转速与 SMC 控制模式（`auto`、`manual` 或 `unknown`）；明确区分专职控扇软件（`Macs Fan Control`、`TG Pro`、`smcFanControl`）与监测软件（`Fanny`、`Stats`、`iStat Menus`），模式不可读时如实报告未知而绝不臆测控制方。
- **双语实时面板与安全亮度调节**：在 Antigravity 侧边栏（`EN / 中文`）查看遥测、切换守护或档位；调节内建亮度前会在锁内核验真实硬件开盖与唤醒状态，合盖时仅保存恢复目标而不误点亮黑屏。
- **可核验的一键锁屏**：通过 `SACLockScreenImmediate`（回退 `CGSession` / `pmset`）锁屏且不休眠后台任务，统一轮询 `CGSSessionScreenIsLocked` 并明确区分「已核验（`verified`）」与「已提交（`submitted`）」（`pmset` 回退未核验锁屏则报错）。

| 档位 | 触发条件 | 实际效果 |
| :--- | :--- | :--- |
| **Normal / 正常** | 开盖且温度正常 | 全速运行，不限流 |
| **Utility / 控温** | 合盖插电 | Utility QoS（`nice 10`），按能效核限流（`2–4` 并发） |
| **Eco / 节能** | 合盖电池供电 | Background QoS（`nice 15`），最多 `2` 并发（`<=20%` 降为 `1`） |
| **Strict / 强限** | 严重/临界热压力 | 最低优先级（`taskpolicy -b`、`nice 19`），单任务串行 |

## 快速开始

支持 macOS（Apple Silicon 与 Intel）。一键下载并校验发行版二进制，同时安装 Antigravity 插件：

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
```

也可直接在终端使用：

```sh
mac-clamshell-mcp status
mac-clamshell-mcp run -- cargo test
mac-clamshell-mcp guard install --interval 10   # 可选：24/7 常驻 LaunchAgent
```

## 给 AI Agent 使用

将通用 Skill 与 AI 指南接入 Claude Code、Codex、Cursor、OpenCode 或 Antigravity（完整的 CLI 参数、MCP 工具定义与限流档位均集中在此）：

```sh
npx skills add eatingDream-antigravity-plugins/mac-clamshell-mcp
```

> 可直接对你的 Agent 说：*「请阅读 [`llms.txt`](llms.txt) 和 [`docs/ai-guide.md`](docs/ai-guide.md)，帮我配置 `mac-clamshell-mcp` stdio MCP，并在限流时自动包裹重型构建命令。」*

## 安全与隐私边界

- **运行时零主动联网**：二进制不发起任何网络请求，无统计上报。
- **本地 MCP stdio 返回字段透明**：服务通过本地标准输入输出向已连接的 MCP 宿主返回设备遥测，包含主机名、芯片与 macOS 版本、负载/内存/风扇/显示状态，以及缩写后的本地配置路径与可执行文件路径（`~/...`）。
- **零 root、零 SMC 写入**：全程无需 `sudo`，不写 SMC/NVRAM/内核扩展；本地配置与守护状态使用仅当前用户可读写的权限（`0700`/`0600`）。
- **可逆的用户态亮度控制**：仅调用 macOS `DisplayServices` 用户态接口，硬件未确认开盖且内屏唤醒时绝不写入背光。

## 文档索引

- [AI Agent 指南与完整参考（`docs/ai-guide.md`）](docs/ai-guide.md) — MCP 工具参数、CLI 选项、限流档位表与 LaunchAgent 运维说明
- [通用 Agent Skill（`skills/mac-clamshell/SKILL.md`）](skills/mac-clamshell/SKILL.md)
- [LLM 索引（`llms.txt`）](llms.txt) · [贡献与开发指南（`AGENTS.md`）](AGENTS.md)
- [参与贡献](CONTRIBUTING.md) · [更新日志](CHANGELOG.md) · [安全说明](SECURITY.md)

## Antigravity 插件套件

隶属于 [eatingDream Antigravity 插件套件](https://eatingdream.dev)：[ContextRadar](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp)（`context-radar-mcp`）· **ClamGuard**（`mac-clamshell-mcp`）· [RemoteCast](https://github.com/eatingDream-antigravity-plugins/chrome-stream-mcp)（`chrome-stream-mcp`）· [ColabDeck](https://github.com/eatingDream-antigravity-plugins/colab-mcp)（`colab-mcp`），由 [PanelKit](https://github.com/eatingDream-antigravity-plugins/agy-mcp-kit)（`agy-mcp-kit`）统一驱动。

## 许可证

[MIT](LICENSE)
