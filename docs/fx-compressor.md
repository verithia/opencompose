# Synth compressor: three-band RMS compander and lookahead limiter

The synth rack's `compressor` module is a three-band RMS compander with a
lookahead limiter at its output. It replaces the single-band feed-forward peak
compressor. The module keeps its numeric FX type (`1`) and the `compressor`
spelling; the control palette grew from one band to three bands.

The implementation is `crates/oc-dsp/src/compander.rs`; the rack wrapper and
the latency plumbing are in `crates/oc-synth-fx/src/lib.rs`. Measured tables
below come from the regression tests in those files at 48 kHz unless stated.

Sections 1–11 describe that synth-rack module. Section 12 documents the mixer
`compression` device's detector sidechain EQ, which lives in
`crates/oc-dsp/src/compressor.rs` and is a separate device; its full parameter
table is in `devices/params/compression.md`.

## 1. Signal path

```
x ──► 3-band crossover (120 Hz, 2.5 kHz) ──► RMS detector + static gain per band
                                                   │
       dry ──────────────────────────────► mix ◄───┘
                                              │
                                   2 ms lookahead limiter ──► out
```

- The split is the complementary crossover in `oc-dsp::crossover`: three bands,
  **12 dB/oct Linkwitz-Riley alignment**, `PhaseMode::Minimum`. The low split is
  at **120 Hz** and the high split at **2.5 kHz**; both are controls and a
  preset can override them.
- Each band has its own mean-square detector and static gain. Threshold, ratio,
  below-threshold ratio and attack/release are shared by the three bands.
- The wet signal is the sum of the three bands. `mix` is the rack's shared
  linear crossfade (`dry = 1 − mix`, `wet = mix`); the limiter is after the
  crossfade, so wet 0 is a delayed dry pass, not a bit-exact bypass.
- The limiter delay is the module's reported latency (§5).

## 2. Crossover flatness

The band sum must be magnitude-flat, otherwise a static gain would tilt the
module even with no gain reduction. Measured with all bands below threshold,
expansion off, limiter off, a 0.2-peak sine from 60 Hz to 16 kHz:

| Frequency | 60 Hz | 250 Hz | 900 Hz | 2.5 kHz | 9 kHz | 16 kHz |
|---|---:|---:|---:|---:|---:|---:|
| Band-sum deviation | < 0.0001 dB | < 0.0001 dB | < 0.0001 dB | < 0.0001 dB | < 0.0001 dB | < 0.0001 dB |

The measured worst deviation in the test is below the 0.0001 dB print
resolution; the test accepts up to **0.1 dB**. The individual bands are not
unity at every frequency even though the sum is: at the probe centres the
detected band level is about +0.9 dB (low), +1.8 dB (mid) and +2.2 dB (high)
relative to the input. The detector drives the gain law from the band signal,
so that offset is part of the per-band threshold behaviour, not a level error
in the sum.

## 3. RMS detector and per-band gain

The detector is a **one-pole mean-square follower per band**:

```
power ← c·power + (1−c)·½(L² + R²)
c = exp(−1 / (τ·fs))          τ = attack when power rises, release when it falls
level_dB = 10·log10(max(power, 1e−30))
```

- **Window:** the detector is exponential, not block-based. Its memory is the
  time constant `τ`; after a step the power reaches `1 − 1/e` of the new value
  in `τ`. Attack and release are separate `τ`s, shared by all three bands.
  Defaults: attack 10 ms, release 100 ms; the controls are 0.1–200 ms and
  5–2000 ms.
- **RMS, not peak:** `power` is the stereo mean square; the reported level is
  the RMS dBFS of the band. The threshold is therefore an RMS threshold.
- **Per-band gain:** each band has an independent static gain (`band1_gain` …
  `band3_gain`, −24..+24 dB). The band's detector produces its own gain change;
  the three changes are not linked.

Static law for one band, with `g` the static band gain:

```
over = level − threshold
g -= (1 − 1/ratio)·over                    when over > 0
g += (1 − 1/ratio_below)·(threshold_below − level)   when level < threshold_below
g = clamp(g, −60 dB, +36 dB)
```

The curve has a hard knee at the threshold; the detector's attack and release
smooth the gain change. Without expansion, a band below its threshold passes at
its static gain (zero by default), so **0 dB of gain reduction is unity gain**.
Measured at
threshold −20 dB RMS, ratio 4, expansion off, attack 1 ms, release 50 ms:

| Band | Probe | Input RMS | Detected in-band | Gain change |
|---|---:|---:|---:|---:|
| Low | 60 Hz | −40.0 dB | −39.13 dB | 0.00 dB |
| Low | 60 Hz | −30.0 dB | −29.13 dB | 0.00 dB |
| Low | 60 Hz | −10.0 dB | −9.13 dB | −8.15 dB |
| Low | 60 Hz | 0.0 dB | +0.87 dB | −15.65 dB |
| Mid | 700 Hz | −40.0 dB | −38.22 dB | 0.00 dB |
| Mid | 700 Hz | −30.0 dB | −28.22 dB | 0.00 dB |
| Mid | 700 Hz | −10.0 dB | −8.22 dB | −8.84 dB |
| Mid | 700 Hz | 0.0 dB | +1.78 dB | −16.34 dB |
| High | 8 kHz | −40.0 dB | −37.82 dB | 0.00 dB |
| High | 8 kHz | −30.0 dB | −27.82 dB | 0.00 dB |
| High | 8 kHz | −10.0 dB | −7.82 dB | −9.14 dB |
| High | 8 kHz | 0.0 dB | +2.18 dB | −16.64 dB |

The gain change is the law evaluated at the detected level; for example at
−9.13 dB the low band is `−0.75·(−9.13 + 20) = −8.15 dB`.

## 4. Below-threshold expansion

The expansion is **upward**: a band below `threshold_below` is boosted by
`(1 − 1/ratio_below)·(threshold_below − level)`, clamped at +36 dB. A quiet
band therefore does not simply pass at unity; it follows a slope steeper than
1:1. Downward expansion (gating) would attenuate quiet bands instead, which is
a different behaviour class and would not preserve the measured
below-threshold control. `ratio_below = 1` disables the region; that is the
default for imported single-band presets, which do not store the control.

Measured with threshold 0 dB, ratio 4, `threshold_below` −30 dB,
`ratio_below = 2` (slope 0.5), attack 1 ms, release 50 ms:

| Input RMS | Detected in-band | Gain change |
|---:|---:|---:|
| −40.0 dB | −39.13 dB | **+4.57 dB** |
| −30.0 dB | −29.13 dB | 0.00 dB |
| −20.0 dB | −19.13 dB | 0.00 dB |
| +6.0 dB | +6.87 dB | −5.15 dB |

The quiet band is lifted 4.57 dB; the region above `threshold_below` is
neutral until the compression threshold; above 0 dB the ratio applies.

## 5. Lookahead limiter

The limiter is a sample-peak limiter with a **2 ms lookahead** and a fixed
**−0.3 dBFS ceiling**. Two milliseconds covers the rack's 1 ms modulation
control tick plus the sample-accurate attack, while keeping the added PDC far
below the delay's 62.5 ms; the ceiling matches the existing limiter's default
and the 80 ms release is slower than the compressor's fastest release, so the
limiter does not pump. It keeps a delayed copy of the mixed signal and a
monotonic window of the gain each recent input sample requires; the applied
gain is the window minimum, so a delayed sample is always attenuated by at
least what its own peak needs. The release is a one-pole toward unity with an
80 ms time constant; attack is sample-accurate.

Measured at 48 kHz:

| Property | Value |
|---|---:|
| Lookahead / reported latency | **96 samples** (2.0 ms) |
| Latency at 44.1 kHz / 96 kHz | 88 / 192 samples |
| Ceiling | −0.3 dBFS sample peak |
| 2.0-amplitude impulse | output peak ≤ 0.9772, at +96 samples |
| 0.1-amplitude impulse | 0.1, at +96 samples |
| 0-dB-GR transparency (440 Hz) | **−0.001 dB** |
| `mix = 0` | input delayed by 96 samples, sample-exact |

The limiter is enabled by default (`limiter = 1`). Its delay is the module's
latency; see §6. It is a sample-peak limiter, not a true-peak limiter; the
mastering limiter remains the true-peak stage.

## 6. Latency and PDC

The module reports its limiter lookahead through the same path as the delay:

- `Compander::latency_samples` returns the lookahead when the limiter is
  enabled and 0 when it is disabled.
- `SynthFxChain::latency_samples` sums it with the other enabled modules.
- `fx_rack_latency` returns the same serial sum from the patch, and
  `SynthPatch::fx_latency_samples` exposes it to the engine.
- The built-in track renderer returns it as the track's source latency and the
  render cache passes it to the mixer's PDC.

A rack with a 3000-sample delay, a 97-sample shaper and two 96-sample
compressors reports `3000 + 97 + 2×96 = 3289` samples. A bass rack reporting
6097 keeps the file's leading PDC silence at 6097: **the render length does not
change** (see §7). The pluck is delayed by 2808 samples in PDC instead of 3000;
its stem, captured before the per-track PDC delay, moves by the 192 samples the
compressors added plus the crossover's group delay.

## 9. Failure modes

- **Wet 0 is not a bypass.** The limiter is after the mix, so `mix = 0` is the
  dry signal delayed by 96 samples at 48 kHz; it still moves the track in PDC.
- **Upward expansion lifts noise.** With `ratio_below > 1`, a quiet band is
  boosted; near silence the +36 dB clamp bounds it, but there is no gate or
  expansion floor. Use `ratio_below = 1` on noisy material.
- **Shared controls.** Threshold, ratio, below-threshold ratio, attack and
  release are shared; only the static band gain is per-band. There is no
  per-band ratio or per-band threshold.
- **Band thresholds are detector-referenced.** The minimum-phase split gives
  the bands different magnitudes at the probe centres (+0.9/+1.8/+2.2 dB in
  §3), so the same threshold value acts up to 2.2 dB earlier on the high band
  than on the input level suggests. The sum is still flat.
- **Phase, not magnitude, moves transients.** The crossover is minimum-phase;
  the band sum is flat in magnitude but not a pure delay. The pluck peak
  changes 0.39 dB with no gain reduction.
- **The threshold is RMS.** A sine's peak is 3.01 dB above its RMS; an
  `.ocp` author setting `threshold: -20` with a sine at −20 dBFS peak will
  compress 3 dB earlier than the number suggests. The importer compensates only
  for the measured single-band taper, not for hand-written patches.
- **The limiter is fixed.** 2 ms, −0.3 dBFS sample peak, 80 ms release; it is
  not true-peak and has no user ceiling or lookahead control.
- **The module is synth-rack only.** It is not an `AudioEffect` and is not in
  the mixer device schema; the mixer's `multiband` device still uses the older
  peak-linked detector.
- **Above 96 kHz** the lookahead grows with the sample rate; the rack's state
  is allocated on the load path and a sample-rate change follows the rack's
  existing prepare path.

## 10. Regression tests

`crates/oc-dsp/src/compander.rs`:

- `band_sum_is_flat_within_a_tenth_of_a_db` — 60 Hz..16 kHz, ≤0.1 dB.
- `rms_detector_follows_the_static_curve_in_every_band` — four levels × three
  bands, gain law evaluated at the detected level.
- `below_threshold_expansion_is_upward` — −40/−30/−20/+6 dB, slope 0.5.
- `lookahead_limiter_delays_by_its_reported_latency_and_holds_the_ceiling` —
  96 samples, ceiling, 0.1 impulse exact.
- `zero_gain_reduction_is_unit_magnitude` — 0-dB-GR transparency.
- `mix_zero_is_the_delayed_dry_signal` — sample-exact delayed dry.
- `latency_follows_the_sample_rate_and_the_limiter_switch`.
- `band_gain_is_a_static_trim`.
- `threshold_clamps_keep_the_expansion_below_compression`.

`crates/oc-synth-fx/src/lib.rs`:

- `compressor_reports_its_lookahead_latency` — `fx_rack_latency` and
  `SynthFxChain::latency_samples` agree; a disabled limiter reports 0.

`crates/oc-synth-import/src/foreign_preset.rs`:

- `compressor_reads_the_imported_threshold` — the imported threshold includes the
  sine crest offset.
- `single_band_compressor_maps_onto_the_three_band_compander` — crossovers,
  ratio, timing, make-up, per-band gain, and one deduplicated warning.

`crates/oc-engine/src/render_cache/tests/mix.rs`:

- `builtin_synth_compressor_latency_is_reported_for_source_compensation` — a
  preset with one compressor reports 96 samples.

## 11. What is still missing

- The `multiband` synth FX type still
  uses the older peak-linked three-band detector. Folding it onto this
  compander would need its per-band thresholds and ratios, which the current
  compressor palette does not expose.
- No per-band threshold, ratio or timing, and no sidechain filter or external
  key.
- No true-peak detection, auto release or user ceiling in the output limiter.
- The crossover is minimum-phase only; the linear-phase mode is available in
  the shared crossover but is not wired into this module.
- The detector is a one-pole mean-square follower; it is not a windowed RMS
  block and has no program-dependent release.

## 12. Mixer compressor: detector sidechain EQ

The mixer `compression` device (see `devices/params/compression.md`) has six
optional filter bands in front of its detector. They shape what the compressor
listens to, never what it outputs: the audio path is the input sample for
sample, so a band can boost 6 kHz by 12 dB and the dry signal still passes
untouched.

```
key (input or key port) ──► detector HPF ──► band 1 ──► … ──► band 6 ──► peak detector
```

Each band is one biquad, not a slope cascade: `sidechain_eqN_type` selects
Bell, Low Shelf, High Shelf, Low Cut or High Cut, and `sidechain_eqN_frequency`,
`sidechain_eqN_gain` and `sidechain_eqN_q` set it. Bands run in index order
after `detector_hpf`, and they filter an external `key` port as well as the
device input. All six default to off, and an off band costs one branch and keeps
its state reset, so the default detector path is the shipped one.

Measured at 48 kHz, threshold −30 dB, ratio 4:1, knee 0, −20 dBFS probes:

| Detector band | 8 kHz output | 2 kHz output |
|---|---:|---:|
| none | 0.03322 | 0.02982 |
| High Shelf, 6 kHz, +12 dB, Q 0.707 | 0.01399 (**−7.51 dB**) | 0.02949 (−0.10 dB) |

| Detector band | 60 Hz output | 4 kHz output |
|---|---:|---:|
| none | reference | reference |
| Low Cut, 400 Hz, Q 0.707 | **+7.18 dB** | +0.06 dB |

The shelf makes the detector hear the sibilant band 7.5 dB louder, so with the
shelf the same threshold reduces it and leaves the body alone; the low cut removes the
low band from the detector, so a 60 Hz tone stops triggering 7.2 dB of
reduction while a 4 kHz tone keeps it.

An external key is filtered the same way: an 8 kHz key at 0.1 amplitude under a
−24 dB threshold gains 12 dB of detector drive from the shelf and is reduced by
**7.47 dB** more than the unfiltered key.

Failure modes:

- **The bands are not a sidechain EQ on the audio.** A mix that needs an audio
  high-pass still needs an EQ device; a detector band changes when the
  compressor acts, not the tone.
- **Detector level is not output level.** Boosting a band changes the gain
  reduction, so the output gets quieter; `makeup` or `auto_makeup` compensates
  by the static curve, not by the band's gain.
- **Bands are one biquad each.** A 24 dB/oct detector slope needs two bands at
  the same frequency, and both count against the six.
- **Gain above about +18 dB can drive quiet material over the threshold on its
  own.** That is the intent of a detector band, but it also raises the noise the
  detector sees.

Tests: `detector_high_shelf_ducks_only_the_band_it_boosts`,
`detector_low_cut_removes_low_frequency_detection`,
`detector_eq_never_reaches_the_audio_path`,
`detector_eq_filters_an_external_key`, and
`defaults_render_the_pre_detector_eq_digest`, which pins the default render
digest captured before the bands existed.
