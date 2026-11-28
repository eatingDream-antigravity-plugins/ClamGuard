#!/bin/sh
# Install and load the Clam Guard (`mac-clamshell-mcp`) LaunchAgent (`dev.mac-clamshell-mcp.display-guard`).
# Usage: ./scripts/install-launchagent.sh [--bin /path/to/mac-clamshell-mcp] [--data-dir /path/to/data] [--interval 10]

set -eu

LABEL="dev.mac-clamshell-mcp.display-guard"
INTERVAL="10"
DATA_DIR="${MAC_CLAMSHELL_DATA_DIR:-${HOME}/.gemini/antigravity/plugin_data/mac-clamshell-mcp}"
BIN_PATH=""

while [ $# -gt 0 ]; do
  case "$1" in
    --bin)
      if [ $# -lt 2 ]; then
        printf 'install-launchagent: --bin requires a path\n' >&2
        exit 2
      fi
      BIN_PATH="$2"
      shift 2
      ;;
    --data-dir)
      if [ $# -lt 2 ]; then
        printf 'install-launchagent: --data-dir requires a path\n' >&2
        exit 2
      fi
      DATA_DIR="$2"
      shift 2
      ;;
    --interval)
      if [ $# -lt 2 ]; then
        printf 'install-launchagent: --interval requires an integer >= 2\n' >&2
        exit 2
      fi
      INTERVAL="$2"
      shift 2
      ;;
    -h|--help)
      printf 'Usage: %s [--bin /path/to/mac-clamshell-mcp] [--data-dir /path/to/data] [--interval 10]\n' "$0"
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

case "$INTERVAL" in
  ''|*[!0-9]*)
    printf 'install-launchagent: --interval must be an integer >= 2\n' >&2
    exit 2
    ;;
esac
if [ "$INTERVAL" -lt 2 ]; then
  printf 'install-launchagent: --interval must be >= 2\n' >&2
  exit 2
fi

if [ -z "$BIN_PATH" ]; then
  SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
  if [ -x "${SCRIPT_DIR}/../bin/mac-clamshell-mcp" ]; then
    BIN_PATH="$(CDPATH= cd -- "${SCRIPT_DIR}/../bin" && pwd)/mac-clamshell-mcp"
  elif [ -x "${HOME}/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp" ]; then
    BIN_PATH="${HOME}/.gemini/config/plugins/mac-clamshell-mcp/bin/mac-clamshell-mcp"
  elif command -v mac-clamshell-mcp >/dev/null 2>&1; then
    BIN_PATH="$(command -v mac-clamshell-mcp)"
  else
    BIN_PATH="${SCRIPT_DIR}/../target/release/mac-clamshell-mcp"
  fi
fi

if [ ! -x "$BIN_PATH" ]; then
  printf 'install-launchagent: executable not found at %s\n' "$BIN_PATH" >&2
  exit 1
fi

case "${BIN_PATH}${DATA_DIR}" in
  *'<'*|*'>'*|*'&'*|*'"'*|*"'"*)
    printf 'install-launchagent: paths must not contain XML special characters\n' >&2
    exit 2
    ;;
esac
CLEAN_PATHS="$(printf '%s' "${BIN_PATH}${DATA_DIR}" | LC_ALL=C tr -d '[:cntrl:]')"
if [ "$CLEAN_PATHS" != "${BIN_PATH}${DATA_DIR}" ]; then
  printf 'install-launchagent: paths must not contain control or newline characters\n' >&2
  exit 2
fi

AGENTS_DIR="${HOME}/Library/LaunchAgents"
LOG_DIR="${DATA_DIR}/logs"
PLIST_PATH="${AGENTS_DIR}/${LABEL}.plist"

if [ -L "$AGENTS_DIR" ]; then
  printf 'install-launchagent: refusing symlink directory: %s\n' "$AGENTS_DIR" >&2
  exit 1
fi
if [ -L "$DATA_DIR" ] || [ -L "$LOG_DIR" ]; then
  printf 'install-launchagent: refusing symlink data/log directory\n' >&2
  exit 1
fi
if [ -L "$PLIST_PATH" ]; then
  printf 'install-launchagent: refusing symlink plist target: %s\n' "$PLIST_PATH" >&2
  exit 1
fi
if [ -e "$PLIST_PATH" ] && [ ! -f "$PLIST_PATH" ]; then
  printf 'install-launchagent: refusing non-regular plist target: %s\n' "$PLIST_PATH" >&2
  exit 1
fi

mkdir -p "$AGENTS_DIR"
(umask 077 && mkdir -p "$DATA_DIR" "$LOG_DIR")
chmod 0700 "$DATA_DIR" "$LOG_DIR"

TMP_PLIST="$(mktemp "${AGENTS_DIR}/.display-guard.plist.XXXXXX")"
trap 'rm -f "$TMP_PLIST"' EXIT HUP INT TERM
chmod 0600 "$TMP_PLIST"

cat > "$TMP_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${BIN_PATH}</string>
    <string>watch</string>
    <string>--data-dir</string>
    <string>${DATA_DIR}</string>
  </array>
  <key>StartInterval</key>
  <integer>${INTERVAL}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>LowPriorityBackgroundIO</key>
  <true/>
  <key>Nice</key>
  <integer>15</integer>
  <key>StandardOutPath</key>
  <string>${LOG_DIR}/display-guard.log</string>
  <key>StandardErrorPath</key>
  <string>${LOG_DIR}/display-guard.err.log</string>
</dict>
</plist>
EOF

if ! /usr/bin/plutil -lint "$TMP_PLIST" >/dev/null; then
  rm -f "$TMP_PLIST"
  exit 1
fi
if [ -L "$PLIST_PATH" ]; then
  rm -f "$TMP_PLIST"
  printf 'install-launchagent: refusing symlink plist target: %s\n' "$PLIST_PATH" >&2
  exit 1
fi
mv -f "$TMP_PLIST" "$PLIST_PATH"
trap - EXIT HUP INT TERM

UID_NUM="$(id -u)"
launchctl bootout "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/${UID_NUM}" "$PLIST_PATH"
launchctl kickstart -k "gui/${UID_NUM}/${LABEL}" 2>/dev/null || true
printf 'Installed and loaded %s -> %s\n' "$PLIST_PATH" "$BIN_PATH"
