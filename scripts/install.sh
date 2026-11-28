#!/bin/sh
# Install the Clam Guard (mac-clamshell-mcp) Antigravity plugin from GitHub Releases (macOS).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/clam-guard/main/scripts/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/clam-guard/main/scripts/install.sh | sh -s -- --uninstall
#   ./scripts/install.sh [--version v1.0.0] [--dest /path/to/plugin-dir] [--reload] [--no-reload] [--uninstall]

set -eu

REPO="eatingDream-antigravity-plugins/clam-guard"
VERSION="${MAC_CLAMSHELL_VERSION:-}"
DEST="${ANTIGRAVITY_PLUGINS_DIR:-${HOME}/.gemini/config/plugins}/mac-clamshell-mcp"
UNINSTALL=0
DO_RELOAD=0
NO_RELOAD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version|--tag)
      if [ $# -lt 2 ]; then
        printf 'install.sh: %s requires a version tag (e.g. v1.0.0)\n' "$1" >&2
        exit 2
      fi
      VERSION="$2"
      shift 2
      ;;
    --dest)
      if [ $# -lt 2 ]; then
        printf 'install.sh: --dest requires a directory path\n' >&2
        exit 2
      fi
      DEST="$2"
      shift 2
      ;;
    --reload)
      DO_RELOAD=1
      shift
      ;;
    --no-reload)
      NO_RELOAD=1
      shift
      ;;
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    -h|--help)
      printf 'Usage: %s [--version vX.Y.Z] [--dest DIR] [--reload] [--no-reload] [--uninstall]\n' "$0"
      printf 'Env:   MAC_CLAMSHELL_VERSION, ANTIGRAVITY_PLUGINS_DIR\n'
      exit 0
      ;;
    *)
      printf 'install.sh: unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

if [ "$UNINSTALL" -eq 1 ]; then
  case "$DEST" in
    ""|"/"|"$HOME"|"$HOME/")
      printf 'install.sh: refusing to remove unsafe path: %s\n' "$DEST" >&2
      exit 1
      ;;
  esac
  if [ -x "${DEST}/bin/mac-clamshell-mcp" ]; then
    "${DEST}/bin/mac-clamshell-mcp" guard uninstall >/dev/null 2>&1 || true
  fi
  if [ -d "$DEST" ]; then
    rm -rf "$DEST"
    printf 'Uninstalled Clam Guard (mac-clamshell-mcp) from %s\n' "$DEST"
  else
    printf 'Clam Guard (mac-clamshell-mcp) is not installed at %s\n' "$DEST"
  fi
  exit 0
fi

OS="$(uname -s)"
if [ "$OS" != "Darwin" ]; then
  printf 'install.sh: mac-clamshell-mcp supports macOS (Darwin) only; detected %s\n' "$OS" >&2
  exit 1
fi

ARCH="$(uname -m)"
case "$ARCH" in
  arm64|aarch64)
    TARGET="aarch64-apple-darwin"
    ;;
  x86_64|amd64)
    TARGET="x86_64-apple-darwin"
    ;;
  *)
    printf 'install.sh: unsupported macOS architecture: %s\n' "$ARCH" >&2
    exit 1
    ;;
esac

if ! command -v curl >/dev/null 2>&1; then
  printf 'install.sh: curl is required\n' >&2
  exit 1
fi

resolve_latest_tag() {
  effective_url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
  case "$effective_url" in
    */releases/tag/*)
      printf '%s\n' "${effective_url##*/releases/tag/}"
      return 0
      ;;
  esac
  api_json="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)"
  tag="$(printf '%s\n' "$api_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  if [ -n "$tag" ]; then
    printf '%s\n' "$tag"
    return 0
  fi
  return 1
}

if [ -z "$VERSION" ]; then
  if ! VERSION="$(resolve_latest_tag)"; then
    printf 'install.sh: failed to resolve latest release tag for %s (pass --version vX.Y.Z)\n' "$REPO" >&2
    exit 1
  fi
fi

case "$VERSION" in
  v*) ;;
  *) VERSION="v${VERSION}" ;;
esac

case "$VERSION" in
  ''|*[!0-9A-Za-z.-]*)
    printf 'install.sh: invalid version tag: %s\n' "$VERSION" >&2
    exit 1
    ;;
esac
if ! printf '%s\n' "$VERSION" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$'; then
  printf 'install.sh: invalid version tag format: %s (expected vX.Y.Z)\n' "$VERSION" >&2
  exit 1
fi

ARCHIVE="mac-clamshell-mcp-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

TMPDIR_WORK="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_WORK"' EXIT HUP INT TERM

printf 'Downloading %s (%s)...\n' "$ARCHIVE" "$VERSION"
curl -fsSL -o "${TMPDIR_WORK}/${ARCHIVE}" "${BASE_URL}/${ARCHIVE}"
curl -fsSL -o "${TMPDIR_WORK}/${ARCHIVE}.sha256" "${BASE_URL}/${ARCHIVE}.sha256"

EXPECTED_SHA="$(awk '{print $1}' "${TMPDIR_WORK}/${ARCHIVE}.sha256" | head -n 1 | tr '[:upper:]' '[:lower:]')"
case "$EXPECTED_SHA" in
  ''|*[!0-9a-f]*)
    printf 'install.sh: invalid SHA-256 checksum format\n' >&2
    exit 1
    ;;
esac
if ! printf '%s\n' "$EXPECTED_SHA" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$'; then
  printf 'install.sh: invalid SHA-256 checksum length\n' >&2
  exit 1
fi

if command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA="$(shasum -a 256 "${TMPDIR_WORK}/${ARCHIVE}" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA="$(sha256sum "${TMPDIR_WORK}/${ARCHIVE}" | awk '{print $1}')"
else
  printf 'install.sh: neither shasum nor sha256sum found\n' >&2
  exit 1
fi

if [ "$EXPECTED_SHA" != "$ACTUAL_SHA" ]; then
  printf 'install.sh: SHA-256 mismatch!\n  expected: %s\n  actual:   %s\n' "$EXPECTED_SHA" "$ACTUAL_SHA" >&2
  exit 1
fi

STAGE="${TMPDIR_WORK}/extract"
mkdir -p "$STAGE"
tar -xzf "${TMPDIR_WORK}/${ARCHIVE}" -C "$STAGE"

if [ -n "$(find "$STAGE" -type l 2>/dev/null)" ]; then
  printf 'install.sh: refusing archive containing symlinks\n' >&2
  exit 1
fi

if [ ! -f "${STAGE}/bin/mac-clamshell-mcp" ] || [ -L "${STAGE}/bin/mac-clamshell-mcp" ] || [ ! -f "${STAGE}/plugin.json" ] || [ -L "${STAGE}/plugin.json" ]; then
  printf 'install.sh: archive is missing required plugin files\n' >&2
  exit 1
fi

copy_staged_file() {
  src="$1"
  dst="$2"
  if [ -L "$src" ] || [ ! -f "$src" ]; then
    printf 'install.sh: refusing unsafe staged file: %s\n' "$src" >&2
    exit 1
  fi
  if [ -L "$dst" ]; then
    printf 'install.sh: refusing symlink destination: %s\n' "$dst" >&2
    exit 1
  fi
  cp "$src" "$dst"
}

mkdir -p "${DEST}/bin" "${DEST}/scripts" "${DEST}/panel/dist" "${DEST}/rules" "${DEST}/skills/mac-clamshell" "${DEST}/assets" "${DEST}/docs"

# Atomic binary swap so a running MCP server never sees a truncated executable
copy_staged_file "${STAGE}/bin/mac-clamshell-mcp" "${DEST}/bin/mac-clamshell-mcp.new"
chmod 0755 "${DEST}/bin/mac-clamshell-mcp.new"
mv -f "${DEST}/bin/mac-clamshell-mcp.new" "${DEST}/bin/mac-clamshell-mcp"

if [ -f "${STAGE}/scripts/install-launchagent.sh" ]; then
  copy_staged_file "${STAGE}/scripts/install-launchagent.sh" "${DEST}/scripts/install-launchagent.sh"
  chmod 0755 "${DEST}/scripts/install-launchagent.sh"
fi

copy_staged_file "${STAGE}/panel/dist/panel.html" "${DEST}/panel/dist/panel.html"
copy_staged_file "${STAGE}/plugin.json" "${DEST}/plugin.json"
copy_staged_file "${STAGE}/mcp_config.json" "${DEST}/mcp_config.json"
copy_staged_file "${STAGE}/hooks.json" "${DEST}/hooks.json"
copy_staged_file "${STAGE}/rules/AGENTS.md" "${DEST}/rules/AGENTS.md"
copy_staged_file "${STAGE}/skills/mac-clamshell/SKILL.md" "${DEST}/skills/mac-clamshell/SKILL.md"
copy_staged_file "${STAGE}/assets/logo.svg" "${DEST}/assets/logo.svg"
[ -f "${STAGE}/docs/ai-guide.md" ] && copy_staged_file "${STAGE}/docs/ai-guide.md" "${DEST}/docs/ai-guide.md"
[ -f "${STAGE}/llms.txt" ] && copy_staged_file "${STAGE}/llms.txt" "${DEST}/llms.txt"
[ -f "${STAGE}/LICENSE" ] && copy_staged_file "${STAGE}/LICENSE" "${DEST}/LICENSE"
[ -f "${STAGE}/README.md" ] && copy_staged_file "${STAGE}/README.md" "${DEST}/README.md"
[ -f "${STAGE}/README.zh-CN.md" ] && copy_staged_file "${STAGE}/README.zh-CN.md" "${DEST}/README.zh-CN.md"

reload_antigravity_mcp() {
  if [ "$NO_RELOAD" -eq 1 ]; then
    return 0
  fi
  if [ -n "${ANTIGRAVITY_CSRF_TOKEN:-}" ]; then
    ls_addr="${ANTIGRAVITY_LS_ADDRESS:-localhost:5387}"
    clean_addr="$(printf '%s' "$ls_addr" | LC_ALL=C tr -d '[:cntrl:]')"
    if [ "$clean_addr" != "$ls_addr" ] || \
      ! printf '%s\n' "$ls_addr" | LC_ALL=C grep -Eq '^(localhost|127\.0\.0\.1|\[::1\]):[0-9]{1,5}$'; then
      printf 'Note: ignoring non-loopback ANTIGRAVITY_LS_ADDRESS; restart Clam Guard in Customizations > Installed.\n'
      return 0
    fi
    clean_token="$(printf '%s' "$ANTIGRAVITY_CSRF_TOKEN" | LC_ALL=C tr -d '\000-\037\177\"\\')"
    if [ "$clean_token" != "$ANTIGRAVITY_CSRF_TOKEN" ]; then
      printf 'Note: ignoring malformed ANTIGRAVITY_CSRF_TOKEN; restart Clam Guard in Customizations > Installed.\n'
      return 0
    fi
    if printf 'header = "Content-Type: application/json"\nheader = "x-codeium-csrf-token: %s"\n' \
        "$ANTIGRAVITY_CSRF_TOKEN" | \
      curl -sf --connect-timeout 2 --max-time 5 -K - \
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

printf 'Installed Clam Guard %s (%s) to %s\n' "$VERSION" "$TARGET" "$DEST"
reload_antigravity_mcp
printf 'Verify: %s/bin/mac-clamshell-mcp status\n' "$DEST"
printf 'Optional 24/7 standalone LaunchAgent: %s/bin/mac-clamshell-mcp guard install\n' "$DEST"
