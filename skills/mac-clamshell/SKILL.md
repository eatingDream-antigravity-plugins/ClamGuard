---
name: mac-clamshell
description: >-
  macOS clamshell performance throttling, display thermal guard, and built-in
  display brightness control. Use when an active `[mac-clamshell]` throttled
  notice (`utility`, `eco`, or `strict`) is present, or when the user asks to
  inspect lid/power/thermal status, wrap heavy builds with `mac-clamshell-mcp run`,
  or adjust built-in display brightness.
---

# Mac Clamshell MCP — Throttling & Display Control

Binary: `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp` (written `mac-clamshell-mcp` below; invoke the full path if not on `$PATH`).

## Activation

- **Normal (`profile=normal` or no `[mac-clamshell]` notice)**: Operate normally without throttling. Before unusually heavy jobs (full compiles, ML training, video encodes), check `mac-clamshell-mcp status` if lid state is uncertain.
- **Throttled (`utility`, `eco`, or `strict`)**: Apply the rules below until a later notice reports `profile=normal`.

## Rules While Throttled

- **Wrap heavy commands**: `mac-clamshell-mcp run -- <cmd> [args...]` (single command) or `mac-clamshell-mcp run --sh "<cmd>"` (pipes, `&&`, redirects). It re-checks live lid/power/thermal state and sets `taskpolicy`, `nice`, and standard build/thread concurrency env vars automatically. Also pass matching tool flags (`--maxWorkers`, `-p`, `-jobs`, `--workspace-concurrency`) when applicable.
- **Skip wrapping** quick lightweight commands (`ls`, `git status`, `git diff`, `cat`, small `rg`) and **never alter** exact pre-authorized commands in scheduled automations.
- **Profile tiers**:
  - `utility` (lid closed + AC): `taskpolicy -c utility`, `nice 10`, `max_jobs` = E-cores clamped `2..4`; at most 1 heavy build/test at a time.
  - `eco` (lid closed + Battery): `taskpolicy -c background`, `nice 15`, `max_jobs <= 2` (`1` at `<= 20%` battery); run sequentially, prefer incremental builds/tests, avoid parallel fan-outs.
  - `strict` (thermal warning/throttle): `taskpolicy -b`, `nice 19`, `max_jobs=1`; do not start new heavy compute without alerting the user and letting the Mac cool down.
- **Tools & user overrides**: Use `clamshell_panel` for status/dashboard, `clamshell_set_brightness` (or `brightness set <0..100>`, safely updates wake-restore target while closed), and `clamshell_lock_screen` (or `lock`, **only when explicitly requested**) to lock without sleeping. If the user explicitly asks for full speed on a task, comply and briefly note that throttling was bypassed.

## Cross-Client Setup

Any stdio MCP client (Antigravity, Claude Code, Codex, Cursor, OpenCode) can connect via `mac-clamshell-mcp serve --data-dir <dir> --panel-html <panel.html>`, or call `mac-clamshell-mcp status` / `run` directly from shell tools without MCP.
