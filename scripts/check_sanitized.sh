#!/usr/bin/env bash
# Fail if any tracked or staged file matches a pattern from .github/sanitize-patterns.txt
# (generic secret/private-path shapes only). Prints ONLY file:line (never secret text).
# Fails closed (exit 2) on missing/empty patterns or git grep errors.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
patterns=.github/sanitize-patterns.txt
if [ ! -f "$patterns" ]; then
  echo "sanitization guard: missing $patterns" >&2
  exit 2
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
grep -vE '^[[:space:]]*(#|$)' "$patterns" > "$tmp" || true
if [ ! -s "$tmp" ]; then
  echo "sanitization guard: $patterns contains no active patterns" >&2
  exit 2
fi

scan_scope() {
  local label="$1"
  shift
  local out=""
  local rc=0
  set +e
  out="$(git grep -nEI -f "$tmp" "$@" -- . ":(exclude)$patterns" ":(exclude)scripts/check_sanitized.sh" 2>&1)"
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    printf 'sanitization guard (%s): forbidden pattern matched at:\n' "$label" >&2
    printf '%s\n' "$out" | cut -d: -f1,2 | sort -u >&2
    return 1
  elif [ "$rc" -eq 1 ]; then
    return 0
  else
    printf 'sanitization guard (%s): git grep failed (exit %d)\n' "$label" "$rc" >&2
    return 2
  fi
}

found=0
rc=0
scan_scope "tracked" || rc=$?
if [ "$rc" -eq 1 ]; then
  found=1
elif [ "$rc" -ne 0 ]; then
  exit 2
fi

rc=0
scan_scope "staged" --cached || rc=$?
if [ "$rc" -eq 1 ]; then
  found=1
elif [ "$rc" -ne 0 ]; then
  exit 2
fi

if [ "$found" -ne 0 ]; then
  exit 1
fi

echo "sanitization guard: clean"
