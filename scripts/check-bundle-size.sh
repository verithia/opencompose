#!/usr/bin/env bash
# Fail when a packaged app or distributable exceeds the 100 MiB budget.
#
# Usage: scripts/check-bundle-size.sh <path> [<path> ...]
# Set BUNDLE_SIZE_LIMIT_KIB to override the limit for an experiment.

set -euo pipefail

LIMIT_KIB="${BUNDLE_SIZE_LIMIT_KIB:-102400}"
status=0

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <path> [<path> ...]" >&2
  exit 2
fi

for path in "$@"; do
  if [ ! -e "$path" ]; then
    echo "bundle size: missing '$path'" >&2
    status=1
    continue
  fi
  kib="$(du -sk "$path" | awk '{print $1}')"
  printf 'bundle size: %s %s KiB\n' "$path" "$kib"
  if [ "$kib" -gt "$LIMIT_KIB" ]; then
    echo "bundle size: '$path' exceeds ${LIMIT_KIB} KiB" >&2
    status=1
  fi
done

exit "$status"
