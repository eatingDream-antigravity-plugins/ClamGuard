#!/bin/sh
# Build (unless --no-build) and install the Clam Guard (mac-clamshell-mcp) binary, panel bundle, and Antigravity plugin manifests.
# Usage: ./scripts/install-plugin.sh [--dest ~/.gemini/config/plugins/mac-clamshell-mcp] [--no-build] [--reload] [--no-reload]

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
DEST="${MAC_CLAMSHELL_PLUGIN_DIR:-${ANTIGRAVITY_PLUGINS_DIR:-${HOME}/.gemini/config/plugins}/mac-clamshell-mcp}"
DO_BUILD=1
DO_RELOAD=0
NO_RELOAD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dest)
      if [ $# -lt 2 ]; then
        printf 'install-plugin: --dest requires a directory argument\n' >&2
        exit 2
      fi
      DEST="$2"
      shift 2
      ;;
    --no-build)
      DO_BUILD=0
      shift
      ;;
    --reload)
      DO_RELOAD=1
      shift
      ;;
    --no-reload)
      NO_RELOAD=1
      shift
      ;;
    -h|--help)
      printf 'Usage: %s [--dest DIR] [--no-build] [--reload] [--no-reload]\n' "$0"
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

BIN_SRC="${REPO_ROOT}/target/release/mac-clamshell-mcp"
PANEL_SRC="${REPO_ROOT}/panel/dist/panel.html"

if [ "$DO_BUILD" -eq 1 ]; then
  CARGO_DIR="${CARGO_HOME:-${HOME}/.cargo}"
  REMAP_FLAGS="--remap-path-prefix=${HOME}=/build --remap-path-prefix=${CARGO_DIR}=/cargo --remap-path-prefix=${REPO_ROOT}=/src"
  for reg_dir in "${CARGO_DIR}"/registry/src/*; do
    if [ -d "$reg_dir" ]; then
      REMAP_FLAGS="${REMAP_FLAGS} --remap-path-prefix=${reg_dir}=/cargo/registry/src"
    fi
  done
  RUSTFLAGS="${REMAP_FLAGS}" cargo build --release --locked --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

if [ ! -x "$BIN_SRC" ]; then
  printf 'install-plugin: release binary missing at %s (run cargo build --release first)\n' "$BIN_SRC" >&2
  exit 1
fi
if [ ! -f "$PANEL_SRC" ]; then
  printf 'install-plugin: panel bundle missing at %s (run node panel/pack.ts first)\n' "$PANEL_SRC" >&2
  exit 1
fi

if strings "$BIN_SRC" | grep -E '(/Users/|/home/)' >/dev/null 2>&1 \
  || strings "$BIN_SRC" | grep -F -e "${REPO_ROOT}" -e "${HOME}" >/dev/null 2>&1; then
  printf 'install-plugin: local path leaked into release binary %s (rebuild with --remap-path-prefix)\n' "$BIN_SRC" >&2
  exit 1
fi

mkdir -p "${DEST}/bin" "${DEST}/scripts" "${DEST}/assets" "${DEST}/docs" "${DEST}/rules" "${DEST}/skills/mac-clamshell" "${DEST}/panel/dist"

# Atomic binary swap so a running MCP server never sees a truncated executable
cp "$BIN_SRC" "${DEST}/bin/mac-clamshell-mcp.new"
chmod 0755 "${DEST}/bin/mac-clamshell-mcp.new"
mv -f "${DEST}/bin/mac-clamshell-mcp.new" "${DEST}/bin/mac-clamshell-mcp"

cp "${REPO_ROOT}/scripts/install-launchagent.sh" "${DEST}/scripts/install-launchagent.sh"
chmod 0755 "${DEST}/scripts/install-launchagent.sh"
cp "${REPO_ROOT}/plugin/plugin.json" "${DEST}/plugin.json"
cp "${REPO_ROOT}/plugin/mcp_config.json" "${DEST}/mcp_config.json"
cp "${REPO_ROOT}/plugin/hooks.json" "${DEST}/hooks.json"
cp "${REPO_ROOT}/plugin/rules/AGENTS.md" "${DEST}/rules/AGENTS.md"
cp "${REPO_ROOT}/skills/mac-clamshell/SKILL.md" "${DEST}/skills/mac-clamshell/SKILL.md"
cp "${REPO_ROOT}/assets/logo.svg" "${DEST}/assets/logo.svg"
cp "${REPO_ROOT}/docs/ai-guide.md" "${DEST}/docs/ai-guide.md"
cp "${REPO_ROOT}/llms.txt" "${DEST}/llms.txt"
cp "${REPO_ROOT}/LICENSE" "${DEST}/LICENSE"
cp "${REPO_ROOT}/README.md" "${DEST}/README.md"
cp "${REPO_ROOT}/README.zh-CN.md" "${DEST}/README.zh-CN.md"
cp "$PANEL_SRC" "${DEST}/panel/dist/panel.html"

rm -f "${DEST}"/*.bak "${DEST}/bin/"*.new 2>/dev/null || true

reload_antigravity_mcp() {
  if [ "$NO_RELOAD" -eq 1 ]; then
    return 0
  fi
  if [ -n "${ANTIGRAVITY_CSRF_TOKEN:-}" ]; then
    ls_addr="${ANTIGRAVITY_LS_ADDRESS:-localhost:5387}"
    if curl -sf -H "Content-Type: application/json" \
      -H "x-codeium-csrf-token: ${ANTIGRAVITY_CSRF_TOKEN}" \
      -d '{"serverName":"mac-clamshell-mcp_mac-clamshell-mcp"}' \
      "http://${ls_addr}/exa.language_server_pb.LanguageServerService/RefreshMcpServers" >/dev/null 2>&1; then
      printf 'Refreshed live Antigravity MCP server (close & reopen any open Clam Guard panel tab).\n'
    else
      printf 'Note: could not reach Antigravity RefreshMcpServers; restart Clam Guard in Customizations > Installed.\n'
    fi
  elif [ "$DO_RELOAD" -eq 1 ]; then
    if command -v agy >/dev/null 2>&1; then
      agy plugin disable mac-clamshell-mcp >/dev/null 2>&1 || true
      agy plugin enable mac-clamshell-mcp >/dev/null 2>&1 || true
      printf 'Reloaded Antigravity plugin mac-clamshell-mcp via agy CLI.\n'
    else
      printf 'Note: agy CLI not found on PATH; reload in Antigravity Customizations > Installed > Clam Guard > MCP > Restart.\n'
    fi
  else
    printf 'Next: run "agy plugin disable mac-clamshell-mcp && agy plugin enable mac-clamshell-mcp" or restart in Antigravity Customizations > Installed.\n'
  fi
}

printf 'Installed Clam Guard plugin cleanly to %s\n' "$DEST"
reload_antigravity_mcp
