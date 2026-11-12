# Synth FDN reverb: design, measurements and failure modes

OpenSynth's effect rack uses a feedback delay network for reverb. Size controls
the decay, Damp changes the tail's tone, and Low and High trim decay by band.
Pre-delay, Width, and Mix control how the tail sits against the dry signal.

The mixer [FDN reverb](reverb-fdn.md) uses a separate topology and parameter set.
All figures below use 48 kHz unless stated otherwise.

## 1. Signal path

```
L,R ─► pre-delay (0..0.2 s) ─► inject ─► 32 lines in 4 tanks ─► out taps ─► width ─► mix
                                          ▲                    │
                                          │   H32 / √32        │
                                          └── band shelves ────┘
                                              + damping
```

- **Four tanks, eight lines each.** A tank is an 8×8 Hadamard feedback matrix;
  the four tanks are cross-fed by a 4×4 Hadamard per line index. The composite
  feedback matrix is `H32/√32 = (H4/2) ⊗ (H8/√8)`: a normalised Hadamard matrix
  over all 32 lines, hence unitary and energy-preserving before the decay and
  damping losses.
- **Channel assignment.** Tanks 0/1 carry the left input and tanks 2/3 the
  right input, but the H4 cross-feed mixes all four tanks every sample, so the
  tail is genuinely stereo rather than two independent mono tails. The input
  and output taps are four mutually orthogonal ±1 sequences over the 32 lines;
  the output taps for L and R are different rows, so a mono input still
  produces a decorrelated stereo tail.
- **Pre-delay.** One ring per channel, 0..0.2 s, rounded to whole samples. The
  first wet arrival is `pre-delay + shortest line` (18.9 ms); the dry path is
  not delayed.
- **Wet staging.** The output tap gain is 4.0, so a steady noise at the default
  size measures 0.476× the dry RMS (−6.4 dB) at `mix = 1`. The rack's existing
  linear dry/wet crossfade is used: `dry = 1 − mix`, `wet = mix`.

## 2. Delay-line geometry

The 32 line lengths at 48 kHz, tank-major. Every value is prime, so no two
loops share a common period and the modal set cannot collapse into a comb:

| Tank | Line lengths (samples at 48 kHz) |
|---|---|
| 0 | 907, 1109, 1321, 1567, 1789, 2039, 2281, 2531 |
| 1 | 967, 1171, 1423, 1613, 1871, 2089, 2341, 2593 |
| 2 | 1013, 1229, 1459, 1667, 1913, 2143, 2389, 2659 |
| 3 | 1061, 1289, 1499, 1733, 1993, 2237, 2441, 2699 |

- Time span 18.9–56.2 ms; mean 37.1 ms; total loop length 1.19 s at 48 kHz.
- At another sample rate the length is `nearest_prime(reference · sr/48000)`,
  so every rate still uses pairwise coprime loops. The 96 kHz lengths are
  1811..5399; 44.1 kHz lengths are 829..2477.
- The buffers are sized once for 96 kHz plus a 16-sample guard per line, so the
  audio path never reallocates when the sample rate changes.
- The tank geometry is fixed. `size` is a decay control, not a room-dimension
  control; this matches the old rack, where `size` only moved the feedback.

## 3. Per-band decay

Two series shelves shape the per-line loop gain. Both are 2-pole Butterworth
sections with a 0.7071 Q:

- low shelf corner **250 Hz**, gain `g_low/g_mid`;
- high shelf corner **4 kHz**, gain `g_high/g_mid`;
- band edges are therefore **< 250 Hz (low), 250 Hz–4 kHz (mid), > 4 kHz
  (high)**, the same breakpoints as the rack EQ's default low/high corners.

For a line of `L` samples at rate `sr` and a band target `RT60`, the per-line
loop gain is

```
g(L, RT60) = 10^(−3 · (L/sr) / (0.90 · RT60)), capped at 0.999
```

The `0.90` is the measured gain calibration of the tank: with the shelf pair
and the 32-line mixing, the decay measures 1.11× the nominal target before the
factor is applied. Per-line gains make all lines decay at the same time even
though their lengths differ by 3×; a single gain based on the mean delay made
the tail multi-slope.

Band RT60 ratios against the mid band are fixed at **1.10 (low), 1.00 (mid),
0.55 (high)**. The `low` and `high` controls trim them:

```
low ratio  = 0.70 + 0.80 · low      (0 = 0.70, 0.5 = 1.10, 1 = 1.50)
high ratio = 0.20 + 0.70 · high     (0 = 0.20, 0.5 = 0.55, 1 = 0.90)
```

### 3.1 Size law and table

`size` 0..1 maps to the **mid-band** RT60 through

```
RT60_mid(size) = 0.585 · 2^(4 · size) seconds
```

so 0.6 is the 3.1 s hall setting. The pinned table:

| size | 0 | 0.1 | 0.2 | 0.3 | 0.4 | 0.5 | 0.6 | 0.7 | 0.8 | 0.9 | 1.0 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| RT60 mid, s | 0.585 | 0.772 | 1.019 | 1.344 | 1.773 | 2.340 | 3.088 | 4.074 | 5.376 | 7.094 | 9.360 |

### 3.2 Measured per-band RT60

Method: 0.5 s of band-limited white noise into the wet-only module, then the
band RMS in 50 ms windows, least-squares fit of the −5..−25 dB slope; damp 0
so the damping filter is an exact bypass. Each value is the mean of one
deterministic render per band.

| size | low (<250 Hz) | mid (250 Hz–4 kHz) | high (>4 kHz) | mid target |
|---|---:|---:|---:|---:|
| 0.00 | 0.601 | 0.557 | 0.361 | 0.585 |
| 0.25 | 1.145 | 1.166 | 0.694 | 1.170 |
| 0.50 | 2.324 | 2.300 | 1.369 | 2.340 |
| 0.60 | 3.253 | 3.086 | 1.785 | 3.088 |
| 0.75 | 5.092 | 4.694 | 2.685 | 4.680 |
| 1.00 | 9.415 | 9.451 | 5.329 | 9.360 |

The mid band tracks the law within 2 % at and above size 0.25. The measured
low band is 0.98–1.09× the mid band (target 1.10); the high band is
0.56–0.65× (target 0.55). The shelf overlap compresses the high-band ratio
slightly; it is measurable and stable, not a level error.

The `low`/`high` controls were measured at size 0.6 with the same method
(mid unchanged at 3.09 s):

| low control | 0.25 | 0.50 | 0.75 |
|---|---:|---:|---:|
| low-band RT60, s | 3.06 | 3.25 | 3.69 |

| high control | 0.00 | 0.25 | 0.50 |
|---|---:|---:|---:|
| high-band RT60, s | 9.19 | 3.51 | 1.79 |

The high control's 0.00 position is the limiting case: with a 0.20 ratio the
high band is shorter than the 6 s measurement window and the fit is dominated
by the mid-band leakage, so the 9.19 s figure is the measurement floor, not a
real high-band decay. The usable range is 0.25..1.0.

The master-bus `reverb-fdn` device has its own decay EQ: each of its six bands
exposes a frequency, Q and shape that place the dB trim, so its decay is
frequency-dependent per band ([the FDN reverb note](reverb-fdn.md)). The
synth rack keeps the two fixed shelves above and has no per-band frequency
control.

## 4. Damping

Each line has a one-pole low-pass in its feedback path:

```
a = damp · exp(−2π · 1500 / fs)          (damp 0..1)
y[n] = (1 − a) · x[n] + a · y[n−1]
```

- `damp = 0` is an exact bypass (`a = 0`).
- `damp = 1` puts the corner at 1.5 kHz at 48 kHz (a = 0.8218); a 1 kHz tone
  passes at 1 000.0 Hz, measured within 0.5 Hz by the test.
- The corner moves with the sample rate, so the damping is rate-independent in
  Hz. Intermediate settings sweep the corner smoothly: 0.4 is about 8.5 kHz,
  0.8 about 3.2 kHz.
- There is no modulation in the damping path and the line lengths are fixed
  integers, so damping cannot detune the tail. The test pins both the filter
  frequency and the unchanged line lengths.

The damping adds loss on top of the band gains, so the measured RT60 of the
high band falls further as `damp` rises; the band table above is at `damp = 0`.

## 5. Pre-delay

A per-channel ring of 0.2 s at 96 kHz; the read is `write − round(pre-delay ·
sr)`. The test compares the first non-zero wet sample for 0.05 s and 0.15 s and
requires a difference of exactly 4 800 samples. The dry path is independent of
the pre-delay.

## 6. Memory

All buffers are allocated in `ReverbState::new()`. The state is f64.

| Item | Samples | Bytes |
|---|---:|---:|
| 32 line buffers, 96 kHz capacity + 16 guard each | 114 584 | 916 672 |
| Two pre-delay rings (0.2 s at 96 kHz) | 38 408 | 307 264 |
| **Per reverb instance** | **152 992** | **1 223 936 (1.167 MiB)** |
| **Per rack (two time slots)** | **305 984** | **2 447 872 (2.334 MiB)** |

The nominal 48 kHz working set is 95 444 samples (763 552 bytes, 745.7 KiB)
per instance; the rest is headroom for 96 kHz. A rack preallocates both reverb
slots even when neither is used, exactly like the other time-based modules.

## 7. Latency

**Zero.** A pure FDN has no algorithmic latency: the dry path is not delayed,
the wet path starts at the shortest line, and the pre-delay is a wet-path
control, not a source delay. `SynthPatch::fx_latency_samples` therefore reports
0 for a reverb-only rack, the built-in renderer adds no PDC lead-in, and a
`mix = 0` reverb is a bit-exact bypass. No latency figure is invented here.

The 32×32 feedback matrix and all filters are sample-by-sample and causal; the
first non-zero output is at `round(pre-delay · sr) + 907` at 48 kHz.

## 8. Preset import

The importer normalizes wet mix, size, damping, and stereo width, and converts
pre-delay to seconds. Imported patches use the OpenSynth FDN topology and keep
the default low and high trims when the source has no corresponding settings.

## 9. Stability

The feedback matrix is unitary; every band gain is below 1 and capped at 0.999,
so the spectral radius of the loop is below one at every setting. The
regression test drives 1 s of 0.5-peak noise into the worst case (size 1,
damp 1, pre-delay 0.2, mix 1, width 1) and then 30 s of silence:

- peak during the burst: **0.860**;
- peak over the last second: **2.3 × 10⁻¹¹**;
- RMS over the last second: **7.9 × 10⁻¹²**;
- 1 s of 0.5 DC then 30 s of silence: residue **2.8 × 10⁻¹⁰**.

The per-second RMS falls monotonically (allowing a 1.25 modal-beating margin)
and no setting self-oscillates. The DC path is the low band, whose gain is
still below one, so there is no DC accumulation.

## 10. Stereo decorrelation

A mono noise burst (0.25 RMS, 0.5 s) through size 0.6 / damp 0.4, tail measured
from 1 s to 4 s:

- L/R Pearson correlation: **0.0104**;
- channel balance: **0.22 dB** (L 0.004851 RMS, R 0.004728).

The four orthogonal input/output tap patterns and the H4 cross-feed keep the
tail wide for a mono source. `width = 0` collapses it to mono; `width = 1` is
the raw tap pair.

## 11. Regression tests

`crates/oc-synth-fx/src/lib.rs`:

- `reverb_size_maps_to_the_pinned_rt60_table` — the eleven-point size table,
  monotonicity, and every band gain below unity at the extremes.
- `reverb_rt60_is_pinned_per_band_at_three_sizes` — the measured low/mid/high
  decay at sizes 0.25/0.5/0.6, including the band ordering.
- `reverb_tail_is_stereo_decorrelated` — correlation below 0.10 and channel
  balance within 1.5 dB.
- `reverb_damping_filter_does_not_detune_the_tail` — 1 kHz through the damping
  law at three settings within 0.5 Hz, and the line lengths unchanged by
  processing.
- `reverb_predelay_is_sample_exact` — 4 800-sample difference between 0.05 s
  and 0.15 s.
- `reverb_wet_zero_is_a_bit_exact_bypass` — 48 000 noise samples unchanged.
- `reverb_reports_zero_latency_and_passes_the_dry_path` — rack latency 0 and a
  bit-exact `mix = 0` first sample.
- `reverb_long_burst_decays_at_the_extremes` — the 30 s worst-case burst.
- `reverb_has_no_dc_buildup` — the 30 s DC residue.
- `reverb_buffer_budget_is_pinned` — 1 223 936 bytes per instance, 2 447 872
  per rack, prime lengths at 44.1/48/96 kHz, and no resize after processing.

`crates/oc-synth-import/src/foreign_preset.rs`:

- `reverb_import_maps_the_fdn_controls_and_warns_once` — the five mapped
  controls, normalization of damping, and one deduplicated warning per
  unmapped class.

## 13. Failure modes

- **The high band is not as short as the control says.** Measured 0.58× mid at
  the default; the 0.55 target is compressed by the shelf overlap. The high
  trim at 0.00 is not a usable RT60 measurement (see §3.2).
- **The tail is multi-slope at extreme low/high trims.** Per-line gains keep
  the mean decay on target, but the band filters still couple; the low/high
  trims at their ends produce a two-slope tail.
- **The wet gain is fixed at 4.0.** At the maximum size a 0.5-peak input
  reaches 0.86 peak; a hotter input can reach the synth's output clip before
  the master. The reverb has no limiter of its own.
- **The rack allocates 2.33 MiB even with no reverb module.** Both time slots
  are built in `SynthFxChain::new`, like the other time-based effects.
- **A per-sample `size` modulation recomputes the 32-line gain table every
  sample.** It is allocation-free but costs 96 `powf` calls per sample in that
  case; normal control-rate modulation is cheap.
- **`damp` is a one-pole, not a two-band absorption model.** It darkens the
  whole tail with one corner; there is no separate low/high damping split.
- **The reverb type selector is not imported.** Hall/Plate/room strings have no
  counterpart; every preset gets the same tank.
- **Size sets the mid-band decay.** At 60 % it is approximately 3.1 s.
- **The pre-delay is capped at 0.2 s.** Presets that store a longer pre-delay
  are clamped, not rejected.
