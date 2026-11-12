#!/usr/bin/env bash
# Check that the documentation site is complete, linked and parseable.
#
# Rules, all fatal:
#   1. Every docs/**/*.md page is a chapter in docs/SUMMARY.md.
#   2. Every entry in docs/SUMMARY.md exists and appears once.
#   3. Every relative link and image in a chapter resolves within the book.
#   4. Every tool name written in a book chapter exists in the tool
#      identity table (plus docs/tool-names-allow.txt for identifiers that only look
#      like tool names).
#   5. Every ```ocp block parses, and so does every snippet in the product's
#      dsl-examples.json (crates/oc-dsl/examples/check_doc_snippets.rs).
#   6. No page contains non-English text, and links into api/ name a crate that
#      `cargo doc` produces.
#
# What it does not catch: anchors inside a page, prose that is wrong about
# behaviour (only a test or a reviewer catches that), external URLs, images
# referenced by absolute URL, and non-Markdown fences (JSON, TOML, shell).
#
# Usage: scripts/check-docs.sh [--list] [--no-snippets]
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root" || exit 2

snippets=1
for arg in "$@"; do
    case "$arg" in
        --list)
            sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        --no-snippets) snippets=0 ;;
        *)
            echo "check-docs: unknown option $arg" >&2
            exit 2
            ;;
    esac
done

if [ ! -f docs/SUMMARY.md ]; then
    echo "check-docs: docs/SUMMARY.md is missing" >&2
    exit 2
fi

status=0

# Rules 1-4 and 6: structure, links, tool names, language, api/ targets.
python3 - "$root" <<'PY' || status=1
import os
import re
import sys

root = sys.argv[1]
docs = os.path.join(root, "docs")
problems = []


def rel(path):
    return os.path.relpath(path, root)


def strip_code(text):
    """Drop fenced blocks: an example may contain brackets that are not links."""
    out = []
    fence = False
    for line in text.split("\n"):
        if line.lstrip().startswith("```"):
            fence = not fence
            out.append("")
            continue
        out.append("" if fence else line)
    return "\n".join(out)


def markdown_files(base):
    for dirpath, _dirs, files in os.walk(base):
        for name in sorted(files):
            if name.endswith(".md"):
                yield os.path.join(dirpath, name)


# --- the table of contents ------------------------------------------------

summary_path = os.path.join(docs, "SUMMARY.md")
summary_text = open(summary_path, encoding="utf-8").read()
chapters = []
for target in re.findall(r"\]\(([^)]+)\)", strip_code(summary_text)):
    target = target.split("#", 1)[0].strip()
    if not target or target.startswith(("http://", "https://", "mailto:")):
        continue
    # mdBook maps README.md to index.html, so the summary names it index.md.
    if target == "index.md":
        target = "README.md"
    chapters.append(target)

seen = {}
for name in chapters:
    seen[name] = seen.get(name, 0) + 1
for name, count in seen.items():
    if count > 1:
        problems.append(f"docs/SUMMARY.md: '{name}' appears {count} times")
chapter_set = set(chapters)

for name in sorted(chapter_set):
    path = os.path.normpath(os.path.join(docs, name))
    if not os.path.isfile(path):
        problems.append(f"docs/SUMMARY.md: '{name}' does not exist")

for path in markdown_files(docs):
    name = os.path.relpath(path, docs)
    if name != "SUMMARY.md" and name not in chapter_set:
        problems.append(f"{rel(path)}: not a chapter of docs/SUMMARY.md")

# --- links ----------------------------------------------------------------

link_re = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")

for path in markdown_files(docs):
    name = os.path.relpath(path, docs)
    if name == "SUMMARY.md":
        continue
    text = open(path, encoding="utf-8").read()
    for target in link_re.findall(strip_code(text)):
        if target.startswith(("http://", "https://", "mailto:", "tel:", "#")):
            continue
        raw = target
        target = target.split("#", 1)[0]
        if not target:
            continue
        # api/ pages are generated into book/api and checked by their own rule.
        if target.startswith("api/") or target.startswith("../api/"):
            continue
        resolved = os.path.normpath(os.path.join(os.path.dirname(path), target))
        exists = os.path.exists(resolved)
        if not exists:
            problems.append(f"{rel(path)}: link target '{raw}' does not exist")
            continue
        if not resolved.startswith(docs + os.sep):
            problems.append(
                f"{rel(path)}: link target '{raw}' is outside docs/, so it is not published"
            )
            continue
        if resolved.startswith(docs + os.sep) and target.endswith(".md"):
            linked = os.path.relpath(resolved, docs)
            if linked not in chapter_set:
                problems.append(
                    f"{rel(path)}: links to '{raw}', which is not published on the site"
                )
        if resolved == docs or os.path.isdir(resolved):
            linked = os.path.relpath(resolved, docs)
            if linked in ("benchmarks",):
                problems.append(
                    f"{rel(path)}: links to '{raw}', a directory with no published chapter"
                )

# --- api/ links name a real crate ----------------------------------------

api_re = re.compile(r"\]\((?:[.][.]/)*api/([^)#\s]+)")
api_dir = os.path.join(root, "book", "api")
crates = set()
if os.path.isdir(api_dir):
    crates = {entry for entry in os.listdir(api_dir) if os.path.isdir(os.path.join(api_dir, entry))}
for path in markdown_files(docs):
    name = os.path.relpath(path, docs)
    if name == "SUMMARY.md":
        continue
    text = open(path, encoding="utf-8").read()
    for target in api_re.findall(strip_code(text)):
        if target == "index.html":
            continue
        segment = target.split("/", 1)[0]
        if not segment:
            continue
        if crates and segment not in crates:
            problems.append(f"{rel(path)}: 'api/{target}' is not a crate page")
        elif crates:
            resolved = os.path.join(api_dir, target)
            if target.endswith("/"):
                resolved = resolved.rstrip("/")
            if not os.path.exists(resolved):
                problems.append(f"{rel(path)}: 'api/{target}' does not exist in book/api")

# --- tool names -----------------------------------------------------------

identity_path = os.path.join(
    root, "crates/oc-gui/frontend/src-tauri/src/tool_registry/identity.rs"
)
tools = set()
if os.path.isfile(identity_path):
    identities = open(identity_path, encoding="utf-8").read()
    declarations = identities.split("tool_ids!", 1)[-1]
    rows = re.findall(
        r'\b[A-Z]\w*\s*=>\s*\(\s*"([a-z][a-z0-9_]*)"\s*,.*?,\s*'
        r'(Backend|PanelLocal|HiddenBackend)\s*\)',
        declarations,
        re.S,
    )
    tools.update(name for name, handler in rows if handler != "HiddenBackend")
if len(tools) < 90:
    problems.append(
        f"only {len(tools)} tool names found in the registry; the extraction is broken"
    )

allow_path = os.path.join(docs, "tool-names-allow.txt")
allowed = set()
if os.path.isfile(allow_path):
    for line in open(allow_path, encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#"):
            allowed.add(line.split()[0])

# A tool name is `area_verb`. The areas come from the registry itself, so a
# typo or a removed tool is caught while an ordinary snake_case identifier that
# does not start with an area prefix (a JSON field, a parameter) is left alone.
areas = {name.split("_", 1)[0] for name in tools if "_" in name}
tool_pages = [rel(path) for path in markdown_files(docs)]
span_re = re.compile(r"`([^`\n]+)`")
identifier = re.compile(r"^[a-z][a-z0-9]*(_[a-z0-9]+)+$")
for page in tool_pages:
    path = os.path.join(root, page)
    if not os.path.isfile(path):
        problems.append(f"{page}: missing")
        continue
    reported = set()
    for token in span_re.findall(strip_code(open(path, encoding="utf-8").read())):
        token = token.strip()
        if token in reported or not identifier.match(token):
            continue
        if token in tools or token in allowed:
            continue
        if token.split("_", 1)[0] not in areas:
            continue
        reported.add(token)
        problems.append(
            f"{page}: '{token}' looks like a tool name but is not in the registry "
            f"(add it to docs/tool-names-allow.txt if it is an ordinary identifier)"
        )

# --- non-English text -----------------------------------------------------

cjk = re.compile(r"[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff00-\uffef]")
for path in markdown_files(docs):
    for number, line in enumerate(open(path, encoding="utf-8"), 1):
        if cjk.search(line):
            problems.append(f"{rel(path)}:{number}: non-English text")
            break

for problem in problems:
    print(f"check-docs: {problem}", file=sys.stderr)
print(
    f"check-docs: {len(chapter_set)} chapters, "
    f"{len(tools)} tool names",
    file=sys.stderr,
)
sys.exit(1 if problems else 0)
PY

# Rule 5: every ocp fence and every product example parses.
if [ "$snippets" -eq 1 ]; then
    files=()
    while IFS= read -r file; do
        files+=("$file")
    done < <(find docs -name '*.md' -not -name SUMMARY.md | sort)
    examples=crates/oc-gui/frontend/src/ai/dsl-examples.json
    args=()
    if [ -f "$examples" ]; then
        args=(--examples "$examples")
    fi
    cargo_command=${CARGO:-cargo}
    if ! "$cargo_command" run --quiet -p oc-dsl --example check_doc_snippets -- "${args[@]}" "${files[@]}"; then
        status=1
    fi
fi

if [ "$status" -eq 0 ]; then
    echo "check-docs: clean"
fi
exit "$status"
