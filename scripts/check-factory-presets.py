#!/usr/bin/env python3
"""Render every OpenSynth factory preset and check the bank's measurements.

The preset table lives in the GUI frontend, so this script reads it from
`crates/oc-gui/frontend/src/data/factoryPresets.ts`, writes one throwaway
project per preset, renders it through `ocp render`, and measures the result:

    peak dBFS, RMS dBFS, crest dB, DC offset, spectral centroid, NaN / clip.

The render probe is the audition figure (six eighths, 140 BPM) transposed into
the category's playing register (bass two octaves down), plus a release tail.

    python3 scripts/check-factory-presets.py --ocp target/debug/ocp
    python3 scripts/check-factory-presets.py --ocp ... --json /tmp/bank.json
    python3 scripts/check-factory-presets.py --ocp ... --no-check --only "Deep Anchor"
"""

from __future__ import annotations

import argparse
import json
import math
import re
import subprocess
import sys
import tempfile
import wave
from dataclasses import dataclass
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parent.parent
PRESETS_TS = REPO / "crates/oc-gui/frontend/src/data/factoryPresets.ts"

CATEGORIES = ("bass", "lead", "pad", "pluck", "keys", "brass", "fx")

# The audition figure: six ascending eighths at 140 BPM, one second of tail.
TEMPO = 140
PHRASE_NOTES = (60, 62, 64, 67, 69, 72)
BASS_NOTES = (36, 38, 40, 43, 45, 48)
DYNAMICS = ("mf", "f", "mf", "f", "mf", "ff")
PROBE_SECONDS = 2.4
SAMPLE_RATE = 48_000

# 16-bit full scale, used to spot samples that reached the file ceiling.
CEILING = 32767.0 / 32768.0

# Every bound the bank must hold. Bounds are per category; the bank-wide RMS
# spread keeps browsing from jumping in loudness between presets. Keep these in
# sync with the windows documented in docs/factory-presets.md.
BOUNDS = {
    "peak_max_dbfs": -1.0,
    "peak_min_dbfs": -24.0,
    "dc_max_abs": 0.005,
    "rms_span_db": 10.0,
}
CATEGORY_BOUNDS = {
    # category: (rms_min, rms_max, crest_min, crest_max, centroid_min, centroid_max)
    "bass": (-25.0, -18.0, 4.0, 21.0, 40.0, 2000.0),
    "lead": (-27.0, -19.5, 9.0, 23.0, 800.0, 9500.0),
    "pad": (-29.0, -23.5, 9.0, 20.0, 200.0, 5500.0),
    "pluck": (-25.0, -19.0, 11.0, 22.0, 500.0, 12000.0),
    "keys": (-25.0, -19.0, 10.0, 19.0, 500.0, 5500.0),
    "brass": (-25.0, -20.0, 11.0, 21.0, 800.0, 4200.0),
    "fx": (-25.0, -19.0, 7.0, 21.0, 300.0, 4000.0),
}


@dataclass
class Preset:
    name: str
    category: str
    description: str
    params: list[tuple[str, str | None]]


@dataclass
class Measurement:
    preset: Preset
    peak_dbfs: float
    rms_dbfs: float
    crest_db: float
    dc_abs: float
    centroid_hz: float
    clipped_samples: int
    nan: bool

    def row(self) -> dict:
        return {
            "name": self.preset.name,
            "category": self.preset.category,
            "description": self.preset.description,
            "peak_dbfs": round(self.peak_dbfs, 2),
            "rms_dbfs": round(self.rms_dbfs, 2),
            "crest_db": round(self.crest_db, 2),
            "dc_dbfs": round(20.0 * math.log10(self.dc_abs + 1e-12), 1),
            "centroid_hz": round(self.centroid_hz),
            "clipped_samples": self.clipped_samples,
            "nan": self.nan,
        }


def parse_presets(path: Path) -> list[Preset]:
    """Read the TS literal: name/category/description plus `["key", "value"]` rows."""
    source = path.read_text(encoding="utf-8")
    start = source.index("export const EXTRA_PRESETS")
    body = source[start:]
    presets: list[Preset] = []
    for block in re.split(r"\n  \{\n", body)[1:]:
        name = re.search(r'\bname: "([^"]+)"', block)
        category = re.search(r'\bcategory: "([^"]+)"', block)
        description = re.search(r'\bdescription: "([^"]+)"', block)
        if not (name and category and description):
            continue
        params: list[tuple[str, str | None]] = []
        for match in re.finditer(r'\["([^"]+)",\s*(?:"([^"]*)"|null)\]', block):
            params.append((match.group(1), match.group(2)))
        presets.append(
            Preset(name=name.group(1), category=category.group(1), description=description.group(1), params=params)
        )
    # A block that loses its `name:` line would otherwise vanish silently.
    declared = len(re.findall(r'^    name: "', body, flags=re.MULTILINE))
    if declared != len(presets):
        raise SystemExit(f"{path}: parsed {len(presets)} of {declared} presets")
    return presets


def note_name(midi: int) -> str:
    names = ("C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B")
    return f"{names[midi % 12]}{midi // 12 - 1}"


def project_text(preset: Preset) -> str:
    notes = BASS_NOTES if preset.category == "bass" else PHRASE_NOTES
    lines = [
        '@ocp "0.2.0"',
        "",
        f'@project "Factory probe {preset.name}"',
        f"  tempo: {TEMPO}",
        "  time_signature: 4/4",
        "",
        "@preset probe",
        "  source: builtin",
        '  plugin: "builtin.open_synth"',
    ]
    pairs = [f"{key}: {value}" for key, value in preset.params if value is not None]
    # Keep the params line inside a single `params: { }` map.
    lines.append("  params: {" + ", ".join(pairs) + "}")
    lines += [
        "",
        "@track probe | preset: probe, instrument: synth/virtual_analog",
        "  mix: vol=0dB, pan=0",
        "",
        "  @phrase figure [1:1:0 - 2:4:0]",
    ]
    lines += [f"    {note_name(note)} 8th {dynamic}" for note, dynamic in zip(notes, DYNAMICS)]
    return "\n".join(lines) + "\n"


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as handle:
        channels = handle.getnchannels()
        width = handle.getsampwidth()
        rate = handle.getframerate()
        frames = handle.readframes(handle.getnframes())
    if width != 2:
        raise SystemExit(f"{path}: expected 16-bit PCM, got {width * 8}-bit")
    data = np.frombuffer(frames, dtype="<i2").astype(np.float64) / 32768.0
    data = data.reshape(-1, channels)
    return data, rate


FRAME = 1024
HOP = 512


def frames(data: np.ndarray, frame: int = FRAME, hop: int = HOP):
    mono = data.mean(axis=1)
    window = np.hanning(frame)
    for offset in range(0, max(len(mono) - frame, 0) + 1, hop):
        block = mono[offset : offset + frame]
        yield block, np.abs(np.fft.rfft(block * window))


def frames_mono(data: np.ndarray) -> np.ndarray:
    return data.mean(axis=1)


def measure(preset: Preset, path: Path, active_db: float = -40.0) -> Measurement:
    """Peak over the whole render; RMS, crest and centroid over the sounding frames.

    The phrase has gaps and a tail, so a plain full-file RMS would report the
    silence as part of the level. Frames within `active_db` of the loudest
    frame are the sounding part.
    """
    data, rate = read_wav(path)
    if data.size == 0:
        raise SystemExit(f"{preset.name}: empty render")
    nan = bool(np.isnan(data).any() or np.isinf(data).any())
    if nan:
        return Measurement(preset, -160.0, -160.0, 0.0, 0.0, 0.0, 0, True)
    peak = float(np.max(np.abs(data)))
    peak_dbfs = 20.0 * math.log10(max(peak, 1e-12))
    clipped = int(np.count_nonzero(np.abs(data) >= CEILING))

    magnitudes = []
    frame_rms = []
    for block, magnitude in frames(data):
        frame_rms.append(float(np.sqrt(np.mean(block * block))))
        magnitudes.append(magnitude)
    frame_rms = np.array(frame_rms)
    loudest = float(frame_rms.max()) if frame_rms.size else 0.0
    active = frame_rms >= loudest * 10.0 ** (active_db / 20.0)
    if not active.any():
        active[:] = True
    rms = math.sqrt(float(np.mean(frame_rms[active] ** 2)))
    rms_dbfs = 20.0 * math.log10(max(rms, 1e-12))
    # A DC offset only matters while the patch sounds. Averaging over the whole
    # sounding span cancels the fundamental instead of the offset.
    shaped = frames_mono(data)
    first = int(np.flatnonzero(active)[0]) * HOP
    last = min(int(np.flatnonzero(active)[-1]) * HOP + FRAME, shaped.size)
    dc_abs = abs(float(np.mean(shaped[first:last])))
    freqs = np.fft.rfftfreq(FRAME, 1.0 / rate)
    weighted = sum(float(np.dot(freqs, magnitudes[index])) for index in np.flatnonzero(active))
    total = sum(float(magnitudes[index].sum()) for index in np.flatnonzero(active))
    centroid = weighted / total if total > 0.0 else 0.0
    return Measurement(preset, peak_dbfs, rms_dbfs, peak_dbfs - rms_dbfs, dc_abs, centroid, clipped, False)


def render(preset: Preset, ocp: Path, workdir: Path, seconds: float) -> Path:
    directory = workdir / re.sub(r"[^a-z0-9]+", "-", preset.name.lower())
    directory.mkdir(parents=True, exist_ok=True)
    project = directory / "probe.ocp"
    wav = directory / "probe.wav"
    project.write_text(project_text(preset), encoding="utf-8")
    command = [
        str(ocp),
        "render",
        str(project),
        "--output",
        str(wav),
        "--sample-rate",
        str(SAMPLE_RATE),
        "--minimum-duration-seconds",
        str(seconds),
    ]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"{preset.name}: ocp render failed\n{result.stdout}\n{result.stderr}")
    return wav


def check(rows: list[Measurement]) -> list[str]:
    failures: list[str] = []
    for row in rows:
        name = row.preset.name
        low, high, crest_low, crest_high, centroid_low, centroid_high = CATEGORY_BOUNDS[row.preset.category]
        if row.nan:
            failures.append(f"{name}: NaN or Inf in the render")
            continue
        if row.peak_dbfs > BOUNDS["peak_max_dbfs"]:
            failures.append(f"{name}: peak {row.peak_dbfs:.2f} dBFS above {BOUNDS['peak_max_dbfs']} dBFS")
        if row.peak_dbfs < BOUNDS["peak_min_dbfs"]:
            failures.append(f"{name}: peak {row.peak_dbfs:.2f} dBFS below {BOUNDS['peak_min_dbfs']} dBFS")
        if row.clipped_samples:
            failures.append(f"{name}: {row.clipped_samples} samples at full scale")
        if row.dc_abs > BOUNDS["dc_max_abs"]:
            failures.append(f"{name}: DC offset {row.dc_abs:.4f} above {BOUNDS['dc_max_abs']}")
        if not low <= row.rms_dbfs <= high:
            failures.append(f"{name}: RMS {row.rms_dbfs:.2f} dBFS outside [{low}, {high}]")
        if not crest_low <= row.crest_db <= crest_high:
            failures.append(f"{name}: crest {row.crest_db:.2f} dB outside [{crest_low}, {crest_high}]")
        if not centroid_low <= row.centroid_hz <= centroid_high:
            failures.append(f"{name}: centroid {row.centroid_hz:.0f} Hz outside [{centroid_low}, {centroid_high}]")
    rms_values = [row.rms_dbfs for row in rows if not row.nan]
    if rms_values and max(rms_values) - min(rms_values) > BOUNDS["rms_span_db"]:
        failures.append(f"bank RMS spread {max(rms_values) - min(rms_values):.1f} dB above {BOUNDS['rms_span_db']}")
    return failures


def markdown(rows: list[Measurement]) -> str:
    lines = [
        "| Preset | Category | Peak dBFS | RMS dBFS | Crest dB | DC dBFS | Centroid Hz |",
        "|---|---|---|---|---|---|---|",
    ]
    for row in rows:
        lines.append(
            f"| {row.preset.name} | {row.preset.category} | {row.peak_dbfs:.2f} | {row.rms_dbfs:.2f} | "
            f"{row.crest_db:.2f} | {20.0 * math.log10(row.dc_abs + 1e-12):.1f} | {row.centroid_hz:.0f} |"
        )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ocp", default=str(REPO / "target/debug/ocp"), help="ocp binary to render with")
    parser.add_argument("--presets", default=str(PRESETS_TS), help="factoryPresets.ts")
    parser.add_argument("--json", help="write the measurement table here")
    parser.add_argument("--markdown", action="store_true", help="print the table as markdown")
    parser.add_argument("--only", action="append", default=[], help="restrict to these presets")
    parser.add_argument("--seconds", type=float, default=PROBE_SECONDS, help="minimum rendered length")
    parser.add_argument("--no-check", action="store_true", help="measure without enforcing the bounds")
    parser.add_argument("--keep", help="keep the rendered projects in this directory")
    args = parser.parse_args()

    presets = parse_presets(Path(args.presets))
    if not presets:
        raise SystemExit(f"no presets parsed from {args.presets}")
    selected = [preset for preset in presets if not args.only or preset.name in args.only]
    if not selected:
        raise SystemExit("no preset matched --only")
    ocp = Path(args.ocp)
    if not ocp.exists():
        raise SystemExit(f"{ocp} does not exist; build with `cargo build -p oc-cli`")

    with tempfile.TemporaryDirectory(prefix="ocp-presets-") as scratch:
        workdir = Path(args.keep) if args.keep else Path(scratch)
        rows: list[Measurement] = []
        for preset in selected:
            wav = render(preset, ocp, workdir, args.seconds)
            rows.append(measure(preset, wav))
            row = rows[-1]
            print(
                f"{preset.name:22} {preset.category:6} peak {row.peak_dbfs:7.2f}  rms {row.rms_dbfs:7.2f}  "
                f"crest {row.crest_db:5.2f}  centroid {row.centroid_hz:7.0f} Hz",
                flush=True,
            )

    if args.json:
        Path(args.json).write_text(json.dumps([row.row() for row in rows], indent=2) + "\n", encoding="utf-8")
    if args.markdown:
        print()
        print(markdown(rows))
    if args.no_check:
        return 0
    failures = check(rows)
    if failures:
        print("\nfailed bounds:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(f"\n{len(rows)} presets inside every bound")
    return 0


if __name__ == "__main__":
    sys.exit(main())
