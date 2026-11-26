<div align="center">

<img src="assets/logo.svg" alt="Mac Clamshell MCP" width="80" height="80">

# Mac Clamshell MCP

**Keep your closed MacBook cool, quiet, and predictable when running builds or AI agents.**

English | [简体中文](README.zh-CN.md) · [Releases](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases) · [AI Guide](docs/ai-guide.md) · [`llms.txt`](llms.txt)

[![CI](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/actions/workflows/ci.yml)
[![Release](assets/release-badge.svg)](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases)
![Built with Rust](https://img.shields.io/badge/built%20with-Rust-1A1916?logo=rust&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-Apple%20Silicon%20%7C%20Intel-1A1916?logo=apple&logoColor=white)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

<img src="docs/social-preview.png" alt="Mac Clamshell MCP panel preview" width="860">

</div>

## What It Is

A native Rust macOS MCP server, SEP-1865 live dashboard, and CLI wrapper that watches lid, power, and thermal pressure to throttle heavy commands and guard closed-lid display state.

It is **not** a fan-curve writer, kernel extension, or global `pmset` modifier—it never requires `sudo` and leaves other apps and fan managers untouched.

## Features

- **Closed-lid display guard (external-display safe)**: Without external monitors, puts the display to sleep (`pmset displaysleepnow`) on lid close and restores your saved brightness on open. When docked with external monitors, preserves external displays without triggering `displaysleepnow`, relies on macOS clamshell handling for the internal panel, and reports `lidClosed`, `internalOffline`, and `confirmedAsleep` separately while keeping wake-restore targets synced.
- **Automatic build throttling**: Adapts CPU QoS (`taskpolicy`, `nice`) and parallel job caps (`normal`, `utility`, `eco`, `strict`) from live lid, battery/AC, and macOS thermal pressure (Apple Silicon & Intel).
- **Silent when open and cool**: Emits zero hook noise while open under nominal thermals; only nudges the agent to wrap heavy builds when throttled.
- **Read-only AppleSMC fan telemetry**: Reads live fan RPM and SMC control mode (`auto`, `manual`, or `unknown`) with zero SMC writes. Separately detects dedicated fan-control apps (`Macs Fan Control`, `TG Pro`, `smcFanControl`) and read-only monitors (`Fanny`, `Stats`, `iStat Menus`) without guessing a controller when SMC mode is unknown.
- **Live bilingual panel & brightness control**: Inspect telemetry in Antigravity (`EN / 中文`), toggle guard or mode override, and adjust built-in brightness (or safely queue the wake-restore target while closed).
- **Screen lock with session verification**: Locks without sleeping background tasks (`SACLockScreenImmediate`, with `CGSession` and `pmset` fallbacks), polls macOS `CGSSessionScreenIsLocked`, and explicitly reports `verified` vs `submitted` (`pmset` fails unless session lock is verified).

| Profile | Trigger | Effect |
| :--- | :--- | :--- |
| **Normal** | Lid open & cool | Full speed, no throttling |
| **Utility** | Lid closed on AC | Utility QoS (`nice 10`), E-core job cap (`2–4`) |
| **Eco** | Lid closed on battery | Background QoS (`nice 15`), max `2` jobs (`1` at `<=20%`) |
| **Strict** | Serious / critical thermal | Lowest priority (`taskpolicy -b`, `nice 19`), `1` job |

## Quick Start

Requires macOS (Apple Silicon or Intel). Downloads the verified release binary and installs the Antigravity plugin:

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
```

Or run directly from your terminal:

```sh
mac-clamshell-mcp status
mac-clamshell-mcp run -- cargo test
mac-clamshell-mcp guard install --interval 10   # Optional 24/7 LaunchAgent
```

## For AI Agents

Point Claude Code, Codex, Cursor, OpenCode, or Antigravity to the portable skill and AI guide—all CLI flags, MCP schemas, and throttling profiles live there:

```sh
npx skills add eatingDream-antigravity-plugins/mac-clamshell-mcp
```

> Tell your agent: *"Read [`llms.txt`](llms.txt) and [`docs/ai-guide.md`](docs/ai-guide.md) to configure `mac-clamshell-mcp` stdio MCP and wrap heavy builds automatically when throttled."*

## Safety & Privacy

- **Zero outbound network traffic**: The binary makes no network calls and collects no analytics.
- **Local stdio telemetry scope**: Over local MCP stdio, tool responses share host status with your connected MCP client—including hostname, chip/macOS version, CPU/memory/fan/display telemetry, and abbreviated local paths (`~/...`) for settings and binary location.
- **Zero root & zero SMC writes**: Never needs `sudo`, never writes SMC/NVRAM/kexts, and stores local state under owner-only (`0700`/`0600`) permissions.
- **Reversible user-space brightness**: Uses macOS `DisplayServices` user-space APIs and refuses to write brightness unless hardware confirms the lid is open and the internal panel is awake.

## Documentation

- [AI Agent Guide & Reference (`docs/ai-guide.md`)](docs/ai-guide.md) — MCP tools, CLI flags, throttle tiers, and LaunchAgent workflows
- [Portable Agent Skill (`skills/mac-clamshell/SKILL.md`)](skills/mac-clamshell/SKILL.md)
- [LLM Index (`llms.txt`)](llms.txt) · [Contributor Guide (`AGENTS.md`)](AGENTS.md)
- [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md)

## License

[MIT](LICENSE)
