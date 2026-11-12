#!/usr/bin/env bash
# Fail when a generated frontend catalog no longer matches its Rust owner.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root" || exit 2

cargo_command=${CARGO:-cargo}
"$cargo_command" run -q --locked -p oc-device-schema --bin gen-device-catalog -- --check
"$cargo_command" run -q --locked -p oc-device-schema --bin gen-synth-catalog -- --check
