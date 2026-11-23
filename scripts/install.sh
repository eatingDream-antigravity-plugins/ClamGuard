#!/bin/sh
# Install the Mac Clamshell MCP Antigravity plugin from GitHub Releases (macOS).
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/mac-clamshell-mcp/main/scripts/install.sh | sh
#   ./scripts/install.sh [--version v1.0.0] [--dest /path/to/plugin-dir]

set -eu

REPO="eatingDream-antigravity-plugins/mac-clamshell-mcp"
VERSION="${MAC_CLAMSHELL_VERSION:-}"
DEST="${ANTIGRAVITY_PLUGINS_DIR:-${HOME}/.gemini/config/plugins}/mac-clamshell-mcp"

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
    -h|--help)
      printf 'Usage: %s [--version vX.Y.Z] [--dest DIR]\n' "$0"
      printf 'Env:   MAC_CLAMSHELL_VERSION, ANTIGRAVITY_PLUGINS_DIR\n'
      exit 0
      ;;
    *)
      printf 'install.sh: unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

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

ARCHIVE="mac-clamshell-mcp-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

TMPDIR_WORK="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_WORK"' EXIT HUP INT TERM

printf 'Downloading %s (%s)...\n' "$ARCHIVE" "$VERSION"
curl -fsSL -o "${TMPDIR_WORK}/${ARCHIVE}" "${BASE_URL}/${ARCHIVE}"
curl -fsSL -o "${TMPDIR_WORK}/${ARCHIVE}.sha256" "${BASE_URL}/${ARCHIVE}.sha256"

EXPECTED_SHA="$(awk '{print $1}' "${TMPDIR_WORK}/${ARCHIVE}.sha256" | head -n 1)"
if [ -z "$EXPECTED_SHA" ]; then
  printf 'install.sh: empty SHA-256 checksum file\n' >&2
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

if [ ! -f "${STAGE}/bin/mac-clamshell-mcp" ] || [ ! -f "${STAGE}/plugin.json" ]; then
  printf 'install.sh: archive is missing required plugin files\n' >&2
  exit 1
fi

mkdir -p "${DEST}/bin" "${DEST}/panel/dist" "${DEST}/rules" "${DEST}/skills/mac-clamshell" "${DEST}/assets"

# Atomic binary swap so a running MCP server never sees a truncated executable
cp "${STAGE}/bin/mac-clamshell-mcp" "${DEST}/bin/mac-clamshell-mcp.new"
chmod 0755 "${DEST}/bin/mac-clamshell-mcp.new"
mv -f "${DEST}/bin/mac-clamshell-mcp.new" "${DEST}/bin/mac-clamshell-mcp"

cp "${STAGE}/panel/dist/panel.html" "${DEST}/panel/dist/panel.html"
cp "${STAGE}/plugin.json" "${DEST}/plugin.json"
cp "${STAGE}/mcp_config.json" "${DEST}/mcp_config.json"
cp "${STAGE}/hooks.json" "${DEST}/hooks.json"
cp "${STAGE}/rules/AGENTS.md" "${DEST}/rules/AGENTS.md"
cp "${STAGE}/skills/mac-clamshell/SKILL.md" "${DEST}/skills/mac-clamshell/SKILL.md"
cp "${STAGE}/assets/logo.svg" "${DEST}/assets/logo.svg"
[ -f "${STAGE}/LICENSE" ] && cp "${STAGE}/LICENSE" "${DEST}/LICENSE"
[ -f "${STAGE}/README.md" ] && cp "${STAGE}/README.md" "${DEST}/README.md"

printf 'Installed Mac Clamshell MCP %s (%s) to %s\n' "$VERSION" "$TARGET" "$DEST"
printf 'Verify: %s/bin/mac-clamshell-mcp status\n' "$DEST"
