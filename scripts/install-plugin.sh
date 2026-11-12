#!/usr/bin/env bash
# Install one plugin from the catalog in one command.
#
# This is a thin wrapper: it finds the `ocp` binary and hands the arguments to
# `ocp plugin install`, which resolves the entry against the catalog, verifies
# the download against the pinned sha256, and unpacks it. Nothing else happens
# here, so the agent tool and this script share one resolver and one refusal
# vocabulary.
#
#   scripts/install-plugin.sh asb2m10.dexed
#   scripts/install-plugin.sh asb2m10.dexed --dry-run
#   scripts/install-plugin.sh asb2m10.dexed --index https://plugins.example.com/ --into /tmp/plugins
#
# Set OCP_BIN to an existing binary; otherwise the release, perf, and debug
# builds under target/ are tried in that order.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
    cat <<'EOF'
usage: scripts/install-plugin.sh ID [options]

Resolves, downloads, verifies, and installs one catalog entry.

Options are passed straight to `ocp plugin install`:
  --index URL        install from a signed index base URL
  --catalog FILE     install from a local catalog JSON file
  --public-key FILE  pinned Ed25519 public key for --index
  --platform NAME    macos-aarch64 | macos-x86_64 | windows-x86_64 | linux-x86_64
  --format NAME      vst3 | vst2 | clap | au | lv2 | standalone
  --into DIR         install directory (default: the host plugin directory)
  --dry-run          print the verified plan without downloading
  --open             open the vendor installer for a build that cannot be unpacked
  --overwrite        replace an existing bundle
  --json             print the report as JSON

Environment:
  OCP_BIN            path to the ocp binary
EOF
}

if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" ]]; then
    usage
    exit 0
fi

ocp="${OCP_BIN:-}"
if [[ -z "$ocp" ]]; then
    for candidate in \
        "$root/target/release/ocp" \
        "$root/target/perf/ocp" \
        "$root/target/debug/ocp"; do
        if [[ -x "$candidate" ]]; then
            ocp="$candidate"
            break
        fi
    done
fi
if [[ -z "$ocp" || ! -x "$ocp" ]]; then
    echo "error: no ocp binary found; build one with 'cargo build -p oc-cli' or set OCP_BIN" >&2
    exit 1
fi

exec "$ocp" plugin install "$@"
