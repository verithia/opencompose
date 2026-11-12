#!/usr/bin/env python3
"""Check the license boundary between the application and the language crates.

The application is AGPL-3.0-or-later. The language components stay
MIT OR Apache-2.0 so other tools can read `.ocp` projects; that only holds if
none of them, including their dev-dependencies, depends on an AGPL crate.
"""

from __future__ import annotations

import json
import subprocess
import sys

APPLICATION = "AGPL-3.0-or-later"
PERMISSIVE = "MIT OR Apache-2.0"
PERMISSIVE_CRATES = {"oc-dsl", "oc-device-schema", "oc-lsp", "oc-test-support"}


def violations(packages: list[dict]) -> list[str]:
    by_name = {package["name"]: package for package in packages}
    errors = []
    for name, package in sorted(by_name.items()):
        expected = PERMISSIVE if name in PERMISSIVE_CRATES else APPLICATION
        if package.get("license") != expected:
            errors.append(f"{name}: license is {package.get('license')!r}, expected {expected!r}")
    for name in sorted(PERMISSIVE_CRATES & by_name.keys()):
        for dependency in by_name[name]["dependencies"]:
            target = dependency["name"]
            if target in by_name and target not in PERMISSIVE_CRATES:
                errors.append(f"{name}: permissive crate depends on {APPLICATION} crate {target}")
    missing = PERMISSIVE_CRATES - by_name.keys()
    errors.extend(f"{name}: listed as permissive but not a workspace crate" for name in sorted(missing))
    return errors


def main() -> int:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    )
    errors = violations(json.loads(result.stdout)["packages"])
    for error in errors:
        print(f"check-licenses: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"check-licenses: {len(PERMISSIVE_CRATES)} permissive crates, boundary clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
