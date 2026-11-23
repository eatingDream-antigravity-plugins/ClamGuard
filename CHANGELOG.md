# Changelog

All notable changes to this project will be documented in this file.

## [1.0.0](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/releases/tag/v1.0.0) (2026-11-23)

### Features

* Native Rust single-binary macOS clamshell controller (`mac-clamshell-mcp`) with LTO and symbol stripping
* Direct `dlopen`/`dlsym` FFI bindings for `CoreGraphics` and `DisplayServices`, plus `sysctlbyname`, `getloadavg`, and Mach `host_statistics64` telemetry
* 4-tier adaptive performance policy (`normal`, `utility`, `eco`, `strict`) via `mac-clamshell-mcp run` (`taskpolicy`, `nice`, and 10 concurrency env caps)
* Display thermal guard (`mac-clamshell-mcp watch` + LaunchAgent installer) that dims built-in backlight to `0%` and sleeps the panel while closed, synchronizing wake-restore targets under `O_NOFOLLOW` `flock`
* Built-in brightness management (`brightness get|set` & `clamshell_set_brightness`) safe across open, closed, and asleep display transitions
* Zero-flicker dark-first SEP-1865 MCP Apps dashboard (`ui://mac-clamshell-mcp/panel`) with startup pre-warm, SWR cache (`<0.5 ms`), SHA-256 ETag deduplication, RTT-adaptive polling, and transparent-background host-CSS hardening
* Low-intrusion Antigravity plugin bundle (`rules/AGENTS.md` `<400 B`, on-demand `skills/mac-clamshell/SKILL.md`, and silent `{}` fail-open lifecycle hooks)
* Automated GitHub Actions CI, Release Please workflow, universal macOS (`aarch64` + `x86_64`) release packaging, and one-line `scripts/install.sh` installer
