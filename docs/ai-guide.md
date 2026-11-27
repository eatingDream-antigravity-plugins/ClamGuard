# Clam Guard (mac-clamshell-mcp) — AI Agent Guide & Operational Reference

Complete operational reference for AI coding agents and advanced users configuring **Clam Guard** (`mac-clamshell-mcp`). Homepage: <https://clamguard.eatingdream.dev> · Human overview: [README.md](../README.md) | [README.zh-CN.md](../README.zh-CN.md).

## Throttling Profile Matrix

| Profile | Activation Condition | CPU QoS & Nice | Concurrency Cap (`max_jobs`) |
| :--- | :--- | :--- | :--- |
| `normal` | Lid open & nominal/fair thermals | None | Uncapped (all logical cores) |
| `utility` | Lid closed + AC power | `taskpolicy -c utility`, `nice 10` | E-cores clamped `2..4` (Intel: `max(2, total / 2)`, capped at `total`) |
| `eco` | Lid closed + battery power | `taskpolicy -c background`, `nice 15` | `<= 2` (`1` when battery `<= 20%`) |
| `strict` | Serious or critical thermal pressure | `taskpolicy -b`, `nice 19` | `1` (serial execution) |

## MCP Server Configuration (stdio)

Supported MCP protocol versions: `2025-11-25`, `2025-06-18`, `2025-03-26` (with JSON-RPC 2.0 batch support), and `2024-11-05`.
SEP-1865 MCP Apps UI requires client `capabilities.extensions["io.modelcontextprotocol/ui"].mimeTypes` to explicitly include `"text/html;profile=mcp-app"`.

```json
{
  "mcpServers": {
    "mac-clamshell-mcp": {
      "command": "~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp",
      "args": [
        "serve",
        "--data-dir", "~/.gemini/antigravity/plugin_data/mac-clamshell-mcp",
        "--panel-html", "~/.gemini/config/plugins/mac-clamshell-mcp/panel/dist/panel.html"
      ]
    }
  }
}
```

### MCP Tools

- **Public tools (all MCP sessions)**:
  - `clamshell_panel` (`{ fresh?: boolean }`): Opens the live dashboard and returns current lid, power, thermal, fan, display, guard, and throttling telemetry.
  - `clamshell_set_brightness` (`{ percent: 0..100 }`): Sets built-in display brightness only when hardware confirms lid open and built-in display awake; otherwise safely updates the wake-restore target without lighting the screen.
  - `clamshell_lock_screen` (`{}`): Locks the macOS screen immediately without sleeping background tasks (`SACLockScreenImmediate` -> `CGSession -suspend` -> `pmset displaysleepnow`). Polls `CGSSessionScreenIsLocked` and returns `{ ok, method, verified, status: "verified" | "submitted", locked_at_ms, serverMs }`; `pmset` fallback fails unless session lock is verified.
- **App-only tools (enforced in both `tools/list` and `tools/call` — only available after SEP-1865 UI negotiation)**:
  - `clamshell_refresh` (`{ ifHash?: string (maxLength: 64), force?: boolean, fresh?: boolean }`)
  - `clamshell_configure` (`{ modeOverride?: "auto" | "eco" | "utility" | "strict" | "normal", maxJobsOverride?: integer (0..64), acClosedProfile?: "utility" | "eco", guardEnabled?: boolean, thermalBrightCapPct?: integer (0..100), lang?: "auto" | "en" | "zh-CN" }`)

### Telemetry Accuracy Notes

- **Display state**: `display` reports `lidClosed`, `internalOffline`, `confirmedAsleep`, `asleep`, `builtinCount`, `externalCount`, `percent`, and `savedPercent` separately. When docked with external displays, the guard preserves external displays without running `displaysleepnow`, relies on macOS clamshell handling, and never fabricates `asleep: true` when an internal panel remains online and awake.
- **Fan telemetry (strictly read-only)**: Reports `readOnly: true`, `smcMode` (`"auto" | "manual" | "unknown" | "none"`), `externalManagers` (dedicated fan-control apps: `Macs Fan Control`, `TG Pro`, `smcFanControl`), and `detectedMonitors` (read-only monitors: `Fanny`, `Stats`, `iStat Menus`). When SMC mode cannot be read, `smcMode` is `"unknown"` and `controller` is `"Unknown"` rather than guessing macOS auto or naming a monitor app.
- **Daemon ownership**: `serve` coordinates with `watch --daemon` and LaunchAgent via `DaemonLock` + `launchctl print`, automatically yielding when LaunchAgent loads and taking over if an earlier lock holder exits.

## CLI Reference

```sh
mac-clamshell-mcp status [--json] [--data-dir DIR]
mac-clamshell-mcp run [--mode auto|normal|utility|eco|strict] [--jobs N] [--quiet|-q] [--data-dir DIR] (--sh "<cmd>" | [--] <cmd> [args...])
mac-clamshell-mcp brightness [get | set <0..100>] [--json] [--lid-state open|closed] [--data-dir DIR]
mac-clamshell-mcp lock [--json] [--data-dir DIR]
mac-clamshell-mcp guard <install | uninstall | status> [--interval SECS] [--bin PATH] [--json] [--data-dir DIR]
mac-clamshell-mcp watch [--daemon] [--interval SECS] [--data-dir DIR]
mac-clamshell-mcp hook <session-start | pre-invocation> [--data-dir DIR]
mac-clamshell-mcp serve [--data-dir DIR] [--panel-html PATH]
```

Note on `--lid-state`: `--lid-state closed` forces a safe deferred write (`saved_target`), whereas `--lid-state open` still verifies real hardware lid state inside the guard lock before any hardware brightness write.

## Build From Source

Requires macOS, Rust (`cargo`), and Node.js 22.18+:

```sh
git clone https://github.com/eatingDream-antigravity-plugins/clam-guard.git
cd clam-guard
(cd panel && npm ci --ignore-scripts) && npm --prefix panel run typecheck && npm --prefix panel run build && npm --prefix panel test
cargo test --locked && ./scripts/install-plugin.sh
```
