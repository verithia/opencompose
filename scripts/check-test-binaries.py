#!/usr/bin/env python3
"""Check the integration-test binary budget for every workspace crate.

The default is one aggregate `all` binary per crate. Crates that need extra
process-isolated or allocation harnesses must list every allowed target here.
"""

from __future__ import annotations

import json
import subprocess
import sys

ALLOWED: dict[str, set[str]] = {
    "oc-synth": {
        "all",
        "drumkit_synth_alloc",
        "fx_parallel_alloc",
        "source_render_alloc",
    },
    "oc-host": {
        "all",
        "vst3_editor_lifecycle",
        "vst3_editor_probe",
        "vst3_rt_alloc",
    },
    "oc-synth-import": {"filter_off_import"},
}


def targets() -> dict[str, list[str]]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    )
    packages = json.loads(result.stdout)["packages"]
    return {
        package["name"]: [target["name"] for target in package["targets"] if "test" in target["kind"]]
        for package in packages
    }


def main() -> int:
    failures: list[str] = []
    for name, binaries in sorted(targets().items()):
        if not binaries:
            continue
        allowed = ALLOWED.get(name, {"all"})
        unexpected = sorted(set(binaries) - allowed)
        if unexpected:
            failures.append(
                f"{name}: unexpected integration test binary/binaries {unexpected}; allowed {sorted(allowed)}"
            )
        if name not in ALLOWED and len(binaries) != 1:
            failures.append(f"{name}: expected one integration test binary, found {sorted(binaries)}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("integration test binaries: within budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
