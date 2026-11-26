# Changelog

All notable changes to this project will be documented in this file.

## [1.2.3](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/compare/v1.2.2...v1.2.3) (2026-11-26)


### Bug Fixes

* **arch:** decouple display guard from brightness and MCP server loops ([#14](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/14)) ([d23da71](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/d23da7123a8400a185444fbcf1348f3f9f2c78e2))

## [1.2.2](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/compare/v1.2.1...v1.2.2) (2026-11-25)


### Bug Fixes

* **guard:** reliably restore prior brightness on lid open ([#12](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/12)) ([33619c4](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/33619c41f060d99bbce5dcaef41ab688da5bf228))

## [1.2.1](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/compare/v1.2.0...v1.2.1) (2026-11-24)


### Code Refactoring

* deduplicate README/skill docs and tidy helper tests ([#10](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/10)) ([f97bdb7](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/f97bdb76fb4bfb0b2d28bb66fd832972a00b0870))

## [1.2.0](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/compare/v1.1.0...v1.2.0) (2026-11-24)


### Features

* add one-click lock screen and background/reveal panel refresh ([#9](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/9)) ([8ad6dd6](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/8ad6dd6a3ab23c682c7074862ccbc64d30b20c20))


### Bug Fixes

* **docs:** serve release badge from first-party assets/release-badge.svg ([#8](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/8)) ([8f6ea84](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/8f6ea849569486154bb98184f40745cabd9eb23c))
* **docs:** use shields static/v1 query URL so release-please preserves badge color ([#7](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/7)) ([c363811](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/c3638110d59c233693c8cf5feec5ec098cd3d0f8))


### Documentation

* replace dynamic release badge with auto-bumped static badge ([#5](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/5)) ([ee21fd0](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/ee21fd04e672c021c5edd5a8bdb49380ad9ac467))

## [1.1.0](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/compare/v1.0.0...v1.1.0) (2026-11-24)


### Features

* add active display guard, thermal pressure FFI, bilingual UI cleanup, and safety docs ([#4](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/4)) ([50910a1](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/50910a143e474521b570e591b0e4d456cc8ad52f))


### Documentation

* **readme:** rewrite bilingual READMEs for human readability and usage focus ([#1](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/issues/1)) ([9718097](https://github.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/commit/9718097ad2609a3a03d20355ab7275e7e858069d))

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
