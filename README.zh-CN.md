<div align="center">

<img src="assets/logo.svg" alt="Mac Clamshell MCP" width="80" height="80">

# Mac Clamshell MCP

**让合盖干活的 MacBook 保持凉快安静，同时确保合盖后内置屏幕彻底熄灭。**

[![CI](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![macOS](https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-1A1916?logo=apple&logoColor=white)

[English](README.md) | 简体中文

<img src="docs/social-preview.png" alt="Mac Clamshell MCP 面板预览" width="860">

</div>

## 它能帮你做什么

合盖跑编译、测试或 AI Agent 时，MacBook 往往发烫且内屏背光容易偷偷常亮。Mac Clamshell MCP 会实时监测开合盖、供电和热压力，让设备保持安静凉爽：

- **合盖彻底熄屏，开盖恢复亮度**：只要 Antigravity 运行，内置 MCP 服务（`serve`）就会自动运行轻量熄屏守护——合盖即把内置屏背光降到 0% 并休眠内屏，开盖自动恢复原亮度。
- **按状态自动限流降温**：结合开合盖、电池/插电状态以及 macOS 热压力（Apple Silicon 与 Intel 均支持），自动调整 CPU 优先级与并发上限。
- **开盖时完全不打扰**：开盖且温度正常时在 Antigravity 中保持静默；仅在进入限流档位后才提醒 Agent 包裹重任务并限制并发。
- **只读风扇遥测与第三方风扇软件零冲突**：通过无特权只读 IOKit `AppleSMC` 白名单读取实时风扇转速（零 root、零 SMC 写入），自动检测运行中的第三方风扇管理工具（`Macs Fan Control`、`TG Pro`、`Stats`、`iStat Menus`、`smcFanControl`、`Fanny`）并显示「只读监测」或「已让位」归属，绝不篡改用户自定义风扇曲线。
- **双语面板、亮度调节与一键锁屏**：在 Antigravity 侧边栏（`EN / 中文`）查看盒盖、电源、温控、风扇与守护状态，切换熄屏守护或手动档位，调节亮度，或一键锁屏（核验 macOS 会话锁状态）并保持后台任务运行。

| 档位 | 触发条件 | 实际效果 |
| :--- | :--- | :--- |
| **Normal / 正常** | 开盖且温度正常 | 不限制，全速运行 |
| **Utility / 控温** | 合盖 + 插电 | Utility QoS（`nice 10`），并发限制在能效核数量（`2–4`，Intel 为总核数一半） |
| **Eco / 节能** | 合盖 + 电池供电 | 后台 QoS（`nice 15`），最多 2 并发（电量低于等于 20% 时降为 1） |
| **Strict / 强限** | 系统达到严重/临界热压力 | 最低优先级（`taskpolicy -b`、`nice 19`），单任务串行运行 |

## 限流原理与隐私安全

- **进程级生效、零 root、只读 SMC**：全程无需 `sudo`，绝不写入 NVRAM、SMC、内核扩展或全局 `pmset` 设置。风扇遥测仅以只读方式读取安全 SMC 键值，检测到第三方风扇工具时自动让位。`mac-clamshell-mcp run` 仅对被包裹的命令树应用 macOS `taskpolicy` QoS、POSIX `nice` 与标准构建并发环境变量，不影响其他应用，退出后零残留。
- **可逆的用户态熄屏守护**：通过 macOS 标准 `DisplayServices` 用户态接口（与控制中心亮度滑块同级）调节背光，开盖或热压力恢复后自动还原原有亮度。
- **100% 本地运行、零隐私上传**：无任何联网请求或统计上报；配置与守护状态仅保存在本地用户目录且仅当前用户可读写。

## 安装

支持 macOS（Apple Silicon 与 Intel）。安装脚本会下载并校验 SHA-256，随后安装到 `~/.gemini/config/plugins/mac-clamshell-mcp`（完成后重启或重载 Antigravity）：

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
# 或指定版本：
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh -s -- --version v1.2.0
```

Antigravity 打开期间已自带合盖守护；如需退出 Antigravity 后仍 24/7 常驻守护，可执行下方终端命令中的 `mac-clamshell-mcp guard install` 安装 LaunchAgent。

## 怎么用

### 在 Antigravity 里用

从侧边栏打开 **Mac Clamshell** 面板，或直接让 Agent 操作（例如：*"看看开合盖、电量和温度状态"*、*"把内置屏幕亮度调到 30%"*、*"用合盖限流模式跑测试"*）：

| 工具 | 作用 |
| :--- | :--- |
| `clamshell_panel` | 打开实时控制面板，返回当前开合盖、供电、温度和档位信息。 |
| `clamshell_set_brightness` | 调节内置屏幕亮度（0–100%）；合盖时仅更新开盖恢复亮度，不会误点亮黑屏。 |
| `clamshell_lock_screen` | 立即锁屏（原生 `SACLockScreenImmediate`，自动回退 `CGSession`/`pmset`），保持 Mac 唤醒不中断后台任务。 |

### 在终端里单独用

```sh
mac-clamshell-mcp status                                         # 查看开合盖、供电、温度、亮度与档位
mac-clamshell-mcp run -- cargo test                              # 按当前档位自动包裹重型命令
mac-clamshell-mcp run --mode eco --jobs 2 --sh "npm run build && npm test"
mac-clamshell-mcp brightness get                                 # 读取或设置内置屏幕亮度（0-100%）
mac-clamshell-mcp brightness set 60
mac-clamshell-mcp lock                                           # 立即锁屏且保持 Mac 唤醒
# 可选：安装/管理 24/7 LaunchAgent 守护（或运行 ./scripts/install-launchagent.sh）/ 前台守护：
mac-clamshell-mcp guard install --interval 10
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall
mac-clamshell-mcp watch --daemon --interval 10
```

## AI Agent 接入指南

- **Antigravity**：执行上方一键安装脚本即自动注册 MCP 服务、生命周期钩子、实时面板与 [`skills/mac-clamshell/SKILL.md`](skills/mac-clamshell/SKILL.md)。
- **Claude Code / Codex / Cursor / OpenCode（任意 MCP 或终端 Agent）**：加载通用 Skill [`skills/mac-clamshell/SKILL.md`](skills/mac-clamshell/SKILL.md)（或运行 `npx skills add eatingDream-antigravity-plugins/mac-clamshell-mcp`），按下方配置 stdio MCP，或在终端直接调用 `mac-clamshell-mcp status` / `run`：

```json
{
  "mcpServers": {
    "mac-clamshell-mcp": {
      "command": "/path/to/mac-clamshell-mcp",
      "args": ["serve", "--data-dir", "/path/to/mac-clamshell-data", "--panel-html", "/path/to/panel.html"]
    }
  }
}
```

- **LLM 索引与协作指南**：[`llms.txt`](llms.txt) · [`AGENTS.md`](AGENTS.md)

## 从源码构建

需要 macOS、Rust（`cargo`）和 Node.js 20+：

```sh
git clone https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp.git
cd mac-clamshell-mcp
(cd panel && npm install) && node panel/pack.mjs && node --test panel/test/panel.test.mjs
cargo test --locked && cargo build --release --locked && ./scripts/install-plugin.sh
```

## 参与贡献与许可证

欢迎提 PR 或反馈问题，详情见 [CONTRIBUTING.md](CONTRIBUTING.md)、[CHANGELOG.md](CHANGELOG.md) 和 [SECURITY.md](SECURITY.md)。本项目基于 [MIT License](LICENSE) 开源。
