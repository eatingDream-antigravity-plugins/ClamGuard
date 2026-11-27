<div align="center">

<img src="assets/logo.svg" alt="Clam Guard" width="76" height="76">

# Clam Guard (`mac-clamshell-mcp`)

**合盖跑 Agent，不让 Mac 发烫。**

Clam Guard 能在你合上 MacBook 机盖时保持系统唤醒且内屏熄灭，并随温度变化自动放缓重任务；它只读取风扇和温度状态，从不接管风扇控制。

[主页](https://clamguard.eatingdream.dev) · [English](README.md) | **简体中文** · [Releases](https://github.com/eatingDream-antigravity-plugins/clam-guard/releases) · [AI 指南](docs/ai-guide.md) · [`llms.txt`](llms.txt)

[![CI](https://github.com/eatingDream-antigravity-plugins/clam-guard/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/clam-guard/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/clam-guard/releases)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-12151E?logo=rust&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-12151E?logo=apple&logoColor=white)
[![License: MIT](https://img.shields.io/badge/license-MIT-12151E)](LICENSE)

<br>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/clamguard-dashboard-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/images/clamguard-dashboard-light.png">
  <img src="docs/images/clamguard-dashboard-dark.png" alt="Clam Guard 实时控制面板截图，展示开合盖状态、供电与温控、内屏亮度控制、限流档位及只读风扇信息" width="640">
</picture>

</div>

## 适合你吗？

- **适合**：经常合上 MacBook 让 AI Agent、编译或测试在后台持续运行，希望自动控温限流、保护合盖屏幕并随时调亮度或锁屏，且不想给任何工具 `sudo` 权限。
- **不需要**：需要手动改写风扇转速曲线、安装内核扩展或强改全局系统休眠策略——Clam Guard 绝不写风扇控制，也不需要管理员权限。

## 你能直接看到与做到什么

| | |
| :--- | :--- |
| **合盖自动控温限流** | 根据开合盖、插电/电池与 macOS 热压力自动调节 CPU 优先级与并发上限；开盖且凉爽时完全静默零打扰。 |
| **外屏友好的合盖守护** | 未接外屏合盖时自动休眠内屏并在开盖后恢复原亮度；连接外屏时完整保留外屏工作不误休眠。 |
| **安全调亮与一键锁屏** | 在面板或对话里调节内屏亮度（合盖时仅保存唤醒目标，绝不误点亮黑屏），并支持不中断后台任务的一键锁屏。 |
| **只读风扇状态透视** | 查看实时风扇转速与当前控制方（系统或 `Macs Fan Control`、`TG Pro`、`smcFanControl`），全程只读零写入。 |
| **原生 Rust、零 `sudo`** | 单个本地可执行文件，支持 Apple Silicon 与 Intel，零外网请求，配置采用仅当前用户可读写（`0600`）权限。 |

| 模式 | 触发条件 | 实际效果 |
| :--- | :--- | :--- |
| **正常** (`Normal`) | 开盖且温度正常 | 全速运行，不限制 |
| **控温** (`Cool`) | 合盖插电 | 降低优先级（`nice 10`），按能效核限流（`2–4` 个任务） |
| **节能** (`Eco`) | 合盖电池供电 | 后台优先级（`nice 15`），最多 `2` 个任务（电量 `<=20%` 降为 `1`） |
| **严格** (`Strict`) | 过热降频或严重过热 | 最低优先级（`taskpolicy -b`、`nice 19`），一次 `1` 个任务 |

## 快速开始

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/clam-guard/main/scripts/install.sh | sh
```

<sub>安装脚本自动校验发行版 SHA-256 · 在 Antigravity 内自动通过 `RefreshMcpServers` 热刷新 MCP 服务（外部终端可加 `--reload`，跳过可加 `--no-reload`）· 支持 macOS（Apple Silicon / Intel）· 可用 `MAC_CLAMSHELL_VERSION=vX.Y.Z` 指定版本 · 随时可用 `curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/clam-guard/main/scripts/install.sh | sh -s -- --uninstall` 干净卸载。</sub>

重载 Antigravity 后打开 **Clam Guard** 面板，或直接在终端运行：

```sh
mac-clamshell-mcp status
mac-clamshell-mcp run -- cargo test
mac-clamshell-mcp guard install --interval 10   # 可选：24/7 后台守护
```

## 给 AI Agent 使用

为 Antigravity、Claude Code、Codex、Cursor 或 OpenCode 安装通用 Skill：

```sh
npx skills add eatingDream-antigravity-plugins/clam-guard
```

> **给 Agent 的提示**：完整的 MCP 工具参数、CLI 选项、限流档位表与 LaunchAgent 运维说明见 [`llms.txt`](llms.txt)、[`skills/mac-clamshell/SKILL.md`](skills/mac-clamshell/SKILL.md) 与 [`docs/ai-guide.md`](docs/ai-guide.md)。

## 安全与隐私边界

- **零联网、零 `sudo`**：不发起任何网络请求，无统计上报，绝不写入 SMC、NVRAM 或内核扩展。
- **可逆的用户态控制**：仅在硬件确认开盖且内屏唤醒时写入亮度，合盖状态只记录恢复目标。
- **本地遥测字段透明**：仅通过本地 MCP stdio 向已连接的本机客户端返回开合盖、电量、温控、风扇转速、负载内存与 `~/...` 路径。

## 文档索引

- [AI Agent 指南与完整参考](docs/ai-guide.md) — MCP 工具参数、CLI 选项、限流档位、显示与风扇语义、源码构建
- [通用 Agent Skill（`SKILL.md`）](skills/mac-clamshell/SKILL.md) · [LLM 索引（`llms.txt`）](llms.txt)
- [开发指南](AGENTS.md) · [参与贡献](CONTRIBUTING.md) · [更新日志](CHANGELOG.md) · [安全说明](SECURITY.md)

## Antigravity 插件套件

隶属于 [Antigravity Plugins](https://github.com/eatingDream-antigravity-plugins) 套件：[Context Radar](https://github.com/eatingDream-antigravity-plugins/context-radar) · **Clam Guard** · [Remote Cast](https://github.com/eatingDream-antigravity-plugins/remote-cast) · [Colab Deck](https://github.com/eatingDream-antigravity-plugins/colab-deck) · [Panel Kit](https://github.com/eatingDream-antigravity-plugins/panel-kit)

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
