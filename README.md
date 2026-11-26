<div align="center">

<img src="assets/logo.svg" alt="Mac Clamshell MCP" width="80" height="80">

# Mac Clamshell MCP

**Keep your MacBook cool, quiet, and its built-in screen truly off when running builds or AI agents with the lid closed.**

[![CI](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![macOS](https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-1A1916?logo=apple&logoColor=white)

English | [简体中文](README.zh-CN.md)

<img src="docs/social-preview.png" alt="Mac Clamshell MCP panel preview" width="860">

</div>

## What it does

Closed MacBooks running docked or remote builds often overheat and leave the internal backlight lit. Mac Clamshell MCP watches lid, power, and thermal state to keep your Mac quiet and cool:

- **Turns off the closed built-in screen**: While Antigravity is open, the MCP server (`serve`) runs a lightweight display guard automatically—dropping backlight to 0% and sleeping the internal display on lid close, then restoring your previous brightness on wake.
- **Throttles heavy builds automatically**: Adapts CPU priority and parallel job limits from lid state, AC vs. battery power, and macOS thermal pressure (Apple Silicon & Intel).
- **Stays out of the way when open**: Silent in Antigravity while open and cool; only nudges the agent to wrap heavy commands and cap concurrency when throttled.
- **Live bilingual panel, brightness & one-click lock**: Inspect status in Antigravity (`EN / 中文`), toggle the display guard or mode override, adjust brightness, or lock the screen without sleeping background jobs.

| Mode | When it activates | What it does |
| :--- | :--- | :--- |
| **Normal** | Lid open and cool | Full speed, no throttling |
| **Utility** | Lid closed on AC power | Utility CPU QoS (`nice 10`), limits jobs to E-cores (`2–4`, or half of cores on Intel) |
| **Eco** | Lid closed on battery | Background QoS (`nice 15`), max 2 jobs (1 job at 20% battery or below) |
| **Strict** | Serious or critical thermal pressure | Lowest priority (`taskpolicy -b`, `nice 19`), 1 job at a time |

## How Throttling Works & Privacy

- **Process-scoped, zero root**: Needs no `sudo` and never touches NVRAM, SMC, kernel extensions, or global `pmset` settings. `mac-clamshell-mcp run` applies macOS `taskpolicy` QoS, POSIX `nice`, and standard build-concurrency variables only to the wrapped command tree, leaving other apps untouched with zero residue on exit.
- **Reversible user-space display guard**: Uses standard macOS `DisplayServices` user-space APIs (the same layer as Control Center) so your prior brightness restores cleanly when the lid opens or thermal pressure clears.
- **100% local & private**: Zero network requests and no telemetry; settings and guard state stay in your local user directory with owner-only permissions.

## Install

Requires macOS (Apple Silicon or Intel). Downloads the verified release binary and installs the plugin into `~/.gemini/config/plugins/mac-clamshell-mcp` (restart or reload Antigravity afterward):

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
# Or pin a specific release:
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh -s -- --version v1.2.0
```

Antigravity already runs the display guard while open; to keep closed-lid protection running 24/7 even when Antigravity is closed, install the LaunchAgent via `mac-clamshell-mcp guard install` below.

## How to use it

### In Antigravity

Open **Mac Clamshell** in the side pane, or ask your agent (e.g., *"Check my Mac's lid, battery, and thermal status"*, *"Dim the built-in display to 30%"*, or *"Run the test suite using clamshell throttling"*):

| Tool | What it does |
| :--- | :--- |
| `clamshell_panel` | Opens the live dashboard and reports current lid, power, thermal, and mode status. |
| `clamshell_set_brightness` | Sets built-in screen brightness (0–100%) immediately when open, or updates the wake-restore level while closed without lighting the screen. |
| `clamshell_lock_screen` | Locks the macOS screen immediately (`SACLockScreenImmediate` with `CGSession`/`pmset` fallback) while keeping the Mac awake for background tasks. |

### From the terminal

```sh
mac-clamshell-mcp status                                         # Lid, power, thermal, brightness & active mode
mac-clamshell-mcp run -- cargo test                              # Run a command with automatic throttling
mac-clamshell-mcp run --mode eco --jobs 2 --sh "npm run build && npm test"
mac-clamshell-mcp brightness get                                 # Read or set built-in brightness (0-100%)
mac-clamshell-mcp brightness set 60
mac-clamshell-mcp lock                                           # Lock screen immediately without sleeping
# Optional 24/7 LaunchAgent guard (or run ./scripts/install-launchagent.sh) / foreground daemon:
mac-clamshell-mcp guard install --interval 10
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall
mac-clamshell-mcp watch --daemon --interval 10
```

## AI Agent Integration

- **Antigravity**: One-line installer above installs the MCP server, hooks, live panel, and `skills/mac-clamshell/SKILL.md` automatically.
- **Claude Code / Codex / Cursor / OpenCode (any MCP client or shell agent)**: Add the portable skill [`skills/mac-clamshell/SKILL.md`](skills/mac-clamshell/SKILL.md) (or `npx skills add eatingDream-antigravity-plugins/mac-clamshell-mcp`) and configure stdio MCP (or invoke `mac-clamshell-mcp status` / `run` directly):

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

- **LLM index & coding-agent guide**: [`llms.txt`](llms.txt) · [`AGENTS.md`](AGENTS.md)

## Build from source

Requires macOS, Rust (`cargo`), and Node.js 20+:

```sh
git clone https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp.git
cd mac-clamshell-mcp
(cd panel && npm install) && node panel/pack.mjs && node --test panel/test/panel.test.mjs
cargo test --locked && cargo build --release --locked && ./scripts/install-plugin.sh
```

## Contributing & License

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), and [SECURITY.md](SECURITY.md). Licensed under the [MIT License](LICENSE).
