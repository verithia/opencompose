#!/usr/bin/env bash
# Check tracked files for unresolved conflict markers.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

if git grep -n -I -E '^(<{7} |>{7} )' -- .; then
    echo "check-conflict-markers: unresolved conflict markers" >&2
    exit 1
else
    status=$?
    if [ "$status" -ne 1 ]; then
        exit "$status"
    fi
fi
