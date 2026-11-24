#!/bin/sh
# Install the compiled Mac Clamshell MCP binary, panel bundle, and Antigravity plugin manifests.
# Usage: ./scripts/install-plugin.sh [--dest ~/.gemini/config/plugins/mac-clamshell-mcp]

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
DEST="${MAC_CLAMSHELL_PLUGIN_DIR:-${ANTIGRAVITY_PLUGINS_DIR:-${HOME}/.gemini/config/plugins}/mac-clamshell-mcp}"

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
    -h|--help)
      printf 'Usage: %s [--dest DIR]\n' "$0"
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

if [ ! -x "$BIN_SRC" ]; then
  printf 'install-plugin: release binary missing at %s (run cargo build --release first)\n' "$BIN_SRC" >&2
  exit 1
fi
if [ ! -f "$PANEL_SRC" ]; then
  printf 'install-plugin: panel bundle missing at %s (run npm run build inside panel/ first)\n' "$PANEL_SRC" >&2
  exit 1
fi

mkdir -p "${DEST}/bin" "${DEST}/scripts" "${DEST}/assets" "${DEST}/rules" "${DEST}/skills/mac-clamshell" "${DEST}/panel/dist"

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
cp "${REPO_ROOT}/plugin/skills/mac-clamshell/SKILL.md" "${DEST}/skills/mac-clamshell/SKILL.md"
cp "${REPO_ROOT}/assets/logo.svg" "${DEST}/assets/logo.svg"
cp "${REPO_ROOT}/LICENSE" "${DEST}/LICENSE"
cp "${REPO_ROOT}/README.md" "${DEST}/README.md"
cp "${REPO_ROOT}/README.zh-CN.md" "${DEST}/README.zh-CN.md"
cp "$PANEL_SRC" "${DEST}/panel/dist/panel.html"

rm -f "${DEST}"/*.bak "${DEST}/bin/"*.new 2>/dev/null || true

printf 'Installed Mac Clamshell MCP plugin cleanly to %s\n' "$DEST"
