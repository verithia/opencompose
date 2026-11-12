# Breath split

`breath_split` separates a take into a voiced path and a breath path with a
linear-phase complementary pair, classifies which path dominates a short window,
and applies the breath level only in windows the detector calls breath. The
implementation is `crates/oc-dsp/src/breath_split.rs`; the schema id is
`breath-split`.

All measurements are 48 kHz, stereo, from the ignored harness:

```
cargo test -p oc-dsp measurements_for_docs -- --ignored --nocapture
```

## Split law

```
low    = FIR_low(x)                 # 257-tap linear-phase low-pass, delay 128
dry    = x delayed by 128
breath = dry - low                  # exact complement
out    = voice_gain * low + breath_gain * breath
       = voice_gain * dry + (breath_gain - voice_gain) * breath
```

The pair is a **linear-phase complementary pair**: a windowed-sinc low-pass
(Blackman-Harris window, 257 taps, group delay 128 samples at every rate) and its
exact complement. `low + breath = dry` by construction, so at unity the output is
the input delayed by 128 samples, exactly (measured error 0). Both paths carry
the same linear phase, so changing one gain does not rotate the other.

Why not a power-complementary pair (`|L|^2 + |H|^2 = 1`)? Summed in phase, that
law has a +3 dB bump at the crossover; the amplitude complement has no bump and
reconstructs the delayed input. Why not a minimum-phase pair? Its two paths have
different phase, so the sum is not the input and the reconstruction moves when a
gain moves. The linear-phase pair costs 128 samples, reported to PDC.

- The cutoff is 1.5–6 kHz, default 2 kHz. The breath path is everything above
  the corner; the breath level reduces that band only. Breath energy below the
  corner belongs to the voice path and is not reduced by design.
- `voice_level` scales the low path at all times. `breath_level` scales the high
  path only in detected breath windows. `monitor` outputs the high path after
  the breath gain.
- At breath level 0 dB, voice level 0 dB and monitor off, the device bypasses
  and is bit-exact with zero latency.

## Detector

```
window 30 ms, hop 10 ms, low path decimated to 12 kHz
lags 70..1000 Hz
r(tau) = sum x[n]*x[n-tau] / sqrt(sum x[n]^2 * sum x[n-tau]^2)
low_share  = E_low / (E_low + E_high)
high_share = 1 - low_share
confidence = (1 - r_peak * low_share) * high_share
breath     = confidence > threshold
target     = breath_level_db when breath, else 0 dB
gain       = one-pole toward target with the smoothing time
```

- The normalized autocorrelation peak is measured on the **low (voice) path**, so
  the high-frequency noise of a breath does not create a pitch peak.
- The peak is weighted by `low_share`: a pitch peak counts only where the low
  band carries the energy. Without the weight, the stopband leakage of a pure
  breath makes the normalized autocorrelation fluctuate around the threshold and
  the gain chatters; with it, the synthetic 4–8 kHz breath measures 1.000 and the
  vowel 0.000–0.010.
- `E_low` and `E_high` are 30 ms one-pole followers on the two paths. The
  detector uses the mid signal `(L+R)/2` and one gain is applied to both
  channels.
- `threshold` is the confidence a window must **exceed**; 1.0 disables detection
  without bypassing the split. `smoothing` is the 63 % time constant of the
  breath gain, 1–200 ms, default 20 ms.

## Measurements

### Sum exactness

The engaged split is fed a mixed vowel-plus-breath program with the threshold at
1.0 so the detector cannot fire; the output is compared with the input delayed
by 128 samples.

| case | max abs error |
|---|---:|
| defaults (bypass) | 0.0 (f32 bits equal) |
| engaged, threshold 1.0, breath −60 dB | 0.0 |
| low-level mixed phrase, unity | 0.0 |

The exactness is structural: `breath = dry - low`, so `low + breath` is `dry`
before any gain is applied.

### Attenuation

The synthetic breath is a sixth-order 4–8 kHz noise at 0.1 RMS, measured over
1.5 s after the detector settles.

| requested | measured |
|---:|---:|
| −12 dB | −11.998 dB |
| −24 dB | −23.974 dB |
| +6 dB | +6.000 dB |

The −24 dB residual is the part of the test noise that the split puts in the low
path; it is 0.026 dB at this level and band.

Band dependence at −12 dB:

| noise band | measured |
|---|---:|
| 2–4 kHz | −9.514 dB |
| 4–8 kHz | −11.998 dB |
| 6–10 kHz | −12.000 dB |

The 2–4 kHz band straddles the 2 kHz corner: its content below the corner is the
voice path and is not reduced. That is the split law, not an error; lower
`split_frequency` to move the corner.

### Cross-talk

A 150 Hz vowel with 24 harmonics at 0.3 peak, breath level −12 dB and −30 dB:

| breath level | max sample movement | RMS change |
|---:|---:|---:|
| −12 dB | 0.000e0 | +0.000 dB |
| −30 dB | 0.000e0 | +0.000 dB |

The detector never calls the vowel breath, so the breath gain stays at unity and
the output is the delayed input. The acceptance bound is 0.3 dB.

### Transition

A mixed phrase (0.75 s vowel, 0.75 s breath, 0.75 s vowel) at breath −30 dB and
20 ms smoothing:

| metric | value |
|---|---:|
| source max sample-to-sample step | 0.227405 |
| output max sample-to-sample step | 0.211959 |
| added step | −0.015446 |

The output's own slew is smaller than the source's, so the transition adds no
step. On a low-level phrase (80 Hz at 0.03 plus 4–8 kHz noise at 0.00005 RMS)
the source step is 0.000314 and the output step is 0.000337, both below the
−60 dBFS bound of 1e-3. The breath gain moves by less than 2e-3 per sample at the
20 ms default.

### Latency

| configuration | reported latency |
|---|---:|
| defaults / bypass | 0 |
| engaged (breath or voice level, or monitor) | 128 samples (2.67 ms at 48 kHz) |
| tail | 256 samples |

The 128 samples are the linear-phase FIR group delay, not lookahead. The
detector reads only past samples and does not move the audio ahead. The bypass
is structural: moving breath or voice from 0 dB engages the split and changes
the reported latency from 0 to 128; PDC must re-align. This is the same pattern
as the de-esser's split-band mode.

## Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `breath_level` | dB | −60…+12 | 0 | gain on the breath path in detected breath windows |
| `voice_level` | dB | −60…+12 | 0 | gain on the voiced (low) path at all times |
| `threshold` | none | 0…1 | 0.5 | confidence a window must exceed to count as breath |
| `smoothing` | ms | 1…200 | 20 | 63 % time constant of the breath-path gain |
| `monitor` | bool | 0/1 | 0 | listen-only breath path, after the breath gain |
| `split_frequency` | Hz | 1500…6000 | 2000 | low-pass corner; the breath path is the complement above it |

## Defaults are bit-identical

`defaults_are_a_bit_exact_no_op` asserts that a deterministic mixed signal
passes through bit-for-bit and that `new(SR).latency_samples()` is 0.
`voice_only_material_is_bit_exact_at_unity` sets breath level −24 dB but the
vowel never triggers the detector; the maximum difference is 0.

## Tests

- `defaults_are_a_bit_exact_no_op`
- `voice_only_material_is_bit_exact_at_unity`
- `synthetic_breath_is_attenuated_by_the_requested_amount`
- `synthetic_vowel_is_untouched_at_any_breath_level`
- `split_sums_to_the_input_within_1e_6_at_unity`
- `mixed_phrase_transition_adds_no_step_above_minus_sixty_dbfs`
- `mixed_phrase_transition_stays_below_minus_sixty_dbfs`
- `smoothing_limits_the_gain_motion_per_sample`
- `threshold_gates_the_breath_path`
- `monitor_plays_only_the_breath_path`
- `latency_is_reported_and_the_impulse_lands_there`
- `detector_window_is_thirty_ms_with_a_ten_ms_hop`
- `reset_is_deterministic`
- `extreme_settings_stay_finite`

## Real-time safety

`process` allocates nothing, takes no locks and starts no threads. The split is
257 taps per channel (24.7 M multiply-adds/s at 48 kHz); the detector is a
360-sample window with 161 lags every 10 ms (about 43 k multiply-adds per hop,
4.3 M/s at 48 kHz). The detector ring, scratch and prefix buffers are allocated
in `new`. A `split_frequency` change designs into a stack array and copies into
the preallocated taps.

## Limits

- The breath level acts on the breath path only. Content below the split corner
  stays at unity; a 2–4 kHz band gets −9.5 dB at −12 dB. A real breath with
  low-frequency energy gets less than the requested reduction. Lowering the
  split frequency to 1.5 kHz captures more of it, at the cost of moving more
  voice formants into the breath path; the detector still gates voiced windows.
- The detector is mid-linked; a hard-panned breath and a centre voice share one
  decision.
- The monitor is breath-only; there is no voice-only monitor.
- At low sample rates the split frequency is capped at 0.45 fs; a request above
  the cap is rejected by the factory rather than silently clamped.
- There is no lookahead. A breath shorter than the 30 ms analysis window is only
  partly detected.
