#!/usr/bin/env bash
# Build, verify, and print the upload commands for the self-hosted plugin index.
# The host is always an argument; this script contains no deployment target.
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/publish-plugin-index.sh --key FILE [options] HOST:PATH

Builds a signed plugin index into dist/, verifies it, and prints the rsync
commands that publish it. Nothing is uploaded by this script.

Required:
  --key FILE            Ed25519 PKCS#8 private key (or OCP_PLUGIN_INDEX_KEY)
  HOST:PATH             rsync target directory, e.g. user@host:/srv/plugin-index/

Options:
  --catalog FILE        catalog JSON (default: crates/oc-gui/frontend/src/data/plugin-catalog.json)
  --public-key FILE     public key used for verification (default: repo plugin-index.pub)
  --out DIR             build directory (default: dist/)
  --min-app-version V   default: 0.1.0
  --catalog-version V   opaque version (default: derived from generated_at)
  --generated-at TIME   RFC 3339 UTC; default: current time
  --mirror NAME=URL     https mirror base URL (repeatable)

Environment:
  OCP_BIN               path to the ocp binary (default: target/release/ocp)
EOF
}

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
key="${OCP_PLUGIN_INDEX_KEY:-}"
catalog="$root/crates/oc-gui/frontend/src/data/plugin-catalog.json"
public_key="$root/crates/oc-market/plugin-index.pub"
out="$root/dist"
min_app_version="0.1.0"
catalog_version=""
generated_at=""
host=""
mirrors=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --key) key="$2"; shift 2 ;;
        --catalog) catalog="$2"; shift 2 ;;
        --public-key) public_key="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --min-app-version) min_app_version="$2"; shift 2 ;;
        --catalog-version) catalog_version="$2"; shift 2 ;;
        --generated-at) generated_at="$2"; shift 2 ;;
        --mirror) mirrors+=("$2"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
        *)
            if [[ -n "$host" ]]; then
                echo "only one HOST:PATH target is allowed" >&2
                exit 2
            fi
            host="$1"
            shift
            ;;
    esac
done

if [[ ! -s "$public_key" ]]; then
    echo "error: remote trust is unconfigured; configure crates/oc-market/plugin-index.pub or pass --public-key FILE" >&2
    exit 2
fi

if [[ -z "$key" ]]; then
    echo "error: a signing key path is required (--key FILE or OCP_PLUGIN_INDEX_KEY)" >&2
    exit 2
fi
if [[ ! -f "$key" ]]; then
    echo "error: signing key not found: $key" >&2
    exit 2
fi
if [[ -z "$host" ]]; then
    echo "error: HOST:PATH is required" >&2
    usage >&2
    exit 2
fi

ocp="${OCP_BIN:-$root/target/release/ocp}"
if [[ ! -x "$ocp" ]]; then
    echo "error: ocp binary not found at $ocp" >&2
    echo "build it with: cargo build --release -p oc-cli" >&2
    echo "or point OCP_BIN at an existing binary" >&2
    exit 1
fi

args=(plugin-index build --catalog "$catalog" --out "$out" --key "$key"
    --min-app-version "$min_app_version")
[[ -n "$catalog_version" ]] && args+=(--catalog-version "$catalog_version")
[[ -n "$generated_at" ]] && args+=(--generated-at "$generated_at")
if [[ ${#mirrors[@]} -gt 0 ]]; then
    for mirror in "${mirrors[@]}"; do
        args+=(--mirror "$mirror")
    done
fi

"$ocp" "${args[@]}"
"$ocp" plugin-index verify "$out" --public-key "$public_key"

shopt -s nullglob
catalogs=("$out"/catalog-*.json.zst)
if [[ ${#catalogs[@]} -ne 1 ]]; then
    echo "error: expected exactly one catalog-*.json.zst in $out, found ${#catalogs[@]}" >&2
    exit 1
fi
catalog_file=$(basename "${catalogs[0]}")

target="$host"
[[ "$target" == */ ]] || target="$target/"

cat <<EOF

Built $out/$catalog_file
No files were uploaded. Review and run, in this order:
  rsync -av --chmod=F644 "$out/$catalog_file" "$out/index.json.sig" "$target"
  rsync -av --chmod=F644 "$out/index.json" "$target"

The manifest goes last: it is the commit point. Serve index.json with a short
TTL and an ETag, and catalog-*.json.zst as immutable. See docs/plugin-index.md.
EOF
