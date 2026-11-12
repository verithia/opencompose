#!/usr/bin/env python3
"""Check and generate the third-party license notices that ship with OpenCompose.

The desktop app, the CLI and the language server link about 700 crates and a
few npm packages, and bundle a SoundFont and a font. Their licenses require
the notices to travel with the binary, so this script:

* refuses a dependency whose license is not on the allow list, and refuses a
  copyleft license anywhere in the graph of the permissive language crates;
* collects each package's own LICENSE / COPYING / NOTICE files, falling back to
  the standard SPDX text in `scripts/license-texts/` when a package ships none;
* writes `THIRD_PARTY_LICENSES.txt`, which the app bundles and shows on the
  Help & About page. Each distinct notice appears once; a full Apache-2.0 copy
  becomes a reference to one shared copy plus the package's copyright lines.

`--check` verifies the policy and that the committed file is current. It
needs the crate sources (`cargo fetch --locked`) and the frontend packages
(`npm ci` in `crates/oc-gui/frontend`).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FRONTEND = ROOT / "crates" / "oc-gui" / "frontend"
OUTPUT = FRONTEND / "src-tauri" / "resources" / "THIRD_PARTY_LICENSES.txt"
STANDARD_TEXTS = Path(__file__).resolve().parent / "license-texts"

# Binaries that ship to users. Their normal and build dependencies are linked
# or bundled into a release; dev-dependencies are not.
SHIPPED_ROOTS = ("oc-gui", "oc-cli", "oc-lsp")
# Crates published under MIT OR Apache-2.0. Their dependency graph must stay
# free of copyleft so that other tools can use them without the AGPL.
PERMISSIVE_ROOTS = ("oc-dsl", "oc-device-schema", "oc-lsp")

PERMISSIVE = {
    "0BSD",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT-0",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
}
# Weak copyleft that is compatible with the AGPL application when the library
# is used unmodified. Not allowed in the permissive crates' graph.
APPLICATION_ONLY = {"MPL-2.0"}

# Third-party assets bundled with the app, with the file that carries their terms.
ASSETS = (
    (
        "General MIDI SoundFont (FluidR3Mono_GM.sf3, MS_General 0.1)",
        FRONTEND / "src-tauri" / "resources" / "soundfonts" / "FluidR3Mono_License.md",
    ),
    ("Quicksand font (quicksand-latin.woff2)", FRONTEND / "src" / "assets" / "fonts" / "OFL.txt"),
)

# Notices a package needs beyond its own files. The `vst3` bindings are
# generated from the VST 3 SDK headers, which carry Steinberg's MIT notice.
EXTRA_NOTICES = {
    "vst3": [("VST 3 SDK (the headers the bindings are generated from)", STANDARD_TEXTS / "vst3-sdk.txt")],
}

NOTICE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")


class PolicyError(Exception):
    pass


@dataclass
class Package:
    ecosystem: str
    name: str
    version: str
    license: str
    authors: list[str] = field(default_factory=list)
    directory: Path | None = None
    repository: str = ""

    @property
    def label(self) -> str:
        return f"{self.name} {self.version}"


# ---------------------------------------------------------------------------
# SPDX expressions
# ---------------------------------------------------------------------------


def tokenize(expression: str) -> list[str]:
    # Old crates write "MIT/Apache-2.0"; that slash means OR.
    expression = expression.replace("/", " OR ")
    return re.findall(r"\(|\)|[^\s()]+", expression)


def alternatives(expression: str) -> list[frozenset[str]]:
    """Every set of licenses that, taken together, satisfies the expression."""
    tokens = tokenize(expression)
    position = 0

    def parse_or() -> list[frozenset[str]]:
        nonlocal position
        result = parse_and()
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            result = result + parse_and()
        return result

    def parse_and() -> list[frozenset[str]]:
        nonlocal position
        result = parse_atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            right = parse_atom()
            result = [a | b for a in result for b in right]
        return result

    def parse_atom() -> list[frozenset[str]]:
        nonlocal position
        if position >= len(tokens):
            raise PolicyError(f"incomplete license expression {expression!r}")
        token = tokens[position]
        position += 1
        if token == "(":
            inner = parse_or()
            if position >= len(tokens) or tokens[position] != ")":
                raise PolicyError(f"unbalanced license expression {expression!r}")
            position += 1
            return inner
        if position < len(tokens) and tokens[position] == "WITH":
            token = f"{token} WITH {tokens[position + 1]}"
            position += 2
        return [frozenset([token])]

    result = parse_or()
    if position != len(tokens):
        raise PolicyError(f"unparsed license expression {expression!r}")
    return result


def chosen(expression: str, allowed: set[str]) -> frozenset[str] | None:
    """The smallest satisfying set within `allowed`, preferring MIT then Apache."""
    options = [option for option in alternatives(expression) if option <= allowed]
    if not options:
        return None
    rank = {"MIT": 0, "Apache-2.0": 1}
    return min(options, key=lambda option: (len(option), sorted(rank.get(i, 9) for i in option), sorted(option)))


# ---------------------------------------------------------------------------
# Package discovery
# ---------------------------------------------------------------------------


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def reachable(metadata: dict, roots: tuple[str, ...]) -> set[str]:
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    ids = {package["name"]: package["id"] for package in metadata["packages"] if package["source"] is None}
    stack = [ids[name] for name in roots]
    seen: set[str] = set()
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dependency in nodes[current]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in dependency["dep_kinds"]):
                stack.append(dependency["pkg"])
    return seen


def crate_packages(metadata: dict, ids: set[str]) -> list[Package]:
    packages = []
    for package in metadata["packages"]:
        if package["id"] not in ids or package["source"] is None:
            continue
        if not package.get("license"):
            raise PolicyError(f"crate {package['name']} {package['version']} declares no SPDX license")
        packages.append(
            Package(
                ecosystem="crate",
                name=package["name"],
                version=package["version"],
                license=package["license"],
                authors=package.get("authors") or [],
                directory=Path(package["manifest_path"]).parent,
                repository=package.get("repository") or "",
            )
        )
    return packages


def npm_packages() -> list[Package]:
    lock = json.loads((FRONTEND / "package-lock.json").read_text(encoding="utf-8"))
    packages = []
    for path, entry in sorted(lock["packages"].items()):
        if not path or entry.get("dev") or entry.get("devOptional"):
            continue
        if not entry.get("license"):
            raise PolicyError(f"npm package {path} declares no license")
        directory = FRONTEND / path
        if not directory.is_dir():
            raise PolicyError(f"{path} is missing; run `npm ci` in {FRONTEND.relative_to(ROOT)}")
        packages.append(
            Package(
                ecosystem="npm",
                name=path.removeprefix("node_modules/"),
                version=entry["version"],
                license=entry["license"],
                directory=directory,
            )
        )
    return packages


# ---------------------------------------------------------------------------
# Policy and notices
# ---------------------------------------------------------------------------


def policy_errors(shipped: list[Package], permissive: list[Package]) -> list[str]:
    errors = []
    for package in shipped:
        try:
            if chosen(package.license, PERMISSIVE | APPLICATION_ONLY) is None:
                errors.append(f"{package.ecosystem} {package.label}: license {package.license!r} is not allowed")
        except PolicyError as error:
            errors.append(f"{package.ecosystem} {package.label}: {error}")
    for package in permissive:
        try:
            if chosen(package.license, PERMISSIVE) is None:
                errors.append(
                    f"{package.ecosystem} {package.label}: license {package.license!r} is not permissive, "
                    f"but a MIT OR Apache-2.0 crate ({', '.join(PERMISSIVE_ROOTS)}) depends on it"
                )
        except PolicyError as error:
            errors.append(f"{package.ecosystem} {package.label}: {error}")
    return errors


def notice_files(package: Package) -> list[tuple[str, str]]:
    assert package.directory is not None
    files = []
    for path in sorted(package.directory.iterdir()):
        if path.is_file() and path.name.upper().startswith(NOTICE_PREFIXES):
            files.append((path.name, path.read_text(encoding="utf-8", errors="replace")))
    return files


def standard_texts(package: Package) -> list[tuple[str, str]]:
    licenses = chosen(package.license, PERMISSIVE | APPLICATION_ONLY)
    assert licenses is not None
    holders = ", ".join(package.authors) or f"the {package.name} authors"
    texts = []
    for license_id in sorted(licenses):
        base = license_id.split(" WITH ")[0]
        path = STANDARD_TEXTS / f"{base}.txt"
        if not path.is_file():
            raise PolicyError(
                f"{package.label} ships no license file and there is no standard text for {base}; "
                f"add {path.relative_to(ROOT)}"
            )
        text = path.read_text(encoding="utf-8")
        text = re.sub(r"<year> <(copyright holders|owner)>", holders, text)
        texts.append((f"{base} (standard text; the package ships no license file)", text))
    return texts


def normalize(text: str) -> str:
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    return "\n".join(lines).strip("\n")


APACHE_MARKERS = ("Apache License", "TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION")
APACHE_PLACEHOLDER = re.compile(r"Copyright\s*(\[yyyy\]|\{yyyy\}|<year>)", re.IGNORECASE)


def condense(name: str, text: str) -> tuple[str, str]:
    """Replace a full Apache-2.0 copy with a reference to the one shared copy.

    Apache-2.0 asks for a copy of the license, not one per package. A package's
    own copyright lines in the appendix are notices, so they stay.
    """
    if not all(marker in text for marker in APACHE_MARKERS):
        return name, normalize(text)
    holders = [
        line.strip()
        for line in text.splitlines()
        if line.strip().lower().startswith("copyright") and not APACHE_PLACEHOLDER.search(line)
    ]
    body = "Licensed under the Apache License, Version 2.0; see [Apache-2.0] below."
    if holders:
        body = "\n".join(holders) + "\n\n" + body
    return name, body


def render(packages: list[Package]) -> str:
    notices: dict[str, tuple[int, str, str]] = {}
    index: list[tuple[Package, list[int]]] = []
    for package in sorted(packages, key=lambda p: (p.ecosystem, p.name, p.version)):
        numbers = []
        extra = [(name, path.read_text(encoding="utf-8")) for name, path in EXTRA_NOTICES.get(package.name, [])]
        for name, text in (notice_files(package) or standard_texts(package)) + extra:
            name, text = condense(name, text)
            key = re.sub(r"\s+", " ", text).strip()
            if key not in notices:
                notices[key] = (len(notices) + 1, name, text)
            numbers.append(notices[key][0])
        index.append((package, sorted(set(numbers))))

    rule = "=" * 78
    out = [
        "OpenCompose third-party notices",
        "",
        "OpenCompose includes the third-party software and assets listed below.",
        "Each is used under the license reproduced here. This file is generated by",
        "scripts/third-party-licenses.py; do not edit it by hand.",
        "",
        "Source code for every package, including the MPL-2.0 packages, is available",
        "from the repository listed with it.",
        "",
        rule,
        "Bundled assets",
        rule,
    ]
    for title, path in ASSETS:
        out += ["", f"--- {title} ---", "", normalize(path.read_text(encoding="utf-8"))]
    out += ["", rule, "Packages (license expression, notice numbers)", rule, ""]
    for package, numbers in index:
        source = f" <{package.repository}>" if package.repository else ""
        refs = ", ".join(f"[{n}]" for n in numbers)
        out.append(f"{package.ecosystem} {package.label} ({package.license}){source}: {refs}")
    out += ["", rule, "Notices", rule]
    for number, name, text in notices.values():
        out += ["", f"--- [{number}] {name} ---", "", text]
    apache = normalize((STANDARD_TEXTS / "Apache-2.0.txt").read_text(encoding="utf-8"))
    out += ["", rule, "[Apache-2.0] Apache License, Version 2.0", rule, "", apache]
    return "\n".join(out).rstrip("\n") + "\n"


def collect() -> tuple[list[Package], list[str]]:
    metadata = cargo_metadata()
    shipped = crate_packages(metadata, reachable(metadata, SHIPPED_ROOTS)) + npm_packages()
    permissive = crate_packages(metadata, reachable(metadata, PERMISSIVE_ROOTS))
    return shipped, policy_errors(shipped, permissive)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="fail if the policy or the committed file is out of date")
    parser.add_argument("--output", type=Path, default=OUTPUT)
    args = parser.parse_args(argv)

    try:
        packages, errors = collect()
        text = render(packages) if not errors else ""
    except (PolicyError, subprocess.CalledProcessError) as error:
        print(f"third-party-licenses: {error}", file=sys.stderr)
        return 1
    for error in errors:
        print(f"third-party-licenses: {error}", file=sys.stderr)
    if errors:
        return 1

    if args.check:
        current = args.output.read_text(encoding="utf-8") if args.output.is_file() else ""
        if current != text:
            print(
                f"third-party-licenses: {args.output.relative_to(ROOT)} is out of date; "
                "run scripts/third-party-licenses.py",
                file=sys.stderr,
            )
            return 1
        print(f"third-party-licenses: {len(packages)} packages, notices current")
        return 0

    args.output.write_text(text, encoding="utf-8")
    print(f"third-party-licenses: wrote {len(packages)} packages to {args.output.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
