# HyperD and Conv: wide-field stereo and convolution

The rack's last two FX slots have their own engines: `SynthFxType::HyperD`
(id 16) and `SynthFxType::Conv` (id 17). Both ids follow the numeric ids 0..15,
so stored `fx.N.type` values, presets and DSL files keep their meaning. They use
the same layout code (three serial racks, split/parallel nodes) as every other
FX type.

Both engines are post-voice-sum stereo inserts. They share the rack's value
palette (`MAX_FX_VALUE_SLOTS = 12`), the amplitude wet/dry crossfade and the
`level_out` trim. `process` allocates nothing, takes no locks and starts no
threads; every buffer is allocated in `SynthFxChain::new`.

## Parameter surface

The factory-preset importer maps these two classes onto the new types. The
names that were already emitted stay the same, so imported racks and their
`fxmod` routes are unaffected.

| Type | Parameters (default) | Notes |
|---|---|---|
| HyperD | `rate` 0.35 Hz, `depth` 0.35, `feedback` 0.5, `spread` 1.0, `mix` 0.5, `level_out` 1.0 | `spread` is the only new name: it scales the network time base. `feedback` is the allpass regeneration coefficient, not a delay loop. |
| Conv | `size` 0.5, `damp` 0.3, `predelay` 0.02 s, `width` 0.8, `mix` 0.25, `level_out` 1.0 | `size` selects the generated IR bucket (four rooms/plates); the other names are shared with the old reverb palette. |

Project files that stored the old chorus/reverb fallback keep those types and
sound unchanged; re-importing the preset selects the new engines.

The importer keeps mapping the wet and output trims, plus `rate`/`detune` for
HyperD and `size`/`damping`/`predelay`/`width` for Conv. The imported `size` is
the stored percentage divided by 100 and clamped, so it is a bucket hint rather
than a measured room size. Unmapped preset controls remain in the raw parameter
list.

## HyperD topology

The widener builds a decorrelated **side** signal and adds it to the mono mid,
so the mono sum is preserved by construction:

```
m  = (L + R) / 2
a  = four-stage allpass diffuser A (m)       chain A
b  = four-stage allpass diffuser B (m)       chain B
s  = 1.8 * (a - b) / 2                       side, then 1-pole HP at 150 Hz
L' = m + s        R' = m - s                 wet pair
out = (1 - mix) * (L, R) + mix * (L', R')
```

Each diffuser is four first-order Schroeder allpass units
`v[n] = x[n] + g*v[n-D]`, `y[n] = -g*v[n] + v[n-D]`, with a fractional
`z^-D` read (linear interpolation). The delay is modulated per sample:

| Stage | Base delay A/B (ms) | Modulation scale | LFO rate multiplier A/B | LFO phase A/B |
|---|---|---|---|---|
| 1 | 0.9 / 1.1 | 1.00 | 1.00 / 1.17 | 0.00 / 0.50 |
| 2 | 1.5 / 1.8 | 0.80 | 1.31 / 1.53 | 0.17 / 0.71 |
| 3 | 2.2 / 2.6 | 0.60 | 1.73 / 1.91 | 0.41 / 0.93 |
| 4 | 3.1 / 3.5 | 0.45 | 2.17 / 2.39 | 0.66 / 0.19 |

- `D = base * spread + 0.8 ms * depth * scale * sin(2π(phase * mult + offset))`,
  clamped to the 1024-sample stage buffer (10.7 ms at 96 kHz).
- `g = 0.35 + 0.4 * feedback`, so the knob spans light (0.35) to dense (0.73)
  diffusion. The allpass structure keeps the magnitude flat; the impulse
  response is a dense geometric series rather than a single echo.
- `spread` (0.25..2.0) scales all eight delays together.
- The side is high-passed at 150 Hz so the sub band stays centred. The mid is
  the mono input itself, so `L' + R' = L + R` exactly at every mix.

The two chains use different delay lengths and different LFO rates/phases,
which is what decorrelates L and R. The phase accumulator advances at `rate`
and all eight stages read scaled phases from it.

### Measured (48 kHz, release)

| Quantity | Value |
|---|---|
| side/mid energy, wet-only (`mix` 1) | **-2.17 dB** |
| side/mid energy at default `mix` 0.5 | -8.19 dB |
| mono-sum loss, mono or stereo input, any mix | **0.0000 dB** (identity by construction) |
| impulse response taps above -60 dB | **1123** taps, last at 1815 samples (37.8 ms) |
| single modulated delay at comparable settings | 2 taps (a plain chorus) |
| output change between LFO phase 0.00 and 0.37 | **86.3 %** of signal RMS |
| output change between the same phases at `depth` 0 | 0 (bit-identical) |
| CPU per instance | **1.0 %** of one core at 48 kHz |

The phase test is the "not a plain chorus" check: the same block at two
modulation phases diverges substantially, while switching the phase with the
LFO depth at zero changes nothing, so the difference comes from modulation and
not from a fixed delay offset. The impulse response confirms the structural
difference: a chorus is one delayed copy, the diffuser spreads the impulse
across more than a thousand taps.

## Conv: generator and partitioned convolution

### IR generation law

Each state owns two independent IR channels (true stereo). The channels use
different SplitMix64 seed streams, so the taps are decorrelated. For a preset
with length `T` seconds:

1. `RT60 = T / 2`, `τ = RT60 / ln(1000)`, so the exponential envelope reaches
   -120 dB at `T` before the IR is truncated.
2. **Early reflections**: `early` taps at `2 ms + u^1.6 * 0.35 T`, each with a
   sign and an amplitude `(0.35 + 0.5 u) * exp(-t/τ)`.
3. **Diffuse tail** from 2 ms to `T`: white noise through a one-pole low-pass
   whose cutoff falls from `damping_hz` to `0.12 * damping_hz` across the IR,
   multiplied by `exp(-t/τ)` and 0.6. This is the documented damping law.
4. **Diffusion**: three Schroeder allpass stages (1.7, 2.3, 3.1 ms) with the
   preset coefficient smear the sparse onset into a diffuse field.
5. **Direct tap** 0.25 at sample 0, added after diffusion so it stays clean.
6. **Fade**: a 10 ms raised-cosine fade-out at the end.
7. **Normalisation**: the stored IR has unit sum of squares, so a white-noise
   input keeps its RMS through a wet-only convolution.

| Bucket (`size`) | Length | RT60 | Diffusion | Damping start | Early taps | Measured -120 dB at | Measured initial slope |
|---|---|---|---|---|---|---|---|
| 0 small room | 240 ms | 120 ms | 0.60 | 7.0 kHz | 12 | 233 ms | -506 dB/s |
| 1 medium room | 320 ms | 160 ms | 0.64 | 5.5 kHz | 14 | 313 ms | -362 dB/s |
| 2 large room | 400 ms | 200 ms | 0.66 | 4.5 kHz | 16 | 380 ms | -299 dB/s |
| 3 plate | 360 ms | 180 ms | 0.72 | 9.0 kHz | 10 | 352 ms | -333 dB/s |

The slope is a least-squares fit of the 5 ms energy windows from 5 ms to
105 ms of the wet impulse response. `-120 dB` is the last sample above
`1e-6` of the response peak, measured without the post-damping filter.

### Partitioned convolution

- **Block size** `B = 128` samples, **FFT size** `2B = 256`.
- The IR is split into `P = ceil(ir_len / B)` partitions; `P <= 300`.
- The frequency-domain delay line stores bins `0..=128` of the last `P` input
  block spectra (Hermitian symmetry halves the storage). The IR spectra are
  precomputed when the rate or bucket changes.
- Per block: forward FFT of `[x, 0]`, multiply-accumulate over the `P`
  partitions, mirror the Hermitian half, inverse FFT, overlap-add with the
  previous block's second half.
- **Maximum IR length**: 0.4 s at 96 kHz (38 400 taps); 0.4 s at 48 kHz is
  19 200 taps.
- **Reported latency**: `CONV_LATENCY = B = 128` samples, i.e. 2.67 ms at
  48 kHz and 1.33 ms at 96 kHz. It is exposed as
  `SynthFxChain::latency_samples()` and `OpenSynth::fx_latency_samples()`.
- **Compensation**: the module delays its dry path by exactly `B` samples
  before the crossfade, so dry and wet are sample-aligned at every mix. Tests
  check the identity sample by sample: `out = (1-m) * dry[n-B] + m * wet[n-B]`,
  the first wet sample of an impulse at `B`, and `mix = 0` reproducing the
  input delayed by `B`. The rack does not take part in the mixer's cross-track
  delay compensation, so a convolution insert shifts its instrument by `B`
  samples; place it where that is acceptable or compensate downstream.

### State and cost

| Item | Per state |
|---|---|
| FDL + IR spectra (two channels) | 2 × (300 × 129 × 16 B + 300 × 129 × 16 B) = 2.36 MiB |
| Time-domain taps (two channels) | 2 × 38 400 × 8 B = 0.59 MiB |
| Pre-delay, dry delay, diffusion lines, FFT tables | 0.31 MiB |
| **Total** | **≈ 3.25 MiB** |

The rack initially reserves two Conv states (≈ 6.5 MiB). Additional Conv modules
receive independent states during rack preparation, up to the 16-module rack limit.
Removed modules retain their prepared storage for reuse. HyperD holds eight 1024-sample buffers per module: 64 KiB × 16
modules = 1 MiB.

Measured CPU per instance in a release build, one core, one insert and no other
FX (360 ms plate, `mix` 1):

| Rate | Realtime factor | Share of one core |
|---|---|---|
| 48 kHz | 0.035x | **3.5 %** |
| 96 kHz | 0.110x | **11.0 %** |

Changing the sample rate or the `size` bucket regenerates the IR and its
partition spectra on the next block. That work is bounded by the fixed
`MAX_IR_SAMPLES` and the preallocated FFT scratch; it allocates nothing and is
not per-sample. A rate change also clears the convolution state.

## Tests

`crates/oc-synth-fx/src/lib.rs`:

- `hyperd_impulse_response_is_a_diffuse_network` — at least 16 significant taps
  and a tail past 5 ms.
- `hyperd_modulation_phase_changes_the_output` — two phases differ by > 5 % RMS;
  the same phases at `depth` 0 are bit-identical.
- `hyperd_side_is_wide_and_the_mono_sum_is_exact` — side/mid above -6 dB and the
  mono-sum error below 1e-12.
- `hyperd_is_loudness_sane`, `convolution_and_hyperd_wet_levels_are_sane` — wet
  RMS within ±6 dB of the input.
- `convolution_ir_generation_is_deterministic` — two constructions produce
  identical taps; a different bucket produces different taps.
- `convolution_matches_direct_convolution` — the partitioned output equals the
  direct FIR within `1e-9` of the peak.
- `convolution_latency_and_crossfade_are_exact` — the one-block latency, the
  delayed dry path and the `mix = 0.5` crossfade are exact.
- `convolution_tail_decays_below_minus_120_db` — every preset is below -120 dB
  after its documented length.
- `convolution_reset_is_idempotent` — two resets followed by an impulse match a
  fresh state; HyperD reset leaves silence.

`crates/oc-synth/tests/fx_parallel_alloc.rs` adds a counting-allocator test for
each new type over two seconds of stereo playback, including a rate change for
Conv.

## Deliberate divergences and remaining limits

- **HyperD is mono-derived.** The wet pair is built from `(L+R)/2`, so it never
  collapses an existing stereo field; it widens the mono image. That is why the
  mono sum is exact and why a hard-panned stereo source is centred at `mix = 1`.
- **HyperD has no long tail.** The allpass regenerators ring for tens of
  milliseconds; the rack reports a 150 ms tail, which covers the measured 37.8 ms
  response with margin.
- **Conv has no file IO and no IR library.** The four synthetic rooms/plates are
  generated deterministically from the law above. There is no way to load a
  measured IR, and the bucket change is a switch, not a morph.
- **Conv is short.** 240–400 ms keeps the memory and the -120 dB tail inside the
  fixed state budget. Longer halls would need a bigger `MAX_IR_SAMPLES` and more
  partitions; the partition scheme itself is unchanged.
- **`damp` is a post filter.** It does not rebuild the IR; `damp = 0` bypasses
  it so the wet path is exactly the IR.
- **Side-chain/PDC.** The synth rack does not report latency into the mixer's
  delay compensation; only the module's own dry/wet alignment is compensated.
