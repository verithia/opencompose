#!/usr/bin/env bash
# Check that every index origin serves the committed plugin-index/ byte for
# byte, and that the served files verify against the release trust.
#
# The origins are read from crates/oc-market/src/index/verified.rs, the single
# owner of the URLs the app and the CLI fetch from.
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/check-plugin-index-live.sh [--wait SECONDS] [--origin URL]...

Downloads index.json, index.json.sig and the catalog it names from each origin
with identity encoding, compares them with plugin-index/, and verifies them
with `ocp plugin-index verify`.

Options:
  --wait SECONDS   keep retrying a mismatching origin for up to SECONDS
                   (a deployment or CDN refresh takes a few minutes)
  --origin URL     check this base URL instead of the built-in origins (repeatable)

Environment:
  OCP_BIN          path to the ocp binary (default: target/release/ocp)
EOF
}

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
committed="$root/plugin-index"
public_key="$root/crates/oc-market/plugin-index.pub"
wait_seconds=0
origins=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --wait) wait_seconds="$2"; shift 2 ;;
        --origin) origins+=("$2"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

ocp="${OCP_BIN:-$root/target/release/ocp}"
if [[ ! -x "$ocp" ]]; then
    echo "error: ocp binary not found at $ocp (cargo build --release -p oc-cli)" >&2
    exit 1
fi
if [[ ! -f "$committed/index.json" ]]; then
    echo "error: $committed/index.json is missing" >&2
    exit 1
fi

if [[ ${#origins[@]} -eq 0 ]]; then
    while IFS= read -r origin; do
        origins+=("$origin")
    done < <(python3 - "$root/crates/oc-market/src/index/verified.rs" <<'PY'
import re
import sys

source = open(sys.argv[1]).read()
for name in ("PRIMARY_BASE_URL", "FALLBACK_BASE_URL"):
    match = re.search(name + r':\s*&str\s*=\s*"([^"]+)"', source)
    if not match:
        sys.exit(f"{name} not found")
    print(match.group(1))
PY
)
fi

catalog=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["catalog"]["path"])' \
    "$committed/index.json")
files=(index.json index.json.sig "$catalog")

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT

# Fetch every file of one origin into a directory and compare it with the
# committed copy. Prints the first difference and returns non-zero on any.
check_origin() {
    local base=$1 dir=$2 file
    rm -rf -- "$dir"
    mkdir -p "$dir"
    for file in "${files[@]}"; do
        if ! curl -fsS --max-time 30 -H 'Accept-Encoding: identity' \
            -H 'Cache-Control: no-cache' -o "$dir/$file" "$base$file"; then
            echo "  $file: download failed"
            return 1
        fi
        if ! cmp -s "$dir/$file" "$committed/$file"; then
            echo "  $file: differs from plugin-index/$file"
            return 1
        fi
    done
    "$ocp" plugin-index verify "$dir" --public-key "$public_key"
}

status=0
for index in "${!origins[@]}"; do
    base=${origins[$index]}
    [[ "$base" == */ ]] || base="$base/"
    echo "$base"
    deadline=$((SECONDS + wait_seconds))
    until check_origin "$base" "$scratch/$index"; do
        if (( SECONDS >= deadline )); then
            echo "  FAILED"
            status=1
            continue 2
        fi
        sleep 15
    done
done
exit "$status"
