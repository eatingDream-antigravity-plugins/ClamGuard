---
name: mac-clamshell
description: >-
  macOS clamshell performance throttling, lid thermal guard, and built-in
  screen brightness control. Use when an active `[mac-clamshell]` throttled
  notice (`utility`, `eco`, or `strict`) is present, or when the user asks to
  inspect lid/power/heat status, wrap heavy builds with `mac-clamshell-mcp run`,
  or adjust built-in display brightness.
---

# Clam Guard — Throttling & Display Control

Binary: `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp` (written `mac-clamshell-mcp` below; invoke the full path if not on `$PATH`).

## Activation

- **Normal (`profile=normal` or no `[mac-clamshell]` notice)**: Operate normally without throttling. Before unusually heavy jobs (full compiles, ML training, video encodes), check `mac-clamshell-mcp status` if lid state is uncertain.
- **Throttled (`utility`, `eco`, or `strict`)**: Apply the rules below until a later notice reports `profile=normal`.

## Rules While Throttled

- **Wrap heavy commands**: `mac-clamshell-mcp run -- <cmd> [args...]` (single command) or `mac-clamshell-mcp run --sh "<cmd>"` (pipes, `&&`, redirects). It re-checks live lid/power/thermal state and sets `taskpolicy`, `nice`, and standard build/thread concurrency env vars automatically (including `maxJobsOverride` in `normal` mode). Also pass matching tool flags (`--maxWorkers`, `-p`, `-jobs`, `--workspace-concurrency`) when applicable.
- **Skip wrapping** quick lightweight commands (`ls`, `git status`, `git diff`, `cat`, small `rg`) and **never alter** exact pre-authorized commands in scheduled automations.
- **Profile tiers**:
  - `utility` (Cool — lid closed + AC): `taskpolicy -c utility`, `nice 10`, `max_jobs` = E-cores clamped `2..4`; at most 1 heavy build/test at a time.
  - `eco` (Eco — lid closed + Battery): `taskpolicy -c background`, `nice 15`, `max_jobs <= 2` (`1` at `<= 20%` battery); run sequentially, prefer incremental builds/tests, avoid parallel fan-outs.
  - `strict` (Strict — thermal warning/throttle): `taskpolicy -b`, `nice 19`, `max_jobs=1`; do not start new heavy compute without alerting the user and letting the Mac cool down.
- **Tools & user overrides**: Use `clamshell_panel` for status/dashboard (including read-only AppleSMC fan RPM, `smcMode`, and detected fan-control apps vs read-only monitors — zero root, zero SMC writes), `clamshell_set_brightness` (or `brightness set <0..100>`, verifies real hardware lid open + awake before writing and safely updates wake-restore target while closed), and `clamshell_lock_screen` (or `lock`, **only when explicitly requested**) to lock without sleeping. If the user explicitly asks for full speed on a task, comply and briefly note that throttling was bypassed.

## Configuration (`settings.json`)

- **Path**: `~/.gemini/antigravity/plugin_data/mac-clamshell-mcp/settings.json` (`0600`, atomic writes with `flock`).
- **Hot-reload**: Automatically reloaded on file `mtime`/size/inode change on the next poll or tool call — no Antigravity restart needed. Invalid edits fail closed (last good settings stay active and `settingsError` is surfaced in the panel).
- **GUI & Agent keys**:
  - `lang`: `"auto"` | `"en"` | `"zh-CN"` (panel & CLI language)
  - `acClosedProfile`: `"utility"` | `"eco"` (default mode when plugged in & lid closed)
  - `thermalBrightCapPct`: `0` (off) or `1..100` (open-lid brightness cap when hot; GUI presets `0`/`30`/`50`/`70`)
  - `guardEnabled`: `true` | `false` (keep built-in screen off when closed)
  - `pollIntervalSec`: `0` (auto) or `1..300` (foreground refresh rate)
  - `bgPollIntervalSec`: `0` (auto), `-1` (paused), or `1..3600` (background refresh rate)
  - `modeOverride` (`auto`/`normal`/`utility`/`eco`/`strict`) & `maxJobsOverride` (`0..64`) — see `docs/ai-guide.md` for full reference.

## Cross-Client Setup

Any stdio MCP client (Antigravity, Claude Code, Codex, Cursor, OpenCode) can connect via `mac-clamshell-mcp serve --data-dir <dir> --panel-html <panel.html>`, or call `mac-clamshell-mcp status` / `run` directly from shell tools without MCP.
