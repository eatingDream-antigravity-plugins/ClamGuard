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

## 为什么做这个插件

把 MacBook 合上盖子跑编译、跑测试或者让 AI Agent 后台干活（比如远程控制或外接使用时），经常会遇到两件烦心事：满载任务把合盖的机身烤得发烫、风扇狂转，而且有些软件会阻止屏幕休眠，导致盖子合上了内置屏幕背光还偷偷亮着。

Mac Clamshell MCP 会监测 Mac 的开合盖、供电和发热状态：合盖时确保内屏真正熄灭，自动把重任务降到更温和的负载档位，并在 Antigravity 里提供一个直观的状态和亮度控制面板。

## 它能帮你做什么

- **合盖彻底熄屏，开盖恢复亮度**：只要 Antigravity 运行，内置 MCP 服务（`serve`）就会自动启动带 `flock` 互斥协调的轻量熄屏守护线程——合盖即把内置屏背光降到 0% 并休眠内屏，开盖自动恢复原亮度。如需退出 Antigravity 后仍 24 小时守护，可执行 `mac-clamshell-mcp guard install`。
- **按状态自动限流降温**：结合开合盖、电池/插电状态以及系统真实热压力（Apple Silicon 读取 `com.apple.system.thermalpressurelevel`，Intel 结合 `pmset` 遥测），自动调整任务调度优先级与并发上限。
- **开盖时完全不打扰**：平时开着盖子且温度正常时，插件在 Antigravity 里保持静默；进入合盖或发热限流档位后，才会提醒 Agent 自动限制编译并发。
- **双语面板实时调控与一键锁屏**：在侧边栏随时查看状态、一键切换 `EN / 中文`、暂停或开启熄屏守护、手动切换性能档位、调节内建屏幕亮度，或一键锁屏并保持后台任务继续运行。

### 四档自动策略

| 档位 | 触发条件 | 实际效果 |
| :--- | :--- | :--- |
| **Normal / 正常** | 开盖且温度正常 | 不限制，全速运行 |
| **Utility / 控温** | 合盖 + 插电 | Utility QoS（`nice 10`），并发限制在能效核数量（`2–4`，Intel 为总核数一半） |
| **Eco / 节能** | 合盖 + 电池供电 | 后台 QoS（`nice 15`），最多 2 并发（电量低于等于 20% 时降为 1） |
| **Strict / 强限** | 系统达到严重/临界热压力 | 最低优先级（`taskpolicy -b`、`nice 19`），单任务串行运行 |

## 性能约束与护眼守护的实现原理

Mac Clamshell MCP 只做进程级与用户态调控，不会改动任何系统底层或全局配置：

1. **零 root 权限、零全局侵入**：全程无需 `sudo` 或管理员权限，绝不修改 NVRAM、SMC、内核扩展或全局 `pmset` 电源设置。你 Mac 上的其他应用（浏览器、IDE、音视频、访达等）100% 不受任何影响。
2. **进程级原生调度原语**：`mac-clamshell-mcp run` 通过 `exec()` 仅对被包裹的构建/测试子进程树生效，调用 macOS 原生 `/usr/sbin/taskpolicy`（Utility/Background QoS，在 Apple Silicon 上优先调度至能效核）+ POSIX `nice` + 标准构建并发环境变量（`CARGO_BUILD_JOBS`、`MAKEFLAGS=-jN`、`CMAKE_BUILD_PARALLEL_LEVEL`、`NINJAJOBS`、`RAYON_NUM_THREADS`、`OMP_NUM_THREADS`、`GOMAXPROCS`、`PYTEST_XDIST_AUTO_NUM_WORKERS` 等）。命令退出后不留任何后台调度残留。
3. **可逆的用户态护眼熄屏守护**：通过 macOS 标准 `DisplayServices` 用户态接口（与控制中心亮度滑块同一层级）调节背光，并配合本地 `0600` 权限状态文件与 `flock` 文件锁防冲突；开盖或热压力恢复后自动还原原有亮度。

## 安装

### macOS（Apple Silicon 与 Intel 均支持）

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
```

脚本会下载对应芯片架构的预编译版本，校验 SHA-256 后安装到 `~/.gemini/config/plugins/mac-clamshell-mcp`。装完后重启或重载 Antigravity 即可。

如需指定版本：

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh -s -- --version v1.0.0
```

### 可选：退出 Antigravity 后仍 24/7 常驻守护

Antigravity 打开期间已自带合盖守护；如希望完全不依赖 Antigravity 也保持常驻保护，可安装 macOS LaunchAgent：

```sh
mac-clamshell-mcp guard install --interval 10
# 随时查看状态或卸载：
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall
```

（也可以直接在插件目录运行 `./scripts/install-launchagent.sh --interval 10`。）

## 怎么用

### 在 Antigravity 里用

从侧边栏打开 **Mac Clamshell** 面板，可以查看实时状态、手动覆盖档位或拖动亮度条。

也可以直接让 Agent 帮你操作：

- *"看看我这台 Mac 现在的开合盖、电量和温度状态。"*
- *"把内置屏幕亮度调到 30%。"*
- *"用合盖限流模式跑一下测试。"*

| 工具 | 作用 |
| :--- | :--- |
| `clamshell_panel` | 打开控制面板，返回当前开合盖、供电、温度和档位信息。 |
| `clamshell_set_brightness` | 调节内置屏幕亮度（0% 到 100%）。合盖状态下只会记住开盖后要恢复的亮度，不会误点亮黑屏。 |
| `clamshell_lock_screen` | 立即锁屏（原生 `SACLockScreenImmediate`，自动回退 `CGSession`/`pmset`），保持 Mac 唤醒不中断后台构建或 Agent。 |

### 在终端里单独用

`mac-clamshell-mcp` 也可以直接当命令行工具使用：

```sh
# 查看当前开合盖、供电、温度、亮度和档位
mac-clamshell-mcp status

# 自动按当前档位包裹重型编译或测试命令
mac-clamshell-mcp run -- cargo test
mac-clamshell-mcp run --mode eco --jobs 2 --sh "npm run build && npm test"

# 读取或设置内置屏幕亮度（0-100%）
mac-clamshell-mcp brightness get
mac-clamshell-mcp brightness set 60

# 立即锁屏且保持 Mac 唤醒
mac-clamshell-mcp lock

# 安装、查看或卸载 24/7 常驻合盖熄屏 LaunchAgent
mac-clamshell-mcp guard install --interval 10
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall

# 或在前台持续运行合盖熄屏保护
mac-clamshell-mcp watch --daemon --interval 10
```

## 隐私与安全

完全在你的 Mac 本地运行，没有任何联网请求和统计上报。配置与状态文件只保存在本地用户目录中，仅当前用户可读写。

## 从源码构建

需要 macOS、Rust（`cargo`）和 Node.js 20+（用于打包和测试前端面板）。

```sh
git clone https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp.git
cd mac-clamshell-mcp
(cd panel && npm install) && node panel/pack.mjs && node --test panel/test/panel.test.mjs
cargo test --locked
cargo build --release --locked
./scripts/install-plugin.sh
```

## 参与贡献与许可证

欢迎提 PR 或反馈问题，详情见 [CONTRIBUTING.md](CONTRIBUTING.md)、[CHANGELOG.md](CHANGELOG.md) 和 [SECURITY.md](SECURITY.md)。

本项目基于 [MIT License](LICENSE) 开源。
