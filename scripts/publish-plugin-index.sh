#!/usr/bin/env bash
# Sign the plugin catalog into the committed plugin-index/ directory.
#
# The documentation site deploys plugin-index/ byte for byte, and the app also
# reads the same files from the default branch as its fallback origin, so a
# publish is: run this script, review, commit plugin-index/ and push to main.
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/publish-plugin-index.sh --key FILE [options]

Builds a signed index from the catalog, verifies it against the release trust
in crates/oc-market/plugin-index.pub, and writes it into plugin-index/. The
previous catalog file stays next to the new one so a client holding the old
manifest can still finish its fetch. Nothing is committed or pushed.

Required:
  --key FILE            Ed25519 PKCS#8 private key (or OCP_PLUGIN_INDEX_KEY)

Options:
  --catalog FILE        catalog JSON (default: crates/oc-gui/frontend/src/data/plugin-catalog.json)
  --dest DIR            index directory (default: plugin-index/)
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
dest="$root/plugin-index"
min_app_version="0.1.0"
catalog_version=""
generated_at=""
mirrors=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --key) key="$2"; shift 2 ;;
        --catalog) catalog="$2"; shift 2 ;;
        --dest) dest="$2"; shift 2 ;;
        --min-app-version) min_app_version="$2"; shift 2 ;;
        --catalog-version) catalog_version="$2"; shift 2 ;;
        --generated-at) generated_at="$2"; shift 2 ;;
        --mirror) mirrors+=("$2"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ -z "$key" ]]; then
    echo "error: a signing key path is required (--key FILE or OCP_PLUGIN_INDEX_KEY)" >&2
    exit 2
fi
if [[ ! -f "$key" ]]; then
    echo "error: signing key not found: $key" >&2
    exit 2
fi

ocp="${OCP_BIN:-$root/target/release/ocp}"
if [[ ! -x "$ocp" ]]; then
    echo "error: ocp binary not found at $ocp" >&2
    echo "build it with: cargo build --release -p oc-cli" >&2
    echo "or point OCP_BIN at an existing binary" >&2
    exit 1
fi

# Print one dotted field of a manifest, e.g. `manifest_field index.json catalog.path`.
manifest_field() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1]))
for part in sys.argv[2].split("."):
    value = value[part]
print(value)
PY
}

out=$(mktemp -d)
trap 'rm -rf -- "$out"' EXIT

args=(plugin-index build --catalog "$catalog" --out "$out" --key "$key"
    --min-app-version "$min_app_version")
[[ -n "$catalog_version" ]] && args+=(--catalog-version "$catalog_version")
[[ -n "$generated_at" ]] && args+=(--generated-at "$generated_at")
if [[ ${#mirrors[@]} -gt 0 ]]; then
    for mirror in "${mirrors[@]}"; do
        args+=(--mirror "$mirror")
    done
fi

"$ocp" "${args[@]}" >/dev/null
# The signing key must be one the app trusts; a build signed by anything else
# would be refused by every client.
"$ocp" plugin-index verify "$out" --public-key "$public_key"

new_generated=$(manifest_field "$out/index.json" generated_at)
new_catalog=$(manifest_field "$out/index.json" catalog.path)
previous_catalog=""
if [[ -f "$dest/index.json" ]]; then
    old_generated=$(manifest_field "$dest/index.json" generated_at)
    # Canonical UTC timestamps order as strings. Clients refuse an older or
    # equal timestamp, so a publish must move strictly forward.
    if [[ ! "$new_generated" > "$old_generated" ]]; then
        echo "error: generated_at $new_generated is not later than the published $old_generated" >&2
        exit 1
    fi
    previous_catalog=$(manifest_field "$dest/index.json" catalog.path)
fi

mkdir -p "$dest"
cp "$out/$new_catalog" "$dest/$new_catalog"
cp "$out/index.json.sig" "$dest/index.json.sig"
cp "$out/index.json" "$dest/index.json"

shopt -s nullglob
for file in "$dest"/catalog-*.json.zst; do
    name=$(basename "$file")
    if [[ "$name" != "$new_catalog" && "$name" != "$previous_catalog" ]]; then
        rm -- "$file"
    fi
done

"$ocp" plugin-index verify "$dest" --public-key "$public_key" >/dev/null

relative=${dest#"$root"/}
cat <<EOF

Wrote $relative/ (generated_at $new_generated, catalog $new_catalog).
Review, then commit and push to main:
  git add $relative
  git commit -m "Publish the plugin index for $new_generated"
  git push origin HEAD:main
The documentation workflow deploys the files; check them with
scripts/check-plugin-index-live.sh once it finishes.
EOF
