#!/usr/bin/env python3
"""Generate synthetic `.ffp` fixtures for the preset parser.

These files encode only the byte layout and values the parser must handle.
Anything not needed by a test is an explicit 0.0 placeholder.

Run from this directory: python3 make_fixtures.py
"""

import hashlib
import math
import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOG2_10_HZ = math.log2(10.0)
LOG2_1000_HZ = math.log2(1000.0)


def binary(magic: bytes, version: int, values: list[float]) -> bytes:
    # The third header field is the parameter count, not a byte length.
    return struct.pack("<4sII", magic, version, len(values)) + struct.pack(f"<{len(values)}f", *values)


def binary_eq_low_cut() -> bytes:
    """Binary EQ fixture: 50 Hz low cut, 48 dB/oct, Q plain 0.5.

    Band 1 carries the test values; the other bands use neutral defaults and
    the global slots stay 0.0 placeholders.
    """
    values: list[float] = []
    for band in range(24):
        # Per-band test defaults: Unused, 1 kHz, 0 dB, Q plain 0.5, Bell,
        # 12 dB/oct, Stereo.
        state = 2.0
        frequency = LOG2_1000_HZ
        gain = 0.0
        q = 0.5
        shape = 0.0
        slope = 1.0
        stereo = 2.0
        if band == 0:
            state = 1.0
            frequency = math.log2(50.0)
            shape = 2.0  # Low Cut
            slope = 6.0  # 48 dB/oct
        values += [state, frequency, gain, q, shape, slope, stereo]

    # VST3 ids 168..188 are zero placeholders; these slots are not part of the
    # parser behaviour this fixture exercises.
    values += [0.0] * 20
    values += [-1.0]  # 188 Solo Band default
    assert len(values) == 189
    return binary(b"FQ2p", 2, values)


def text_preset(
    signature: str,
    version: int,
    name: str,
    description: str,
    parameters: list[tuple[str, str]],
) -> bytes:
    lines = [
        "[Preset]",
        f"Signature={signature}",
        f"Version={version}",
        f"Name={name}",
        "Author=OpenCompose",
        f'Description="{description}"',
        "Tags=Basic",
        "",
        "[Parameters]",
    ]
    lines += [f"{key}={value}" for key, value in parameters]
    lines.append("")
    return "\n".join(lines).encode("ascii")


def main() -> None:
    low_cut = binary_eq_low_cut()
    clean = text_preset(
        "FC3p",
        2,
        "Clean",
        "Clean, transparent compression",
        [
            # Reduced parameter set for text-format coverage.
            ("Style", "0"),
            ("Threshold", "-16"),
            ("Ratio", "0.560000002384186"),
            ("Knee", "7.34594345092773"),
            ("Attack", "0.142277777194977"),
            ("Release", "0.277943134307861"),
            ("Auto Gain", "1"),
            ("Knee Display Enabled", "0"),
            ("Maximum Lookahead", "0"),
        ],
    )
    vocal = text_preset(
        "FC3p",
        2,
        "Vocal",
        "Vocal compression",
        [
            # Reduced parameter set for the second text-format case.
            ("Style", "10"),
            ("Threshold", "-14"),
            ("Ratio", "0.580194473266602"),
            ("Knee", "4.90366697311401"),
            ("Attack", "0.116499997675419"),
            ("Release", "0.471833318471909"),
            ("Auto Gain", "1"),
        ],
    )

    normalised = bytearray(low_cut)
    # A value in the normalised 0..1 domain where the file must store log2(Hz).
    struct.pack_into("<f", normalised, 12 + 4, 0.201024)

    version_3 = bytearray(low_cut)
    struct.pack_into("<I", version_3, 4, 3)

    files = {
        "binary-eq-lowcut.ffp": low_cut,
        "binary-eq-truncated.ffp": low_cut[:100],
        "binary-eq-bad-magic.ffp": b"XXXX" + low_cut[4:],
        "binary-eq-length-mismatch.ffp": low_cut + b"\x00\x00\x00\x00",
        "binary-eq-normalised.ffp": bytes(normalised),
        "binary-eq-version-3.ffp": bytes(version_3),
        "text-comp-clean.ffp": clean,
        "text-comp-vocal.ffp": vocal,
    }
    for filename, data in files.items():
        (HERE / filename).write_bytes(data)
        print(f"{filename:32} {len(data):5} bytes  sha256={hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
