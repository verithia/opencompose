# Mastering limiter

`crates/oc-dsp/src/limiter.rs` is a stereo true-peak limiter used by the
`limiter` device and by the loudness path in `oc-engine`. It is a limiter, not
a gain computer: the ceiling applies to the inter-sample peak, the attack opens
before the peak reaches the output, and the release is program dependent.

## Laws

### Detection

- Polyphase interpolation with a selectable **4x/8x/16x** phase grid, 32 taps
  per phase, windowed with a Blackman-Harris window over the prototype. The 4x
  coefficient set is the original one; 8x and 16x refine the phase grid over
  the same 32-frame support instead of repeating the 4x kernel, so the group
  delay stays about 16 frames at every factor.
- The detector reports `max(|L|, |R|, interpolated phases)` for the current
  frame, so the sample peak is always included. This is the linked detector used
  at `link = 100 %`; lower values add one detector chain per channel (see
  Channel link).
- The detector is causal over a 32-frame history with a group delay of about
  16 frames (0.33 ms at 48 kHz), so an inter-sample peak is reported roughly
  16 frames after it occurs. The lookahead must exceed that delay to catch
  inter-sample peaks; the 5 ms default still leaves about 224 frames (4.7 ms)
  of early opening. With a shorter lookahead the sample peak is still caught,
  but inter-sample protection weakens; at `lookahead = 0` it is not available.
- `analysis::true_peak_dbfs` runs the 4x detector, so file analysis and the
  default limiter cannot disagree about the peak of the same signal. The
  analysis path stays at 4x when the limiter is set to 8x or 16x.

### Ceiling

The ceiling is a true-peak limit. With `true_peak = 1` the gain is computed
against `ceiling * 0.995` (−0.04 dB), a safety factor that keeps the 4x
estimate from leaving an overshoot after reconstruction. With `true_peak = 0`
the detector is replaced by the sample peak and the safety factor is removed;
that is the explicit legacy path.

### Attack and lookahead

Audio is delayed by a ring of `lookahead` frames. A monotonic deque holds the
largest detected peak inside that window, so each delayed sample is attenuated
by at least what its own peak requires. Attack is sample-accurate (instant
attack, no overshoot); the lookahead is what makes it open *before* the peak
arrives at the output:

- the peak enters the detector,
- the ring holds the already-reduced sample for `lookahead` frames,
- the peak appears at the output with the gain already applied.

For a sample peak at input frame `P`, the gain reduction starts at output
frame `P - lookahead`, i.e. `lookahead` frames ahead of the peak. For an
inter-sample peak the detection delay reduces that to about
`lookahead - 16` frames. The unit test
`lookahead_opens_the_attack_before_the_peak_reaches_the_output` pins the
sample-peak case exactly (240 frames of lookahead, 240 frames of early
opening).

### Release

Four behaviours, all one-pole toward the lookahead peak (no hard clamp) and
all using the same `release` knob:

- **fast** — `τ = release_ms`. The gain returns toward unity at a fixed rate;
  this is the pre-mastering behaviour and is kept for compatibility.
- **auto** (default) — `τ_eff = release_ms · (1 + 3 · program)` where
  `program = max(depth, memory, sustain)`:
  - `depth = clamp(GR / 12 dB, 0, 1)` — how far into limiting the current gain
    is,
  - `memory` — the peak depth, decaying with a 3 s time constant,
  - `sustain` — a 2 s rise / 3 s fall memory of the time spent over the
    ceiling.

  Deeper and longer limiting therefore lengthens the tail; a short transient
  releases quickly.
- **punch** — `τ_eff = release_ms · (1 + 1.2 · depth)`. Only the instantaneous
  gain reduction is used; programme duration is ignored, so a dense passage
  does not build a long tail. Isolated peaks still release fast.
- **sustain** — `τ_eff = release_ms · (1 + 5 · sustain)`. Only the duration
  memory is used; the current GR does not shorten the tail. A long loud
  passage stays slow after it ends, while a short transient never builds the
  memory in the first place.

The release remains monotonic in every style because the effective coefficient
stays in `(0, 1)`.

### Channel link

`link` (percent, 0…100, default 100) selects how the two channels share the
detector:

- **100 % (default)** — one detector chain runs on the linked peak
  `max(|L|, |R|, interpolated phases)` and both channels receive its gain. Every
  earlier render used this path.
- **0 %** — each channel runs its own detector chain (same lookahead window,
  same release programme) and its own gain, so a peak on one side never ducks
  the other.
- **Between** — the applied gains are blended in amplitude:
  `g_c = w · g_linked + (1 − w) · g_own,c` with `w = link / 100`. At 50 % a
  channel that is not limiting keeps half of the linked reduction.

The lookahead window, the attack and the release programme are per chain, so an
unlinked channel still opens early and releases with the selected style. The
oversampled interpolation is computed once per frame and shared by all three
chains.

Metering still describes the linked detector: `gain_reduction_db`,
`max_gain_reduction_db` and `ceiling_margin_db` report its input peak, and the
reported reduction is the larger of the two applied reductions. `latency_samples`
does not depend on `link`.

Link controls both the transient and the release. The detector uses peak level.

### Latency

`latency_samples = round(lookahead_ms · sample_rate / 1000)`, clamped to
`0..=20 ms`. Defaults: 5 ms → **240 frames at 48 kHz** (5.0 ms), reported by
`AudioEffect::latency_samples` so hosts and the offline renderer apply PDC.
The detector itself adds no reported latency. The limiter does not shift its
own output; callers compensate the reported latency.

### Offline alignment (master PDC)

The limiter delays its output by `latency_samples`; the offline renderer does
not let that time-shift the file. `RenderCache` renders
`arrangement + pre-master PDC + master-chain latency + effect tail` frames,
then shifts the master buffer left by the master-chain latency and truncates
to `arrangement + pre-master PDC + effect tail`. The extra frames are what keep
the last `latency` frames of program from disappearing. The visible file
length therefore follows the existing arrangement + tail rule, and a note-on
that starts at frame `N` in the project starts at frame `N` in the render
whether the limiter is engaged or bypassed. The per-track offline path applies
the same shift. Test:
`render_cache::tests::master_chain_latency_is_compensated_without_losing_the_program_end`.

### Real-time constraints

Ring buffers, the monotonic deque and the interpolation coefficients are
allocated in `new`. `process` performs no allocation, takes no locks and
starts no threads; all state is scalar or preallocated.

## Parameters

| id | unit | range | default |
|---|---|---|---|
| `ceiling` | dBTP (`true_peak=1`) | -12..0 | -0.3 |
| `release` | ms | 5..500 | 50 |
| `release_mode` | enum | Fast / Auto / Punch / Sustain | Auto |
| `lookahead` | ms | 0..20 | 5 |
| `true_peak` | bool | 0/1 | 1 |
| `oversampling` | enum | 4x / 8x / 16x | 4x |
| `link` | % | 0..100 | 100 |

Explicit compatibility path: `true_peak=0`, `release_mode=fast`,
`lookahead=1` reproduces the pre-mastering sample-peak limiter exactly.

## Metering

`AudioEffect` exposes `gain_reduction_db`, `max_gain_reduction_db` and
`ceiling_margin_db` (positive below the ceiling). `meter_snapshot` publishes
them per effect row as `effects`, `effects_max_gain_reduction_db` and
`effects_ceiling_margin_db`; the limiter panel shows current GR, max GR and the
ceiling margin. The loudness report carries the same numbers plus the GR
percentiles (`p50/p95/p99`), the limited fraction, the pre-limiter true peak
and the short-term maximum.

### Gain-reduction history strip

The limiter panel draws a six-second GR history below the live meter. It is fed
by the same shared 10 Hz `meter_snapshot` poll: each poll that reports an
available, playing meter appends `gainReductionDb / 12 dB` to a fixed
60-sample ring, so the newest sample is at the right and old samples scroll
off. The 12 dB full scale is the limiter's own depth reference
(`depth = GR / 12 dB` in the auto release); the bars use the panel accent and
reuse the 8 px `.device-meter-track` chrome.

- **Fresh open**: the ring is empty and the strip shows "No gain reduction
  yet" until the first playing poll.
- **Silence**: zero-GR polls keep the window advancing but draw no bars; the
  peak readout stays at 0.0 dB.
- **Stopped transport**: polls are ignored, so the last window freezes instead
  of draining; the idle note from the live meter still explains the state.
- **Bounded**: the ring is allocated once per open panel and never grows, so a
  panel left open for hours holds 60 floats.
- **Reduced motion**: the strip is data, not animation; the media query
  disables the meter transitions.

The strip adds no audio-thread work: it reads the existing meter path on the
UI side only.

## Verification

The limiter tests use impulses, sustained tones and short peaks to measure
ceiling accuracy, channel linking and release. Offline alignment is verified
against a delayed pass-through. Device meter tests cover gain reduction and
ceiling margin separately from the transport's level meters.

## Tests

`crates/oc-dsp/src/limiter.rs`:

- `explicit_legacy_path_matches_the_pre_mastering_limiter_exactly`
- `defaults_are_a_mastering_limiter`
- `delayed_impulse_never_exceeds_declared_ceiling`
- `true_peak_ceiling_holds_on_an_intersample_peak_signal`
- `true_peak_detection_limits_intersample_overshoot_that_sample_peak_ignores`
- `ceiling_accuracy_is_within_a_tenth_of_a_db`
- `gain_reduction_readout_matches_applied_attenuation`
- `gain_reduction_metering_reports_current_max_margin_and_distribution`
- `lookahead_opens_the_attack_before_the_peak_reaches_the_output`
- `signal_below_the_ceiling_passes_unchanged`
- `release_is_monotonic_after_the_peak_ends`
- `auto_release_tail_depends_on_the_program_but_fast_does_not`
- `steady_tone_limit_does_not_pump`
- `partition_and_reset_are_exact`
- `two_instances_are_bit_identical`
- `defaults_keep_the_frozen_render` — pins the default 4x / Auto render digest.
- `link_is_a_parameter_clamped_to_its_range`
- `full_link_ducks_both_channels_equally`
- `zero_link_limits_each_channel_on_its_own_level`
- `partial_link_blends_the_two_detectors`
- `oversampling_is_a_parameter`
- `oversampling_improves_the_intersample_peak_estimate`
- `oversampling_cost_report` (ignored harness, release build)
- `release_styles_measure_distinctly`

`crates/oc-engine/src/analysis.rs`:

- `true_peak_estimate_agrees_with_the_limiter_detector` — the analysis
  estimator and the limiter detector agree to better than 1e-9 dB on three
  test signals, and a below-ceiling signal passes unchanged.

`oc-gui`:

- `meters::tests::limiter_effect_rows_report_current_max_and_ceiling_margin`
- `meters::tests::non_finite_and_extreme_values_are_clamped`
- `frontend/src/__tests__/stores/meters.test.ts` (max GR and margin
  normalisation, fallbacks and clamping).
- `frontend/src/__tests__/grHistory.test.ts` (the history ring wraps, keeps
  the newest samples, stays bounded and clamps input).
- `frontend/src/__tests__/components/Devices.test.tsx` (the limiter strip
  appears, starts empty, follows meter values and has no strip on other
  dynamics devices).
