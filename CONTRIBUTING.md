# Contributing

Contributions to [Clam Guard (mac-clamshell-mcp)](https://github.com/eatingDream-antigravity-plugins/clam-guard) are welcome.

## Workflow

- Use short-lived branches from `main` (`feat/`, `fix/`, `docs/`, `refactor/`, `test/`, `chore/`, `ci/`) and submit Pull Requests with [Conventional Commits](https://www.conventionalcommits.org/) titles.
- Configure a GitHub noreply commit email (`<id>+<login>@users.noreply.github.com`); never commit secrets, tokens, private keys, personal hostnames/paths, or real credentials.
- Contributions are licensed under the [MIT License](LICENSE).

## Local checks before pushing

```bash
(cd panel && npm ci --ignore-scripts)
node panel/pack.mjs
node --test panel/test/panel.test.mjs
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
bash scripts/check_sanitized.sh
```

Release binaries should be built with `RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$PWD=/src" cargo build --release --locked` so local paths are not embedded.
