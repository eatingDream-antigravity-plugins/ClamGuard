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

## Why this exists

If you leave a MacBook running builds, tests, or AI agents with the lid closed (for example while docked or working remotely), two annoying things happen: heavy jobs heat up the closed laptop and spin up the fans, and background apps sometimes keep the built-in screen lit right under the closed lid.

Mac Clamshell MCP watches your Mac's lid, power, and temperature. It makes sure the closed screen actually turns off, steers heavy tasks onto lower-heat settings automatically, and puts a clean status and brightness panel inside Antigravity.

## What it does

- **Turns off the closed built-in screen**: While Antigravity is open, the MCP server (`serve`) runs a lightweight, lock-coordinated display guard thread automatically—dropping backlight to 0% and sleeping the internal display when the lid shuts, then restoring your previous brightness on wake. Optionally install the 24/7 LaunchAgent (`mac-clamshell-mcp guard install`) to protect closed-lid displays even when Antigravity is closed.
- **Throttles heavy builds automatically**: Adapts CPU priority and parallel job limits based on lid state, battery vs. AC power, and macOS thermal pressure (`com.apple.system.thermalpressurelevel` on Apple Silicon plus `pmset` telemetry).
- **Stays out of the way when open**: When your lid is open and temps are fine, it stays completely silent in Antigravity. As soon as your Mac enters a throttled mode, it nudges the agent to wrap heavy commands and cap concurrency.
- **Live bilingual panel, brightness & one-click lock screen**: Check lid, power, thermal pressure, and guard status in Antigravity (`EN / 中文`), toggle the display guard or override the mode temporarily, adjust built-in display brightness, or lock the screen immediately while keeping background builds awake.

### Power & thermal modes

| Mode | When it activates | What it does |
| :--- | :--- | :--- |
| **Normal** | Lid open and cool | Full speed, no throttling |
| **Utility** | Lid closed on AC power | Utility CPU QoS (`nice 10`), limits jobs to E-cores (`2–4`, or half of cores on Intel) |
| **Eco** | Lid closed on battery | Background QoS (`nice 15`), max 2 jobs (1 job at 20% battery or below) |
| **Strict** | Serious or critical thermal pressure | Lowest priority (`taskpolicy -b`, `nice 19`), 1 job at a time |

## How Throttling & `max_jobs` Work (Safety & Scope)

Mac Clamshell MCP never alters system-wide hardware or power settings:

1. **Zero root & zero global modifications**: Requires no `sudo` or root privileges, never writes to NVRAM, SMC, kernel extensions, or global `pmset` preferences. All other apps on your Mac (browser, IDE, video/music, Finder) remain 100% unaffected.
2. **Process-scoped OS primitives**: `mac-clamshell-mcp run` wraps only the target build or test command via `exec()` using standard macOS `/usr/sbin/taskpolicy` (Utility/Background QoS, steering threads onto E-cores on Apple Silicon), POSIX `nice`, and standard build-system concurrency environment variables (`CARGO_BUILD_JOBS`, `MAKEFLAGS=-jN`, `CMAKE_BUILD_PARALLEL_LEVEL`, `NINJAJOBS`, `RAYON_NUM_THREADS`, `OMP_NUM_THREADS`, `GOMAXPROCS`, `PYTEST_XDIST_AUTO_NUM_WORKERS`, etc.). Once the command exits, zero background scheduler residue remains.
3. **Reversible user-space display guard**: Uses standard macOS `DisplayServices` user-space APIs (the same layer as the Control Center brightness slider) coordinated through an advisory `flock` and a local `0600` state file, so your previous brightness is safely restored when the lid opens or thermal pressure clears.

## Install

### macOS (Apple Silicon & Intel)

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
```

This downloads the macOS release binary, verifies its SHA-256 checksum, and installs the plugin into `~/.gemini/config/plugins/mac-clamshell-mcp`. Restart or reload Antigravity afterward.

To pin a specific release:

```sh
curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh -s -- --version v1.0.0
```

### Optional: 24/7 closed-lid screen guard (even when Antigravity is closed)

Antigravity already runs the display guard automatically while open. To keep closed-lid protection running 24/7 via a macOS LaunchAgent:

```sh
mac-clamshell-mcp guard install --interval 10
# Check or remove anytime:
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall
```

(Or run `./scripts/install-launchagent.sh --interval 10` directly from the installed plugin directory.)

## How to use it

### In Antigravity

Open **Mac Clamshell** in the Antigravity side pane to check status, switch modes, or drag the brightness slider.

You can also ask your agent directly:

- *"Check my Mac's lid, battery, and thermal status."*
- *"Dim the built-in display to 30%."*
- *"Run the test suite using clamshell throttling."*

| Tool | What it does |
| :--- | :--- |
| `clamshell_panel` | Opens the live dashboard and reports current lid, power, thermal, and mode status. |
| `clamshell_set_brightness` | Sets built-in screen brightness (0 to 100%) immediately when open, or updates the wake-restore level while closed without lighting the screen. |
| `clamshell_lock_screen` | Locks the macOS screen immediately (`SACLockScreenImmediate` with `CGSession`/`pmset` fallback) while keeping the Mac awake for background tasks. |

### From the terminal

You can also use the `mac-clamshell-mcp` CLI on its own:

```sh
# Check current lid, power, thermal, brightness, and active mode
mac-clamshell-mcp status

# Run a heavy build or test command with automatic throttling
mac-clamshell-mcp run -- cargo test
mac-clamshell-mcp run --mode eco --jobs 2 --sh "npm run build && npm test"

# Read or set built-in screen brightness (0-100%)
mac-clamshell-mcp brightness get
mac-clamshell-mcp brightness set 60

# Lock the screen immediately without sleeping the Mac
mac-clamshell-mcp lock

# Install, inspect, or remove the 24/7 LaunchAgent display guard
mac-clamshell-mcp guard install --interval 10
mac-clamshell-mcp guard status
mac-clamshell-mcp guard uninstall

# Or run the closed-lid display guard in the foreground
mac-clamshell-mcp watch --daemon --interval 10
```

## Privacy

Everything runs 100% locally on your Mac with zero network requests and no telemetry. Settings and display-guard state are stored only in your local user directory with owner-only permissions.

## Build from source

Requires macOS, Rust (`cargo`), and Node.js 20+ (to bundle and test the panel).

```sh
git clone https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp.git
cd mac-clamshell-mcp
(cd panel && npm install) && node panel/pack.mjs && node --test panel/test/panel.test.mjs
cargo test --locked
cargo build --release --locked
./scripts/install-plugin.sh
```

## Contributing & License

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), and [SECURITY.md](SECURITY.md).

Licensed under the [MIT License](LICENSE).
