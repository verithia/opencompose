#!/bin/sh
# Compile the fixture LV2 plugin as a cdylib with the host toolchain.
# Usage: build_lv2_fixture.sh <output.dylib|output.so|output.dll>
set -eu
out=${1:?output path}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src="$dir/lv2_fixture_plugin.rs"
rustc_bin=${RUSTC:-}
if [ -z "$rustc_bin" ]; then
  rustc_bin=$(command -v rustc || true)
fi
if [ -z "$rustc_bin" ]; then
  rustc_bin="$HOME/.cargo/bin/rustc"
fi
"$rustc_bin" --edition 2021 --crate-type cdylib -O -o "$out" "$src"
