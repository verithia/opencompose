#!/usr/bin/env python3
"""Rigorous comparison between two audio renders.

The tool compares a reference WAV with a candidate WAV and reports level,
spectrum, time/dynamics, stereo and pitch differences plus two heuristic
headline scores: an absolute score (level and body included) and a
level-invariant score (uniform gain removed, level reported separately).  The
harmonic profile validates its detected f0 against the strongest 20-300 Hz
partial and reports N/A instead of a distance when the partial lock is
ambiguous.  It is not specific to any instrument or renderer.

It runs on the standard library plus numpy.  ``pyloudnorm`` is used for
integrated loudness when importable; otherwise a built-in ITU-R BS.1770-4
implementation is used.  Every spectrum, mel, MFCC and DTW stage is numpy-only
so the bundled Python interpreter (numpy, no scipy) is sufficient.

Usage::

    wav_compare.py reference.wav candidate.wav [--json out.json] [--stems]
    wav_compare.py --selftest

For the related agent tool and metric definitions, see ``docs/agent-loop.md``.
"""

from __future__ import annotations

import argparse
import json
import math
import struct
import sys
import tempfile
import wave
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

import numpy as np

VERSION = "1.3.0"

# Headline score weights and normalisation scales.  A 12 dB mean level-matched
# log-mel error and a 6 LU loudness error each saturate their term at 1.0.
# The seven v1.2.0 weights are scaled by 0.78 and the released 0.22 is assigned
# to the five new terms, so the weights still sum to 1.
SCORE_WEIGHTS = {
    "spectral": 0.2800,
    "level": 0.1270,
    "envelope": 0.1010,
    "body": 0.0940,
    "tilt": 0.0780,
    "crest_attack": 0.0540,
    "harmonic": 0.0460,
    "mfcc": 0.0600,
    "third_octave": 0.0500,
    "lsd": 0.0450,
    "onset": 0.0400,
    "density": 0.0250,
}
LEGACY_SCORE_WEIGHTS = {"spectral": 0.55, "level": 0.25, "envelope": 0.20}
# v1.2.0 weights, kept so `previous_value` can reproduce every quoted number.
PREVIOUS_SCORE_WEIGHTS = {
    "spectral": 0.3575,
    "level": 0.1625,
    "envelope": 0.1300,
    "body": 0.1200,
    "tilt": 0.1000,
    "crest_attack": 0.0700,
    "harmonic": 0.0600,
}
# The second score removes the uniform-gain sensitivity: the level term is not
# part of it and every spectral/body term is measured after both files are
# normalised to a common RMS.  The remaining weights are the absolute ones.
LEVEL_INVARIANT_WEIGHTS = {
    "spectral": 0.2800,
    "envelope": 0.1010,
    "body": 0.0940,
    "tilt": 0.0780,
    "crest_attack": 0.0540,
    "harmonic": 0.0460,
    "mfcc": 0.0600,
    "third_octave": 0.0500,
    "lsd": 0.0450,
    "onset": 0.0400,
    "density": 0.0250,
}
PREVIOUS_LEVEL_INVARIANT_WEIGHTS = {name: weight for name, weight in PREVIOUS_SCORE_WEIGHTS.items() if name != "level"}
SPECTRAL_SCALE_DB = 12.0
LEVEL_SCALE_LU = 6.0
BODY_SCALE_DB = 4.0
TILT_SCALE_DB = 10.0
CREST_SCALE_DB = 6.0
FLUX_SCALE_DB = 6.0
HARMONIC_SCALE_DB = 6.0
# New in v1.3.0.  Scales are set from the stored pairs, not tuned to a track:
# a 0.10 MFCC cosine, 6 dB third-octave RMS, 12 dB level-matched LSD, 6 dB
# per-attack flux and 6 dB events/s each saturate their term at 1.0.
MFCC_SCALE = 0.10
THIRD_OCTAVE_SCALE_DB = 6.0
LSD_SCALE_DB = 12.0
ONSET_SCALE_DB = 6.0
DENSITY_SCALE_DB = 6.0
# Events/s from fewer than this many detected attacks is not a density estimate.
DENSITY_MIN_EVENTS = 5

BODY_LOW_HZ = (20.0, 120.0)
BODY_MID_HZ = (120.0, 2000.0)
TILT_LOW_CENTERS = (63.0, 125.0, 250.0, 500.0)
TILT_HIGH_CENTERS = (4000.0, 8000.0, 16000.0)
HARMONIC_COUNT = 8
HARMONIC_F0_RANGE = (20.0, 300.0)
HARMONIC_N_FFT = 8192
HARMONIC_HOP = 2048
HARMONIC_MIN_FRAMES = 5
# Wrong-partial guard.  A detected f0 is only published when it agrees with the
# strongest 20-300 Hz partial of the same file (10 %, or two STFT bins when the
# partial is unresolved) and is not a harmonic of a strong lower partial; the
# two files' medians must also agree within 200 cents.
HARMONIC_DOMINANT_TOL = 0.10
HARMONIC_DOMINANT_BINS = 2.0
HARMONIC_LOWER_PARTIAL_DB = 15.0
HARMONIC_LOWER_PARTIAL_MAX = 6
PARTIAL_N_FFT = 32768
PARTIAL_HOP = 16384
HARMONIC_MAX_CENTS = 200.0

OCTAVE_CENTERS = (31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0)
THIRD_OCTAVE_CENTERS = (
    25.0,
    31.5,
    40.0,
    50.0,
    63.0,
    80.0,
    100.0,
    125.0,
    160.0,
    200.0,
    250.0,
    315.0,
    400.0,
    500.0,
    630.0,
    800.0,
    1000.0,
    1250.0,
    1600.0,
    2000.0,
    2500.0,
    3150.0,
    4000.0,
    5000.0,
    6300.0,
    8000.0,
    10000.0,
    12500.0,
    16000.0,
    20000.0,
)

# ITU-R BS.1770-4 K-weighting coefficients at 48 kHz.
_KW_B1 = (1.53512485958697, -2.69169618940638, 1.19839281085285)
_KW_A1 = (1.0, -1.69065929318241, 0.73248077421585)
_KW_B2 = (1.0, -2.0, 1.0)
_KW_A2 = (1.0, -1.99004745483398, 0.99007225036621)

_NEG_INF_DB = -120.0


# ---------------------------------------------------------------------------
# Small numeric helpers
# ---------------------------------------------------------------------------


def _db(x: float | np.ndarray, floor: float = _NEG_INF_DB) -> float | np.ndarray:
    """Amplitude ratio to dB, floored so silence stays finite."""
    value = np.maximum(np.asarray(x, dtype=np.float64), 1e-12)
    return np.maximum(20.0 * np.log10(value), floor)


def _power_db(x: float | np.ndarray, floor: float = _NEG_INF_DB) -> float | np.ndarray:
    """Power ratio to dB, floored so silence stays finite."""
    value = np.maximum(np.asarray(x, dtype=np.float64), 1e-12)
    return np.maximum(10.0 * np.log10(value), floor)


def _num(x: float | None, digits: int = 3) -> float | None:
    """Round a float for JSON, mapping non-finite values to None."""
    if x is None:
        return None
    value = float(x)
    if not math.isfinite(value):
        return None
    return round(value, digits)


def _nums(values: np.ndarray, digits: int = 3) -> list[float | None]:
    return [_num(value, digits) for value in np.asarray(values).ravel()]


# ---------------------------------------------------------------------------
# WAV input (PCM 8/16/24/32 and IEEE float 32/64)
# ---------------------------------------------------------------------------


@dataclass
class Audio:
    """Decoded audio at a single sample rate, shape (frames, channels)."""

    samples: np.ndarray
    rate: int
    path: str = ""

    @property
    def channels(self) -> int:
        return int(self.samples.shape[1])

    @property
    def duration_s(self) -> float:
        return float(self.samples.shape[0]) / float(self.rate)

    @property
    def mono(self) -> np.ndarray:
        return self.samples.mean(axis=1)


def read_wav(path: str) -> Audio:
    """Read a RIFF/WAVE file into float64 samples in [-1, 1].

    Supports PCM 8/16/24/32-bit and IEEE float 32/64-bit, including
    WAVE_FORMAT_EXTENSIBLE.  Multi-channel files are kept as-is.
    """
    data = Path(path).read_bytes()
    if len(data) < 12 or data[0:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError(f"{path}: not a RIFF/WAVE file")

    fmt: bytes | None = None
    payload: bytes | None = None
    offset = 12
    while offset + 8 <= len(data):
        chunk_id = data[offset : offset + 4]
        size = struct.unpack_from("<I", data, offset + 4)[0]
        start = offset + 8
        end = min(start + size, len(data))
        if chunk_id == b"fmt ":
            fmt = data[start:end]
        elif chunk_id == b"data":
            payload = data[start:end]
        offset = start + size + (size & 1)

    if fmt is None or len(fmt) < 16:
        raise ValueError(f"{path}: missing or truncated fmt chunk")
    if payload is None:
        raise ValueError(f"{path}: missing data chunk")

    format_tag, channels, rate, _byte_rate, _block_align, bits = struct.unpack_from("<HHIIHH", fmt, 0)
    if format_tag == 0xFFFE and len(fmt) >= 26:
        format_tag = struct.unpack_from("<H", fmt, 24)[0]
    if channels < 1 or rate < 1:
        raise ValueError(f"{path}: invalid channel count or sample rate")

    bytes_per_sample = bits // 8
    if bytes_per_sample < 1:
        raise ValueError(f"{path}: unsupported sample width {bits} bits")
    frame_bytes = channels * bytes_per_sample
    raw = payload[: len(payload) - len(payload) % frame_bytes]

    if format_tag == 1:
        if bits == 8:
            values = (np.frombuffer(raw, dtype=np.uint8).astype(np.float64) - 128.0) / 128.0
        elif bits == 16:
            values = np.frombuffer(raw, dtype="<i2").astype(np.float64) / 32768.0
        elif bits == 24:
            octets = np.frombuffer(raw, dtype=np.uint8).reshape(-1, 3).astype(np.int32)
            packed = octets[:, 0] | (octets[:, 1] << 8) | (octets[:, 2] << 16)
            packed = np.where(packed >= (1 << 23), packed - (1 << 24), packed)
            values = packed.astype(np.float64) / 8388608.0
        elif bits == 32:
            values = np.frombuffer(raw, dtype="<i4").astype(np.float64) / 2147483648.0
        else:
            raise ValueError(f"{path}: unsupported PCM width {bits} bits")
    elif format_tag == 3:
        if bits == 32:
            values = np.frombuffer(raw, dtype="<f4").astype(np.float64)
        elif bits == 64:
            values = np.frombuffer(raw, dtype="<f8").astype(np.float64)
        else:
            raise ValueError(f"{path}: unsupported float width {bits} bits")
    else:
        raise ValueError(f"{path}: unsupported WAV format tag {format_tag}")

    samples = values.reshape(-1, channels)
    if not np.all(np.isfinite(samples)):
        raise ValueError(f"{path}: non-finite samples")
    return Audio(samples=samples, rate=int(rate), path=str(path))


def _resample(audio: np.ndarray, src_rate: int, dst_rate: int) -> np.ndarray:
    """FFT band-limited resampling (amplitude preserving).

    The analysis only needs spectral fidelity; the circular edge wrap of the
    FFT method is far below the metrics' resolution.
    """
    if src_rate == dst_rate:
        return audio
    n_in, channels = audio.shape
    n_out = int(round(n_in * dst_rate / src_rate))
    if n_out < 1:
        raise ValueError("resampling produced an empty signal")
    spectrum = np.fft.rfft(audio, axis=0)
    out_bins = n_out // 2 + 1
    shifted = np.zeros((out_bins, channels), dtype=np.complex128)
    keep = min(spectrum.shape[0], out_bins)
    shifted[:keep] = spectrum[:keep]
    return np.fft.irfft(shifted, n=n_out, axis=0) * (n_out / n_in)


# ---------------------------------------------------------------------------
# Spectral primitives
# ---------------------------------------------------------------------------


def stft_power(audio: np.ndarray, rate: int, n_fft: int, hop: int) -> tuple[np.ndarray, np.ndarray]:
    """Hann-windowed power spectrogram with per-bin mean-square scaling.

    Channel powers are averaged, so a dual-mono file has the same spectrum as
    the mono file.  Summing the returned bins over frequency yields the mean
    square of the (unwindowed) frame.
    """
    signal = np.asarray(audio, dtype=np.float64)
    if signal.ndim == 1:
        signal = signal[:, None]
    n_frames = 1 + max(0, signal.shape[0] - n_fft) // hop
    if n_frames < 1 or signal.shape[0] < n_fft:
        raise ValueError("signal shorter than one analysis window")
    window = np.hanning(n_fft)
    window_energy = float(np.sum(window * window))
    channels = signal.shape[1]
    bins = n_fft // 2 + 1
    power = np.empty((n_frames, bins), dtype=np.float64)
    chunk = 256
    for start in range(0, n_frames, chunk):
        stop = min(start + chunk, n_frames)
        indices = (np.arange(start, stop) * hop)[:, None] + np.arange(n_fft)[None, :]
        frames = signal[indices] * window[None, :, None]
        spectrum = np.fft.rfft(frames, axis=1)
        block = (spectrum.real**2 + spectrum.imag**2).sum(axis=2)
        power[start:stop] = block / (window_energy * channels)
    weights = np.full(bins, 2.0)
    weights[0] = 1.0
    if n_fft % 2 == 0:
        weights[-1] = 1.0
    power *= weights[None, :]
    freqs = np.fft.rfftfreq(n_fft, d=1.0 / rate)
    return power, freqs


@lru_cache(maxsize=8)
def _mel_filterbank(rate: int, n_fft: int, n_mels: int, fmin: float, fmax: float) -> np.ndarray:
    """HTK mel triangles, area-normalised (Slaney-style), shape (n_mels, bins)."""

    def hz_to_mel(hz: float) -> float:
        return 2595.0 * math.log10(1.0 + hz / 700.0)

    def mel_to_hz(mel: float) -> float:
        return 700.0 * (10.0 ** (mel / 2595.0) - 1.0)

    points = np.array(
        [mel_to_hz(value) for value in np.linspace(hz_to_mel(fmin), hz_to_mel(fmax), n_mels + 2)],
        dtype=np.float64,
    )
    freqs = np.fft.rfftfreq(n_fft, d=1.0 / rate)
    filters = np.zeros((n_mels, freqs.size), dtype=np.float64)
    for index in range(n_mels):
        left, center, right = points[index], points[index + 1], points[index + 2]
        rising = (freqs - left) / max(center - left, 1e-9)
        falling = (right - freqs) / max(right - center, 1e-9)
        filters[index] = np.maximum(0.0, np.minimum(rising, falling))
        filters[index] *= 2.0 / max(right - left, 1e-9)
    return filters


@lru_cache(maxsize=4)
def _dct_matrix(n_in: int, n_out: int) -> np.ndarray:
    """Orthonormal DCT-II matrix of shape (n_out, n_in)."""
    index = np.arange(n_out)[:, None]
    sample = np.arange(n_in)[None, :]
    matrix = np.cos(np.pi / n_in * (sample + 0.5) * index) * math.sqrt(2.0 / n_in)
    matrix[0, :] /= math.sqrt(2.0)
    return matrix


def _log_mel(power: np.ndarray, filters: np.ndarray, floor: float = 1e-12) -> np.ndarray:
    mel = power @ filters.T
    return 10.0 * np.log10(np.maximum(mel, floor))


def _true_peak_db(audio: np.ndarray, factor: int = 4) -> float:
    """4x band-limited oversampling, returning dBTP."""
    peak = 0.0
    for channel in range(audio.shape[1]):
        x = audio[:, channel]
        n_out = x.size * factor
        spectrum = np.fft.rfft(x)
        bins = n_out // 2 + 1
        padded = np.zeros(bins, dtype=np.complex128)
        keep = min(spectrum.size, bins)
        padded[:keep] = spectrum[:keep]
        oversampled = np.fft.irfft(padded, n=n_out) * factor
        peak = max(peak, float(np.max(np.abs(oversampled))))
    return float(_db(peak))


# ---------------------------------------------------------------------------
# Loudness (ITU-R BS.1770-4)
# ---------------------------------------------------------------------------


def _k_weight(audio: np.ndarray, rate: int) -> np.ndarray:
    """Apply the BS.1770 K-weighting biquads in the frequency domain."""
    x = audio if rate == 48000 else _resample(audio, rate, 48000)
    n = x.shape[0]
    freqs = np.fft.rfftfreq(n, d=1.0 / 48000.0)
    z = np.exp(-2j * np.pi * freqs / 48000.0)
    shelf = (_KW_B1[0] + _KW_B1[1] * z + _KW_B1[2] * z * z) / (_KW_A1[0] + _KW_A1[1] * z + _KW_A1[2] * z * z)
    highpass = (_KW_B2[0] + _KW_B2[1] * z + _KW_B2[2] * z * z) / (_KW_A2[0] + _KW_A2[1] * z + _KW_A2[2] * z * z)
    spectrum = np.fft.rfft(x, axis=0) * (shelf * highpass)[:, None]
    return np.fft.irfft(spectrum, n=n, axis=0)


def _block_powers(channel_sum_power: np.ndarray, rate: int, window_s: float, hop_s: float) -> np.ndarray:
    window = int(round(window_s * rate))
    hop = int(round(hop_s * rate))
    if window < 1 or hop < 1 or channel_sum_power.size < window:
        return np.zeros(0)
    cumulative = np.concatenate(([0.0], np.cumsum(channel_sum_power)))
    starts = np.arange(0, channel_sum_power.size - window + 1, hop)
    return (cumulative[starts + window] - cumulative[starts]) / window


def _bs1770_integrated(audio: np.ndarray, rate: int) -> dict:
    weighted = _k_weight(audio, rate)
    channel_sum = np.sum(weighted * weighted, axis=1)
    powers = _block_powers(channel_sum, 48000, 0.4, 0.1)
    if powers.size == 0:
        return {"lufs": None, "blocks": 0, "gated_blocks": 0, "implementation": "bs1770-internal"}
    loudness = -0.691 + 10.0 * np.log10(np.maximum(powers, 1e-12))
    absolute = loudness > -70.0
    if not np.any(absolute):
        return {"lufs": None, "blocks": int(powers.size), "gated_blocks": 0, "implementation": "bs1770-internal"}
    relative_threshold = -0.691 + 10.0 * np.log10(float(np.mean(powers[absolute]))) - 10.0
    keep = absolute & (loudness > relative_threshold)
    if not np.any(keep):
        return {"lufs": None, "blocks": int(powers.size), "gated_blocks": 0, "implementation": "bs1770-internal"}
    value = -0.691 + 10.0 * np.log10(float(np.mean(powers[keep])))
    return {
        "lufs": float(value),
        "blocks": int(powers.size),
        "gated_blocks": int(np.sum(keep)),
        "implementation": "bs1770-internal",
    }


def integrated_loudness(audio: np.ndarray, rate: int) -> dict:
    """Integrated loudness in LUFS, preferring pyloudnorm when importable."""
    try:
        import pyloudnorm  # type: ignore

        meter = pyloudnorm.Meter(rate)
        values = audio if audio.shape[1] > 1 else audio[:, 0]
        value = float(meter.integrated_loudness(values))
        if math.isfinite(value):
            return {"lufs": value, "blocks": None, "gated_blocks": None, "implementation": "pyloudnorm"}
    except Exception:
        pass
    return _bs1770_integrated(audio, rate)


def _short_term_lufs(audio: np.ndarray, rate: int) -> dict:
    weighted = _k_weight(audio, rate)
    channel_sum = np.sum(weighted * weighted, axis=1)
    window_s = min(3.0, audio.shape[0] / rate)
    if window_s <= 0.0:
        return {"min": None, "max": None, "range": None, "windows": 0, "window_s": 0.0}
    powers = _block_powers(channel_sum, 48000, window_s, 0.25)
    if powers.size == 0:
        return {"min": None, "max": None, "range": None, "windows": 0, "window_s": window_s}
    loudness = -0.691 + 10.0 * np.log10(np.maximum(powers, 1e-12))
    loudness = loudness[loudness > -70.0]
    if loudness.size == 0:
        return {"min": None, "max": None, "range": None, "windows": 0, "window_s": window_s}
    low = float(np.min(loudness))
    high = float(np.max(loudness))
    return {
        "min": low,
        "max": high,
        "range": high - low,
        "windows": int(loudness.size),
        "window_s": float(window_s),
    }


def level_metrics(audio: Audio) -> dict:
    peak = float(np.max(np.abs(audio.samples)))
    rms = float(np.sqrt(np.mean(audio.samples * audio.samples)))
    loudness = integrated_loudness(audio.samples, audio.rate)
    short_term = _short_term_lufs(audio.samples, audio.rate)
    return {
        "peak_dbfs": float(_db(peak)),
        "true_peak_dbtp": _true_peak_db(audio.samples),
        "rms_dbfs": float(_db(rms)),
        "crest_db": float(_db(peak) - _db(rms)),
        "integrated_lufs": _num(loudness["lufs"], 2),
        "loudness_implementation": loudness["implementation"],
        "loudness_blocks": loudness["blocks"],
        "short_term_lufs": {
            "min": _num(short_term["min"], 2),
            "max": _num(short_term["max"], 2),
            "range": _num(short_term["range"], 2),
            "windows": short_term["windows"],
            "window_s": _num(short_term["window_s"], 3),
        },
    }


# ---------------------------------------------------------------------------
# Band energies and spectral shape
# ---------------------------------------------------------------------------


def _band_table(power: np.ndarray, freqs: np.ndarray, centers: tuple[float, ...], factor: float) -> dict:
    mean_power = power.mean(axis=0)
    reference_db: list[float] = []
    for center in centers:
        low = center / factor
        high = center * factor
        band = mean_power[(freqs >= low) & (freqs < high)]
        reference_db.append(float(_power_db(float(band.sum()))))
    return {"centers_hz": [float(value) for value in centers], "reference_db": reference_db}


def band_energies(reference: Audio, candidate: Audio) -> dict:
    """Octave and third-octave mean band energy, both files and delta.

    A 16k FFT keeps the low third-octave bands from collapsing into one bin.
    ``delta = candidate - reference`` in dB; the floor is -120 dBFS.
    """

    def measure(audio: Audio) -> tuple[dict, dict]:
        power, freqs = stft_power(audio.samples, audio.rate, 16384, 8192)
        octave = _band_table(power, freqs, OCTAVE_CENTERS, math.sqrt(2.0))
        third = _band_table(power, freqs, THIRD_OCTAVE_CENTERS, 2.0 ** (1.0 / 6.0))
        return octave, third

    ref_octave, ref_third = measure(reference)
    cand_octave, cand_third = measure(candidate)

    def combine(ref: dict, cand: dict) -> dict:
        delta = [c - r for r, c in zip(ref["reference_db"], cand["reference_db"])]
        return {
            "centers_hz": ref["centers_hz"],
            "reference_db": [_num(value, 2) for value in ref["reference_db"]],
            "candidate_db": [_num(value, 2) for value in cand["reference_db"]],
            "delta_db": [_num(value, 2) for value in delta],
        }

    return {"octave": combine(ref_octave, cand_octave), "third_octave": combine(ref_third, cand_third)}


def third_octave_rms(third: dict) -> dict:
    """RMS of third-octave deltas over bands within 60 dB of the loudest band.

    A narrow resonance between two octave centres shows up here while the octave
    table averages it away.  Floor bands are excluded so FFT leakage cannot
    inflate the number.  The level-matched value removes the median band delta,
    so a uniform gain does not read as a spectral-shape change.
    """
    reference = third["reference_db"]
    candidate = third["candidate_db"]
    peak = max(max(reference), max(candidate))
    usable = [(cand - ref) for ref, cand in zip(reference, candidate) if max(ref, cand) >= peak - 60.0]
    raw = float(np.sqrt(np.mean(np.square(usable)))) if usable else None
    matched = float(np.sqrt(np.mean(np.square(np.array(usable) - np.median(usable))))) if usable else None
    return {
        "rms_delta_db": _num(raw, 3),
        "level_matched_rms_delta_db": _num(matched, 3),
        "bands_used": len(usable),
    }


def _frame_features(power: np.ndarray, freqs: np.ndarray) -> dict:
    frame_power = power.sum(axis=1)
    peak = float(frame_power.max()) if frame_power.size else 0.0
    gate = max(1e-12, peak * 1e-6)  # -60 dB relative to the loudest frame
    valid = frame_power > gate
    if not np.any(valid):
        return {
            "valid": np.zeros(0, dtype=bool),
            "centroid": np.zeros(0),
            "rolloff": np.zeros(0),
            "flatness": np.zeros(0),
        }
    p = power[valid]
    total = p.sum(axis=1)
    centroid = (p * freqs[None, :]).sum(axis=1) / np.maximum(total, 1e-12)
    cumulative = np.cumsum(p, axis=1)
    threshold = 0.85 * total
    rolloff = freqs[np.argmax(cumulative >= threshold[:, None], axis=1)]
    flatness = np.exp(np.mean(np.log(np.maximum(p, 1e-12)), axis=1)) / np.maximum(np.mean(p, axis=1), 1e-12)
    return {"valid": valid, "centroid": centroid, "rolloff": rolloff, "flatness": flatness}


def _power_distances(
    ref_power: np.ndarray,
    cand_power: np.ndarray,
    rate: int,
    frame_rate: float,
    floor: float = 1e-12,
) -> tuple[dict, dict, dict]:
    """Log-mel, MFCC cosine and LSD from two equal-length power spectrograms."""
    filters = _mel_filterbank(rate, 2048, 64, 30.0, 16000.0)
    ref_logmel = _log_mel(ref_power, filters, floor)
    cand_logmel = _log_mel(cand_power, filters, floor)
    floor_db = float(max(ref_logmel.max(), cand_logmel.max()) - 80.0)
    ref_logmel = np.maximum(ref_logmel, floor_db)
    cand_logmel = np.maximum(cand_logmel, floor_db)
    difference = ref_logmel - cand_logmel
    log_mel_distance = {
        "bands": 64,
        "floor_db": _num(floor_db, 2),
        "mean_abs_delta_db": _num(float(np.mean(np.abs(difference))), 3),
        "mean_abs_delta_db_level_matched": _num(float(np.mean(np.abs(difference - np.median(difference)))), 3),
        "p90_abs_delta_db": _num(float(np.percentile(np.abs(difference), 90)), 3),
        "frame_rate_hz": _num(frame_rate, 2),
    }

    dct = _dct_matrix(64, 20)
    ref_mfcc = ref_logmel @ dct.T
    cand_mfcc = cand_logmel @ dct.T
    ref_mean = ref_mfcc[:, 1:14].mean(axis=0)
    cand_mean = cand_mfcc[:, 1:14].mean(axis=0)
    denominator = float(np.linalg.norm(ref_mean) * np.linalg.norm(cand_mean))
    vector_cosine = 1.0 - float(np.dot(ref_mean, cand_mean)) / denominator if denominator > 1e-12 else None
    ref_frames = ref_mfcc[:, 1:14]
    cand_frames = cand_mfcc[:, 1:14]
    ref_norms = np.linalg.norm(ref_frames, axis=1)
    cand_norms = np.linalg.norm(cand_frames, axis=1)
    usable = (ref_norms > 1e-9) & (cand_norms > 1e-9)
    if np.any(usable):
        frame_cosine = np.sum(ref_frames[usable] * cand_frames[usable], axis=1) / (
            ref_norms[usable] * cand_norms[usable]
        )
        mean_cosine = float(np.mean(1.0 - frame_cosine))
    else:
        mean_cosine = None
    mfcc = {
        "coefficients": 13,
        "mean_cosine_distance": _num(mean_cosine, 4),
        "mean_vector_cosine_distance": _num(vector_cosine, 4),
    }

    ref_db = 10.0 * np.log10(np.maximum(ref_power, floor))
    cand_db = 10.0 * np.log10(np.maximum(cand_power, floor))
    frame_lsd = np.sqrt(np.mean((ref_db - cand_db) ** 2, axis=1))
    level_matched = (ref_db - ref_db.mean(axis=1, keepdims=True)) - (cand_db - cand_db.mean(axis=1, keepdims=True))
    lsd = {
        "broadband_db": _num(float(np.mean(frame_lsd)), 3),
        "level_matched_db": _num(float(np.mean(np.sqrt(np.mean(level_matched**2, axis=1)))), 3),
    }
    return log_mel_distance, mfcc, lsd


def _shape_distances(reference: Audio, candidate: Audio, hop: int = 512, floor: float = 1e-12) -> dict:
    """Log-mel/MFCC/LSD on two signals; `floor=1e-30` makes the terms scale-invariant."""
    n_fft = 2048
    ref_power, _ = stft_power(reference.samples, reference.rate, n_fft, hop)
    cand_power, _ = stft_power(candidate.samples, candidate.rate, n_fft, hop)
    frames = min(ref_power.shape[0], cand_power.shape[0])
    log_mel, mfcc, lsd = _power_distances(
        ref_power[:frames], cand_power[:frames], reference.rate, reference.rate / hop, floor
    )
    return {"log_mel": log_mel, "mfcc": mfcc, "lsd": lsd}


def spectral_shape(reference: Audio, candidate: Audio, hop: int = 512) -> dict:
    """Per-frame centroid/rolloff/flatness plus medians and log-mel/MFCC/LSD."""
    n_fft = 2048
    ref_power, freqs = stft_power(reference.samples, reference.rate, n_fft, hop)
    cand_power, _ = stft_power(candidate.samples, candidate.rate, n_fft, hop)
    frames = min(ref_power.shape[0], cand_power.shape[0])
    ref_power = ref_power[:frames]
    cand_power = cand_power[:frames]

    ref_features = _frame_features(ref_power, freqs)
    cand_features = _frame_features(cand_power, freqs)
    frame_rate = reference.rate / hop
    ref_times = np.flatnonzero(ref_features["valid"]) / frame_rate
    cand_times = np.flatnonzero(cand_features["valid"]) / frame_rate

    def frame_block(features: dict, times: np.ndarray) -> dict:
        return {
            "time_s": [_num(value, 3) for value in times],
            "centroid_hz": _nums(features["centroid"], 2),
            "rolloff_hz": _nums(features["rolloff"], 2),
            "flatness": _nums(features["flatness"], 5),
        }

    def median(values: np.ndarray) -> float | None:
        return _num(float(np.median(values)), 2) if values.size else None

    medians = {
        "reference": {
            "centroid_hz": median(ref_features["centroid"]),
            "rolloff_hz": median(ref_features["rolloff"]),
            "flatness": _num(float(np.median(ref_features["flatness"])), 5) if ref_features["flatness"].size else None,
        },
        "candidate": {
            "centroid_hz": median(cand_features["centroid"]),
            "rolloff_hz": median(cand_features["rolloff"]),
            "flatness": _num(float(np.median(cand_features["flatness"])), 5)
            if cand_features["flatness"].size
            else None,
        },
    }
    medians["delta"] = {
        key: (
            _num(
                medians["candidate"][key] - medians["reference"][key],
                2 if key != "flatness" else 5,
            )
            if medians["candidate"][key] is not None and medians["reference"][key] is not None
            else None
        )
        for key in ("centroid_hz", "rolloff_hz", "flatness")
    }

    log_mel_distance, mfcc, lsd = _power_distances(ref_power, cand_power, reference.rate, frame_rate)

    return {
        "spectral_features": {
            "n_fft": n_fft,
            "hop": hop,
            "gate": "frames within 60 dB of the loudest frame",
            "reference": frame_block(ref_features, ref_times),
            "candidate": frame_block(cand_features, cand_times),
            "medians": medians,
        },
        "log_mel": log_mel_distance,
        "mfcc": mfcc,
        "lsd": lsd,
    }


# ---------------------------------------------------------------------------
# Body level, octave tilt and harmonic profile
# ---------------------------------------------------------------------------


def _band_limited_peak(samples: np.ndarray, rate: int, low_hz: float, high_hz: float) -> float:
    """Time-domain peak of a brick-wall FFT band split."""
    frames = samples.shape[0]
    spectrum = np.fft.rfft(samples, axis=0)
    freqs = np.fft.rfftfreq(frames, d=1.0 / rate)
    spectrum[(freqs < low_hz) | (freqs >= high_hz)] = 0.0
    filtered = np.fft.irfft(spectrum, n=frames, axis=0)
    return float(np.max(np.abs(filtered)))


def _band_power(audio: Audio, low_hz: float, high_hz: float) -> float:
    """Band mean square from the 16k STFT, so both implementations agree."""
    if audio.samples.shape[0] < 16384:
        return 0.0
    power, freqs = stft_power(audio.samples, audio.rate, 16384, 8192)
    mask = (freqs >= low_hz) & (freqs < high_hz)
    return float(power[:, mask].sum(axis=1).mean()) / 16384.0


def body_metrics(reference: Audio, candidate: Audio) -> dict:
    """Absolute (not level-matched) 20-120 Hz and 120 Hz-2 kHz band level.

    A broadband or low-mid loss that is invisible to the level-matched
    log-mel shows up here because the whole signal is level-matched. Bands more
    than 60 dB below the louder file overall are numerical floor, not evidence,
    so their delta is zeroed.
    """

    def measure(audio: Audio) -> tuple[float, float, float, float]:
        overall = float(np.mean(audio.samples * audio.samples))
        low_power = _band_power(audio, *BODY_LOW_HZ)
        mid_power = _band_power(audio, *BODY_MID_HZ)
        low_peak = _band_limited_peak(audio.samples, audio.rate, *BODY_LOW_HZ)
        return overall, low_power, mid_power, low_peak

    ref_overall, ref_low_power, ref_mid_power, ref_low_peak = measure(reference)
    cand_overall, cand_low_power, cand_mid_power, cand_low_peak = measure(candidate)
    gate = max(ref_overall, cand_overall) * 1e-6
    low_gated = max(ref_low_power, cand_low_power) < gate
    mid_gated = max(ref_mid_power, cand_mid_power) < gate
    low_delta = 0.0 if low_gated else float(_power_db(cand_low_power) - _power_db(ref_low_power))
    mid_delta = 0.0 if mid_gated else float(_power_db(cand_mid_power) - _power_db(ref_mid_power))
    peak_delta = 0.0 if low_gated else float(_db(cand_low_peak) - _db(ref_low_peak))
    return {
        "low": {
            "range_hz": [BODY_LOW_HZ[0], BODY_LOW_HZ[1]],
            "reference_rms_dbfs": _num(_power_db(ref_low_power), 2),
            "candidate_rms_dbfs": _num(_power_db(cand_low_power), 2),
            "rms_delta_db": _num(low_delta, 2),
            "reference_peak_dbfs": _num(_db(ref_low_peak), 2),
            "candidate_peak_dbfs": _num(_db(cand_low_peak), 2),
            "peak_delta_db": _num(peak_delta, 2),
            "gated": bool(low_gated),
        },
        "mid": {
            "range_hz": [BODY_MID_HZ[0], BODY_MID_HZ[1]],
            "reference_rms_dbfs": _num(_power_db(ref_mid_power), 2),
            "candidate_rms_dbfs": _num(_power_db(cand_mid_power), 2),
            "rms_delta_db": _num(mid_delta, 2),
            "gated": bool(mid_gated),
        },
    }


def tilt_metrics(octave: dict) -> dict:
    """Octave deltas without level matching plus one low-minus-high scalar.

    Octaves more than 60 dB below the loudest octave in either file are floor,
    so their delta is zeroed before averaging; they carry no tilt evidence. The
    same bands are averaged separately into `sub_gate_tilt_db` so the floor
    still reports a number instead of disappearing (reported-only, not scored).
    """
    reference = octave["reference_db"]
    candidate = octave["candidate_db"]
    peak = max(max(reference), max(candidate))
    gated = [max(ref, cand) < peak - 60.0 for ref, cand in zip(reference, candidate)]
    deltas = [0.0 if gate else cand - ref for ref, cand, gate in zip(reference, candidate, gated)]
    raw = [cand - ref for ref, cand in zip(reference, candidate)]
    lookup = {center: delta for center, delta in zip(octave["centers_hz"], deltas)}
    low = [lookup[center] for center in TILT_LOW_CENTERS if lookup.get(center) is not None]
    high = [lookup[center] for center in TILT_HIGH_CENTERS if lookup.get(center) is not None]
    low_mean = float(np.mean(low)) if low else None
    high_mean = float(np.mean(high)) if high else None
    tilt = low_mean - high_mean if low_mean is not None and high_mean is not None else None

    raw_lookup = dict(zip(octave["centers_hz"], raw))
    gate_lookup = dict(zip(octave["centers_hz"], gated))
    sub_low = [raw_lookup[center] for center in TILT_LOW_CENTERS if gate_lookup.get(center)]
    sub_high = [raw_lookup[center] for center in TILT_HIGH_CENTERS if gate_lookup.get(center)]
    sub_low_mean = float(np.mean(sub_low)) if sub_low else None
    sub_high_mean = float(np.mean(sub_high)) if sub_high else None
    sub_tilt = sub_low_mean - sub_high_mean if sub_low_mean is not None and sub_high_mean is not None else None
    return {
        "level_matched": False,
        "octave_delta_db": [_num(value, 2) for value in deltas],
        "gated_octaves": gated,
        "low_centers_hz": list(TILT_LOW_CENTERS),
        "high_centers_hz": list(TILT_HIGH_CENTERS),
        "low_mean_delta_db": _num(low_mean, 2),
        "high_mean_delta_db": _num(high_mean, 2),
        "tilt_db": _num(tilt, 2),
        "sub_gate_octaves_hz": [center for center, gate in zip(octave["centers_hz"], gated) if gate],
        "sub_gate_low_mean_delta_db": _num(sub_low_mean, 2),
        "sub_gate_high_mean_delta_db": _num(sub_high_mean, 2),
        "sub_gate_tilt_db": _num(sub_tilt, 2),
        "sub_gate_scored": False,
    }


def _harmonic_rows(audio: Audio) -> tuple[np.ndarray, np.ndarray]:
    """Per-frame relative harmonic levels (dB re H1) of the detected bass f0."""
    power, freqs = stft_power(audio.samples, audio.rate, HARMONIC_N_FFT, HARMONIC_HOP)
    bins = np.flatnonzero((freqs >= HARMONIC_F0_RANGE[0]) & (freqs <= HARMONIC_F0_RANGE[1]))
    if bins.size < 3:
        return np.zeros((0, HARMONIC_COUNT)), np.zeros(0)
    bin_hz = audio.rate / HARMONIC_N_FFT
    frame_peak = power.max(axis=1)
    rows: list[list[float]] = []
    fundamentals: list[float] = []
    for frame in range(power.shape[0]):
        if frame_peak[frame] <= 1e-12:
            continue
        spectrum = power[frame]
        peak = int(bins[int(np.argmax(spectrum[bins]))])
        if spectrum[peak] < 1e-4 * frame_peak[frame]:
            continue
        left = math.log(max(float(spectrum[peak - 1]), 1e-12))
        center = math.log(max(float(spectrum[peak]), 1e-12))
        right = math.log(max(float(spectrum[peak + 1]), 1e-12))
        denominator = left - 2.0 * center + right
        offset = 0.5 * (left - right) / denominator if abs(denominator) > 1e-12 else 0.0
        offset = max(-0.5, min(0.5, offset))
        f0 = (peak + offset) * bin_hz
        if not HARMONIC_F0_RANGE[0] <= f0 <= HARMONIC_F0_RANGE[1]:
            continue
        levels: list[float] = []
        for harmonic in range(1, HARMONIC_COUNT + 1):
            center_hz = harmonic * f0
            if center_hz >= audio.rate / 2.0:
                levels = []
                break
            half = max(2.0, 0.02 * center_hz)
            low = max(0, int(math.floor((center_hz - half) / bin_hz)))
            high = min(spectrum.size, int(math.ceil((center_hz + half) / bin_hz)) + 1)
            levels.append(float(spectrum[low:high].sum()))
        if not levels or levels[0] <= 1e-12:
            continue
        rows.append([max(10.0 * math.log10(max(value, 1e-12) / levels[0]), -60.0) for value in levels])
        fundamentals.append(f0)
    return np.asarray(rows, dtype=np.float64), np.asarray(fundamentals, dtype=np.float64)


def _dominant_partial(audio: Audio) -> dict | None:
    """Strongest 20-300 Hz partial of the mean spectrum and a strong lower partial.

    The lower partial is the first local maximum at ``strongest / k`` (k = 2..6)
    within 15 dB of the strongest partial.  Its presence means the strongest
    partial may be a harmonic rather than the fundamental.
    """
    n_fft = PARTIAL_N_FFT if audio.samples.shape[0] >= PARTIAL_N_FFT else HARMONIC_N_FFT
    if audio.samples.shape[0] < n_fft:
        return None
    power, freqs = stft_power(audio.samples, audio.rate, n_fft, max(1, n_fft // 2))
    if power.shape[0] == 0:
        return None
    bins = np.flatnonzero((freqs >= HARMONIC_F0_RANGE[0]) & (freqs <= HARMONIC_F0_RANGE[1]))
    if bins.size == 0:
        return None
    mean = power.mean(axis=0)
    peak = int(bins[int(np.argmax(mean[bins]))])
    strongest = float(freqs[peak])
    bin_hz = audio.rate / n_fft
    lower: dict | None = None
    for ratio in range(2, HARMONIC_LOWER_PARTIAL_MAX + 1):
        target = strongest / ratio
        if target < HARMONIC_F0_RANGE[0]:
            break
        center = target / bin_hz
        half = max(1.0, 0.01 * center)
        low = max(1, int(math.floor(center - half)))
        high = min(mean.size - 2, int(math.ceil(center + half)))
        if high < low:
            continue
        index = low + int(np.argmax(mean[low : high + 1]))
        if not (mean[index] > mean[index - 1] and mean[index] >= mean[index + 1]):
            continue
        relative_db = 10.0 * math.log10(max(float(mean[index]), 1e-30) / max(float(mean[peak]), 1e-30))
        if relative_db < -HARMONIC_LOWER_PARTIAL_DB:
            continue
        lower = {
            "ratio": ratio,
            "frequency_hz": round(float(freqs[index]), 2),
            "relative_db": round(float(relative_db), 1),
        }
        break
    return {
        "strongest_hz": round(strongest, 2),
        "bin_hz": round(float(bin_hz), 3),
        "lower_partial": lower,
    }


def _validate_fundamental(audio: Audio, f0_hz: float) -> dict:
    """Check a detected f0 against the file's strongest 20-300 Hz partial.

    The harmonic profile assumes that f0 is the fundamental.  A detector that
    locks a harmonic instead produces a ladder between unrelated partials, so
    the estimate is rejected when the strongest partial is itself a harmonic of
    a strong lower partial, or when the median f0 does not match the strongest
    partial at all.
    """
    partial = _dominant_partial(audio)
    result: dict = {
        "status": "ok",
        "reason": None,
        "f0_hz": _num(f0_hz, 2),
        "strongest_partial_hz": None,
        "lower_partial": None,
    }
    if partial is None:
        result["status"] = "unknown"
        result["reason"] = "no 20-300 Hz spectrum"
        return result
    result["strongest_partial_hz"] = partial["strongest_hz"]
    result["lower_partial"] = partial["lower_partial"]
    strongest = float(partial["strongest_hz"])
    lower = partial["lower_partial"]
    if lower is not None:
        ratio = f0_hz / float(lower["frequency_hz"])
        step = int(round(ratio))
        if step >= 2 and abs(ratio - step) / step <= 0.05:
            result["status"] = "ambiguous"
            result["reason"] = (
                f"f0 {f0_hz:.1f} Hz is {step}x the strong {float(lower['frequency_hz']):.1f} Hz partial; "
                "the detected peak is a harmonic, not the fundamental"
            )
            return result
    ratio = f0_hz / strongest
    bins_apart = abs(f0_hz - strongest) / float(partial["bin_hz"])
    if abs(ratio - 1.0) <= HARMONIC_DOMINANT_TOL or bins_apart <= HARMONIC_DOMINANT_BINS:
        return result
    result["status"] = "ambiguous"
    result["reason"] = (
        f"median f0 {f0_hz:.1f} Hz is not the strongest 20-300 Hz partial ({strongest:.1f} Hz, "
        f"{bins_apart:.1f} bins away)"
    )
    return result


def _harmonic_not_applicable(reason: str, frames: dict, fundamental: dict, validation: dict) -> dict:
    return {
        "status": "not_applicable",
        "reason": reason,
        "frames": frames,
        "fundamental_hz": fundamental,
        "harmonic_delta_db": None,
        "distance_db": None,
        "thd_percent": {"reference": None, "candidate": None},
        "harmonics": HARMONIC_COUNT,
        "validation": validation,
    }


def harmonic_profile(reference: Audio, candidate: Audio) -> dict:
    """Relative first-8 harmonic levels of the bass fundamental, THD-like."""
    ref_rows, ref_f0 = _harmonic_rows(reference)
    cand_rows, cand_f0 = _harmonic_rows(candidate)
    frames = {"reference": int(ref_rows.shape[0]), "candidate": int(cand_rows.shape[0])}
    if ref_rows.shape[0] < HARMONIC_MIN_FRAMES or cand_rows.shape[0] < HARMONIC_MIN_FRAMES:
        return _harmonic_not_applicable(
            "too few frames with a detected 20-300 Hz fundamental",
            frames,
            {"reference": None, "candidate": None},
            {},
        )
    ref_median = float(np.median(ref_f0))
    cand_median = float(np.median(cand_f0))
    validation = {
        "reference": _validate_fundamental(reference, ref_median),
        "candidate": _validate_fundamental(candidate, cand_median),
    }
    cents_delta = 1200.0 * math.log2(cand_median / ref_median) if ref_median > 0.0 and cand_median > 0.0 else None
    validation["cents_delta"] = _num(cents_delta, 1)
    for side, check in (("reference", validation["reference"]), ("candidate", validation["candidate"])):
        if check["status"] != "ok":
            return _harmonic_not_applicable(
                f"{side}: {check['reason']}",
                frames,
                {"reference": _num(ref_median, 2), "candidate": _num(cand_median, 2)},
                validation,
            )
    if cents_delta is not None and abs(cents_delta) > HARMONIC_MAX_CENTS:
        return _harmonic_not_applicable(
            f"fundamental estimates differ by {abs(cents_delta):.0f} cents "
            f"({ref_median:.1f} Hz vs {cand_median:.1f} Hz); the harmonic ladders are not comparable",
            frames,
            {"reference": _num(ref_median, 2), "candidate": _num(cand_median, 2)},
            validation,
        )
    ref_db = np.median(ref_rows[:, 1:], axis=0)
    cand_db = np.median(cand_rows[:, 1:], axis=0)
    delta = cand_db - ref_db
    distance = float(np.mean(np.abs(delta)))

    def thd(values: np.ndarray) -> float:
        return float(math.sqrt(float(np.sum(10.0 ** (values / 10.0)))) * 100.0)

    return {
        "status": "ok",
        "harmonics": HARMONIC_COUNT,
        "frames": frames,
        "fundamental_hz": {
            "reference": _num(ref_median, 2),
            "candidate": _num(cand_median, 2),
        },
        "cents_delta": _num(cents_delta, 1),
        "harmonic_delta_db": [_num(value, 2) for value in delta],
        "distance_db": _num(distance, 3),
        "thd_percent": {
            "reference": _num(thd(ref_db), 2),
            "candidate": _num(thd(cand_db), 2),
        },
        "validation": validation,
    }


def transient_metrics(reference: Audio, candidate: Audio, mel_floor: float = 1e-12) -> dict:
    """Onset-flux ratio, a gain-invariant measure of attack sharpness."""
    flux_ref, _ = _onset_strength(reference.samples, reference.rate, mel_floor)
    flux_cand, _ = _onset_strength(candidate.samples, candidate.rate, mel_floor)
    size = min(flux_ref.size, flux_cand.size)
    if size < 4:
        return {"frames": size, "reference_flux_mean": None, "candidate_flux_mean": None, "flux_ratio_db": None}
    reference_mean = float(np.mean(flux_ref[:size]))
    candidate_mean = float(np.mean(flux_cand[:size]))
    ratio = (
        20.0 * math.log10(candidate_mean / reference_mean)
        if reference_mean > 1e-12 and candidate_mean > 1e-12
        else None
    )
    return {
        "frames": size,
        "reference_flux_mean": _num(reference_mean, 4),
        "candidate_flux_mean": _num(candidate_mean, 4),
        "flux_ratio_db": _num(ratio, 3),
    }


# ---------------------------------------------------------------------------
# Time and dynamics
# ---------------------------------------------------------------------------


def _rms_envelope(audio: np.ndarray, rate: int, hop: int, window_ms: float = 10.0) -> np.ndarray:
    power = np.mean(audio * audio, axis=1)
    window = max(1, int(round(window_ms * rate / 1000.0)))
    cumulative = np.concatenate(([0.0], np.cumsum(power)))
    smoothed = (cumulative[window:] - cumulative[:-window]) / window
    return np.sqrt(np.maximum(smoothed, 0.0))[::hop]


def _onset_strength(audio: np.ndarray, rate: int, mel_floor: float = 1e-12) -> tuple[np.ndarray, float]:
    n_fft, hop = 1024, 240
    power, _ = stft_power(audio, rate, n_fft, hop)
    filters = _mel_filterbank(rate, n_fft, 48, 30.0, 16000.0)
    logmel = _log_mel(power, filters, mel_floor)
    flux = np.maximum(0.0, np.diff(logmel, axis=0)).sum(axis=1)
    return flux, rate / hop


def _best_shift(
    feature_a: np.ndarray, feature_b: np.ndarray, max_frames: int
) -> tuple[float, int, np.ndarray, np.ndarray]:
    """Best lag (positive = feature_b is late) and normalised correlation."""
    n = min(feature_a.size, feature_b.size)
    a = np.asarray(feature_a[:n], dtype=np.float64)
    b = np.asarray(feature_b[:n], dtype=np.float64)
    a = a - a.mean()
    b = b - b.mean()
    cumulative_a = np.concatenate(([0.0], np.cumsum(a * a)))
    cumulative_b = np.concatenate(([0.0], np.cumsum(b * b)))
    lags = np.arange(-max_frames, max_frames + 1)
    corr = np.zeros(lags.size)
    for index, lag in enumerate(lags):
        if lag >= 0:
            i0, i1, j0, j1 = 0, n - lag, lag, n
        else:
            i0, i1, j0, j1 = -lag, n, 0, n + lag
        energy_a = cumulative_a[i1] - cumulative_a[i0]
        energy_b = cumulative_b[j1] - cumulative_b[j0]
        if energy_a <= 1e-12 or energy_b <= 1e-12:
            continue
        corr[index] = float(np.dot(a[i0:i1], b[j0:j1]) / math.sqrt(energy_a * energy_b))
    best = int(np.argmax(corr))
    return float(corr[best]), int(lags[best]), lags, corr


def _spectral_lag(
    reference_logmel: np.ndarray, candidate_logmel: np.ndarray, max_frames: int
) -> tuple[np.ndarray, np.ndarray]:
    """2-D log-mel correlation over time, averaged across informative bands."""
    n = min(reference_logmel.shape[0], candidate_logmel.shape[0])
    a = reference_logmel[:n]
    b = candidate_logmel[:n]
    a = a - a.mean(axis=0)
    b = b - b.mean(axis=0)
    std_a = a.std(axis=0)
    std_b = b.std(axis=0)
    valid = (std_a > 1e-6) & (std_b > 1e-6)
    if int(valid.sum()) < 2:
        return np.zeros(0), np.zeros(0)
    a = a[:, valid] / std_a[valid]
    b = b[:, valid] / std_b[valid]
    lags = np.arange(-max_frames, max_frames + 1)
    corr = np.zeros(lags.size)
    for index, lag in enumerate(lags):
        if lag >= 0:
            i0, i1, j0, j1 = 0, n - lag, lag, n
        else:
            i0, i1, j0, j1 = -lag, n, 0, n + lag
        if i1 - i0 < 8:
            continue
        corr[index] = float(np.mean(a[i0:i1] * b[j0:j1]))
    return lags, corr


def estimate_alignment(reference: Audio, candidate: Audio, stems: bool) -> dict:
    """Global lag from log-mel correlation, refined on a 1 ms RMS envelope."""
    hop = 512
    filters = _mel_filterbank(reference.rate, 2048, 40, 30.0, 16000.0)
    ref_power, _ = stft_power(reference.samples, reference.rate, 2048, hop)
    cand_power, _ = stft_power(candidate.samples, candidate.rate, 2048, hop)
    ref_logmel = _log_mel(ref_power, filters)
    cand_logmel = _log_mel(cand_power, filters)
    max_lag_ms = 300.0 if stems else 1500.0
    max_frames = max(1, int(round(max_lag_ms / (1000.0 * hop / reference.rate))))
    lags, corr = _spectral_lag(ref_logmel, cand_logmel, max_frames)
    if lags.size == 0:
        lag_frames = 0
        spectral_corr = 0.0
        zero_corr = 0.0
        advantage = 0.0
    else:
        prior = 0.05 * (lags.astype(np.float64) / max(1, max_frames)) ** 2
        index = int(np.argmax(corr - prior))
        zero_corr = float(corr[lags == 0][0])
        advantage = float(corr[index]) - zero_corr
        if advantage < 0.01:
            lag_frames = 0
            spectral_corr = zero_corr
            advantage = 0.0
        else:
            lag_frames = int(lags[index])
            spectral_corr = float(corr[index])
    coarse_ms = lag_frames * 1000.0 * hop / reference.rate

    hop_ms = max(1, int(round(reference.rate / 1000.0)))
    ref_env = _rms_envelope(reference.samples, reference.rate, hop_ms, window_ms=5.0)
    cand_env = _rms_envelope(candidate.samples, candidate.rate, hop_ms, window_ms=5.0)
    center = int(round(coarse_ms))
    radius = 12
    best_corr, best_lag = -2.0, center
    for lag in range(center - radius, center + radius + 1):
        corr_value = _corr_at_lag(ref_env, cand_env, lag)
        if corr_value > best_corr:
            best_corr, best_lag = corr_value, lag
    if advantage >= 0.02 and abs(coarse_ms) >= 1.0:
        lag_ms = float(best_lag) if best_corr >= 0.1 else float(coarse_ms)
    else:
        lag_ms = float(coarse_ms)

    flux_ref, _ = _onset_strength(reference.samples, reference.rate)
    flux_cand, _ = _onset_strength(candidate.samples, candidate.rate)
    onset_max = int(round(max_lag_ms / 5.0))
    onset_corr, onset_lag, _, _ = _best_shift(flux_ref, flux_cand, onset_max)

    if spectral_corr >= 0.50:
        confidence = "high"
    elif spectral_corr >= 0.20:
        confidence = "medium"
    else:
        confidence = "low"
        lag_ms = 0.0
    return {
        "lag_ms": _num(lag_ms, 1),
        "coarse_lag_ms": _num(coarse_ms, 1),
        "spectral_corr": _num(spectral_corr, 3),
        "zero_lag_corr": _num(zero_corr, 3),
        "zero_lag_tie_break": bool(advantage < 0.01),
        "onset_best_lag_ms": _num(onset_lag * 5.0, 1),
        "onset_best_corr": _num(onset_corr, 3),
        "search_ms": max_lag_ms,
        "confidence": confidence,
        "method": "log-mel 2-D correlation; optional 1 ms RMS envelope refinement",
    }


def _corr_at_lag(reference: np.ndarray, candidate: np.ndarray, lag: int) -> float:
    """Pearson correlation of reference[t] with candidate[t + lag]."""
    n = min(reference.size, candidate.size)
    if lag >= 0:
        a = reference[: n - lag]
        b = candidate[lag:n]
    else:
        a = reference[-lag:n]
        b = candidate[: n + lag]
    if a.size < 4 or a.std() < 1e-12 or b.std() < 1e-12:
        return -1.0
    return float(np.corrcoef(a, b)[0, 1])


def _align(audio: Audio, lag_ms: float) -> Audio:
    samples = audio.samples
    shift = int(round(lag_ms * audio.rate / 1000.0))
    if shift > 0:
        samples = samples[shift:]
    elif shift < 0:
        samples = np.vstack([np.zeros((-shift, samples.shape[1])), samples])
    return Audio(samples=samples, rate=audio.rate, path=audio.path)


def detect_onsets(strength: np.ndarray, frame_rate: float, min_gap_s: float = 0.08) -> list[int]:
    """Peak-pick spectral flux with a robust threshold and a minimum gap."""
    if strength.size < 3:
        return []
    smoothed = np.convolve(strength, np.ones(3) / 3.0, mode="same")
    median = float(np.median(smoothed))
    mad = float(np.median(np.abs(smoothed - median)))
    threshold = max(median + 4.0 * mad, 0.05 * float(smoothed.max()))
    candidates: list[int] = []
    for index in range(1, smoothed.size - 1):
        if smoothed[index] < threshold:
            continue
        if smoothed[index] >= smoothed[index - 1] and smoothed[index] >= smoothed[index + 1]:
            candidates.append(index)
    min_gap = max(1, int(round(min_gap_s * frame_rate)))
    accepted: list[int] = []
    for index in sorted(candidates, key=lambda value: -smoothed[value]):
        if all(abs(index - other) >= min_gap for other in accepted):
            accepted.append(index)
    return sorted(accepted)


def _attack_ms(envelope: np.ndarray, onset_s: float, sample_rate: int = 1000) -> float | None:
    """10-90% rise time of the 1 ms RMS envelope around one onset."""
    center = int(round(onset_s * sample_rate))
    start = max(0, center - 50)
    end = min(envelope.size, center + 200)
    if end - start < 5:
        return None
    segment = envelope[start:end]
    peak_offset = int(np.argmax(segment))
    peak = float(segment[peak_offset])
    if peak < 1e-4:
        return None
    baseline = float(np.min(segment[: max(1, peak_offset + 1)]))
    if peak <= baseline * 1.5:
        return None
    low = baseline + 0.1 * (peak - baseline)
    high = baseline + 0.9 * (peak - baseline)
    peak_index = start + peak_offset

    below = np.flatnonzero(envelope[max(0, peak_index - 60) : peak_index + 1] <= low)
    if below.size == 0:
        return None
    index_low = max(0, peak_index - 60) + int(below[-1])
    above = np.flatnonzero(envelope[index_low : peak_index + 1] >= high)
    if above.size == 0:
        return None
    index_high = index_low + int(above[0])
    return float(index_high - index_low)


def _onset_metrics(reference: Audio, candidate: Audio, mel_floor: float = 1e-12) -> dict:
    flux_ref, frame_rate = _onset_strength(reference.samples, reference.rate, mel_floor)
    flux_cand, _ = _onset_strength(candidate.samples, candidate.rate, mel_floor)
    max_frames = int(round(300.0 * frame_rate / 1000.0))
    best_corr, best_lag, _, _ = _best_shift(flux_ref, flux_cand, max_frames)
    onsets_ref = detect_onsets(flux_ref, frame_rate)
    onsets_cand = detect_onsets(flux_cand, frame_rate)
    envelope = _rms_envelope(reference.samples, reference.rate, max(1, reference.rate // 1000), window_ms=1.0)
    attack_ref = [
        {"time_s": _num(index / frame_rate, 3), "attack_ms": _num(_attack_ms(envelope, index / frame_rate), 2)}
        for index in onsets_ref
    ]
    envelope_cand = _rms_envelope(candidate.samples, candidate.rate, max(1, candidate.rate // 1000), window_ms=1.0)
    attack_cand = [
        {"time_s": _num(index / frame_rate, 3), "attack_ms": _num(_attack_ms(envelope_cand, index / frame_rate), 2)}
        for index in onsets_cand
    ]

    paired: list[float] = []
    strength_pairs: list[float] = []
    for ref_position, index in enumerate(onsets_ref):
        time = index / frame_rate
        nearest = min((abs(other / frame_rate - time), other) for other in onsets_cand) if onsets_cand else None
        if nearest is None or nearest[0] > 0.06:
            continue
        candidate_index = nearest[1]
        value_ref = attack_ref[ref_position]["attack_ms"]
        value_cand = attack_cand[onsets_cand.index(candidate_index)]["attack_ms"]
        if value_ref is not None and value_cand is not None:
            paired.append(value_cand - value_ref)
        # Per-attack flux maximum: a strength ratio that density cannot inflate.
        before = float(np.max(flux_ref[max(0, index - 3) : index + 4]))
        after = float(np.max(flux_cand[max(0, candidate_index - 3) : candidate_index + 4]))
        if before > 1e-9 and after > 1e-9:
            strength_pairs.append(20.0 * math.log10(after / before))

    duration = max(1e-9, min(reference.duration_s, candidate.duration_s))
    density_ref = len(onsets_ref) / duration
    density_cand = len(onsets_cand) / duration
    if density_ref > 0.0 and density_cand > 0.0:
        density_ratio = 20.0 * math.log10(density_cand / density_ref)
    elif density_ref <= 0.0 and density_cand <= 0.0:
        density_ratio = 0.0
    else:
        density_ratio = None

    def attack_summary(values: list[dict]) -> dict:
        valid = [entry["attack_ms"] for entry in values if entry["attack_ms"] is not None]
        return {
            "count": len(values),
            "median_ms": _num(float(np.median(valid)), 2) if valid else None,
            "onsets": values,
        }

    return {
        "onset": {
            "reference_count": len(onsets_ref),
            "candidate_count": len(onsets_cand),
            "best_lag_ms": _num(best_lag * 5.0, 1),
            "best_correlation": _num(best_corr, 3),
            "search_ms": 300.0,
            "reference_density_per_s": _num(density_ref, 3),
            "candidate_density_per_s": _num(density_cand, 3),
            "density_ratio_db": _num(density_ratio, 3),
            "paired_count": len(strength_pairs),
            "onset_strength_median_db": _num(float(np.median(strength_pairs)), 3) if strength_pairs else None,
        },
        "attack": {
            "reference": attack_summary(attack_ref),
            "candidate": attack_summary(attack_cand),
            "paired_count": len(paired),
            "median_delta_ms": _num(float(np.median(paired)), 2) if paired else None,
            "median_abs_delta_ms": _num(float(np.median(np.abs(paired))), 2) if paired else None,
        },
    }


def _envelope_correlation(reference: Audio, candidate: Audio) -> dict:
    hop = max(1, int(round(reference.rate / 100.0)))
    env_ref = _rms_envelope(reference.samples, reference.rate, hop, window_ms=10.0)
    env_cand = _rms_envelope(candidate.samples, candidate.rate, hop, window_ms=10.0)
    size = min(env_ref.size, env_cand.size)
    if size < 4:
        return {"correlation": None, "frames": size}
    a = env_ref[:size]
    b = env_cand[:size]
    if a.std() < 1e-9 or b.std() < 1e-9:
        if a.std() < 1e-9 and b.std() < 1e-9:
            return {"correlation": 1.0, "frames": size}
        return {"correlation": None, "frames": size}
    return {"correlation": _num(float(np.corrcoef(a, b)[0, 1]), 4), "frames": size}


def _dtw_features(audio: Audio) -> np.ndarray:
    power, _ = stft_power(audio.samples, audio.rate, 2048, 1024)
    filters = _mel_filterbank(audio.rate, 2048, 40, 30.0, 16000.0)
    logmel = _log_mel(power, filters)
    dct = _dct_matrix(40, 14)
    mfcc = logmel @ dct.T
    features = mfcc[:, 0:14]
    mean = features.mean(axis=0)
    std = features.std(axis=0)
    std[std < 1e-9] = 1.0
    return (features - mean) / std


def _banded_dtw(a: np.ndarray, b: np.ndarray, band: int) -> tuple[list[tuple[int, int]], float] | None:
    """Sakoe-Chiba banded DTW with Euclidean frame cost; numpy-only DP."""
    n, m = a.shape[0], b.shape[0]
    if n < 2 or m < 2:
        return None
    band = max(int(band), abs(n - m) + 2, 1)
    width = 2 * band + 1
    infinity = 1e18
    previous = np.full(width, infinity)
    back = np.zeros((n, width), dtype=np.int8)

    for i in range(n):
        current = np.full(width, infinity)
        base = i - band
        for k in range(width):
            j = base + k
            if j < 0 or j >= m:
                continue
            difference = a[i] - b[j]
            cost = float(np.dot(difference, difference))
            if i == 0 and j == 0:
                current[k] = cost
                continue
            best = infinity
            pointer = 0
            if i > 0 and k < width and previous[k] < best:
                best = previous[k]
                pointer = 0
            if i > 0 and k + 1 < width and previous[k + 1] < best:
                best = previous[k + 1]
                pointer = 1
            if k > 0 and current[k - 1] < best:
                best = current[k - 1]
                pointer = 2
            if best < infinity:
                current[k] = best + cost
                back[i, k] = pointer
        previous = current

    end_k = (m - 1) - ((n - 1) - band)
    if end_k < 0 or end_k >= width or previous[end_k] >= infinity:
        return None
    path: list[tuple[int, int]] = []
    i, k = n - 1, end_k
    while i >= 0 and k >= 0:
        j = i - band + k
        path.append((i, j))
        if i == 0 and j == 0:
            break
        pointer = int(back[i, k])
        if pointer == 0:
            i = i - 1
        elif pointer == 1:
            i, k = i - 1, k + 1
        else:
            k -= 1
    path.reverse()
    return path, float(previous[end_k])


def _dtw_metrics(reference: Audio, candidate: Audio, stems: bool) -> dict:
    features_ref = _dtw_features(reference)
    features_cand = _dtw_features(candidate)
    hop_s = 1024.0 / reference.rate
    duration = min(reference.duration_s, candidate.duration_s)
    if stems:
        band_s = min(max(0.10, 0.03 * duration), 0.40)
    else:
        band_s = min(max(0.25, 0.05 * duration), 1.50)
    band_frames = max(2, int(round(band_s / hop_s)))
    result = _banded_dtw(features_ref, features_cand, band_frames)
    note = (
        "Mean warp includes low-energy and silent regions, where the path can drift cheaply. "
        "On sparse arrangements it is an upper bound and a path-shape diagnostic, not a precise timing error."
    )
    if result is None:
        return {
            "mean_abs_warp_ms": None,
            "max_abs_warp_ms": None,
            "median_signed_warp_ms": None,
            "band_ms": _num(band_s * 1000.0, 1),
            "hop_ms": _num(hop_s * 1000.0, 2),
            "frames": 0,
            "note": note,
        }
    path, cost = result
    warps = np.array([j - i for i, j in path], dtype=np.float64) * hop_s * 1000.0
    return {
        "mean_abs_warp_ms": _num(float(np.mean(np.abs(warps))), 2),
        "max_abs_warp_ms": _num(float(np.max(np.abs(warps))), 2),
        "median_signed_warp_ms": _num(float(np.median(warps)), 2),
        "path_cost": _num(cost, 3),
        "band_ms": _num(band_s * 1000.0, 1),
        "hop_ms": _num(hop_s * 1000.0, 2),
        "frames": len(path),
        "note": note,
    }


# ---------------------------------------------------------------------------
# Stereo and pitch
# ---------------------------------------------------------------------------


def stereo_metrics(reference: Audio, candidate: Audio) -> dict:
    def measure(audio: Audio) -> dict:
        if audio.channels < 2:
            return {"mono": True, "correlation": None, "mid_side_ratio_db": None}
        left = audio.samples[:, 0]
        right = audio.samples[:, 1]
        if left.std() < 1e-12 or right.std() < 1e-12:
            correlation = None
        else:
            correlation = _num(float(np.corrcoef(left, right)[0, 1]), 4)
        mid = (left + right) * 0.5
        side = (left - right) * 0.5
        mid_power = float(np.mean(mid * mid))
        side_power = float(np.mean(side * side))
        ratio = (
            None
            if mid_power <= 1e-12 and side_power <= 1e-12
            else float(np.clip(10.0 * math.log10(max(mid_power, 1e-12) / max(side_power, 1e-12)), -60.0, 60.0))
        )
        return {
            "mono": False,
            "correlation": correlation,
            "mid_side_ratio_db": _num(ratio, 2),
        }

    return {"reference": measure(reference), "candidate": measure(candidate)}


def _f0_track(audio: Audio, fmin: float, fmax: float) -> tuple[np.ndarray, np.ndarray]:
    """YIN-style f0 per 50 ms frame.

    The difference function is computed from the FFT autocorrelation, then
    normalised by its cumulative mean; the first dip below 0.25 is taken as the
    period.  This avoids the octave/harmonic errors of raw autocorrelation on
    bright synth waveforms.  ``clarity`` is 1 minus the normalised difference
    at the chosen period.
    """
    mono = audio.mono
    frame = 8192
    hop = max(1, int(round(0.05 * audio.rate)))
    if mono.size < frame:
        return np.zeros(0), np.zeros(0, dtype=bool), np.zeros(0)
    count = 1 + (mono.size - frame) // hop
    estimates = np.full(count, np.nan)
    voiced = np.zeros(count, dtype=bool)
    clarity = np.zeros(count)
    lag_min = max(2, int(math.floor(audio.rate / fmax * 0.9)))
    tau_max = min(frame - 2, int(math.ceil(audio.rate / fmin * 1.1)))
    if tau_max <= lag_min + 2:
        return estimates, voiced, clarity
    nfft = 1 << (frame + tau_max - 1).bit_length()
    taus = np.arange(tau_max + 1, dtype=np.float64)

    for index in range(count):
        segment = mono[index * hop : index * hop + frame]
        rms = float(np.sqrt(np.mean(segment * segment)))
        if rms < 1e-4:
            continue
        centered = segment - segment.mean()
        spectrum = np.fft.rfft(centered, n=nfft)
        autocorrelation = np.fft.irfft(spectrum.real**2 + spectrum.imag**2, n=nfft)[: tau_max + 1]
        cumulative = np.concatenate(([0.0], np.cumsum(centered * centered)))
        energy_head = cumulative[frame - taus.astype(int)]
        energy_tail = cumulative[frame] - cumulative[taus.astype(int)]
        difference = np.maximum(energy_head + energy_tail - 2.0 * autocorrelation, 0.0)
        running = np.cumsum(difference[1:])
        normalized = np.ones(tau_max + 1)
        normalized[1:] = difference[1:] * taus[1:] / np.maximum(running, 1e-12)

        best: int | None = None
        for tau in range(lag_min + 1, tau_max):
            if normalized[tau] <= normalized[tau - 1] and normalized[tau] <= normalized[tau + 1]:
                if normalized[tau] < 0.25:
                    best = tau
                    break
        if best is None:
            candidate = lag_min + int(np.argmin(normalized[lag_min : tau_max + 1]))
            if normalized[candidate] > 0.35:
                continue
            best = candidate
        if best <= lag_min or best >= tau_max:
            continue
        left, center, right = normalized[best - 1], normalized[best], normalized[best + 1]
        denominator = left - 2.0 * center + right
        offset = 0.5 * (left - right) / denominator if denominator > 1e-12 else 0.0
        offset = float(np.clip(offset, -0.5, 0.5))
        frequency = audio.rate / (best + offset)
        if fmin <= frequency <= fmax:
            estimates[index] = frequency
            voiced[index] = True
            clarity[index] = 1.0 - float(normalized[best])
    return estimates, voiced, clarity


def pitch_metrics(reference: Audio, candidate: Audio, stems: bool, audible: bool = True) -> dict:
    fmin, fmax = (30.0, 1000.0) if stems else (50.0, 1000.0)
    f0_ref, voiced_ref, clarity_ref = _f0_track(reference, fmin, fmax)
    f0_cand, voiced_cand, clarity_cand = _f0_track(candidate, fmin, fmax)
    count = min(f0_ref.size, f0_cand.size)
    clarity_gate = 0.5
    both = voiced_ref[:count] & voiced_cand[:count] & (clarity_ref[:count] >= clarity_gate)
    both &= clarity_cand[:count] >= clarity_gate
    errors = np.array([], dtype=np.float64)
    if np.any(both):
        ratio = f0_cand[:count][both] / f0_ref[:count][both]
        errors = 1200.0 * np.log2(ratio)
    within = int(np.sum(np.abs(errors) <= 25.0)) if errors.size else 0
    ref_voiced = f0_ref[:count][voiced_ref[:count]]
    cand_voiced = f0_cand[:count][voiced_cand[:count]]
    ref_clarity = clarity_ref[:count][voiced_ref[:count]]
    cand_clarity = clarity_cand[:count][voiced_cand[:count]]
    median_ref_clarity = float(np.median(ref_clarity)) if ref_clarity.size else 0.0
    median_cand_clarity = float(np.median(cand_clarity)) if cand_clarity.size else 0.0

    status = "ok"
    reason: str | None = None
    if not stems:
        status, reason = "not_applicable", "full mix is polyphonic; pass --stems with a monophonic stem"
    elif not audible:
        status, reason = "not_applicable", "at least one render is near-silent"
    elif errors.size < 10:
        status, reason = "not_applicable", "fewer than 10 frames with confident f0"
    elif median_ref_clarity < 0.6 or median_cand_clarity < 0.6:
        status, reason = "not_applicable", "weak periodicity (polyphonic or sub-heavy source)"
    elif float(np.median(np.abs(errors))) > 1200.0:
        status, reason = "not_applicable", "octave/harmonic ambiguity (median |cents| > 1200)"
    confidence = "medium" if status == "ok" else "low"

    def visible(value: float | None, digits: int = 2) -> float | None:
        return _num(value, digits) if status == "ok" else None

    return {
        "f0_range_hz": [fmin, fmax],
        "status": status,
        "reason": reason,
        "confidence": confidence,
        "clarity_gate": clarity_gate,
        "reference_frames": int(count),
        "reference_voiced_frames": int(np.sum(voiced_ref[:count])),
        "candidate_voiced_frames": int(np.sum(voiced_cand[:count])),
        "reference_median_clarity": _num(median_ref_clarity, 4) if ref_clarity.size else None,
        "candidate_median_clarity": _num(median_cand_clarity, 4) if cand_clarity.size else None,
        "compared_frames": int(np.sum(both)),
        "reference_median_f0_hz": visible(float(np.median(ref_voiced)) if ref_voiced.size else None),
        "candidate_median_f0_hz": visible(float(np.median(cand_voiced)) if cand_voiced.size else None),
        "median_cents_error": visible(float(np.median(errors)) if errors.size else None),
        "median_abs_cents_error": visible(float(np.median(np.abs(errors))) if errors.size else None),
        "p90_abs_cents_error": visible(float(np.percentile(np.abs(errors), 90)) if errors.size else None),
        "fraction_within_25_cents": visible(within / errors.size if errors.size else None, 4),
    }


# ---------------------------------------------------------------------------
# Score and full analysis
# ---------------------------------------------------------------------------


def _density_term(onset: dict) -> float | None:
    """Events-per-second mismatch in dB, N/A on too few detected events."""
    reference = onset["reference_count"]
    candidate = onset["candidate_count"]
    if reference == 0 and candidate == 0:
        return 0.0
    if reference < DENSITY_MIN_EVENTS or candidate < DENSITY_MIN_EVENTS:
        return None
    return min(abs(float(onset["density_ratio_db"])) / DENSITY_SCALE_DB, 1.0)


def _weighted_distance(terms: dict, weights: dict) -> tuple[float, float]:
    available = sum(weights[name] for name, term in terms.items() if term is not None)
    if available <= 0.0:
        return 0.0, 0.0
    distance = sum(weights[name] * term for name, term in terms.items() if term is not None) / available
    return distance, available


def _score(
    level: dict,
    spectrum: dict,
    time: dict,
    body: dict,
    tilt: dict,
    harmonics: dict,
    bands: dict,
    warnings: list[str],
) -> dict:
    reference_level = level["reference"]
    candidate_level = level["candidate"]
    silent = (
        (reference_level["rms_dbfs"] or -120.0) < -60.0
        or (candidate_level["rms_dbfs"] or -120.0) < -60.0
        or reference_level["peak_dbfs"] < -60.0
        or candidate_level["peak_dbfs"] < -60.0
    )
    spectral_value = spectrum["log_mel"]["mean_abs_delta_db_level_matched"]
    spectral_db = SPECTRAL_SCALE_DB if spectral_value is None else float(spectral_value)
    lufs_delta = level["delta"]["integrated_lufs"]
    if lufs_delta is not None:
        level_delta = abs(float(lufs_delta))
        level_source = "integrated_lufs"
    else:
        level_delta = abs(float(level["delta"]["rms_dbfs"]))
        level_source = "rms"
    envelope_corr = time["envelope_correlation"]["correlation"]

    spectral_term = min(spectral_db / SPECTRAL_SCALE_DB, 1.0)
    level_term = min(level_delta / LEVEL_SCALE_LU, 1.0)
    envelope_term = 1.0 - min(max(float(envelope_corr), 0.0), 1.0) if envelope_corr is not None else 0.0

    low_delta = body["low"]["rms_delta_db"]
    mid_delta = body["mid"]["rms_delta_db"]
    body_term = None
    if low_delta is not None and mid_delta is not None:
        body_term = 0.5 * (
            min(abs(float(low_delta)) / BODY_SCALE_DB, 1.0) + min(abs(float(mid_delta)) / BODY_SCALE_DB, 1.0)
        )
    tilt_db = tilt["tilt_db"]
    tilt_term = min(abs(float(tilt_db)) / TILT_SCALE_DB, 1.0) if tilt_db is not None else None
    crest_delta = level["delta"]["crest_db"]
    crest_term = min(abs(float(crest_delta)) / CREST_SCALE_DB, 1.0) if crest_delta is not None else 0.0
    flux_ratio = time["transient"]["flux_ratio_db"]
    transient_term = min(abs(float(flux_ratio)) / FLUX_SCALE_DB, 1.0) if flux_ratio is not None else 0.0
    crest_attack_term = crest_term
    harmonic_term = (
        min(float(harmonics["distance_db"]) / HARMONIC_SCALE_DB, 1.0)
        if harmonics["status"] == "ok" and harmonics["distance_db"] is not None
        else None
    )

    mfcc_cosine = spectrum["mfcc"]["mean_cosine_distance"]
    mfcc_term = min(float(mfcc_cosine) / MFCC_SCALE, 1.0) if mfcc_cosine is not None else None
    third = third_octave_rms(bands["third_octave"])
    third_db = third["level_matched_rms_delta_db"]
    third_term = min(float(third_db) / THIRD_OCTAVE_SCALE_DB, 1.0) if third_db is not None else None
    lsd_db = spectrum["lsd"]["level_matched_db"]
    lsd_term = min(float(lsd_db) / LSD_SCALE_DB, 1.0) if lsd_db is not None else None
    onset_db = time["onset"]["onset_strength_median_db"]
    onset_term = min(abs(float(onset_db)) / ONSET_SCALE_DB, 1.0) if onset_db is not None else None
    density_term = _density_term(time["onset"])

    terms = {
        "spectral": spectral_term,
        "level": level_term,
        "envelope": envelope_term,
        "body": body_term,
        "tilt": tilt_term,
        "crest_attack": crest_attack_term,
        "harmonic": harmonic_term,
        "mfcc": mfcc_term,
        "third_octave": third_term,
        "lsd": lsd_term,
        "onset": onset_term,
        "density": density_term,
    }
    distance, _ = _weighted_distance(terms, SCORE_WEIGHTS)
    value = 100.0 * (1.0 - distance)
    legacy_distance = (
        LEGACY_SCORE_WEIGHTS["spectral"] * spectral_term
        + LEGACY_SCORE_WEIGHTS["level"] * level_term
        + LEGACY_SCORE_WEIGHTS["envelope"] * envelope_term
    )
    legacy_value = 100.0 * (1.0 - legacy_distance)
    previous_terms = {
        "spectral": spectral_term,
        "level": level_term,
        "envelope": envelope_term,
        "body": body_term,
        "tilt": tilt_term,
        "crest_attack": 0.5 * (crest_term + transient_term),
        "harmonic": harmonic_term,
    }
    previous_distance, _ = _weighted_distance(previous_terms, PREVIOUS_SCORE_WEIGHTS)
    previous_value = 100.0 * (1.0 - previous_distance)
    if silent:
        value = 0.0
        legacy_value = 0.0
        previous_value = 0.0
        warnings.append("headline score pinned to 0 because at least one render is near-silent")
    if value >= 90.0:
        label = "close"
    elif value >= 75.0:
        label = "audible differences"
    elif value >= 50.0:
        label = "clearly different"
    else:
        label = "broken / very different"

    details = {
        "spectral": {"distance_db": _num(spectral_db, 3), "scale_db": SPECTRAL_SCALE_DB},
        "level": {"delta": _num(level_delta, 3), "source": level_source, "scale": LEVEL_SCALE_LU},
        "envelope": {"correlation": _num(envelope_corr, 4)},
        "body": {
            "low_rms_delta_db": low_delta,
            "mid_rms_delta_db": mid_delta,
            "scale_db": BODY_SCALE_DB,
        },
        "tilt": {"tilt_db": tilt_db, "scale_db": TILT_SCALE_DB},
        "crest_attack": {
            "crest_delta_db": _num(crest_delta, 2),
            "flux_ratio_db": _num(flux_ratio, 2),
            "crest_scale_db": CREST_SCALE_DB,
            "flux_scale_db": FLUX_SCALE_DB,
            "attack_strength_scored_in": "onset",
        },
        "harmonic": {
            "distance_db": harmonics["distance_db"],
            "status": harmonics["status"],
            "scale_db": HARMONIC_SCALE_DB,
        },
        "mfcc": {"cosine_distance": _num(mfcc_cosine, 4), "scale": MFCC_SCALE},
        "third_octave": {
            "rms_delta_db": third["rms_delta_db"],
            "level_matched_rms_delta_db": third_db,
            "bands_used": third["bands_used"],
            "scale_db": THIRD_OCTAVE_SCALE_DB,
        },
        "lsd": {"level_matched_db": _num(lsd_db, 3), "scale_db": LSD_SCALE_DB},
        "onset": {
            "median_delta_db": _num(onset_db, 3),
            "paired_count": time["onset"]["paired_count"],
            "scale_db": ONSET_SCALE_DB,
        },
        "density": {
            "reference_per_s": time["onset"]["reference_density_per_s"],
            "candidate_per_s": time["onset"]["candidate_density_per_s"],
            "ratio_db": _num(time["onset"]["density_ratio_db"], 3),
            "scale_db": DENSITY_SCALE_DB,
            "min_events": DENSITY_MIN_EVENTS,
        },
    }
    components = {}
    for name, term in terms.items():
        component = {
            "term": _num(term, 4),
            "weight": SCORE_WEIGHTS[name],
            "contribution": _num(SCORE_WEIGHTS[name] * term, 4) if term is not None else None,
            **details[name],
        }
        if name in LEGACY_SCORE_WEIGHTS:
            component["legacy_weight"] = LEGACY_SCORE_WEIGHTS[name]
            component["legacy_contribution"] = _num(LEGACY_SCORE_WEIGHTS[name] * term, 4)
        components[name] = component

    return {
        "value": _num(value, 1),
        "label": label,
        "legacy_value": _num(legacy_value, 1),
        "previous_value": _num(previous_value, 1),
        "weights": SCORE_WEIGHTS,
        "previous_weights": PREVIOUS_SCORE_WEIGHTS,
        "legacy_weights": LEGACY_SCORE_WEIGHTS,
        "components": components,
        "note": (
            "Heuristic 0-100 similarity, 100 = identical. Compare only within the same "
            "track/gain staging; it is not a perceptual MOS. `legacy_value` is the pre-1.1 "
            "three-term score; `previous_value` is the v1.2.0 seven-term score."
        ),
    }


def _log_mel_level_matched(reference: Audio, candidate: Audio, n_fft: int = 2048, hop: int = 512) -> float:
    """Mean level-matched log-mel distance on already gain-matched signals."""
    ref_power, _ = stft_power(reference.samples, reference.rate, n_fft, hop)
    cand_power, _ = stft_power(candidate.samples, candidate.rate, n_fft, hop)
    frames = min(ref_power.shape[0], cand_power.shape[0])
    filters = _mel_filterbank(reference.rate, n_fft, 64, 30.0, 16000.0)
    ref_logmel = _log_mel(ref_power[:frames], filters, 1e-30)
    cand_logmel = _log_mel(cand_power[:frames], filters, 1e-30)
    floor_db = float(max(ref_logmel.max(), cand_logmel.max()) - 80.0)
    difference = np.maximum(ref_logmel, floor_db) - np.maximum(cand_logmel, floor_db)
    return float(np.mean(np.abs(difference - np.median(difference))))


def _gain_matched(reference: Audio, candidate: Audio) -> tuple[Audio, Audio]:
    """Scale both signals to a common RMS, removing a uniform gain difference."""
    reference_power = float(np.mean(reference.samples * reference.samples))
    candidate_power = float(np.mean(candidate.samples * candidate.samples))
    if reference_power <= 0.0 or candidate_power <= 0.0:
        return reference, candidate
    target = math.sqrt(reference_power * candidate_power)
    reference_gain = target / math.sqrt(reference_power)
    candidate_gain = target / math.sqrt(candidate_power)
    return (
        Audio(reference.samples * reference_gain, reference.rate, reference.path),
        Audio(candidate.samples * candidate_gain, candidate.rate, candidate.path),
    )


def _invariant_score(
    level: dict,
    spectral_db: float,
    shape: dict,
    third: dict,
    time: dict,
    body: dict,
    tilt: dict,
    harmonics: dict,
    warnings: list[str],
) -> dict:
    """Level-invariant 0-100 score: no level term, every term gain-matched."""
    reference_level = level["reference"]
    candidate_level = level["candidate"]
    silent = (
        (reference_level["rms_dbfs"] or -120.0) < -60.0
        or (candidate_level["rms_dbfs"] or -120.0) < -60.0
        or reference_level["peak_dbfs"] < -60.0
        or candidate_level["peak_dbfs"] < -60.0
    )
    envelope_corr = time["envelope_correlation"]["correlation"]

    spectral_term = min(float(spectral_db) / SPECTRAL_SCALE_DB, 1.0)
    envelope_term = 1.0 - min(max(float(envelope_corr), 0.0), 1.0) if envelope_corr is not None else 0.0
    low_delta = body["low"]["rms_delta_db"]
    mid_delta = body["mid"]["rms_delta_db"]
    body_term = None
    if low_delta is not None and mid_delta is not None:
        body_term = 0.5 * (
            min(abs(float(low_delta)) / BODY_SCALE_DB, 1.0) + min(abs(float(mid_delta)) / BODY_SCALE_DB, 1.0)
        )
    tilt_db = tilt["tilt_db"]
    tilt_term = min(abs(float(tilt_db)) / TILT_SCALE_DB, 1.0) if tilt_db is not None else None
    crest_delta = level["delta"]["crest_db"]
    crest_term = min(abs(float(crest_delta)) / CREST_SCALE_DB, 1.0) if crest_delta is not None else 0.0
    flux_ratio = time["transient"]["flux_ratio_db"]
    transient_term = min(abs(float(flux_ratio)) / FLUX_SCALE_DB, 1.0) if flux_ratio is not None else 0.0
    crest_attack_term = crest_term
    harmonic_term = (
        min(float(harmonics["distance_db"]) / HARMONIC_SCALE_DB, 1.0)
        if harmonics["status"] == "ok" and harmonics["distance_db"] is not None
        else None
    )
    mfcc_cosine = shape["mfcc"]["mean_cosine_distance"]
    mfcc_term = min(float(mfcc_cosine) / MFCC_SCALE, 1.0) if mfcc_cosine is not None else None
    third_stats = third_octave_rms(third)
    third_db = third_stats["level_matched_rms_delta_db"]
    third_term = min(float(third_db) / THIRD_OCTAVE_SCALE_DB, 1.0) if third_db is not None else None
    lsd_db = shape["lsd"]["level_matched_db"]
    lsd_term = min(float(lsd_db) / LSD_SCALE_DB, 1.0) if lsd_db is not None else None
    onset_db = time["onset"]["onset_strength_median_db"]
    onset_term = min(abs(float(onset_db)) / ONSET_SCALE_DB, 1.0) if onset_db is not None else None
    density_term = _density_term(time["onset"])

    terms = {
        "spectral": spectral_term,
        "envelope": envelope_term,
        "body": body_term,
        "tilt": tilt_term,
        "crest_attack": crest_attack_term,
        "harmonic": harmonic_term,
        "mfcc": mfcc_term,
        "third_octave": third_term,
        "lsd": lsd_term,
        "onset": onset_term,
        "density": density_term,
    }
    distance, _ = _weighted_distance(terms, LEVEL_INVARIANT_WEIGHTS)
    value = 100.0 * (1.0 - distance)
    previous_terms = {
        "spectral": spectral_term,
        "envelope": envelope_term,
        "body": body_term,
        "tilt": tilt_term,
        "crest_attack": 0.5 * (crest_term + transient_term),
        "harmonic": harmonic_term,
    }
    previous_distance, _ = _weighted_distance(previous_terms, PREVIOUS_LEVEL_INVARIANT_WEIGHTS)
    previous_value = 100.0 * (1.0 - previous_distance)
    if silent:
        value = 0.0
        previous_value = 0.0
        if not any("near-silent" in warning for warning in warnings):
            warnings.append("level-invariant score pinned to 0 because at least one render is near-silent")
    if value >= 90.0:
        label = "close"
    elif value >= 75.0:
        label = "audible differences"
    elif value >= 50.0:
        label = "clearly different"
    else:
        label = "broken / very different"

    details = {
        "spectral": {"distance_db": _num(spectral_db, 3), "scale_db": SPECTRAL_SCALE_DB},
        "envelope": {"correlation": _num(envelope_corr, 4)},
        "body": {
            "low_rms_delta_db": low_delta,
            "mid_rms_delta_db": mid_delta,
            "scale_db": BODY_SCALE_DB,
            "gain_normalised": True,
        },
        "tilt": {"tilt_db": tilt_db, "scale_db": TILT_SCALE_DB},
        "crest_attack": {
            "crest_delta_db": _num(crest_delta, 2),
            "flux_ratio_db": _num(flux_ratio, 2),
            "crest_scale_db": CREST_SCALE_DB,
            "flux_scale_db": FLUX_SCALE_DB,
            "attack_strength_scored_in": "onset",
        },
        "harmonic": {
            "distance_db": harmonics["distance_db"],
            "status": harmonics["status"],
            "scale_db": HARMONIC_SCALE_DB,
        },
        "mfcc": {"cosine_distance": _num(mfcc_cosine, 4), "scale": MFCC_SCALE},
        "third_octave": {
            "rms_delta_db": third_stats["rms_delta_db"],
            "level_matched_rms_delta_db": third_db,
            "scale_db": THIRD_OCTAVE_SCALE_DB,
            "gain_normalised": True,
        },
        "lsd": {"level_matched_db": _num(lsd_db, 3), "scale_db": LSD_SCALE_DB},
        "onset": {
            "median_delta_db": _num(onset_db, 3),
            "paired_count": time["onset"]["paired_count"],
            "scale_db": ONSET_SCALE_DB,
            "gain_normalised": True,
        },
        "density": {
            "reference_per_s": time["onset"]["reference_density_per_s"],
            "candidate_per_s": time["onset"]["candidate_density_per_s"],
            "ratio_db": _num(time["onset"]["density_ratio_db"], 3),
            "scale_db": DENSITY_SCALE_DB,
            "min_events": DENSITY_MIN_EVENTS,
        },
    }
    components = {}
    for name, term in terms.items():
        components[name] = {
            "term": _num(term, 4),
            "weight": LEVEL_INVARIANT_WEIGHTS[name],
            "contribution": _num(LEVEL_INVARIANT_WEIGHTS[name] * term, 4) if term is not None else None,
            **details[name],
        }
    lufs_delta = level["delta"]["integrated_lufs"]
    level_delta = lufs_delta if lufs_delta is not None else level["delta"]["rms_dbfs"]
    level_source = "integrated_lufs" if lufs_delta is not None else "rms"
    return {
        "value": _num(value, 1),
        "label": label,
        "previous_value": _num(previous_value, 1),
        "weights": LEVEL_INVARIANT_WEIGHTS,
        "previous_weights": PREVIOUS_LEVEL_INVARIANT_WEIGHTS,
        "components": components,
        "level": {
            "delta_lu": _num(level_delta, 2),
            "delta_rms_db": _num(level["delta"]["rms_dbfs"], 2),
            "source": level_source,
            "excluded": True,
        },
        "note": (
            "Level-invariant 0-100 similarity: both signals are scaled to a common RMS before the "
            "spectral, body and onset terms and the loudness/level term is excluded, so a uniform "
            "gain change cannot move it. The absolute `score` beside it still includes level and body; "
            "read that one for gain fixes and this one for timbre/character. Not a perceptual MOS."
        ),
    }


def analyze_audio(reference: Audio, candidate: Audio, stems: bool = False) -> dict:
    """Compare two decoded signals and return a JSON-serialisable result."""
    if reference.channels > 2 or candidate.channels > 2:
        raise ValueError("only mono and stereo files are supported")
    reference_meta = {
        "path": reference.path,
        "sample_rate": reference.rate,
        "channels": reference.channels,
        "duration_s": reference.duration_s,
    }
    candidate_meta = {
        "path": candidate.path,
        "sample_rate": candidate.rate,
        "channels": candidate.channels,
        "duration_s": candidate.duration_s,
    }
    resampled = reference.rate != candidate.rate
    if resampled:
        candidate = Audio(
            samples=_resample(candidate.samples, candidate.rate, reference.rate),
            rate=reference.rate,
            path=candidate.path,
        )
    warnings: list[str] = []
    if resampled:
        warnings.append(
            f"sample-rate mismatch: candidate {candidate_meta['sample_rate']} Hz resampled to "
            f"{reference_meta['sample_rate']} Hz for analysis"
        )
    if abs(candidate_meta["duration_s"] - reference_meta["duration_s"]) > 0.1:
        warnings.append(
            f"duration mismatch: reference {reference_meta['duration_s']:.3f} s, "
            f"candidate {candidate_meta['duration_s']:.3f} s"
        )
    if reference.channels == 1 or candidate.channels == 1:
        warnings.append("mono input: stereo correlation and mid/side ratio are null")

    level = {
        "reference": level_metrics(reference),
        "candidate": level_metrics(candidate),
        "delta": {},
    }
    for key in ("peak_dbfs", "true_peak_dbtp", "rms_dbfs", "crest_db", "integrated_lufs"):
        ref_value = level["reference"][key]
        cand_value = level["candidate"][key]
        level["delta"][key] = (
            _num(cand_value - ref_value, 2) if ref_value is not None and cand_value is not None else None
        )

    alignment = estimate_alignment(reference, candidate, stems)
    if alignment["confidence"] == "low":
        warnings.append(
            f"weak alignment evidence (spectral corr {alignment['spectral_corr']}); global lag forced to 0 ms"
        )
    reference_aligned = _align(reference, 0.0)
    candidate_aligned = _align(candidate, float(alignment["lag_ms"] or 0.0))
    frames = min(reference_aligned.samples.shape[0], candidate_aligned.samples.shape[0])
    if frames < 1024:
        raise ValueError("aligned overlap is shorter than one analysis window")
    reference_aligned = Audio(reference_aligned.samples[:frames], reference.rate, reference.path)
    candidate_aligned = Audio(candidate_aligned.samples[:frames], candidate.rate, candidate.path)

    spectrum = spectral_shape(reference_aligned, candidate_aligned)
    bands = band_energies(reference_aligned, candidate_aligned)
    tilt = tilt_metrics(bands["octave"])
    body = body_metrics(reference_aligned, candidate_aligned)
    reference_matched, candidate_matched = _gain_matched(reference_aligned, candidate_aligned)
    body_invariant = body_metrics(reference_matched, candidate_matched)
    bands_invariant = band_energies(reference_matched, candidate_matched)
    tilt_invariant = tilt_metrics(bands_invariant["octave"])
    spectral_invariant_db = _log_mel_level_matched(reference_matched, candidate_matched)
    shape_invariant = _shape_distances(reference_matched, candidate_matched, floor=1e-30)
    time = _onset_metrics(reference_aligned, candidate_aligned)
    time["envelope_correlation"] = _envelope_correlation(reference_aligned, candidate_aligned)
    time["transient"] = transient_metrics(reference_aligned, candidate_aligned)
    time["dtw"] = _dtw_metrics(reference_aligned, candidate_aligned, stems)
    time_invariant = dict(time)
    time_invariant["transient"] = transient_metrics(reference_matched, candidate_matched, 1e-30)
    time_invariant["onset"] = _onset_metrics(reference_matched, candidate_matched, 1e-30)["onset"]
    harmonics = harmonic_profile(reference_aligned, candidate_aligned)
    stereo = stereo_metrics(reference_aligned, candidate_aligned)
    audible = (
        level["reference"]["rms_dbfs"] >= -60.0
        and level["candidate"]["rms_dbfs"] >= -60.0
        and level["reference"]["peak_dbfs"] >= -60.0
        and level["candidate"]["peak_dbfs"] >= -60.0
    )
    pitch = pitch_metrics(reference_aligned, candidate_aligned, stems, audible=audible)

    if level["reference"]["rms_dbfs"] < -60.0 or level["reference"]["peak_dbfs"] < -60.0:
        warnings.append("reference is near-silent; most metrics are undefined")
    if level["candidate"]["rms_dbfs"] < -60.0 or level["candidate"]["peak_dbfs"] < -60.0:
        warnings.append("candidate is near-silent; envelope, DTW and pitch are not meaningful")
    if level["candidate"]["true_peak_dbtp"] > 0.0:
        warnings.append(f"candidate true peak {level['candidate']['true_peak_dbtp']:.2f} dBTP exceeds 0 dBTP")
    if pitch["status"] != "ok":
        warnings.append(f"pitch not applicable: {pitch['reason']}")
    elif pitch["confidence"] == "low":
        warnings.append("pitch: low periodicity/clarity; cents error is not trustworthy for this source")

    score = _score(level, spectrum, time, body, tilt, harmonics, bands, warnings)
    invariant_score = _invariant_score(
        level,
        spectral_invariant_db,
        shape_invariant,
        bands_invariant["third_octave"],
        time_invariant,
        body_invariant,
        tilt_invariant,
        harmonics,
        warnings,
    )

    return {
        "tool": {
            "name": "scripts/wav_compare.py",
            "version": VERSION,
            "stems": stems,
            "score_weights": SCORE_WEIGHTS,
            "previous_score_weights": PREVIOUS_SCORE_WEIGHTS,
            "legacy_score_weights": LEGACY_SCORE_WEIGHTS,
            "level_invariant_weights": LEVEL_INVARIANT_WEIGHTS,
            "previous_level_invariant_weights": PREVIOUS_LEVEL_INVARIANT_WEIGHTS,
            "spectral_scale_db": SPECTRAL_SCALE_DB,
            "level_scale_lu": LEVEL_SCALE_LU,
            "body_scale_db": BODY_SCALE_DB,
            "tilt_scale_db": TILT_SCALE_DB,
            "crest_scale_db": CREST_SCALE_DB,
            "flux_scale_db": FLUX_SCALE_DB,
            "harmonic_scale_db": HARMONIC_SCALE_DB,
            "mfcc_scale": MFCC_SCALE,
            "third_octave_scale_db": THIRD_OCTAVE_SCALE_DB,
            "lsd_scale_db": LSD_SCALE_DB,
            "onset_scale_db": ONSET_SCALE_DB,
            "density_scale_db": DENSITY_SCALE_DB,
            "harmonic_dominant_tol": HARMONIC_DOMINANT_TOL,
            "harmonic_dominant_bins": HARMONIC_DOMINANT_BINS,
            "harmonic_lower_partial_db": HARMONIC_LOWER_PARTIAL_DB,
            "harmonic_max_cents": HARMONIC_MAX_CENTS,
            "harmonic_validation_n_fft": PARTIAL_N_FFT,
        },
        "files": {
            "reference": {
                **reference_meta,
                "duration_s": _num(reference_meta["duration_s"], 3),
            },
            "candidate": {
                **candidate_meta,
                "duration_s": _num(candidate_meta["duration_s"], 3),
            },
            "analysis_rate": reference.rate,
            "resampled": resampled,
            "aligned_duration_s": _num(frames / reference.rate, 3),
        },
        "alignment": alignment,
        "score": score,
        "level_invariant_score": invariant_score,
        "level": level,
        "spectrum": {"bands": bands, **spectrum},
        "body": body,
        "tilt": tilt,
        "harmonics": harmonics,
        "time": time,
        "stereo": stereo,
        "pitch": pitch,
        "warnings": warnings,
    }


def analyze_files(reference_path: str, candidate_path: str, stems: bool = False) -> dict:
    """Load two WAV files and compare them."""
    reference = read_wav(reference_path)
    candidate = read_wav(candidate_path)
    return analyze_audio(reference, candidate, stems=stems)


# ---------------------------------------------------------------------------
# Human-readable summary
# ---------------------------------------------------------------------------


def _fmt(value: float | None, digits: int = 2) -> str:
    return "—" if value is None else f"{value:+.{digits}f}"


def _fmt_abs(value: float | None, digits: int = 2) -> str:
    return "—" if value is None else f"{value:.{digits}f}"


def format_summary(result: dict) -> str:
    """Render the result as a compact human-readable report."""
    lines: list[str] = []
    files = result["files"]
    lines.append(f"WAV comparison — wav_compare.py v{result['tool']['version']}")
    lines.append(
        f"reference: {files['reference']['path']}  "
        f"({files['reference']['sample_rate'] / 1000:.1f} kHz, {files['reference']['channels']} ch, "
        f"{files['reference']['duration_s']:.2f} s)"
    )
    lines.append(
        f"candidate: {files['candidate']['path']}  "
        f"({files['candidate']['sample_rate'] / 1000:.1f} kHz, {files['candidate']['channels']} ch, "
        f"{files['candidate']['duration_s']:.2f} s)"
    )
    alignment = result["alignment"]
    lines.append(
        f"alignment: candidate lag {alignment['lag_ms']:+.1f} ms "
        f"(confidence {alignment['confidence']}, spectral corr {alignment['spectral_corr']:.3f}, "
        f"onset corr {alignment['onset_best_corr']:.3f})"
    )
    score = result["score"]
    lines.append("")
    lines.append(f"headline score (absolute): {score['value']:.1f}/100 — {score['label']}")
    for name, component in score["components"].items():
        if component["term"] is None:
            lines.append(
                f"  {name:12s} n/a          × weight {component['weight']:.3f} "
                f"= 0.000 (term not available, weight removed from the normaliser)"
            )
            continue
        lines.append(
            f"  {name:12s} term {component['term']:.3f} × weight {component['weight']:.3f} "
            f"= {component['contribution']:.3f}   ({_component_detail(name, component)})"
        )
    lines.append(f"  legacy score {score['legacy_value']:.1f}/100 (pre-1.1 three-term formula)")
    lines.append(f"  previous score {score['previous_value']:.1f}/100 (v1.2.0 seven-term formula)")
    invariant = result["level_invariant_score"]
    lines.append("")
    lines.append(
        f"level-invariant score: {invariant['value']:.1f}/100 — {invariant['label']} "
        "(uniform gain removed, level excluded)"
    )
    invariant_parts = []
    for name, component in invariant["components"].items():
        value = "n/a" if component["term"] is None else f"{component['term']:.3f}"
        invariant_parts.append(f"{name} {value}")
    lines.append(f"  terms: {' · '.join(invariant_parts)}")
    lines.append(
        f"  level (reported, not in this score): {_fmt(invariant['level']['delta_lu'])} "
        f"{'LUFS' if invariant['level']['source'] == 'integrated_lufs' else 'dBFS'}"
    )

    lines.append("")
    lines.append("Level")
    lines.append(f"  {'metric':16s} {'reference':>11s} {'candidate':>11s} {'delta':>9s}")
    reference_level = result["level"]["reference"]
    candidate_level = result["level"]["candidate"]
    delta_level = result["level"]["delta"]
    rows = (
        ("peak dBFS", "peak_dbfs", "peak_dbfs"),
        ("true peak dBTP", "true_peak_dbtp", "true_peak_dbtp"),
        ("RMS dBFS", "rms_dbfs", "rms_dbfs"),
        ("crest dB", "crest_db", "crest_db"),
        ("integrated LUFS", "integrated_lufs", "integrated_lufs"),
    )
    for label, key, delta_key in rows:
        lines.append(
            f"  {label:16s} {_fmt_abs(reference_level[key]):>11s} "
            f"{_fmt_abs(candidate_level[key]):>11s} {_fmt(delta_level[delta_key]):>9s}"
        )
    short_ref = reference_level["short_term_lufs"]
    short_cand = candidate_level["short_term_lufs"]
    lines.append(
        f"  short-term LUFS   {_fmt_abs(short_ref['min'])}..{_fmt_abs(short_ref['max'])} "
        f"({_fmt_abs(short_ref['range'])} dB)  |  "
        f"{_fmt_abs(short_cand['min'])}..{_fmt_abs(short_cand['max'])} "
        f"({_fmt_abs(short_cand['range'])} dB)"
    )

    lines.append("")
    lines.append("Spectrum: octave band energy (dBFS), candidate − reference")
    octave = result["spectrum"]["bands"]["octave"]
    lines.append("  " + " ".join(f"{center:>7.0f}" for center in octave["centers_hz"]))
    lines.append("  " + " ".join(f"{value:>+7.1f}" for value in octave["delta_db"]))
    lines.append("Spectrum: third-octave band delta (dB), 25 Hz .. 20 kHz")
    third = result["spectrum"]["bands"]["third_octave"]
    lines.append("  " + " ".join(f"{value:>+7.1f}" for value in third["delta_db"]))
    shape = result["spectrum"]["spectral_features"]["medians"]
    lines.append(
        "  medians: centroid "
        f"{_fmt_abs(shape['reference']['centroid_hz'], 1)} → {_fmt_abs(shape['candidate']['centroid_hz'], 1)} Hz "
        f"(Δ {_fmt(shape['delta']['centroid_hz'], 1)}), "
        f"rolloff {_fmt_abs(shape['reference']['rolloff_hz'], 1)} → "
        f"{_fmt_abs(shape['candidate']['rolloff_hz'], 1)} Hz, "
        f"flatness {_fmt_abs(shape['reference']['flatness'], 4)} → "
        f"{_fmt_abs(shape['candidate']['flatness'], 4)}"
    )
    logmel = result["spectrum"]["log_mel"]
    lines.append(
        f"  log-mel mean |Δ| {logmel['mean_abs_delta_db']:.2f} dB "
        f"({logmel['mean_abs_delta_db_level_matched']:.2f} dB level-matched, "
        f"p90 {logmel['p90_abs_delta_db']:.2f} dB)"
    )
    lines.append(
        f"  MFCC cosine {result['spectrum']['mfcc']['mean_cosine_distance']:.4f} "
        f"(mean vector {result['spectrum']['mfcc']['mean_vector_cosine_distance']:.4f})  |  "
        f"LSD {result['spectrum']['lsd']['broadband_db']:.2f} dB "
        f"({result['spectrum']['lsd']['level_matched_db']:.2f} dB level-matched)"
    )

    body = result["body"]
    lines.append("")
    lines.append("Body level (absolute, not level-matched)")
    lines.append(
        f"  20-120 Hz   RMS {_fmt_abs(body['low']['reference_rms_dbfs'])} → "
        f"{_fmt_abs(body['low']['candidate_rms_dbfs'])} dBFS "
        f"(Δ {_fmt(body['low']['rms_delta_db'])}), peak "
        f"{_fmt_abs(body['low']['reference_peak_dbfs'])} → "
        f"{_fmt_abs(body['low']['candidate_peak_dbfs'])} dBFS "
        f"(Δ {_fmt(body['low']['peak_delta_db'])})"
    )
    lines.append(
        f"  120-2000 Hz RMS {_fmt_abs(body['mid']['reference_rms_dbfs'])} → "
        f"{_fmt_abs(body['mid']['candidate_rms_dbfs'])} dBFS "
        f"(Δ {_fmt(body['mid']['rms_delta_db'])})"
    )
    tilt = result["tilt"]
    lines.append(
        f"  tilt: low {_fmt(tilt['low_mean_delta_db'])} dB − high "
        f"{_fmt(tilt['high_mean_delta_db'])} dB = {_fmt(tilt['tilt_db'])} dB"
    )
    sub_tilt = tilt.get("sub_gate_tilt_db")
    if sub_tilt is not None:
        lines.append(
            f"  sub-gate tilt (reported only): {_fmt(sub_tilt)} dB over "
            f"{', '.join(f'{center:g}' for center in tilt['sub_gate_octaves_hz'])} Hz"
        )
    harmonics = result["harmonics"]
    if harmonics["status"] == "ok":
        lines.append(
            f"  harmonics: f0 {_fmt_abs(harmonics['fundamental_hz']['reference'], 1)} → "
            f"{_fmt_abs(harmonics['fundamental_hz']['candidate'], 1)} Hz, "
            f"distance {_fmt(harmonics['distance_db'])} dB "
            f"({harmonics['frames']['reference']}/{harmonics['frames']['candidate']} frames), "
            f"THD {_fmt_abs(harmonics['thd_percent']['reference'], 1)} → "
            f"{_fmt_abs(harmonics['thd_percent']['candidate'], 1)} %"
        )
    else:
        lines.append(f"  harmonics: not applicable ({harmonics['reason']})")

    lines.append("")
    lines.append("Time / dynamics")
    onset = result["time"]["onset"]
    lines.append(
        f"  onsets: {onset['reference_count']} ref, {onset['candidate_count']} cand; "
        f"density {onset['reference_density_per_s']:.2f} → "
        f"{onset['candidate_density_per_s']:.2f}/s ({_fmt(onset['density_ratio_db'])} dB); "
        f"flux corr {onset['best_correlation']:.3f} at {onset['best_lag_ms']:+.1f} ms"
    )
    lines.append(
        f"  per-attack strength: {onset['paired_count']} pairs, median {_fmt(onset['onset_strength_median_db'])} dB"
    )
    attack = result["time"]["attack"]
    lines.append(
        f"  attack 10-90%: median {_fmt_abs(attack['reference']['median_ms'])} ms ref, "
        f"{_fmt_abs(attack['candidate']['median_ms'])} ms cand; "
        f"{attack['paired_count']} pairs, median |Δ| {_fmt_abs(attack['median_abs_delta_ms'])} ms"
    )
    transient = result["time"]["transient"]
    lines.append(
        f"  onset flux: {_fmt_abs(transient['reference_flux_mean'], 3)} ref, "
        f"{_fmt_abs(transient['candidate_flux_mean'], 3)} cand; "
        f"ratio {_fmt(transient['flux_ratio_db'])} dB"
    )
    lines.append(
        f"  envelope correlation {result['time']['envelope_correlation']['correlation']:.3f} "
        f"({result['time']['envelope_correlation']['frames']} frames)"
    )
    dtw = result["time"]["dtw"]
    lines.append(
        f"  DTW: mean |warp| {_fmt_abs(dtw['mean_abs_warp_ms'])} ms, "
        f"max {_fmt_abs(dtw['max_abs_warp_ms'])} ms, "
        f"band {_fmt_abs(dtw['band_ms'], 1)} ms"
    )

    lines.append("")
    lines.append("Stereo")
    for label in ("reference", "candidate"):
        stereo = result["stereo"][label]
        if stereo["mono"]:
            lines.append(f"  {label:9s} mono")
        else:
            lines.append(
                f"  {label:9s} L/R corr {_fmt_abs(stereo['correlation'], 4)}, "
                f"mid/side {_fmt(stereo['mid_side_ratio_db'])} dB"
            )

    lines.append("")
    pitch = result["pitch"]
    lines.append("Pitch")
    if pitch["status"] != "ok":
        lines.append(
            f"  not applicable ({pitch['reason']}); "
            f"voiced frames {pitch['reference_voiced_frames']} ref / "
            f"{pitch['candidate_voiced_frames']} cand, clarity "
            f"{_fmt_abs(pitch['reference_median_clarity'], 3)} / "
            f"{_fmt_abs(pitch['candidate_median_clarity'], 3)}"
        )
    else:
        lines.append(
            f"  f0 {pitch['f0_range_hz'][0]:.0f}-{pitch['f0_range_hz'][1]:.0f} Hz, "
            f"confidence {pitch['confidence']}; median f0 "
            f"{_fmt_abs(pitch['reference_median_f0_hz'], 2)} Hz ref → "
            f"{_fmt_abs(pitch['candidate_median_f0_hz'], 2)} Hz cand"
        )
        lines.append(
            f"  {pitch['compared_frames']} compared frames: median cents "
            f"{_fmt(pitch['median_cents_error'])}, median |cents| "
            f"{_fmt_abs(pitch['median_abs_cents_error'])}; within 25 cents "
            f"{_fmt_abs((pitch['fraction_within_25_cents'] or 0.0) * 100.0, 1)}%"
        )

    if result["warnings"]:
        lines.append("")
        lines.append("Warnings")
        for warning in result["warnings"]:
            lines.append(f"  ! {warning}")
    return "\n".join(lines)


def _component_detail(name: str, component: dict) -> str:
    if name == "spectral":
        return f"{component['distance_db']:.2f} dB / {component['scale_db']:.0f} dB"
    if name == "level":
        return f"{component['delta']:.2f} {component['source']} / {component['scale']:.0f}"
    if name == "envelope":
        correlation = component["correlation"]
        return f"1 - {correlation:.3f}" if correlation is not None else "undefined"
    if name == "body":
        return (
            f"|{component['low_rms_delta_db']:+.2f}| + |{component['mid_rms_delta_db']:+.2f}| dB "
            f"/ 2 × {component['scale_db']:.0f} dB"
        )
    if name == "tilt":
        return f"|{component['tilt_db']:+.2f}| dB / {component['scale_db']:.0f} dB"
    if name == "crest_attack":
        return f"crest {component['crest_delta_db']:+.2f} dB / {component['crest_scale_db']:.0f} dB"
    if name == "mfcc":
        return f"{component['cosine_distance']:.4f} / {component['scale']:.2f}"
    if name == "third_octave":
        return (
            f"level-matched RMS {component['level_matched_rms_delta_db']:.2f} dB over "
            f"{component['bands_used']} bands / {component['scale_db']:.0f} dB"
        )
    if name == "lsd":
        return f"{component['level_matched_db']:.2f} dB level-matched / {component['scale_db']:.0f} dB"
    if name == "onset":
        return (
            f"per-attack {component['median_delta_db']:+.2f} dB over {component['paired_count']} pairs "
            f"/ {component['scale_db']:.0f} dB"
        )
    if name == "density":
        return (
            f"{component['reference_per_s']:.2f} → {component['candidate_per_s']:.2f} events/s "
            f"({component['ratio_db']:+.2f} dB / {component['scale_db']:.0f} dB)"
            if component["ratio_db"] is not None
            else f"{component['reference_per_s']:.2f} → {component['candidate_per_s']:.2f} events/s"
        )
    return f"{component['distance_db']} dB / {component['scale_db']:.0f} dB ({component['status']})"


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------


def _sine(frequency: float, rate: int, seconds: float, amplitude: float = 0.2) -> np.ndarray:
    time = np.arange(int(rate * seconds), dtype=np.float64) / rate
    return amplitude * np.sin(2.0 * np.pi * frequency * time)


def _burst_signal(rate: int, seconds: float = 2.0) -> np.ndarray:
    time = np.arange(int(rate * seconds), dtype=np.float64) / rate
    gate = ((time % 0.25) < 0.12).astype(np.float64)
    return 0.2 * np.sin(2.0 * np.pi * 440.0 * time) * gate


def _write_test_wav(path: str, mono: np.ndarray, rate: int) -> None:
    samples = np.clip(mono * 32767.0, -32768, 32767).astype("<i2")
    with wave.open(path, "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(rate)
        handle.writeframes(samples.tobytes())


def _run_selftest() -> int:
    rate = 48000
    checks: list[tuple[str, bool, str]] = []

    def check(name: str, condition: bool, detail: str) -> None:
        checks.append((name, bool(condition), detail))

    base = _sine(440.0, rate, 2.0)
    silence = np.zeros_like(base)
    identical = analyze_audio(Audio(base[:, None], rate), Audio(base[:, None], rate))
    check("identical score is 100", (identical["score"]["value"] or 0.0) >= 99.9, f"{identical['score']['value']}")

    noise = base + 0.01 * np.random.default_rng(7).standard_normal(base.size)
    noisy = analyze_audio(Audio(base[:, None], rate), Audio(noise[:, None], rate))
    check(
        "sine vs sine+noise raises mel distance",
        (noisy["spectrum"]["log_mel"]["mean_abs_delta_db"] or 0.0) > 0.05,
        f"{noisy['spectrum']['log_mel']['mean_abs_delta_db']:.4f} dB",
    )
    check(
        "sine vs sine+noise lowers score",
        (noisy["score"]["value"] or 100.0) < 99.0,
        f"{noisy['score']['value']}",
    )

    half = analyze_audio(Audio(base[:, None], rate), Audio((base * 0.5)[:, None], rate))
    lufs_delta = abs(half["level"]["delta"]["integrated_lufs"] or 0.0)
    check("level shift is 6.02 LU", abs(lufs_delta - 6.0206) < 0.2, f"{lufs_delta:.3f} LU")
    check(
        "level shift is 6.02 dB RMS",
        abs(abs(half["level"]["delta"]["rms_dbfs"] or 0.0) - 6.0206) < 0.05,
        f"{half['level']['delta']['rms_dbfs']:.3f} dB",
    )
    check(
        "level-invariant score ignores a 6.02 dB gain",
        (half["level_invariant_score"]["value"] or 0.0) >= 99.9,
        f"{half['level_invariant_score']['value']} vs absolute {half['score']['value']}",
    )
    check(
        "absolute score still sees the 6.02 dB level move",
        (half["score"]["value"] or 100.0) < 90.0,
        f"{half['score']['value']}",
    )

    shifted = _burst_signal(rate)
    delay = int(round(0.030 * rate))
    delayed = np.concatenate([np.zeros(delay), shifted])
    time_result = analyze_audio(Audio(shifted[:, None], rate), Audio(delayed[:, None], rate), stems=True)
    lag = time_result["alignment"]["lag_ms"] or 0.0
    check("time shift recovers 30 ms", abs(lag - 30.0) <= 2.0, f"{lag:.2f} ms")
    correlation = time_result["time"]["envelope_correlation"]["correlation"]
    check(
        "time shift keeps envelope correlation high",
        correlation is not None and correlation > 0.95,
        f"{correlation}",
    )
    warp = time_result["time"]["dtw"]["mean_abs_warp_ms"]
    check("time shift DTW warp is small", warp is not None and warp < 5.0, f"{warp} ms")

    pitch = analyze_audio(
        Audio(_sine(440.0, rate, 2.0)[:, None], rate),
        Audio(_sine(441.5, rate, 2.0)[:, None], rate),
        stems=True,
    )
    cents = pitch["pitch"]["median_cents_error"]
    check("pitch error is ~5.9 cents", cents is not None and abs(cents - 5.9) < 1.5, f"{cents} cents")

    time = np.arange(rate * 2) / rate
    intersample = np.sin(2.0 * np.pi * (rate / 4.0) * time + np.pi / 4.0)
    peak_result = level_metrics(Audio(intersample[:, None], rate))
    check(
        "true peak finds the intersample peak",
        abs(peak_result["true_peak_dbtp"] or 0.0) < 0.1,
        f"{peak_result['true_peak_dbtp']:.3f} dBTP",
    )

    high = _sine(200.0, rate, 2.0) + 0.2 * _sine(9000.0, rate, 2.0)
    high_result = analyze_audio(Audio(_sine(200.0, rate, 2.0)[:, None], rate), Audio(high[:, None], rate))
    octave = high_result["spectrum"]["bands"]["octave"]
    index = octave["centers_hz"].index(8000.0)
    check(
        "9 kHz addition raises the 8 kHz octave band",
        (octave["delta_db"][index] or 0.0) > 15.0,
        f"{octave['delta_db'][index]:+.2f} dB",
    )

    silent_result = analyze_audio(Audio(base[:, None], rate), Audio(silence[:, None], rate))
    check(
        "silent candidate is flagged and scored 0",
        (silent_result["score"]["value"] == 0.0)
        and any("near-silent" in warning for warning in silent_result["warnings"]),
        f"{silent_result['score']['value']}",
    )

    # A -6.02 dB tilt confined to 63-500 Hz: body 20-120 and 120-2k and the
    # low-minus-high tilt scalar all read the analytic value.
    low_reference = _sine(63.0, rate, 2.0) + _sine(125.0, rate, 2.0) + _sine(250.0, rate, 2.0) + _sine(500.0, rate, 2.0)
    high_reference = _sine(8000.0, rate, 2.0)
    tilted_reference = low_reference + high_reference
    tilted_candidate = 0.5 * low_reference + high_reference
    tilted = analyze_audio(Audio(tilted_reference[:, None], rate), Audio(tilted_candidate[:, None], rate), stems=True)
    low_delta = tilted["body"]["low"]["rms_delta_db"]
    mid_delta = tilted["body"]["mid"]["rms_delta_db"]
    tilt_scalar = tilted["tilt"]["tilt_db"]
    check(
        "body 20-120 dB tracks a 6.02 dB low cut",
        low_delta is not None and abs(low_delta + 6.0206) < 0.2,
        f"{low_delta}",
    )
    check(
        "body 120-2k dB tracks the same low cut",
        mid_delta is not None and abs(mid_delta + 6.0206) < 0.2,
        f"{mid_delta}",
    )
    check(
        "octave tilt scalar tracks a 6.02 dB tilt",
        tilt_scalar is not None and abs(tilt_scalar + 6.0206) < 0.3,
        f"{tilt_scalar}",
    )
    tilted_invariant = tilted["level_invariant_score"]["value"]
    check(
        "level-invariant score still sees a band-local cut",
        tilted_invariant is not None and tilted_invariant < 95.0,
        f"{tilted_invariant}",
    )

    # A crest-flattened signal: sparse peaks plus a steady body keeps the same
    # peak order while raising the RMS, so the crest/attack term must saturate.
    sparse = np.zeros(rate * 2)
    sparse[:: rate // 4] = 0.8
    flattened = 0.4 * sparse + 0.2 * _sine(1000.0, rate, 2.0, 1.0)
    flattened_result = analyze_audio(Audio(sparse[:, None], rate), Audio(flattened[:, None], rate), stems=True)
    flattened_crest = flattened_result["level"]["delta"]["crest_db"]
    flattened_term = flattened_result["score"]["components"]["crest_attack"]["term"]
    check(
        "flattening the crest raises the crest/attack term",
        flattened_crest is not None and flattened_crest < -10.0 and flattened_term > 0.5,
        f"crest {flattened_crest:+.2f} dB, term {flattened_term:.3f}",
    )

    # A pure 65.4 Hz sine vs the same sine with a -20 dB third harmonic: 10 %
    # THD, and 40 dB above the -60 dB relative floor over 7 harmonics.
    pure_bass = _sine(65.4, rate, 2.0)
    distorted_bass = pure_bass + 0.1 * _sine(3.0 * 65.4, rate, 2.0)
    harmonics = analyze_audio(Audio(pure_bass[:, None], rate), Audio(distorted_bass[:, None], rate), stems=True)[
        "harmonics"
    ]
    detected_f0 = harmonics["fundamental_hz"]["candidate"]
    harmonic_distance = harmonics["distance_db"]
    thd = harmonics["thd_percent"]["candidate"]
    check(
        "harmonic profile finds the 65.4 Hz fundamental",
        detected_f0 is not None and abs(detected_f0 - 65.4) < 0.5,
        f"{detected_f0} Hz",
    )
    check("distorted sine THD is ~10 %", thd is not None and 8.0 < thd < 12.0, f"{thd} %")
    check(
        "harmonic distance is 40 dB over 7 harmonics at the -60 dB floor",
        harmonic_distance is not None and 5.4 < harmonic_distance < 6.0,
        f"{harmonic_distance} dB",
    )

    # Both files lock the second harmonic of a 55 Hz fundamental: the ladder is
    # between H2 indices, so the distance is meaningless and must be N/A.
    wrong_reference = 0.08 * _sine(55.0, rate, 2.0) + 0.30 * _sine(110.0, rate, 2.0) + 0.02 * _sine(330.0, rate, 2.0)
    wrong_candidate = 0.08 * _sine(55.0, rate, 2.0) + 0.30 * _sine(110.0, rate, 2.0) + 0.10 * _sine(330.0, rate, 2.0)
    wrong = analyze_audio(Audio(wrong_reference[:, None], rate), Audio(wrong_candidate[:, None], rate), stems=True)[
        "harmonics"
    ]
    check(
        "wrong-partial lock reports N/A, not a distance",
        wrong["status"] == "not_applicable" and wrong["distance_db"] is None and "harmonic" in (wrong["reason"] or ""),
        f"{wrong['status']}: {wrong['reason']}",
    )

    # Reference and candidate lock different (individually plausible) notes:
    # both ladders pass the per-file check, but their fundamentals are 900 cents
    # apart, so the distance is not comparable.
    mismatch_reference = 0.20 * _sine(55.0, rate, 2.0) + 0.02 * _sine(165.0, rate, 2.0)
    mismatch_candidate = 0.20 * _sine(92.5, rate, 2.0) + 0.02 * _sine(277.5, rate, 2.0)
    mismatch = analyze_audio(
        Audio(mismatch_reference[:, None], rate), Audio(mismatch_candidate[:, None], rate), stems=True
    )["harmonics"]
    check(
        "different f0 locks report N/A",
        mismatch["status"] == "not_applicable"
        and mismatch["distance_db"] is None
        and ("harmonic" in (mismatch["reason"] or "") or "cents" in (mismatch["reason"] or "")),
        f"{mismatch['status']}: {mismatch['reason']}",
    )

    # v1.3.0 score terms: each must move on its own failure mode and stay put
    # on a uniform gain (the invariant score's requirement).
    noise_seed = np.random.default_rng(11)
    flat_noise = 0.03 * noise_seed.standard_normal(rate * 2)
    gain_noise = analyze_audio(Audio(flat_noise[:, None], rate), Audio((0.5 * flat_noise)[:, None], rate))
    harmonic_reference = 0.2 * _sine(440.0, rate, 2.0) + 0.04 * _sine(880.0, rate, 2.0)
    harmonic_candidate = 0.2 * _sine(440.0, rate, 2.0) + 0.04 * _sine(1320.0, rate, 2.0)
    mfcc_move = analyze_audio(Audio(harmonic_reference[:, None], rate), Audio(harmonic_candidate[:, None], rate))
    check(
        "changing a harmonic moves the MFCC term",
        (mfcc_move["score"]["components"]["mfcc"]["term"] or 0.0) > 0.3,
        f"{mfcc_move['score']['components']['mfcc']['term']:.3f}",
    )
    check(
        "MFCC term ignores a uniform gain",
        abs(gain_noise["score"]["components"]["mfcc"]["term"] or 0.0) < 0.02,
        f"{gain_noise['score']['components']['mfcc']['term']:.4f}",
    )

    resonance = _sine(3150.0, rate, 2.0, amplitude=0.05)
    third_move = analyze_audio(Audio(flat_noise[:, None], rate), Audio((flat_noise + resonance)[:, None], rate))
    check(
        "a 3.15 kHz resonance moves the third-octave term",
        (third_move["score"]["components"]["third_octave"]["term"] or 0.0) > 0.3,
        f"{third_move['score']['components']['third_octave']['term']:.3f}",
    )
    check(
        "third-octave term ignores a uniform gain",
        abs(gain_noise["score"]["components"]["third_octave"]["term"] or 0.0) < 0.02,
        f"{gain_noise['score']['components']['third_octave']['term']:.4f}",
    )

    noise_seed = np.random.default_rng(3)
    hiss = 0.01 * noise_seed.standard_normal(rate * 2)
    lsd_move = analyze_audio(Audio(base[:, None], rate), Audio((base + hiss)[:, None], rate))
    check(
        "broadband hiss moves the LSD term",
        (lsd_move["score"]["components"]["lsd"]["term"] or 0.0) > 0.3,
        f"{lsd_move['score']['components']['lsd']['term']:.3f}",
    )
    check(
        "LSD term ignores a uniform gain",
        abs(gain_noise["score"]["components"]["lsd"]["term"] or 0.0) < 0.02,
        f"{gain_noise['score']['components']['lsd']['term']:.4f}",
    )

    click_body = 0.05 * _sine(220.0, rate, 2.0)
    click_starts = list(np.arange(0.1, 1.9, 0.4))

    def click_track(amplitude: float, extra: bool = False) -> np.ndarray:
        track = click_body.copy()
        starts = list(click_starts) + (list(np.arange(0.3, 1.9, 0.4)) if extra else [])
        for start in starts:
            index = int(start * rate)
            length = int(6 * rate / 1000)
            track[index : index + length] += (
                amplitude * np.sin(2.0 * np.pi * 3000.0 * np.arange(length) / rate) * np.hanning(length)
            )
        return track

    soft_attacks = analyze_audio(
        Audio(click_track(0.6)[:, None], rate), Audio(click_track(0.02)[:, None], rate), stems=True
    )
    check(
        "weaker attacks move the onset-strength term",
        (soft_attacks["score"]["components"]["onset"]["term"] or 0.0) > 0.5,
        f"{soft_attacks['score']['components']['onset']['term']:.3f}",
    )
    check(
        "weaker attacks do not move the density term",
        abs(soft_attacks["score"]["components"]["density"]["term"] or 0.0) < 0.02,
        f"{soft_attacks['score']['components']['density']['term']:.4f}",
    )

    denser = analyze_audio(
        Audio(click_track(0.6)[:, None], rate), Audio(click_track(0.6, extra=True)[:, None], rate), stems=True
    )
    check(
        "extra attacks move the density term",
        (denser["score"]["components"]["density"]["term"] or 0.0) > 0.5,
        f"{denser['score']['components']['density']['term']:.3f}",
    )
    check(
        "extra attacks do not move the onset-strength term",
        abs(denser["score"]["components"]["onset"]["term"] or 0.0) < 0.02,
        f"{denser['score']['components']['onset']['term']:.4f}",
    )

    click_array = click_track(0.6)
    click_delay = int(0.03 * rate)
    delayed_click = np.concatenate([np.zeros(click_delay), click_array])[: rate * 2]
    shifted_onsets = analyze_audio(Audio(click_array[:, None], rate), Audio(delayed_click[:, None], rate), stems=True)
    check(
        "onset-strength term is alignment-invariant",
        abs(shifted_onsets["score"]["components"]["onset"]["term"] or 0.0) < 0.02,
        f"lag {shifted_onsets['alignment']['lag_ms']:.1f} ms, term "
        f"{shifted_onsets['score']['components']['onset']['term']:.4f}",
    )
    gain_click = analyze_audio(Audio(click_array[:, None], rate), Audio((0.5 * click_array)[:, None], rate), stems=True)
    gain_density = gain_click["score"]["components"]["density"]["term"]
    check(
        "density term ignores a uniform gain",
        abs(gain_density or 0.0) < 0.02,
        f"{gain_density}",
    )

    # A 6 dB cut confined to octaves more than 60 dB below the loudest one:
    # the main tilt stays gated at 0, the sub-gate tilt reports it, and the
    # score does not move (sub-gate tilt is reported-only).
    loud_tone = 0.2 * _sine(1000.0, rate, 2.0)
    quiet_low = sum(1e-5 * _sine(frequency, rate, 2.0) for frequency in (63.0, 125.0, 250.0, 500.0))
    quiet_high = sum(1e-5 * _sine(frequency, rate, 2.0) for frequency in (8000.0, 16000.0))
    sub_gate_reference = loud_tone + quiet_low + quiet_high
    sub_gate_candidate = loud_tone + 0.5 * quiet_low + quiet_high
    sub_gate = analyze_audio(
        Audio(sub_gate_reference[:, None], rate), Audio(sub_gate_candidate[:, None], rate), stems=True
    )
    check(
        "sub-gate tilt reports a gated 6 dB band cut",
        sub_gate["tilt"]["sub_gate_tilt_db"] is not None
        and abs(sub_gate["tilt"]["sub_gate_tilt_db"] + 6.0206) < 0.2
        and sub_gate["tilt"]["gated_octaves"][2],
        f"{sub_gate['tilt']['sub_gate_tilt_db']} dB over {sub_gate['tilt']['sub_gate_octaves_hz']}",
    )
    sub_gate_identical = analyze_audio(
        Audio(sub_gate_reference[:, None], rate), Audio(sub_gate_reference[:, None], rate), stems=True
    )
    check(
        "sub-gate tilt is reported-only, not scored",
        sub_gate["tilt"]["sub_gate_scored"] is False
        and abs(sub_gate["score"]["components"]["tilt"]["term"]) < 1e-9
        and abs(sub_gate["score"]["value"] - sub_gate_identical["score"]["value"]) < 0.5,
        f"score {sub_gate['score']['value']} vs {sub_gate_identical['score']['value']}",
    )

    with tempfile.TemporaryDirectory() as directory:
        path = str(Path(directory) / "roundtrip.wav")
        _write_test_wav(path, base, rate)
        loaded = read_wav(path)
        error = float(np.max(np.abs(loaded.samples[:, 0] - base)))
        check("16-bit WAV round trip", error < 2.0 / 32768.0, f"max error {error:.2e}")

    passed = sum(1 for _, ok, _ in checks if ok)
    for name, ok, detail in checks:
        lines = f"[{'PASS' if ok else 'FAIL'}] {name} ({detail})"
        print(lines)
    print(f"selftest: {passed}/{len(checks)} passed")
    return 0 if passed == len(checks) else 1


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="wav_compare.py",
        description="Rigorous level/spectrum/time/stereo/pitch comparison between two WAV renders.",
    )
    parser.add_argument("reference", nargs="?", help="reference WAV")
    parser.add_argument("candidate", nargs="?", help="candidate WAV")
    parser.add_argument("--json", metavar="PATH", help="write the full result as JSON")
    parser.add_argument(
        "--stems",
        action="store_true",
        help="inputs are isolated stems: ±300 ms alignment search, tight DTW band, pitch diagnostic enabled",
    )
    parser.add_argument("--selftest", action="store_true", help="run synthetic metric checks and exit")
    args = parser.parse_args(argv)

    if args.selftest:
        return _run_selftest()
    if not args.reference or not args.candidate:
        parser.error("reference and candidate WAV paths are required (or use --selftest)")

    try:
        result = analyze_files(args.reference, args.candidate, stems=args.stems)
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    print(format_summary(result))

    if args.json:
        path = Path(args.json)
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")
        except (OSError, ValueError) as error:
            print(f"error: cannot write JSON to {path}: {error}", file=sys.stderr)
            return 2
        print(f"\nJSON written to {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
