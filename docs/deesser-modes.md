# De-esser modes

The de-esser is one device with six orthogonal choices: where the threshold
sits, where the gain is applied, which signal the detector reads, where the
sibilance band sits, whether that band is fixed or found per take, and whether
the reduction follows the sibilant segments. The shipped law and the split
recombination are in `crates/oc-dsp/src/deesser.rs`; this document fixes the
modes and their measurements. The defaults are the shipped behaviour, bit for
bit.

- `threshold_mode` — `Absolute` or `Relative`.
- `band_processing` — `Wide Band` (full-band gain) or `Split Band`.
- `detector` — `Sidechain` (before the split) or `In-band` (after the split).
- `sibilance_band` — `Auto` (shipped HP/LP corners) or fixed 5, 6, 7, 8, 9 kHz.
- `band_estimation` — `Off` or `Learn` (running per-take band estimate).
- `segment_detection` — `Off` or `On` (gate the reduction to sibilant segments).

All measurements are 48 kHz, stereo, from the ignored harnesses:

```
cargo test -p oc-dsp measurements_for_docs -- --ignored --nocapture
cargo test -p oc-dsp measurements_for_automatic_band -- --ignored --nocapture
```

## Threshold mode

### Absolute

The shipped law. The band peak is compared with the `threshold` knob:

```
GR = clamp((Range/36) * (L_band - Threshold), 0, Range)
```

`Threshold` is an absolute level, so a louder take crosses it more often and a
quieter take not at all. That is the behaviour old projects render.

### Relative

The threshold follows the program level, so the band is judged against the voice
rather than against a fixed number.

```
P(t)   = 10*log10(one-pole(power, attack 200 ms, release 1 s))   # program RMS, dBFS
power  = (L^2 + R^2) / 2                                         # stereo mean power
T_eff  = P(t) + RelativeThreshold
GR     = clamp((Range/36) * (L_band - T_eff), 0, Range)
```

- **Window:** the program follower is a one-pole on the stereo mean power with a
  200 ms attack and a 1 s release. The 1 s release is the window that makes a
  phrase, not a syllable, the reference; the faster attack keeps a crescendo
  from outrunning the threshold.
- **Ratio law:** the effective threshold tracks the program **1:1** in dB. Every
  decibel the program rises raises `T_eff` by one decibel, so `GR` depends on the
  band-to-program ratio `L_band - P`, not on either level alone.
- `RelativeThreshold` is the required ratio: reduction starts when the band peak
  is that far below the program RMS. The default is −12 dB; the `threshold`
  knob is inactive in this mode.
- The detector, peak hold and 11/15.5 ms ballistics are unchanged. Only the
  threshold input moves.

Measured on the same body-plus-sibilant program at two levels:

| program level | GR | sibilance (8 kHz) | body reduction (1 kHz) |
|---|---:|---:|---:|
| reference | 4.12 dB | 2.13 dB | 0.07 dB |
| −20 dB | 4.12 dB | 2.13 dB | 0.07 dB |

The reduction is level independent to the printed precision.

Crescendo, 1 kHz body plus a fixed-ratio 8 kHz sibilant ramped 20 dB over 6 s:

| window | body gain | sibilance gain |
|---|---:|---:|
| 1 s | −0.11 dB | −2.50 dB |
| 5 s | −0.11 dB | −2.49 dB |

Body movement **0.00 dB**, sibilance movement **0.01 dB**. The table reports the
body movement the relative mode is required not to produce.

## Gain application

`Wide Band` multiplies the whole signal by the reduction. `Split Band` leaves the
low path at unity and scales only the zero-phase `x^2/(1+x^2)` high band, so a
1 kHz body is barely touched and neutral gain remains an exact 512-sample delay.

Full-band gain has two forms. With the **sidechain** detector it is keyed by the
sibilance band and the body is ducked by whatever reduction that band asks for;
with the **in-band** detector the body itself triggers the reduction, which is the
classic analog full-band de-esser. Measured with a 1 kHz body and an 8 kHz
sibilant at `threshold −36`, `range 24`:

| mode | detector | GR | body change |
|---|---|---:|---:|
| `Wide Band` | Sidechain | 9.34 dB | −9.34 dB |
| `Wide Band` | In-band | 10.67 dB | −10.67 dB (body-only probe) |
| `Split Band`, 7 kHz band | Sidechain | 10.27 dB | −0.12 dB |

The wide-band body change is the known trade-off, not an error: the device cannot
remove a band it never split, and the full-band detector cannot separate a vowel
from a sibilant. It is reported here so the mode is chosen deliberately.

## Detector placement

- **Sidechain** (default): the detector reads the band-limited program *before*
  the split. The reduction is keyed only by the incoming sibilance; the applied
  gain never feeds back into detection, and a 1 kHz body produces 0.00 dB of GR
  because the sidechain band rejects it.
- **In-band**: the detector reads the band *after* the split, so it sees the
  signal it attenuates. In `Wide Band` that is the whole program, and the body
  itself triggers the reduction — the classic full-band detector behaviour.

Measured on a 1 kHz body at −20 dBFS (`threshold −36`, `range 24`, background
wide):

| detector | GR |
|---|---:|
| Sidechain | 0.00 dB |
| In-band | 10.67 dB |

In `Split Band` the two placements still differ because the sidechain band and
the split high band have different shapes: on an 8 kHz tone with a 7 kHz band the
reported GR differs by more than 3 dB.

## Sibilance band

`Auto` keeps the shipped layout: the sidechain corners are `high_frequency` and
`low_frequency`, and the split sits at `HP/4`. An explicit step moves the band as
a unit:

```
HP      = band
LP      = 1.87 * band
split   = band
```

The narrow-band resonance boost keeps the measured `LP/HP` ratio, so the sidechain
shape travels with the band. The split corner is the selected band itself, not
`HP/4`, which is why a 7 kHz band leaves a 1 kHz body alone.

Band sweep, `Split Band`, `threshold −36`, `range 24`, sibilance at 1.4 × the
band, body at 1 kHz:

| band | GR | sibilance | body |
|---|---:|---:|---:|
| 5 kHz | 11.36 dB | 5.73 dB | 0.25 dB |
| 6 kHz | 11.37 dB | 5.74 dB | 0.17 dB |
| 7 kHz | 11.40 dB | 5.75 dB | 0.13 dB |
| 8 kHz | 11.45 dB | 5.76 dB | 0.10 dB |
| 9 kHz | 11.55 dB | 5.80 dB | 0.08 dB |

The GR is the requested reduction. The sibilance column is the net attenuation
of the tone after the split recombination; it is smaller than GR by the
`H_split` fraction at that frequency, which is the split law and not a mode
error. The body stays below the 0.5 dB acceptance bound at every step.

## Automatic sibilance band

`band_estimation = Learn` finds the band instead of assuming 5–9 kHz. The
estimator is the same code in two places: `estimate_sibilance_band` analyses a
whole take offline, and the device runs a leaky copy of it while processing.

### Analysis

| item | value | why |
|---|---|---|
| frame | 1024 samples, Hann | 21 ms at 48 kHz; enough bins to separate 6/8/11 kHz |
| hop | 256 samples | 5.3 ms control rate |
| sibilant band | 4–16 kHz | where /s/ lives; clamped to 0.45 × sample rate |
| body band | 200 Hz–2 kHz | the vowel / low counterpart |
| sibilant frame | high share ≥ −12 dB and frame RMS ≥ −80 dBFS | the same share boundary the segment detector uses |

For each sibilant frame the estimator accumulates the high-band power per bin.
The accepted estimate is the energy centroid of that accumulation, so a narrow
tone gives its own frequency and a broadband hiss gives its middle. The band
parameters follow the shipped step law, with the band taken from the centroid:

```
band = peak / 1.4            # sidechain high-pass and split corner
top  = 1.87 * band           # sidechain low-pass corner
```

That `1.4` is the ratio the fixed steps already use (a 7 kHz step is built for a
sibilant near 9.8 kHz), so a learned band and a fixed step behave the same way.

Measured takes: 2 s of 1 kHz body plus a 2 kHz harmonic, with a 0.4 s
amplitude-modulated sibilant at the stated frequency (the same three takes the
test asserts):

| take | estimated peak | `band` | `top` | sibilant frames |
|---|---:|---:|---:|---:|
| 6 kHz | 6000.1 Hz | 4285.8 Hz | 8000.1 Hz | 76 |
| 8 kHz | 8000.0 Hz | 5714.3 Hz | 10666.7 Hz | 76 |
| 11 kHz | 10999.9 Hz | 7857.1 Hz | 14666.6 Hz | 76 |

A body-only take returns `None`: the offline estimator has no sibilant frames,
the device keeps the shipped `high_frequency`/`low_frequency` band and the split
at `HP/4`, and it does not act.

### Running estimator

- A leaky accumulator (`×0.995` per analysis frame) follows the recent sibilant
  frames; a decision is taken every 8 frames once at least 8 candidate frames
  have arrived, so the first lock needs about 43 ms of sibilance.
- A decision replaces the held band only when `band` moves by more than 8 %, so
  a stable take locks once and does not retune on every frame.
- The band is held after the sibilance stops; a take that changes voice or
  processing can move it after the accumulator has seen enough of the new band.
- While `Learn` is on and no band has been found, the device keeps the default
  band **and suppresses the reduction**: a take without sibilance is a
  passthrough even in `Wide Band` with the `In-band` detector.
- `band_override` (Hz) pins `band` and `top = 1.87 * band` and takes precedence
  over both `Learn` and the fixed steps; 0 means "follow them". An offline
  `estimate_sibilance_band` result can be written straight to it when a take is
  analysed before rendering.

### Reporting

The chosen decision is measurable without the GUI:

| key | meaning |
|---|---|
| `get_param("chosen_band")` | effective high-pass and split corner, Hz, in every mode |
| `get_param("detected_band")` | learned band or override, Hz; 0 when no decision |
| `get_param("estimated_peak")` | energy centroid of the held estimate, Hz; 0 when none |
| `DeEsser::band_edges_hz()` | `(high-pass, low-pass)` of the effective sidechain |
| `DeEsser::sibilant_active()` | segment detector state |

## Sibilant segments

`segment_detection = On` gates the gain law to detected sibilant segments: the
reduction engages when the high band dominates and releases when it stops,
instead of riding the whole take.

```
share = 10*log10(E(high_band) / (E(high_band) + E(200 Hz..2 kHz)))
```

- The **high band** is the effective sidechain band (`band`..`top`), so the
  detector follows the learned band as well as a fixed step.
- The **body** is 200 Hz–2 kHz: the low counterpart that makes a segment
  sibilance rather than an isolated HF event (a cymbal or a synth pad).
- Both bands must be above −60 dBFS and the frame above −80 dBFS; a silent or
  HF-only frame leaves the segment.
- **Enter** at `share ≥ −12 dB` for 2 consecutive frames (about 10.7 ms).
- **Exit** at `share < −18 dB` (or lost presence) for 2 consecutive frames,
  and never before 4 frames (about 21 ms) in the segment.
- The 6 dB band between the enter and exit shares is the hysteresis: once in,
  the segment survives a dip that would not have started it.
- The segment gain is a one-pole gate, 3 ms attack and 25 ms release, before the
  existing 11/15.5 ms gain-reduction ballistics. A sustained sibilant stays
  engaged (there is no 300 ms cap): the detector marks where the problem is,
  not a maximum length.
- Detection reads the mid signal `(L+R)/2` for both channels, so the gate never
  moves the stereo image. With `stereo_link < 100 %` the per-channel law still
  differs; only the segment state is shared.

Measured on the 1 s take (1 kHz body + 2 kHz harmonic, 8 kHz sibilant from
0.300 s to 0.600 s, fixed 6 kHz step):

| measurement | value |
|---|---|
| share in the vowel | −121.1 … −120.4 dB |
| share in the sibilant | −3.09 dB |
| entered | 0.312 s |
| left | 0.621 s |

Noisy boundary: the same body with the 8 kHz amplitude modulated ±2 dB across
the enter share (share −14.1 … −10.8 dB, a 3 Hz modulation). With hysteresis the
detector enters once and holds for the whole 2 s. A constant sibilant at the
bottom of that range (−15.5 dB share) never enters. The test also runs the same
signal through a fresh device and asserts exactly one state change; a
single-threshold detector would toggle on every modulation cycle.

## Acceptance

The test signal: 6 s, 1 kHz body plus an 8 kHz narrow sibilant, the program
level swept down 20 dB. `Learn` + `Segments`, `Split Band`, `Relative`
threshold, `relative_threshold −17 dB`, `range 24 dB`:

| window | body (1 kHz) | sibilance (8 kHz) | GR |
|---|---:|---:|---:|
| 1.0 s | −0.17 dB | −6.36 dB | 12.14 dB |
| 3.0 s | −0.16 dB | −6.08 dB | 12.14 dB |
| 5.0 s | −0.16 dB | −5.98 dB | 12.14 dB |

The sibilant stays inside 6 ± 1 dB across the whole sweep and the body inside
0.5 dB. The sibilant move is the split law, not the GR: with the split at the
learned 5714 Hz the 8 kHz tone passes `H_split = 0.71`, so a 12.14 dB reduction
lands at 6 dB of net attenuation.

- **Amount 0:** `range = 0` with `Learn`, `Segments` and the automatic band
  active is bit-identical to the input (test `zero_range_is_bit_identical...`).
- **No sibilance:** the body-only take renders bit-identically, reports
  `detected_band = 0`, and keeps the default `(7500, 14000)` corners.
- **Block size:** 97, 240 and 1024-frame blocks match the 480-frame render
  sample for sample, and a second run of the same tree is identical.
- **No allocation:** the enabled analyzer, estimator retune and segment gate
  allocate nothing in `process` (`oc-engine/tests/deesser_alloc.rs`).

## Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `threshold` | dB | −60–0 | −36 | absolute band-peak threshold |
| `threshold_mode` | enum | Absolute, Relative | Absolute | moving threshold |
| `relative_threshold` | dB | −36–0 | −12 | required band-to-program ratio in Relative |
| `range` | dB | 0–24 | 6 | gain-law slope and ceiling |
| `attack` | ms | 0.1–50 | 11 | ballistic 63% time |
| `release` | ms | 5–1000 | 15.5 | ballistic 63% time |
| `high_frequency` | Hz | 2000–20000 | 7500 | sidechain high-pass; split at HP/4 in Auto |
| `low_frequency` | Hz | 2000–20000 | 14000 | sidechain low-pass |
| `sibilance_band` | enum | Auto, 5–9 kHz | Auto | moves HP, LP and the split together |
| `band_estimation` | enum | Off, Learn | Off | running per-take band estimate |
| `band_override` | Hz | 0–20000 | 0 | pins `band`; overrides Learn and the steps |
| `segment_detection` | enum | Off, On | Off | gates the reduction to sibilant segments |
| `band_processing` | enum | Wide Band, Split Band | Wide Band | where the gain is applied |
| `detector` | enum | Sidechain, In-band | Sidechain | which signal the detector reads |
| `stereo_link` | % | 0–100 | 100 | blend to the mid |
| `lookahead` | ms | 0–15 | 0 | detector anticipation |

## Latency

The modes add no latency:

| configuration | reported latency |
|---|---:|
| wide, absolute, sidechain | 0 |
| wide, in-band | 0 |
| split, any threshold or detector mode | 512 samples (10.7 ms at 48 kHz) |
| split plus lookahead 15 ms | 1232 samples (25.7 ms) |

The split FIR is the only fixed delay; the relative window, the analysis window
and the detector tap do not add one. `lookahead` keeps its shipped meaning: it
moves the detector tap earlier and reports the full 15 ms line, it does not add
a second delay. The automatic band and the segment gate change no reported
latency: the analysis frames look backwards, they do not delay the audio.

## Defaults are bit-identical

The defaults select the shipped signal path: absolute threshold, sidechain
detector, wide band, split at `HP/4`, and the same parameter values. The golden
test hashes the f32 output of a deterministic 3 s body-plus-sibilant program:

| configuration | FNV-1a of the output |
|---|---|
| defaults | `14936a92bc4f0075` |
| split band | `495ef0f3cc1ad463` |
| lookahead 15 ms, link 50% | `4bf892bcf56d89f4` |
| split 6/12 kHz, threshold −40 | `4a5b8c667bd518fc` |

All four hashes are pinned by `defaults_render_bit_identically_to_the_shipped_device`.
`new_mode_defaults_are_no_ops` additionally asserts that setting the four new
parameters to their defaults leaves the output bit-identical.

## Tests

- `defaults_render_bit_identically_to_the_shipped_device`
- `new_mode_defaults_are_no_ops`
- `relative_gain_law_tracks_the_program_at_one_to_one`
- `relative_program_window_is_200_ms_attack_and_1_s_release`
- `relative_threshold_is_level_independent`
- `relative_threshold_does_not_pump_on_a_crescendo`
- `full_band_ducks_the_body_by_the_same_gain_as_the_sibilance`
- `full_band_detection_lets_the_body_trigger_the_reduction`
- `detector_placement_changes_which_signal_triggers_reduction`
- `sibilance_band_steps_move_the_split_and_sidechain`
- `split_corner_is_the_selected_band_not_hp_over_4`
- `analog_modes_add_no_latency`
- `band_estimator_finds_the_sibilant_band_on_three_takes`
- `band_estimator_reports_none_without_sibilance`
- `running_estimator_locks_the_band_and_reports_it`
- `automatic_band_does_not_act_without_sibilance`
- `segment_detector_fires_inside_the_sibilant_not_in_the_vowel`
- `segment_detector_hysteresis_does_not_chatter_on_a_noisy_boundary`
- `automatic_split_band_meets_the_six_db_and_half_db_gates`
- `band_override_pins_the_band`
- `chosen_band_reports_the_mode_in_use`
- `automatic_band_is_block_size_invariant`
- `reset_repeats_the_automatic_path`
- `zero_range_is_bit_identical_with_the_automatic_path_enabled`
- `automatic_band_and_segments_do_not_allocate_in_the_audio_path`

## Real-time safety

`process` allocates nothing, takes no locks and starts no threads. The detector
split, the program follower, the split FIR, the FFT plan, the analysis buffers
and the estimator accumulator are allocated in `new`; a learned band only
rewrites the preallocated FIR, and a parameter change rewrites it in place.
Modes that are off cost one branch; the relative mode adds one `log10` per
sample and the in-band split adds a second FIR per channel. With `Learn` or
`Segments` on, one 1024-point FFT and a few hundred band-bin sums run per 256 samples;
with both off there is no analysis at all.
