# AGENTS.md

Guidance for AI coding agents working in `mac-clamshell-mcp`. For runtime throttling and tool usage rules, see [skills/mac-clamshell/SKILL.md](skills/mac-clamshell/SKILL.md) (and [plugin/rules/AGENTS.md](plugin/rules/AGENTS.md) for the minimal `<400 B` Antigravity plugin notice rule). Humans: see [README.md](README.md).

## Architecture

- `src/platform/` — macOS IOKit/CoreDisplay/DisplayServices FFI (`macos.rs`, `display_ffi.rs`), lid/power/thermal probing, brightness & screen-lock primitives.
- `src/policy.rs` — Pure profile decision matrix (`normal`, `utility`, `eco`, `strict`) and concurrency cap calculation.
- `src/runner.rs` — `mac-clamshell-mcp run` process wrapper (`taskpolicy`, `nice`, standard job-limit env vars).
- `src/guard.rs` & `src/launchagent.rs` — Closed-lid display guard (sleeps display on lid close when undocked, preserves external monitors when docked, restores brightness on open) and optional 24/7 LaunchAgent management.
- `src/mcp/` — MCP stdio JSON-RPC server (`server.rs`), telemetry (`telemetry.rs`), 3 public tools + App-only controls.
- `src/hook.rs` — Silent (`{}`) fail-open Antigravity lifecycle hooks emitting `[mac-clamshell]` notices only when throttled.
- `panel/` — Dark-first SEP-1865 panel (`pack.mjs` -> `panel/dist/panel.html`, kept `<45 KiB`).
- `skills/mac-clamshell/SKILL.md` — Single-source portable Agent Skill (copied into plugin bundles by `scripts/install-plugin.sh` and `.github/workflows/release.yml`).

## Build, Test & Security Gates (run before every PR)

```sh
node panel/pack.mjs && node --test panel/test/panel.test.mjs
cargo test --locked
./scripts/check_sanitized.sh
```

Release build (macOS, remap local paths):
```sh
RUSTFLAGS="--remap-path-prefix=$HOME=/build" cargo build --release --locked
```

## Invariants

- Keep `plugin/rules/AGENTS.md` plain markdown `<= 400 B` and hooks silent (`{}`) under `profile=normal`.
- Keep `skills/mac-clamshell/SKILL.md` as the single canonical skill source.
- Never light the built-in display while the lid is closed when updating brightness; never commit personal paths, tokens, or emails (`./scripts/check_sanitized.sh` must exit 0).
