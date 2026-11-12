# Delay and distortion: laws, measurements and failure modes

This page describes OpenSynth's fractional delay and oversampled distortion.
Both modules report their latency to the render graph for compensation.

## 1. Delay

### 1.1 Signal path

```
x[n] ──┬──────────────────────────────► dry ring L ─► dry = √(1−w) ──┐
       │                                                              ├─ out
       └─► (+) ─► line[2^21] ─► 8-tap/256-phase read ─► wet = √w·d ──┘
            ▲                        │
            └── feedback · ZDF damp ◄┘
```

- The line is **2^21 f64 samples per channel** (16 MiB each): 43.7 s at
  48 kHz, 21.8 s at 96 kHz. A stereo state is 32 MiB; a rack holds at most two
  delay states and allocates one only when an enabled `delay` module enters the
  chain on the load path (`sync_from_patch`), so the audio path never
  allocates.
- The wet tap is the fractional read. The damping filter is in the feedback
  path only, so the first repeat is the input and later repeats darken. The dry
  path is delayed by the module latency (§1.6), so dry and wet stay aligned.
- The wet/dry mix uses a power crossfade: `dry = √(1−w)` and
  `wet = √w·delayed`. At `w = 0` the output is the delayed dry signal.

### 1.2 Fractional read

`pos = write - (t·fs + L)`; the read takes eight taps at integer offsets
`floor(pos)-3 … floor(pos)+4`, selected from a **256-phase** table. The kernel
is a Kaiser-windowed sinc (`β = 8`) over the eight taps, and every phase row is
normalized to unity DC gain. The table is built once when the delay state is
prepared (16 KiB).

Measured response of the read kernel at a half-sample position (the input is a
sine, the read advances by one sample per output sample):

| Frequency | 1 kHz | 5 kHz | 10 kHz | 15 kHz | 18 kHz | 20 kHz | 22 kHz |
|---|---:|---:|---:|---:|---:|---:|---:|
| Gain, dB | 0.000 | +0.003 | −0.112 | −1.279 | −3.502 | −6.398 | −12.032 |

Fractional-read pitch accuracy: a 1 kHz tone read at **1.5 samples per output
sample** (a 1.5x fractional read) measures 1500.000000 Hz, error **< 0.001
cent** over 40 000 samples. The test locates positive zero crossings and
regresses the period.

### 1.3 Time law

The stored `time` parameter is a control, not seconds:

```
t = 2^(20x - 2)
```

| control x | 0 | 0.05 | 0.1 | 0.15 | 0.2 | 0.25 | 0.3 | 0.35 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| seconds | 0.25 | 0.5 | 1 | 2 | 4 | 8 | 16 | 32 |

The minimum is 0.25 s. The top of the curve is clamped by the line and the
latency: 43.7 s at 48 kHz, 21.8 s at 96 kHz.

### 1.4 ZDF feedback damping

The feedback path uses a trapezoidal (topology-preserving) one-pole low-pass.
With `g = tan(π·fc/fs)`:

```
k = g / (1 + g)
v = (x - s)·k
y = v + s
s = y + v
```

`k` is the coefficient law; the state update is `s ← s + 2v`. The cutoff is the
existing tone control `fc = 250·(18000/250)^tone`, clamped to 49 % of the
sample rate. The filter has unity DC gain and reaches −3.01 dB at `fc` by
construction; measured response:

| `fc` | k | 100 Hz | 1 kHz | 5 kHz | 10 kHz | 15 kHz |
|---|---:|---:|---:|---:|---:|---:|
| 250 Hz | 0.0161 | −0.64 | −12.32 | −26.35 | −33.42 | −39.23 |
| 1 kHz | 0.0615 | −0.04 | −3.01 | −14.44 | −21.40 | −27.18 |
| 5 kHz | 0.2534 | −0.00 | −0.16 | −3.01 | −7.86 | −13.10 |
| 18 kHz | 0.7071 | −0.00 | −0.00 | −0.09 | −0.42 | −1.41 |

The filter is lossy at every cutoff below Nyquist, so a feedback loop at
`g = 1` still decays. There is no self-oscillation mode.

### 1.5 Feedback is linear in the control

`feedback = values[1].clamp(0, 1)`; the control is the loop gain. A 0.1 step
input driven through a 0.25 s delay at `mix = 1` settles to
`0.1/(1−g)`:

| feedback control | measured | law | error |
|---|---:|---:|---:|
| 0.2 | 0.125000 | 0.125000 | 0.0000 % |
| 0.5 | 0.200000 | 0.200000 | 0.0000 % |
| 0.8 | 0.499991 | 0.500000 | −0.0018 % |

At 0.95 the loop needs more than 50 repeats to settle (it was still 8.1 %
short); that is the geometric series, not a nonlinearity. The regression test
uses 0.2/0.5/0.8.

### 1.6 Latency and PDC

The module reports a fixed **62.5 ms** latency: `round(0.0625·fs)` samples,
3000 at 48 kHz and 6000 at 96 kHz (2756 at 44.1 kHz). The dry path is delayed by
that many samples and the wet read is at `t + L`, so the first echo is still
`t` after the dry. It is not a hidden offset:

- `fx::fx_rack_latency` returns the enabled rack latency; `SynthFxChain::
  latency_samples` returns the same value for a live chain.
- `SynthPatch::fx_latency_samples` exposes it to the engine. The built-in track
  renderer returns it as the track's source latency
  (`render_builtin_tracks_parallel`), and the render cache passes that vector to
  the graph, so the mixer's PDC delays the other tracks by the difference.
- A bass track with two serial delays (one at wet 0) plus the shaper reports
  6097 samples and the render grows by that lead-in. The pad and pluck report
  3000/3097.

Consequences:

- `w = 0` is a dry pass **delayed by L**, not a bit-exact bypass. A wet-0 delay
  module still reports its latency, because its dry path is delayed.
- The offline renderer keeps the graph's `pre_master_latency_samples` as a
  leading silence and grows the file by that amount (the existing convention
  for source latency). PDC aligns the tracks, not the file origin.
- Stems are captured before the per-track PDC delay (and before the lead-in
  trim), so a stem is offset by its own rack latency; a parallel dry branch
  also appears at its own path latency. The stems are 6097 frames longer than
  the dry render and start at different offsets.
- A rack's latency is the serial sum of its enabled modules. A delay in each of
  two **parallel** branches is counted twice; the rack cannot report one latency
  that aligns both branches. The parallel delay is in one branch and the
  serial sum equals the maximum path because the post-parallel distortion is
  serial. This is the remaining topology limit.

### 1.7 Failure modes

- **Wet 0 is not a bypass.** It delays by 62.5 ms; a chain with a wet-0 delay
  still moves the track in PDC.
- **The time control's minimum is 0.25 s.** A sub-250 ms delay cannot be
  dialled; the old seconds field allowed it.
- **The 8-tap kernel darkens the wet path.** −3.5 dB at 18 kHz and −6.4 dB at
  20 kHz relative to the dry. This is inherent to an 8-tap read at 48 kHz; the
  damping filter then darkens each repeat further.
- Both channels share one delay time.
- **Above 96 kHz** the reported latency is capped at 6000 samples and the line
  holds 2^21 samples, so the maximum time shrinks with the rate.
- **No tape modulation.** Reads are time-invariant; no wow/flutter is added.

## 2. Distortion

### 2.1 Oversampling path

The shaper runs at **8x**. The path is three 2x half-band stages up, the
nonlinearity at 8x, and three half-band stages down. It reuses the synth's
existing **113-tap half-band kernel** (the one that builds the wavetable mip
pyramid) for every stage; no second filter design was added. The cascade has a
fixed **97-sample** nominal latency at the base rate; the module delays its dry
path by the same amount, and the rack latency includes it.

- Identity gain at 1 kHz is 0.000 dB (within 0.05 dB).
- The recombined path is not perfectly transparent: the half-band pair has a
  small phase/amplitude ripple, and the best integer alignment is 97 samples
  (the RMS error against the delayed 1 kHz sine is < 0.02 at 0.5 peak).
- 8x was chosen over 4x on the measured acceptance test: at a 0.5-peak 5 kHz
  input and maximum drive, 4x folded at **−44.4 dBc** while 8x folded at
  **−75.7 dBc**. The alias test requires below −60 dBc.

Measured alias floor for a 5 kHz sine at maximum drive, 2 400-sample DFT
window (non-harmonic bins only):

| mode | stages | peak | worst fold |
|---|---:|---:|---:|
| Soft (0) | 2 | 0.5 | **−75.7 dBc** |
| Fold (2) | 8 | 0.5 | −18.4 dBc |
| Crush (5) | 8 | 0.5 | −55.8 dBc |
| Soft (0) | 16 | 0.5 | −43.6 dBc |

The acceptance test uses the default Soft/2 palette at 0.5 peak and requires
< −60 dBc. The harsher palettes and very high stage counts are still
alias-limited; see §2.5.

### 2.2 Stage laws and the mode enum

`mode` is a numeric enum, `stages` is clamped to 2..16. The cascade starts at
the mode's law and rotates through the palette:

```
law = (mode + stage) % 6
scale = 1 + 0.12 * (stage / 6)
value = law(value * scale) / scale
```

The six laws are normalized to unit small-signal slope:

| mode | law | formula |
|---|---|---|
| 0 Soft | soft clip | `tanh(x)` |
| 1 Hard | hard clip | `clamp(x, -1, 1)` |
| 2 Fold | wave fold | `sin(x)` |
| 3 Asymmetric | asymmetric drive | `(tanh(x) + b·(tanh(x+b) − tanh(b))) / G`, `b = 0.35`, `G = 1 + b·sech²(b) = 1.3104` |
| 4 Diode | rectified blend | `(tanh(x) + 0.6·max(tanh(x), 0)) / 1.3` |
| 5 Crush | quantise-ish | `clamp(((1+0.7)x − 0.7·round(8x)/8) / 1.7, -1, 1)` |

The stage scale makes every stage in a 16-stage cascade a distinct transfer:
stages 0–5 run at scale 1, 6–11 at 1.12, 12–15 at 1.24. The default is
`mode = 0` (Soft) and `stages = 2`; the second law (hard clip) is transparent
below unity, so the default is the measured soft law with a multi-stage path
available.

The biased laws (Asymmetric, Diode) produce DC, so a 10 Hz one-pole DC blocker
(`pole = exp(-2π·10/fs)`) follows the shaper. The tone filter and the `bw`
blend keep their previous positions after the shaper.

Measured stage transfer `shaper_cascade(mode, 4, input·drive)` at inputs
0.25/0.5/1.0:

| mode | drive 0.2 | drive 0.5 | drive 0.9 |
|---|---|---|---|
| Soft | 0.04970 / 0.09842 / 0.19082 | 0.12224 / 0.23366 / 0.40624 | 0.21255 / 0.37743 / 0.55394 |
| Hard | 0.06117 / 0.12113 / 0.23489 | 0.15045 / 0.28768 / 0.50093 | 0.26167 / 0.46523 / 0.68397 |
| Fold | 0.06117 / 0.06966 / 0.13195 | 0.09898 / 0.18474 / 0.29504 | 0.15873 / 0.25934 / 0.42662 |
| Asymmetric | 0.06112 / 0.06975 / 0.13264 | 0.09904 / 0.18532 / 0.30110 | 0.15941 / 0.26520 / 0.39851 |
| Diode | 0.06141 / 0.07108 / 0.13908 | 0.10123 / 0.19593 / 0.30172 | 0.16780 / 0.30351 / 0.47871 |
| Crush | 0.04994 / 0.04847 / 0.09660 | 0.07333 / 0.14549 / 0.28204 | 0.12116 / 0.23710 / 0.47306 |

### 2.3 Drive and trim

Drive increases the level into the shaper. A peak envelope tempers this drive
on louder signals. Output trim follows the effective drive so the small-signal
gain remains stable for a given setting. At zero drive the signal takes a
linear path through the same oversampler and retains the same latency.

### 2.4 Latency

`DISTORTION_LATENCY = 97` samples (2.02 ms at 48 kHz). The dry path is delayed
by 97 samples, the wet path has the same nominal delay, and
`fx_rack_latency`/`SynthFxChain::latency_samples` add it to the track's source
latency. It therefore participates in PDC exactly like the delay's 62.5 ms; it
is not a hidden offset.

### 2.5 Failure modes

- **Alias at hot levels and harsher palettes.** The default Soft/2 law meets
  −60 dBc at 0.5 peak, but Fold/8 folds at −18.4 dBc, Crush/8 at −55.8 dBc and
  Soft/16 at −43.6 dBc. The 8x path cannot remove harmonics that alias inside
  the 8x domain; a higher factor or an antiderivative shaper would be needed.
  The default law is what the factory presets use.
- More oversampled stages increase CPU cost.
- **No pre/post tone switch.** The tone filter is always after the shaper.
- **The mode enum is ours.** Preset mode strings are not imported, so an
  imported patch uses Soft until the user changes the mode.

## 4. Regression tests

`crates/oc-synth-fx/src/lib.rs`:

- `delay_time_control_follows_the_curve_map` — the 0/0.05/0.1/0.15/0.2/0.25/
  0.3/0.35 table, monotonicity and the line clamp.
- `delay_fractional_read_tracks_pitch_within_one_cent` — 1.5x read, < 1 cent.
- `delay_damping_filter_is_a_zdf_one_pole` — −3.01 dB at each cutoff and unity
  step response.
- `delay_feedback_is_linear_in_the_control` — 0.2/0.5/0.8 settle to the law.
- `delay_reports_its_latency_and_delays_the_dry_path` — 3000 samples at 48 kHz,
  the delayed dry and the first echo.
- `delay_staging_is_a_constant_power_crossfade` — dry `√(1−w)` and wet `√w`
  impulse amplitudes at four wets.
- `delay_full_wet_is_level_neutral` — full wet within 0.2 dB of the dry.
- `delay_tail_decays_to_silence`.
- `fx_rack_latency_matches_the_chain` — delay + distortion, disabled modules 0.
- `distortion_oversampler_is_latency_aligned_and_flat` — peak/best lag 97,
  identity gain.
- `distortion_shaper_modes_have_distinct_transfers` — six modes × three drives,
  zero crossing, oddness of the symmetric laws, bounded 16-stage cascade.
- `distortion_oversampling_rejects_alias_above_minus_60_db` — 5 kHz at max
  drive, 0.5 peak, < −60 dBc.
- `distortion_drive_map_is_monotone_and_trim_is_unit_at_default_mode` — drive
  table, `trim(0) = 1`, measured small-signal gain at five controls.
- `distortion_zero_drive_is_a_harmonic_null` — harmonics below −120 dBc and the
  −6.02 dB gain.
- `distortion_small_signal_gain_follows_the_one_pole_law`,
  `distortion_thd_follows_the_multi_stage_law` — the taper and the new THD
  table.

`crates/oc-engine/src/render_cache/tests/mix.rs`:

- `builtin_synth_fx_latency_is_reported_for_source_compensation` — a preset with
  `fx.1.type = delay` and `fx.2.type = distortion` reports 3000 + 97 samples.

## 5. What is still missing

- A single rack latency cannot align both sides of a parallel split when both
  branches contain a delay; the serial sum is reported.
- The delay's L/R offset, stereo cross-mix, bandwidth control, tempo sync and
  tape modulation are not modelled.
- The distortion's mode strings and pre/post tone switch are not imported.
- The harsher shaper palettes remain alias-limited at hot levels.
- The legacy `render::render_to_buffer` fallback (SoundFont, no plugin
  instances) still builds its graph with an empty source-latency list. The
  authoritative CLI/render-cache path reports and compensates the synth rack
  latency; the fallback does not.
