#!/usr/bin/env python3
"""Corpus-backed regression evaluation for `ocp vocal analyze`.

Runs our analyzer on real, licensed singing corpora, compares its f0 grid and
note segmentation against the corpus annotation, and caches one JSON per clip
so a re-run over thousands of clips is resumable.

The corpora live outside this repository (they are large and separately
licensed); `scripts/vocal/README.md` has the manifest, the measured table and the
gate policy.

Usage:

    scripts/vocal/corpus_eval.py --corpus vocadito
    scripts/vocal/corpus_eval.py --corpus all --json-out results.json
    scripts/vocal/corpus_eval.py --gate

Per-clip f0 percentiles are computed inside the clip; corpus/group rows report
the median of those per-clip values (robust to clip length) and pooled frame
counts for rates.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import soundfile as sf

HARNESS_VERSION = 5

# The corpora are separately licensed, so they live in the ignored `private/`
# directory unless the environment points elsewhere.
DEFAULT_CORPUS_DIR = Path(
    os.environ.get(
        "OCP_VOCAL_CORPUS_DIR",
        Path(__file__).resolve().parents[2] / "private" / "corpus" / "vocal",
    )
)

# Vocadito f0 annotations are on an FFT hop grid, not on our 5 ms grid.
MATCH_TOLERANCE_S = 0.010
BOUNDARY_MAX_S = 0.150
BOUNDARY_HIT_S = 0.050
MISSED_OVERLAP_FRACTION = 0.40
PARTIAL_OVERLAP_FRACTION = 0.30
MIN_PARTIAL_OVERLAP_S = 0.020
SHORT_NOTE_S = 0.100
STEADY_GUARD_S = 0.030
OCTAVE_CENTS = 600.0


@dataclass
class Clip:
    corpus: str
    clip_id: str
    audio: Path
    groups: dict[str, str]
    f0_times: np.ndarray
    f0_hz: np.ndarray
    notes: list[tuple[float, float, float]] = field(default_factory=list)
    variant: str = ""
    # Our time t maps to the annotation at t / time_scale (1.0 = same clock).
    time_scale: float = 1.0


def resolve_ocp(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.is_file():
            raise SystemExit(f"ocp binary not found: {path}")
        return path
    candidates = []
    if os.environ.get("OCP_BIN"):
        candidates.append(Path(os.environ["OCP_BIN"]).expanduser())
    target = os.environ.get("CARGO_TARGET_DIR")
    if target:
        candidates.append(Path(target).expanduser() / "release" / "ocp")
    candidates.append(Path(__file__).resolve().parents[2] / "target" / "release" / "ocp")
    for path in candidates:
        if path.is_file():
            return path
    raise SystemExit("ocp binary not found; build it with `cargo build --release -p oc-cli` or pass --ocp")


# --------------------------------------------------------------------------
# Corpus adapters


def read_csv_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        reader = csv.reader(handle)
        header = [cell.strip() for cell in next(reader)]
        rows = []
        for row in reader:
            if len(row) != len(header):
                row = row + [""] * (len(header) - len(row))
            rows.append({key: value.strip() for key, value in zip(header, row)})
    return rows


def read_f0_csv(path: Path) -> tuple[np.ndarray, np.ndarray]:
    times: list[float] = []
    values: list[float] = []
    with path.open(newline="") as handle:
        for row in csv.reader(handle):
            if len(row) < 2:
                continue
            try:
                times.append(float(row[0]))
                values.append(float(row[1]))
            except ValueError:
                continue
    return np.asarray(times), np.asarray(values)


def read_notes_csv(path: Path) -> list[tuple[float, float, float]]:
    notes = []
    with path.open(newline="") as handle:
        for row in csv.reader(handle):
            if len(row) < 3:
                continue
            try:
                start = float(row[0])
                f0 = float(row[1])
                duration = float(row[2])
            except ValueError:
                continue
            if f0 > 0.0 and duration > 0.0:
                notes.append((start, start + duration, f0))
    return notes


def load_vocadito(corpus_dir: Path, annotator: str) -> list[Clip]:
    root = corpus_dir / "vocadito"
    if not root.is_dir():
        raise SystemExit(f"Vocadito not found at {root}; see scripts/vocal/README.md")
    metadata: dict[str, dict[str, str]] = {}
    meta_path = root / "vocadito_metadata.csv"
    if meta_path.is_file():
        for row in read_csv_rows(meta_path):
            metadata[row["track_id"]] = row
    clips = []
    for audio in sorted(
        (root / "Audio").glob("vocadito_*.wav"),
        key=lambda path: int(path.stem.split("_")[1]),
    ):
        track = audio.stem.split("_")[1]
        f0_path = root / "Annotations" / "F0" / f"{audio.stem}_f0.csv"
        notes_path = root / "Annotations" / "Notes" / f"{audio.stem}_notes{annotator}.csv"
        if not f0_path.is_file() or not notes_path.is_file():
            continue
        meta = metadata.get(track, {})
        times, values = read_f0_csv(f0_path)
        clips.append(
            Clip(
                corpus="vocadito",
                clip_id=audio.stem,
                audio=audio,
                groups={
                    "singer": meta.get("singer_id", ""),
                    "language": meta.get("language", ""),
                },
                f0_times=times,
                f0_hz=values,
                notes=read_notes_csv(notes_path),
                variant=annotator,
            )
        )
    return clips


def vocalset_version(annotations: Path, family: str, requested: str | None) -> str:
    if requested:
        return requested
    versions = sorted(
        (path.name for path in annotations.glob(f"{family} *") if path.is_dir()),
        key=lambda name: int(name.split()[-1]),
    )
    if not versions:
        raise SystemExit(f"no '{family} N' annotation directories under {annotations}")
    return versions[-1]


def load_vocalset(corpus_dir: Path, requested_extended: str | None, requested_raw: str | None) -> list[Clip]:
    root = corpus_dir / "vocalset"
    annotations = corpus_dir / "vocalset_annotations"
    if not root.is_dir():
        raise SystemExit(f"VocalSet not found at {root}; see scripts/vocal/README.md")
    if not annotations.is_dir():
        raise SystemExit(f"Annotated-VocalSet not found at {annotations}")
    extended = vocalset_version(annotations, "extended", requested_extended)
    raw = vocalset_version(annotations, "raw", requested_raw)
    notes_by_file: dict[str, list[tuple[float, float, float]]] = {}
    groups_by_file: dict[str, dict[str, str]] = {}
    for row in read_csv_rows(annotations / extended / "all files.csv"):
        name = row["File Name"]
        groups_by_file[name] = {
            "singer": row["Singer Name"],
            "technique": row["Technique"],
            "gender": row["Gender"],
            "vowel": row["Vowel"],
            "music_type": row["Music Type"],
        }
        if row["Type"] != "Sound":
            continue
        try:
            start = float(row["Start time"])
            end = float(row["End time"])
            f0 = float(row["Median F0"])
        except ValueError:
            continue
        if end > start and f0 > 0.0:
            notes_by_file.setdefault(name, []).append((start, end, f0))

    audio_by_stem = {path.stem: path for path in root.rglob("*.wav")}
    clips = []
    for contour in sorted((annotations / raw / "csv").rglob("*.csv")):
        stem = contour.stem
        audio = audio_by_stem.get(stem)
        notes = notes_by_file.get(stem)
        if audio is None or not notes:
            continue
        times, values = read_f0_csv(contour)
        if values.size == 0 or not np.any(values > 0):
            continue
        clips.append(
            Clip(
                corpus="vocalset",
                clip_id=stem,
                audio=audio,
                groups=groups_by_file[stem],
                f0_times=times,
                f0_hz=values,
                notes=notes,
                variant=f"{extended}/{raw}",
            )
        )
    return clips


def load_time_scale_spec(path: Path) -> dict[str, float]:
    """Singer -> audio/annotation duration ratio; empty when the file is absent."""
    if not path.is_file():
        return {}
    payload = json.loads(path.read_text())
    return {str(key): float(value) for key, value in payload.get("audio_duration_ratio", {}).items()}


def apply_time_scales(clips: list[Clip], ratios: dict[str, float]) -> None:
    for clip in clips:
        ratio = ratios.get(clip.groups.get("singer", ""), 1.0)
        clip.time_scale = ratio if ratio > 0.0 else 1.0


def load_corpus(corpus_dir: Path, name: str, args: argparse.Namespace) -> list[Clip]:
    if name == "vocadito":
        return load_vocadito(corpus_dir, args.vocadito_annotator)
    if name == "vocalset":
        clips = load_vocalset(corpus_dir, args.vocalset_extended, args.vocalset_raw)
        if args.techniques:
            keep = {item.strip() for item in args.techniques.split(",") if item.strip()}
            clips = [clip for clip in clips if clip.groups["technique"] in keep]
        return clips
    raise SystemExit(f"unknown corpus '{name}'")


# --------------------------------------------------------------------------
# Analyzer invocation


def project_text(audio: Path, duration: float, sample_rate: int) -> str:
    # One bar is two seconds at 120 BPM; round up and add a bar of slack. The
    # engine trims the window to the media length.
    bars = max(1, math.ceil(duration / 2.0)) + 1
    path = str(audio).replace("\\", "\\\\").replace('"', '\\"')
    return (
        '@project "corpus-eval"\n'
        "  tempo: 120\n"
        f"@audio_clip clip [1:1 - {bars}:1] | track: vocal\n"
        f'  file: "{path}"\n'
        "@track vocal | instrument: audio/clip\n"
        "  mix: vol=0dB\n"
    )


def analyze(ocp: Path, clip: Clip, work_dir: Path, media_cache: Path, derived_cache: Path, sample_rate: int) -> dict:
    info = sf.info(str(clip.audio))
    duration = info.frames / info.samplerate
    project = work_dir / f"{clip.corpus}_{clip.clip_id}.ocp"
    project.write_text(project_text(clip.audio, duration, sample_rate))
    env = dict(os.environ)
    env["OCP_MEDIA_CACHE_DIR"] = str(media_cache)
    env["OCP_DERIVED_CACHE_DIR"] = str(derived_cache)
    result = subprocess.run(
        [
            str(ocp),
            "vocal",
            "analyze",
            str(project),
            "clip",
            "--json",
            "--sample-rate",
            str(sample_rate),
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    if result.returncode != 0:
        raise RuntimeError(f"ocp failed on {clip.clip_id}: {result.stderr.strip()}")
    report = json.loads(result.stdout)
    return {
        "duration_s": report["duration_seconds"],
        "frames": report["frames"],
        "notes": report["notes"],
        "analysis_rate_hz": report["analysis_rate_hz"],
        "hop_seconds": report["hop_seconds"],
        "analyzer_version": report["analyzer_version"],
    }


# --------------------------------------------------------------------------
# Comparison


def nearest(values: np.ndarray, times: np.ndarray) -> np.ndarray:
    """Index of the nearest reference sample for every query time."""
    if values.size == 0:
        return np.zeros(times.shape, dtype=int)
    idx = np.searchsorted(values, times)
    idx = np.clip(idx, 1, values.size - 1)
    left = values[idx - 1]
    right = values[idx]
    return np.where(np.abs(left - times) <= np.abs(right - times), idx - 1, idx)


def boundary_errors(ref_times: list[float], our_times: list[float]) -> tuple[list[float], int]:
    errors = []
    unmatched = 0
    if not our_times:
        return errors, len(ref_times)
    ours = np.asarray(our_times)
    for value in ref_times:
        index = int(np.argmin(np.abs(ours - value)))
        delta = abs(float(ours[index]) - value)
        if delta <= BOUNDARY_MAX_S:
            errors.append(delta * 1000.0)
        else:
            unmatched += 1
    return errors, unmatched


def boundary_source_of(args: argparse.Namespace) -> str:
    """Which note edges the boundary metrics read.

    `detected` uses the analyser's onset/offset, the event the tuner anchors to.
    `reported` uses the frame-grid span the project and the note grid read, so
    the two can be compared side by side.
    """
    return getattr(args, "boundary_source", "detected")


def compare(clip: Clip, analysis: dict, time_scale: float = 1.0, boundary_source: str = "detected") -> dict:
    frames = analysis["frames"]
    times = np.asarray([frame["time_seconds"] for frame in frames], dtype=float)
    f0 = np.asarray([frame["f0_hz"] for frame in frames], dtype=float)
    voiced = np.asarray([bool(frame["voiced"]) for frame in frames], dtype=bool)

    ref_times = clip.f0_times
    ref_f0 = clip.f0_hz
    idx = nearest(ref_times, times / time_scale)
    ref_at = ref_f0[idx] if ref_f0.size else np.zeros(times.shape)
    ref_voiced = ref_at > 0.0
    matched = voiced & ref_voiced

    metrics: dict[str, float | int] = {
        "frames": int(times.size),
        "voiced_frames": int(voiced.sum()),
        "ref_voiced_frames": int(ref_voiced.sum()),
        "matched_frames": int(matched.sum()),
        "ref_unvoiced_frames": int((~ref_voiced).sum()),
        "false_voiced_frames": int((voiced & ~ref_voiced).sum()),
    }

    if matched.sum() >= 5:
        cents = 1200.0 * np.log2(f0[matched] / ref_at[matched])
        folded = cents - 1200.0 * np.round(cents / 1200.0)
        metrics.update(
            f0_median_abs_c=float(np.median(np.abs(cents))),
            f0_p95_abs_c=float(np.percentile(np.abs(cents), 95)),
            f0_median_c=float(np.median(cents)),
            octave_frames=int((np.abs(cents) > OCTAVE_CENTS).sum()),
            octave_up_frames=int((cents > OCTAVE_CENTS).sum()),
            octave_down_frames=int((cents < -OCTAVE_CENTS).sum()),
            folded_p95_abs_c=float(np.percentile(np.abs(folded), 95)),
        )
        steady = np.ones(times.shape, dtype=bool)
        for start, end, _ in clip.notes:
            near_start = (times >= start - STEADY_GUARD_S) & (times <= start + STEADY_GUARD_S)
            near_end = (times >= end - STEADY_GUARD_S) & (times <= end + STEADY_GUARD_S)
            steady &= ~(near_start | near_end)
        steady &= matched
        if steady.sum() >= 5:
            steady_cents = 1200.0 * np.log2(f0[steady] / ref_at[steady])
            metrics["f0_steady_p95_abs_c"] = float(np.percentile(np.abs(steady_cents), 95))
            metrics["f0_steady_median_abs_c"] = float(np.median(np.abs(steady_cents)))
        else:
            metrics["f0_steady_p95_abs_c"] = float("nan")
            metrics["f0_steady_median_abs_c"] = float("nan")
    else:
        metrics.update(
            f0_median_abs_c=float("nan"),
            f0_p95_abs_c=float("nan"),
            f0_median_c=float("nan"),
            octave_frames=0,
            octave_up_frames=0,
            octave_down_frames=0,
            folded_p95_abs_c=float("nan"),
            f0_steady_p95_abs_c=float("nan"),
            f0_steady_median_abs_c=float("nan"),
        )

    metrics["voiced_recall"] = (
        metrics["matched_frames"] / metrics["ref_voiced_frames"] if metrics["ref_voiced_frames"] else float("nan")
    )
    metrics["false_voiced_rate"] = (
        metrics["false_voiced_frames"] / metrics["ref_unvoiced_frames"]
        if metrics["ref_unvoiced_frames"]
        else float("nan")
    )
    metrics["octave_rate"] = (
        metrics["octave_frames"] / metrics["matched_frames"] if metrics["matched_frames"] else float("nan")
    )

    our_notes = [
        (note["start_seconds"] / time_scale, note["end_seconds"] / time_scale, note["f0_median_hz"])
        for note in analysis["notes"]
    ]
    # The boundary metrics read the detected edges: the moment the note sounds,
    # which the frame grid cannot express. The note-shape metrics below read the
    # reported span, the timing the project and the note grid use.
    if boundary_source == "reported":
        our_edges = [(note[0], note[1]) for note in our_notes]
    else:
        our_edges = [
            (
                note.get("onset_seconds", note["start_seconds"]) / time_scale,
                note.get("offset_seconds", note["end_seconds"]) / time_scale,
            )
            for note in analysis["notes"]
        ]
    ref_notes = [(start, end, f0) for start, end, f0 in clip.notes if end - start >= 0.020]
    metrics["ref_notes"] = len(ref_notes)
    metrics["our_notes"] = len(our_notes)
    metrics["ref_short_notes"] = sum(1 for start, end, _ in ref_notes if end - start < SHORT_NOTE_S)
    metrics["note_ratio"] = len(our_notes) / len(ref_notes) if ref_notes else float("nan")

    start_errors, start_missed = boundary_errors([note[0] for note in ref_notes], [note[0] for note in our_edges])
    end_errors, end_missed = boundary_errors([note[1] for note in ref_notes], [note[1] for note in our_edges])
    errors = start_errors + end_errors
    total_boundaries = 2 * len(ref_notes)
    metrics["boundary_p90_ms"] = float(np.percentile(errors, 90)) if errors else float("nan")
    metrics["boundary_hit_rate"] = len(errors) / total_boundaries if total_boundaries else float("nan")
    metrics["boundary_errors_ms"] = [round(value, 1) for value in errors]  # for debugging

    # The mirror of boundary_hit_rate: boundaries we report that the annotation
    # does not have (a cut inside a sustained note, or a spurious edge).
    our_boundaries = [note[0] for note in our_edges] + [note[1] for note in our_edges]
    if ref_notes:
        ref_boundary_times = np.asarray([note[0] for note in ref_notes] + [note[1] for note in ref_notes])
        false_boundaries = sum(
            1 for value in our_boundaries if np.min(np.abs(ref_boundary_times - value)) > BOUNDARY_HIT_S
        )
    else:
        false_boundaries = 0
    metrics["our_boundary_total"] = len(our_boundaries)
    metrics["false_boundaries"] = false_boundaries
    metrics["false_boundary_rate"] = false_boundaries / len(our_boundaries) if our_boundaries else float("nan")

    missed = 0
    split = 0
    merged = 0
    note_cents: list[float] = []
    for ref_start, ref_end, ref_note_f0 in ref_notes:
        ref_len = ref_end - ref_start
        best = None
        best_overlap = 0.0
        pieces = 0
        for our_start, our_end, our_f0 in our_notes:
            overlap = min(our_end, ref_end) - max(our_start, ref_start)
            if overlap <= 0.0:
                continue
            if overlap >= max(MIN_PARTIAL_OVERLAP_S, PARTIAL_OVERLAP_FRACTION * ref_len):
                pieces += 1
            if overlap > best_overlap:
                best_overlap = overlap
                best = (our_start, our_end, our_f0)
        if best_overlap < max(MIN_PARTIAL_OVERLAP_S, MISSED_OVERLAP_FRACTION * ref_len):
            missed += 1
            continue
        if pieces >= 2:
            split += 1
        if best is not None and ref_note_f0 > 0:
            note_cents.append(1200.0 * math.log2(best[2] / ref_note_f0))
    for our_start, our_end, _ in our_notes:
        covered = 0
        for ref_start, ref_end, _ in ref_notes:
            overlap = min(our_end, ref_end) - max(our_start, ref_start)
            ref_len = ref_end - ref_start
            if overlap >= max(MIN_PARTIAL_OVERLAP_S, PARTIAL_OVERLAP_FRACTION * ref_len):
                covered += 1
        if covered >= 2:
            merged += 1

    metrics["missed_notes"] = missed
    metrics["split_notes"] = split
    metrics["merged_notes"] = merged
    metrics["missed_rate"] = missed / len(ref_notes) if ref_notes else float("nan")
    metrics["split_rate"] = split / len(ref_notes) if ref_notes else float("nan")
    metrics["merged_rate"] = merged / len(our_notes) if our_notes else float("nan")
    if note_cents:
        abs_cents = np.abs(np.asarray(note_cents))
        metrics["note_f0_median_abs_c"] = float(np.median(abs_cents))
        metrics["note_octave_rate"] = float(np.mean(abs_cents > OCTAVE_CENTS))
        metrics["note_octave_count"] = int((abs_cents > OCTAVE_CENTS).sum())
        metrics["matched_note_count"] = len(note_cents)
    else:
        metrics["note_f0_median_abs_c"] = float("nan")
        metrics["note_octave_rate"] = float("nan")
        metrics["note_octave_count"] = 0
        metrics["matched_note_count"] = 0
    return metrics


# --------------------------------------------------------------------------
# Result cache


def fingerprint(clip: Clip, ocp: Path, sample_rate: int, boundary_source: str = "detected") -> dict:
    stat = clip.audio.stat()
    binary = ocp.stat()
    return {
        "harness": HARNESS_VERSION,
        "ocp_mtime_ns": binary.st_mtime_ns,
        "ocp_size": binary.st_size,
        "audio_size": stat.st_size,
        "audio_mtime_ns": stat.st_mtime_ns,
        "sample_rate": sample_rate,
        "boundary_source": boundary_source,
        "variant": clip.variant,
        "note_count": len(clip.notes),
    }


def cache_path(cache_dir: Path, corpus: str, clip_id: str, variant: str) -> Path:
    return cache_dir / "results" / corpus / (variant or "default") / f"{clip_id}.json"


def load_cached(path: Path, stamp: dict) -> dict | None:
    if not path.is_file():
        return None
    try:
        payload = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    if payload.get("fingerprint") != stamp:
        return None
    return payload


def store_cached(path: Path, stamp: dict, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps({"fingerprint": stamp, **payload}, indent=1))
    temporary.replace(path)


# --------------------------------------------------------------------------
# Aggregation and reporting


def safe_median(values: list[float]) -> float:
    values = [value for value in values if not math.isnan(value)]
    return float(np.median(values)) if values else float("nan")


def aggregate(rows: list[dict], prefix: str = "") -> dict:
    """Aggregate one metric view. `prefix` selects the as-stored view."""

    def key(name: str) -> str:
        return f"{prefix}{name}"

    def total(name: str) -> float:
        return float(sum(row[key(name)] for row in rows if row.get(key(name)) is not None))

    def ratio(numerator: str, denominator: str) -> float:
        bottom = total(denominator)
        return total(numerator) / bottom if bottom else float("nan")

    def median(name: str) -> float:
        return safe_median([row[key(name)] for row in rows])

    return {
        "clips": len(rows),
        "duration_s": float(sum(row["duration_s"] for row in rows)),
        "f0_median_abs_c": median("f0_median_abs_c"),
        "f0_p95_abs_c": median("f0_p95_abs_c"),
        "f0_steady_p95_abs_c": median("f0_steady_p95_abs_c"),
        "octave_rate": ratio("octave_frames", "matched_frames"),
        "octave_up_rate": ratio("octave_up_frames", "matched_frames"),
        "octave_down_rate": ratio("octave_down_frames", "matched_frames"),
        "voiced_recall": ratio("matched_frames", "ref_voiced_frames"),
        "false_voiced_rate": ratio("false_voiced_frames", "ref_unvoiced_frames"),
        "boundary_p90_ms": median("boundary_p90_ms"),
        "boundary_hit_rate": ratio("boundary_hits", "boundary_total"),
        "false_boundary_rate": ratio("false_boundaries", "our_boundary_total"),
        "note_ratio": ratio("our_notes", "ref_notes"),
        "missed_rate": ratio("missed_notes", "ref_notes"),
        "split_rate": ratio("split_notes", "ref_notes"),
        "merged_rate": ratio("merged_notes", "our_notes"),
        "note_f0_median_abs_c": median("note_f0_median_abs_c"),
        "note_octave_rate": ratio("note_octave_notes", "matched_notes"),
        "ref_short_note_share": ratio("ref_short_notes", "ref_notes"),
    }


def print_time_scale(clips: list[Clip], source: str) -> None:
    scaled = [clip for clip in clips if clip.time_scale != 1.0]
    if not scaled:
        return
    singers = sorted({clip.groups.get("singer", "?") for clip in scaled})
    factors = sorted({clip.time_scale for clip in scaled})
    print(
        f"time scale ({source}): {len(scaled)} clips of singer(s) {', '.join(singers)} "
        f"audio/annotation ratio {'/'.join(f'{value:.3f}' for value in factors)} "
        f"(reference sampled at {'/'.join(f'{1.0 / value:.2f}' for value in factors)}x)",
        file=sys.stderr,
    )


def print_correction_summary(rows: list[dict]) -> None:
    scaled = [row for row in rows if row.get("time_scale", 1.0) != 1.0]
    if not scaled:
        return
    views = [
        ("all clips, annotation as stored", rows, "raw_"),
        ("all clips, time scale applied", rows, ""),
        ("time-scale clips, as stored", scaled, "raw_"),
        ("time-scale clips, applied", scaled, ""),
    ]
    header = (
        f"{'view':<34} {'clips':>5} {'f0med':>6} {'f0p95':>6} {'oct%':>6} "
        f"{'recall':>6} {'bhit':>6} {'fbnd%':>6} {'nratio':>6}"
    )
    print("\nCorpus aggregate (see scripts/vocal/time_scale.json)")
    print(header)
    print("-" * len(header))
    for label, group, prefix in views:
        value = aggregate(group, prefix)
        print(
            f"{label:<34} {value['clips']:>5} {value['f0_median_abs_c']:>6.1f} "
            f"{value['f0_p95_abs_c']:>6.1f} {100 * value['octave_rate']:>6.2f} "
            f"{100 * value['voiced_recall']:>6.1f} {100 * value['boundary_hit_rate']:>6.1f} "
            f"{100 * value['false_boundary_rate']:>6.1f} {value['note_ratio']:>6.2f}"
        )


def group_rows(rows: list[dict], group: str) -> dict[str, dict]:
    buckets: dict[str, list[dict]] = {}
    for row in rows:
        key = row["groups"].get(group, "") if group != "corpus" else row["corpus"]
        buckets.setdefault(key or "(none)", []).append(row)
    return {key: aggregate(value) for key, value in sorted(buckets.items())}


def print_table(title: str, table: dict[str, dict], group: str) -> None:
    print(f"\n{title}")
    header = (
        f"{group:<16} {'clips':>5} {'min':>6} {'f0med':>6} {'f0p95':>6} {'steady':>6} "
        f"{'oct%':>6} {'recall':>6} {'falv%':>6} {'bnd90':>6} {'bhit':>6} "
        f"{'nratio':>6} {'miss%':>6} {'split%':>6} {'merge%':>6}"
    )
    print(header)
    print("-" * len(header))
    for key, row in table.items():
        print(
            f"{key:<16} {row['clips']:>5} {row['duration_s'] / 60.0:>6.1f} "
            f"{row['f0_median_abs_c']:>6.1f} {row['f0_p95_abs_c']:>6.1f} "
            f"{row['f0_steady_p95_abs_c']:>6.1f} "
            f"{100 * row['octave_rate']:>6.2f} {100 * row['voiced_recall']:>6.1f} "
            f"{100 * row['false_voiced_rate']:>6.1f} "
            f"{row['boundary_p90_ms']:>6.1f} {100 * row['boundary_hit_rate']:>6.1f} "
            f"{row['note_ratio']:>6.2f} {100 * row['missed_rate']:>6.1f} "
            f"{100 * row['split_rate']:>6.1f} {100 * row['merged_rate']:>6.1f}"
        )


# --------------------------------------------------------------------------
# Gate


def load_gate_clips(spec_path: Path, gate_dir: Path) -> list[Clip]:
    spec = json.loads(spec_path.read_text())
    clips = []
    for entry in spec["clips"]:
        audio = gate_dir / entry["audio"]
        if not audio.is_file():
            raise SystemExit(f"gate audio missing: {audio} (see scripts/vocal/README.md)")
        times, values = read_f0_csv(gate_dir / entry["f0"])
        clips.append(
            Clip(
                corpus="gate",
                clip_id=entry["id"],
                audio=audio,
                groups={"source": entry.get("corpus", "")},
                f0_times=times,
                f0_hz=values,
                notes=read_notes_csv(gate_dir / entry["notes"]),
                variant="gate",
            )
        )
    return clips


def run_gate(args: argparse.Namespace, ocp: Path, gate_dir: Path) -> int:
    spec_path = Path(args.gate_spec).expanduser() if args.gate_spec else Path(__file__).with_name("gate.json")
    if not spec_path.is_file():
        raise SystemExit(f"gate spec not found: {spec_path}")
    spec = json.loads(spec_path.read_text())
    clips = load_gate_clips(spec_path, gate_dir)
    rows = evaluate(args, ocp, clips)
    summary = aggregate(rows)
    thresholds = spec["thresholds"]

    checks: list[tuple[str, float, str, bool]] = []

    def at_most(key: str, limit: float, label: str | None = None) -> None:
        value = summary[key]
        checks.append((label or key, value, f"<= {limit}", value <= limit))

    def at_least(key: str, limit: float, label: str | None = None) -> None:
        value = summary[key]
        checks.append((label or key, value, f">= {limit}", value >= limit))

    at_most("f0_median_abs_c", thresholds["f0_median_abs_c_max"])
    at_most("f0_p95_abs_c", thresholds["f0_p95_abs_c_max"])
    at_most("octave_rate", thresholds["octave_rate_max"])
    at_most("boundary_p90_ms", thresholds["boundary_p90_ms_max"])
    at_least("boundary_hit_rate", thresholds["boundary_hit_rate_min"])
    at_most("missed_rate", thresholds["missed_rate_max"])
    at_most("split_rate", thresholds["split_rate_max"])
    at_most("false_voiced_rate", thresholds["false_voiced_rate_max"])
    at_least("voiced_recall", thresholds["voiced_recall_min"])
    at_least("note_ratio", thresholds["note_ratio_min"], "note_ratio min")
    at_most("note_ratio", thresholds["note_ratio_max"], "note_ratio max")

    worst_p95 = max(row["f0_p95_abs_c"] for row in rows)
    worst_octave = max(row["octave_rate"] for row in rows)
    checks.append(
        (
            "worst clip f0 p95",
            worst_p95,
            f"<= {thresholds['per_clip_f0_p95_abs_c_max']}",
            worst_p95 <= thresholds["per_clip_f0_p95_abs_c_max"],
        )
    )
    checks.append(
        (
            "worst clip octave",
            worst_octave,
            f"<= {thresholds['per_clip_octave_rate_max']}",
            worst_octave <= thresholds["per_clip_octave_rate_max"],
        )
    )

    print(f"\nVocal corpus gate v{spec.get('version', 1)}: {len(clips)} clips, {summary['duration_s']:.1f} s")
    top = f"{'metric':<22} {'measured':>10}  {'limit':<12} result"
    print(top)
    print("-" * len(top))
    failures = 0
    for name, value, limit, ok in checks:
        failures += 0 if ok else 1
        print(f"{name:<22} {value:>10.3f}  {limit:<12} {'PASS' if ok else 'FAIL'}")
    verdict = "PASS" if failures == 0 else "FAIL"
    detail = f"{failures} of {len(checks)} checks failed" if failures else f"all {len(checks)} checks"
    print(f"\n{verdict}: {detail}")
    return 0 if failures == 0 else 1


# --------------------------------------------------------------------------
# Driver


def evaluate(args: argparse.Namespace, ocp: Path, clips: list[Clip]) -> list[dict]:
    cache_dir = Path(args.cache_dir).expanduser()
    cache_dir.mkdir(parents=True, exist_ok=True)
    media_cache = cache_dir / "media"
    derived_cache = cache_dir / "derived"
    work_dir = cache_dir / "projects"
    work_dir.mkdir(parents=True, exist_ok=True)
    if args.limit:
        clips = clips[: args.limit]

    rows: list[dict] = []
    pending: list[tuple[Clip, dict, Path]] = []
    for clip in clips:
        stamp = fingerprint(clip, ocp, args.sample_rate, boundary_source_of(args))
        path = cache_path(cache_dir, clip.corpus, clip.clip_id, clip.variant)
        cached = None if args.refresh else load_cached(path, stamp)
        if cached is not None:
            rows.append({**cached["result"], "cached": True})
        else:
            pending.append((clip, stamp, path))

    def work(item: tuple[Clip, dict, Path]) -> tuple[dict, dict, Path]:
        clip, stamp, path = item
        analysis = analyze(ocp, clip, work_dir, media_cache, derived_cache, args.sample_rate)
        source = boundary_source_of(args)
        metrics = compare(clip, analysis, clip.time_scale, source)
        if clip.time_scale == 1.0:
            stored = metrics
        else:
            stored = compare(clip, analysis, 1.0, source)
        row = {
            "corpus": clip.corpus,
            "clip_id": clip.clip_id,
            "variant": clip.variant,
            "audio": str(clip.audio),
            "groups": clip.groups,
            "time_scale": clip.time_scale,
            "duration_s": analysis["duration_s"],
            "analysis_rate_hz": analysis["analysis_rate_hz"],
            "hop_seconds": analysis["hop_seconds"],
            **{key: value for key, value in metrics.items() if key != "boundary_errors_ms"},
            "boundary_hits": sum(1 for value in metrics["boundary_errors_ms"] if value <= BOUNDARY_HIT_S * 1000.0),
            "boundary_total": 2 * metrics["ref_notes"],
            "note_octave_notes": metrics["note_octave_count"],
            "matched_notes": metrics["matched_note_count"],
            "boundary_errors_ms": metrics["boundary_errors_ms"][:2000],
            **{f"raw_{key}": value for key, value in stored.items() if key != "boundary_errors_ms"},
            "raw_boundary_hits": sum(1 for value in stored["boundary_errors_ms"] if value <= BOUNDARY_HIT_S * 1000.0),
            "raw_boundary_total": 2 * stored["ref_notes"],
            "raw_note_octave_notes": stored["note_octave_count"],
            "raw_matched_notes": stored["matched_note_count"],
        }
        return row, stamp, path

    if pending:
        workers = max(1, args.workers)
        with ThreadPoolExecutor(max_workers=workers) as pool:
            futures = [pool.submit(work, item) for item in pending]
            for done, future in enumerate(futures, start=1):
                row, stamp, path = future.result()
                store_cached(path, stamp, {"result": row})
                rows.append({**row, "cached": False})
                if args.verbose or done % 100 == 0 or done == len(futures):
                    print(
                        f"[{done}/{len(futures)}] {row['corpus']}/{row['clip_id']} "
                        f"f0med={row['f0_median_abs_c']:.1f}c "
                        f"oct={100 * row['octave_rate']:.1f}% "
                        f"notes={row['our_notes']}/{row['ref_notes']}",
                        file=sys.stderr,
                    )
    rows.sort(key=lambda row: (row["corpus"], row["clip_id"]))
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus-dir", default=str(DEFAULT_CORPUS_DIR))
    parser.add_argument("--ocp", default=None, help="path to the ocp binary")
    parser.add_argument("--cache-dir", default=None, help="per-clip result and media cache")
    parser.add_argument(
        "--boundary-source",
        default="detected",
        choices=["detected", "reported"],
        help="note edges the boundary metrics read: the detected onset/offset or the frame-grid span",
    )
    parser.add_argument("--corpus", default="all", choices=["all", "vocadito", "vocalset"])
    parser.add_argument("--group", default="corpus", help="singer, technique, language, vowel, gender, corpus")
    parser.add_argument("--json-out", default=None, help="write per-clip and aggregate results")
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--every", type=int, default=1, help="keep every Nth clip after sorting (sampling aid)")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--sample-rate", type=int, default=48000)
    parser.add_argument("--refresh", action="store_true", help="ignore cached per-clip results")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--list", action="store_true", help="list clips and exit")
    parser.add_argument("--gate", action="store_true", help="run the installed acceptance gate")
    parser.add_argument("--gate-spec", default=None)
    parser.add_argument("--vocadito-annotator", default="A1", choices=["A1", "A2"])
    parser.add_argument("--vocalset-extended", default=None)
    parser.add_argument("--vocalset-raw", default=None)
    parser.add_argument("--techniques", default=None, help="comma-separated VocalSet technique filter")
    parser.add_argument(
        "--time-scale-spec",
        default=None,
        help="corpus speed exceptions (default scripts/vocal/time_scale.json)",
    )
    args = parser.parse_args()

    corpus_dir = Path(args.corpus_dir).expanduser()
    if args.cache_dir is None:
        args.cache_dir = str(corpus_dir / "eval_cache")
    ocp = resolve_ocp(args.ocp)
    gate_dir = corpus_dir / "gate"
    spec_path = (
        Path(args.time_scale_spec).expanduser() if args.time_scale_spec else Path(__file__).with_name("time_scale.json")
    )
    time_scales = load_time_scale_spec(spec_path)

    if args.gate:
        return run_gate(args, ocp, gate_dir)

    names = ["vocadito", "vocalset"] if args.corpus == "all" else [args.corpus]
    all_rows: list[dict] = []
    for name in names:
        clips = load_corpus(corpus_dir, name, args)
        if args.every > 1:
            clips = clips[:: args.every]
        apply_time_scales(clips, time_scales)
        if args.list:
            for clip in clips:
                print(f"{clip.corpus}\t{clip.clip_id}\t{clip.audio}\t{clip.groups}")
            continue
        print(f"{name}: {len(clips)} annotated clips", file=sys.stderr)
        print_time_scale(clips, spec_path.name)
        rows = evaluate(args, ocp, clips)
        print_correction_summary(rows)
        all_rows.extend(rows)

    if args.list:
        return 0

    tables: dict[str, dict[str, dict]] = {}
    for name in names:
        rows = [row for row in all_rows if row["corpus"] == name]
        if rows:
            tables[name] = group_rows(rows, args.group)
            print_table(f"{name} (grouped by {args.group})", tables[name], args.group)
    if len(names) > 1:
        combined = group_rows(all_rows, "corpus")
        if combined:
            print_table("all corpora", combined, "corpus")

    if args.json_out:
        payload = {
            "harness_version": HARNESS_VERSION,
            "sample_rate": args.sample_rate,
            "ocp": str(ocp),
            "corpus_dir": str(corpus_dir),
            "groups": tables,
            "time_scale": time_scales,
            "aggregates": {
                "as_stored": aggregate(all_rows, "raw_"),
                "time_scale_applied": aggregate(all_rows),
                "time_scale_clips_as_stored": aggregate(
                    [row for row in all_rows if row.get("time_scale", 1.0) != 1.0], "raw_"
                ),
                "time_scale_clips_applied": aggregate([row for row in all_rows if row.get("time_scale", 1.0) != 1.0]),
            },
            "clips": [{key: value for key, value in row.items() if key != "boundary_errors_ms"} for row in all_rows],
        }
        Path(args.json_out).write_text(json.dumps(payload, indent=1))
        print(f"wrote {args.json_out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
