# Plosive

`plosive` removes the low-frequency thump of a plosive (`p`, `b`, `t`, `k`
release bursts) without thinning the vowel that follows. Detection is a
low-band rise against a slow reference, and the action is a dynamic low shelf
with a short attack and a non-pumping release. It is a **zero-latency** insert:
a minimum-phase biquad, no lookahead, no split FIR.

```
effects:
  - plosive: amount=100%, threshold=6dB, frequency=120Hz, release=40ms
```

## Controls

| Parameter | Range | Default | What it does |
|---|---|---|---|
| `amount` | 0–100 % | 0 | Depth of the shelf. 100 % removes 12 dB at DC; 50 % removes 6 dB at DC. `0` is a bit-exact bypass that reports zero latency. |
| `threshold` | 2–24 dB | 6 | Rise of the detector envelope over its slow reference before a burst can fire. Raise it if a legato low note retriggers the shelf. |
| `frequency` | 80–150 Hz | 120 | Low-shelf corner (RBJ low shelf, S = 1). The detector's low band is a 24 dB/oct Butterworth low-pass at the same corner. |
| `release` | 10–300 ms | 40 | One-pole return of the shelf to unity after the last trigger. |
| `monitor` | on/off | off | Listen-only: outputs the part the shelf is removing (`input − shelf output`), so the threshold and frequency can be set up. |

The device ships **bypassed** (`amount` 0). A repair pass sets `amount` to
100 %; the target "below 120 Hz down by 6 dB" gate is met from about 76 %
upward (measured table below).

## Detector

All work is per sample except one control tick per millisecond; nothing is
allocated after `new`.

```
mid      = (L + R) / 2
low      = 4th-order Butterworth low-pass at `frequency` (Q 0.5412, 1.3066)
high     = 2nd-order high-pass at 400 Hz
fast     = power envelope of a band, 1 ms attack / 15 ms release
slow     = power envelope of the same band, 40 ms attack / 250 ms release
rise_db  = 10 log10((low_fast + high_fast) / (low_slow + high_slow))
excess   = max(fast - slow, 0) for each band
share    = excess_low / (excess_low + excess_high)
fire     = rise_db >= threshold && share >= 0.5 && (low_fast + high_fast) > 1e-8
```

Firing arms a 20 ms hold and engages the shelf; a one-pole (1.5 ms attack, the
`release` knob) moves the shelf depth. The tick runs every 1 ms and the shelf
coefficients are retargeted there and interpolated linearly across the next
millisecond, so neither the detector nor the action depends on the process
block size.

The **excess share** is the discriminator, not the rise. A vowel onset also
produces a large rise (from silence), but its excess energy is in the vowel's
harmonics; a plosive's excess is in the low band. Measured on the synthetic
signals below: a plosive shows 0.86–0.99 while a 110 Hz vowel onset shows 0.26,
a 220 Hz vowel 0.04, an 8 kHz sibilant 0.00. The 0.5 gate therefore blocks
every non-low onset while leaving a wide margin for a plosive over a sung note.

The 400 Hz high band is what makes detection work over a sustained note: the
note's low-band level does not jump, so the rise must be seen where the burst is
broadband. The low-band energy is what makes it a *plosive* rather than any
transient.

## Measured thresholds

Synthetic signals, 48 kHz, mono-summed; the S8 burst is 30 ms of low-passed
noise (95 % of its energy below 120 Hz) with a small broadband component.
`amount` 100 %, `threshold` 6 dB, `frequency` 120 Hz, `release` 40 ms.

| Scenario | max rise | share while rising | fires |
|---|---:|---:|---|
| S8 burst | 15.18 dB | 0.993 | yes |
| S8 burst, 20 dB quieter | 15.18 dB | 0.993 | yes |
| S8 burst over a 110 Hz sung note | 14.10 dB | 0.864 | yes |
| vowel onset, 110 Hz | 14.10 dB | 0.260 | no |
| vowel onset, 220 Hz | 14.04 dB | 0.039 | no |
| vowel onset, 440 Hz | 14.20 dB | 0.013 | no |
| 6–9 kHz sibilant, 100 ms | 14.80 dB | 0.000 | no |
| steady 1 kHz tone | 14.76 dB | 0.002 | no |
| 60 Hz thump, 30 ms | 15.10 dB | 0.998 | yes |

The detector is level-invariant above its −80 dBFS power floor: the burst 20 dB
quieter reads the same rise and share. The 60 Hz thump fires by design — a
low-frequency thump is what the device is for. On a vocal track that means kick
or bass bleed can retrigger it; raise `threshold` or keep the device off
non-vocal clips.

The trigger is not instantaneous: the 24 dB/oct low band takes a few
milliseconds to build. Measured on the S8 burst, engagement starts 287 samples
(5.98 ms) after the burst onset and reaches half depth (6 dB) at 336 samples
(7.00 ms); full depth follows within another few milliseconds. That delay is
part of why the band-energy measurement is quoted over the whole burst.

## Action and band shape

The shelf is an RBJ low shelf at `frequency` with S = 1 (maximally flat, no
overshoot), gain = −12 dB × `amount`/100 × engagement. Response at 48 kHz:

| amount | 30 Hz | 60 Hz | 90 Hz | 120 Hz | 150 Hz | 250 Hz | 500 Hz | 1 kHz | 2 kHz |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 % | −11.94 | −11.10 | −8.79 | −6.00 | −3.78 | −0.77 | −0.05 | −0.00 | −0.00 |
| 50 % | −5.97 | −5.62 | −4.51 | −3.00 | −1.78 | −0.32 | −0.02 | −0.00 | −0.00 |

The shelf never boosts at any of these points, and the 500 Hz–2 kHz body moves
by less than 0.05 dB even at full depth. That is the "vowels stay intact"
guarantee: the body is outside the shelf's reach, not merely attenuated less.

Measured drop in the < 120 Hz band over an S8 burst:

| amount | 100 % | 75 % | 50 % | 25 % |
|---|---:|---:|---:|---:|
| < 120 Hz energy | −7.94 dB | −5.99 dB | −4.02 dB | −2.02 dB |

## Acceptance

| Check | Result |
|---|---|
| S8 burst, < 120 Hz energy | **−7.94 dB** (gate ≥ 6 dB) |
| S8 vowel body, 500 Hz–2 kHz | **−0.000 dB** (gate ≤ 0.3 dB) |
| Edge discontinuity | added per-sample step **0.0064** (design bound 0.012; the low band's own slope) |
| Vowel alone, no plosive | bit-identical for 110 / 220 / 440 Hz; meter stays at 0.00 dB |
| Plosive over a sung 110 Hz note | f0 **0.000 cents**, < 120 Hz **−7.64 dB** |
| `amount = 0` | bit-identical (bypass; no filter is run) |
| Block sizes 97 / 480 frames | bit-identical output |
| Allocation in `process` | **0** allocations (counting allocator; blocks 97/480/12 000, detector firing and parameter changes inside the counted region) |
| Determinism | same input and reset reproduce the same output |

## Latency and state

- `latency_samples()` is **0** at every `amount`; there is no lookahead and the
  shelf is minimum phase. The device adds nothing to the monitoring budget.
- `tail_samples()` is 10 ms while engaged (the shelf's ring after the input
  stops), 0 bypassed.
- State is two biquad states, three detectors' states and a few envelopes;
  `reset()` clears all of it and is deterministic.
- `monitor` outputs `input − shelf output`; at unity it is digital silence.

## Limits

- A burst with most of its energy above the corner (a hard `t`/`k` release with
  little low content) does not trigger, and a low cut would not remove it
  anyway. The device targets low-band thumps.
- A kick drum or a bass note onset looks like a plosive to the excess-share
  test. Do not put the device on a drum or bass clip.
- `frequency` is a musical choice: a male fundamental near 100 Hz sits inside
  the shelf. The release is short enough that the note's body returns within a
  few tens of milliseconds, but the first part of a very low note will be
  dulled during the burst. 120 Hz is the default for that reason.
