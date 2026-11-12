#!/usr/bin/env bash
# Count public items with and without a doc comment, per crate.
#
# Documentation coverage is measured, not guessed: a public item counts as
# documented when a `///` line precedes it (attributes may sit in between), or
# when a `pub mod` names a file that opens with `//!`. Items behind
# `#[cfg(test)]` are not part of the public surface and are skipped.
#
# Usage:
#   scripts/doc-coverage.sh [--list] [ROOT]
#
# ROOT defaults to the repository root. `--list` prints the undocumented items
# so a session can pick which ones are worth a doc comment; an item whose name
# already states its contract is not worth `/// Returns the value.`
set -uo pipefail

root=$(CDPATH= cd -- "${2:-$(dirname -- "$0")/..}" && pwd)
list=0
[ "${1:-}" = "--list" ] && list=1

python3 - "$root" "$list" <<'PY'
import os
import re
import sys

root = sys.argv[1]
list_undocumented = sys.argv[2] == "1"

ITEM = re.compile(
    r"^\s*pub (?:async )?(?:unsafe )?(?:const )?(struct|enum|trait|fn|type|const|static|union|mod) (\w+)"
)


def crate_roots():
    crates_dir = os.path.join(root, "crates")
    for name in sorted(os.listdir(crates_dir)):
        src = os.path.join(crates_dir, name, "src")
        if os.path.isdir(src):
            yield name, src
    gui = os.path.join(root, "crates/oc-gui/frontend/src-tauri/src")
    if os.path.isdir(gui):
        yield "oc-gui", gui


def has_module_doc(path):
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                stripped = line.strip()
                if not stripped:
                    continue
                return stripped.startswith("//!")
    except OSError:
        return False


def documented(lines, index):
    cursor = index - 1
    while cursor >= 0:
        stripped = lines[cursor].strip()
        if stripped.startswith("//"):
            return stripped.startswith("///") or stripped.startswith("//!")
        if stripped.startswith("#["):
            cursor -= 1
            continue
        return False
    return False


total_items = 0
total_documented = 0
undocumented = []

for crate, src in crate_roots():
    items = 0
    documented_items = 0
    for dirpath, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d not in ("tests", "fixtures")]
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            text = open(path, encoding="utf-8").read()
            if "#[cfg(test)]" in text:
                text = text.split("#[cfg(test)]", 1)[0]
            lines = text.split("\n")
            for index, line in enumerate(lines):
                match = ITEM.match(line)
                if not match:
                    continue
                kind, item = match.group(1), match.group(2)
                items += 1
                if documented(lines, index):
                    documented_items += 1
                    continue
                if kind == "mod":
                    inline = [l for l in lines[index + 1 : index + 8] if l.strip()]
                    if inline and inline[0].strip().startswith("//!"):
                        documented_items += 1
                        continue
                    module_file = os.path.join(dirpath, item + ".rs")
                    if os.path.isfile(module_file) and has_module_doc(module_file):
                        documented_items += 1
                        continue
                undocumented.append(f"{os.path.relpath(path, root)}: {kind} {item}")
    total_items += items
    total_documented += documented_items
    share = (100.0 * documented_items / items) if items else 100.0
    print(f"{crate:20s} {documented_items:5d}/{items:<5d} {share:5.1f}%")

share = (100.0 * total_documented / total_items) if total_items else 100.0
print(f"{'total':20s} {total_documented:5d}/{total_items:<5d} {share:5.1f}%")

if list_undocumented:
    for entry in undocumented:
        print(entry)
PY
