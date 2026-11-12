#!/usr/bin/env bash
# Run named test cases against one prebuilt test binary.
#
# Reading the names from stdin avoids linking a binary per case:
#
#   printf 'mixdown::a_track\ndynamics::the_ratio\n' | scripts/test-cases.sh oc-engine
#
# The binary defaults to `all` (see `autotests = false` in the crate's
# Cargo.toml); pass a second argument for a separate target (`vocal_tune`,
# `clips`, a `*_alloc` target, a custom harness).
set -euo pipefail

crate=${1:?usage: test-cases.sh <crate> [test-binary] [extra cargo test args...]}
binary=${2:-all}
if [ $# -ge 2 ]; then shift 2; else shift 1; fi

cases=()
while IFS= read -r case_name || [ -n "$case_name" ]; do
  if [ -n "$case_name" ]; then
    cases+=("$case_name")
  fi
done < <(tr -s '[:space:],' '\n')
if [ "${#cases[@]}" -eq 0 ]; then
  echo "test-cases: no case names on stdin" >&2
  exit 2
fi

cargo test -q -p "$crate" --test "$binary" "$@" -- "${cases[@]}"
