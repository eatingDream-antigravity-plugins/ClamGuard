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

The `mac-clamshell-mcp` plugin ships a native Rust binary (`~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp`) that inspects macOS lid state (`AppleClamshellState`), power source (`AC` vs `Battery`), thermal limits (`pmset -g therm`), and built-in display brightness (`DisplayServices`).

## Activation

- **Normal (`profile=normal`, or no active `[mac-clamshell]` throttle notice)**: Operate normally without artificial throttling. Before starting an unusually heavy job (full native compile, ML training, video encode), you may check `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp status` if unsure of the current lid state.
- **Throttled (`profile=utility`, `profile=eco`, or `profile=strict`)**: Follow the controls below until a subsequent notice reports `profile=normal`.

## Performance-Control Rules (While Throttled)

1. **Wrap heavy shell commands with `mac-clamshell-mcp run`**
   - Single command: `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp run -- <cmd> [args...]`
   - Compound command (`&&`, `|`, redirects): `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp run --sh "<cmd>"`
   - `mac-clamshell-mcp run` re-checks live lid/power/thermal state at launch and applies `taskpolicy` (`-c utility` / `-c background` / `-b`), `nice`, and concurrency env vars (`MAKEFLAGS`, `CARGO_BUILD_JOBS`, `CMAKE_BUILD_PARALLEL_LEVEL`, `OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS`, `MKL_NUM_THREADS`, `VECLIB_MAXIMUM_THREADS`, `RAYON_NUM_THREADS`, `UV_THREADPOOL_SIZE`, `GOMAXPROCS`).
   - Also pass tool-specific concurrency flags matching `max_jobs` when applicable (e.g., `jest --maxWorkers=<N>`, `vitest --maxWorkers=<N>`, `go test -p <N>`, `xcodebuild -jobs <N>`, `pnpm --workspace-concurrency=<N>`).
   - **Skip wrapping** for quick lightweight commands (`ls`, `git status`, `git diff`, `cat`, `rg` on small scopes) and **never alter** exact pre-authorized commands required by scheduled automations.

2. **Profile Tiers**
   - **`utility` (lid closed + AC power)**: `taskpolicy -c utility`, `nice 10`, cap concurrency to `max_jobs` (E-core count clamped `2..4`); run at most 1 heavy build/test command at a time.
   - **`eco` (lid closed + Battery power)**: Strict E-core execution (`taskpolicy -c background`, `nice 15`, `max_jobs <= 2`, or `1` when battery `<= 20%`). Execute sequentially, prefer targeted/incremental builds & tests, and avoid parallel heavy fan-outs.
   - **`strict` (thermal warning / throttle detected)**: `taskpolicy -b`, `nice 19`, `max_jobs=1`. Do not start new heavy compute tasks without alerting the user and letting the machine cool down first.

3. **Display Brightness & User Override**
   - Use the `clamshell_set_brightness` MCP tool or `~/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp brightness set <0..100>` to adjust built-in display brightness; while the lid is closed, Mac Clamshell MCP safely updates the wake-restore target without lighting the closed panel.
   - Use the `clamshell_panel` MCP tool to open the live dashboard or inspect hardware/policy status.
   - If the user explicitly requests full performance for a specific task, follow their instruction and briefly note that throttling was bypassed for that run.
