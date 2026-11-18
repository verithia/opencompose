#!/usr/bin/env bash
# Fail when a distributable exceeds its size budget.
#
# Usage: scripts/check-bundle-size.sh [--limit-mib N] <path> [<path> ...]
# The default limit is 100 MiB, the budget for every downloadable file.
# Set BUNDLE_SIZE_LIMIT_KIB to override the limit for an experiment.

set -euo pipefail

LIMIT_KIB="${BUNDLE_SIZE_LIMIT_KIB:-102400}"
status=0

if [ "${1:-}" = "--limit-mib" ]; then
  if ! [[ "${2:-}" =~ ^[1-9][0-9]*$ ]]; then
    echo "usage: $0 [--limit-mib N] <path> [<path> ...]" >&2
    exit 2
  fi
  LIMIT_KIB=$(( $2 * 1024 ))
  shift 2
fi

if [ "$#" -eq 0 ]; then
  echo "usage: $0 [--limit-mib N] <path> [<path> ...]" >&2
  exit 2
fi

for path in "$@"; do
  if [ ! -e "$path" ]; then
    echo "bundle size: missing '$path'" >&2
    status=1
    continue
  fi
  kib="$(du -sk "$path" | awk '{print $1}')"
  printf 'bundle size: %s %s KiB (limit %s KiB)\n' "$path" "$kib" "$LIMIT_KIB"
  if [ "$kib" -gt "$LIMIT_KIB" ]; then
    echo "bundle size: '$path' exceeds ${LIMIT_KIB} KiB" >&2
    status=1
  fi
done

exit "$status"
