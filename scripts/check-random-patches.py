#!/usr/bin/env python3
"""Render seeded random patch variations and check the factory bounds.

The randomiser's safety set guarantees the parameters; this harness proves the
rendered result stays inside the same boundary set as the factory bank, using
`scripts/check-factory-presets.py` for the probe, the bounds and the meter:

    python3 scripts/check-random-patches.py --ocp target/debug/ocp --count 100

Each variation is seeded deterministically (`--seed + index`), so a failing row
can be reproduced by running `ocp randomize` with that seed.
"""

from __future__ import annotations

import argparse
import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CHECKS = REPO / "scripts/check-factory-presets.py"


def load_checks():
    """Load the factory checker as a module, with the sample tables."""
    spec = importlib.util.spec_from_file_location("ocp_factory_checks", CHECKS)
    module = importlib.util.module_from_spec(spec)
    # dataclasses resolve their module through sys.modules during execution.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def slug(path: Path) -> str:
    return path.stem


def render(project: Path, ocp: Path, seconds: float) -> Path:
    wav = project.with_suffix(".wav")
    command = [
        str(ocp),
        "render",
        str(project),
        "--output",
        str(wav),
        "--sample-rate",
        "48000",
        "--minimum-duration-seconds",
        str(seconds),
    ]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"{project.name}: ocp render failed\n{result.stdout}\n{result.stderr}")
    return wav


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ocp", default=str(REPO / "target/debug/ocp"), help="ocp binary")
    parser.add_argument("--count", type=int, default=100, help="variations to render")
    parser.add_argument("--seed", type=int, default=20261028, help="base seed")
    parser.add_argument("--keep", help="keep the generated .ocp and .wav files here")
    parser.add_argument("--only", action="append", default=[], help="restrict to these bases")
    parser.add_argument("--determinism", action="store_true", help="also prove same seed, same bytes")
    args = parser.parse_args()

    checks = load_checks()
    ocp = Path(args.ocp)
    if not ocp.exists():
        raise SystemExit(f"{ocp} does not exist; build with `cargo build -p oc-cli`")
    bases = checks.parse_presets(checks.PRESETS_TS)
    if args.only:
        bases = [preset for preset in bases if preset.name in args.only]
    if not bases:
        raise SystemExit("no factory preset matched")

    workdir = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="ocp-random-"))
    workdir.mkdir(parents=True, exist_ok=True)
    failures: list[str] = []
    rows: list[checks.Measurement] = []
    categories: dict[str, int] = {}
    seeds: dict[str, int] = {}

    for index in range(args.count):
        base = bases[index % len(bases)]
        seed = args.seed + index
        probe = workdir / f"probe-{index:03}.ocp"
        probe.write_text(checks.project_text(base), encoding="utf-8")
        outdir = workdir / f"variation-{index:03}"
        outdir.mkdir(exist_ok=True)
        result = subprocess.run(
            [
                str(ocp),
                "randomize",
                str(probe),
                "--count",
                "1",
                "--seed",
                str(seed),
                "--output",
                str(outdir),
            ],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            failures.append(f"{base.name} seed {seed}: randomize failed\n{result.stderr}")
            continue
        variants = sorted(outdir.glob("*-000.ocp"))
        if len(variants) != 1:
            failures.append(f"{base.name} seed {seed}: expected one variant, got {len(variants)}")
            continue
        if args.determinism and index < 3:
            again = outdir / "again"
            again.mkdir(exist_ok=True)
            subprocess.run(
                [str(ocp), "randomize", str(probe), "--count", "1", "--seed", str(seed), "--output", str(again)],
                check=True,
                capture_output=True,
                text=True,
            )
            mate = sorted(again.glob("*-000.ocp"))[0]
            if variants[0].read_bytes() != mate.read_bytes():
                failures.append(f"{base.name} seed {seed}: same seed produced different patches")

        wav = render(variants[0], ocp, checks.PROBE_SECONDS)
        label = checks.Preset(
            name=f"{base.name} #{index}",
            category=base.category,
            description=base.description,
            params=[],
        )
        measurement = checks.measure(label, wav)
        rows.append(measurement)
        categories[base.category] = categories.get(base.category, 0) + 1
        seeds[label.name] = seed
        bound_failures = checks.check([measurement])
        if bound_failures:
            failures.extend(f"{label.name} (seed {seed}): {failure}" for failure in bound_failures)
        print(
            f"{index:3} {base.name:16} {base.category:6} seed {seed:>10} "
            f"peak {measurement.peak_dbfs:7.2f}  rms {measurement.rms_dbfs:7.2f}  "
            f"crest {measurement.crest_db:5.2f}  centroid {measurement.centroid_hz:7.0f} Hz",
            flush=True,
        )

    if args.keep:
        print(f"\nkept: {workdir}")
    peaks = [row.peak_dbfs for row in rows if not row.nan]
    rms = [row.rms_dbfs for row in rows if not row.nan]
    nan = sum(1 for row in rows if row.nan)
    print(
        f"\n{len(rows)}/{args.count} rendered; {categories}; "
        f"peak {min(peaks):.2f}..{max(peaks):.2f} dBFS; "
        f"rms {min(rms):.2f}..{max(rms):.2f} dBFS; nan {nan}"
    )
    if failures:
        print(f"\n{len(failures)} failure(s):", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("every variation inside the factory boundary set")
    return 0


if __name__ == "__main__":
    sys.exit(main())
