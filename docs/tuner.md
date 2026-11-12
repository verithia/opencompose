# Tuner

`tuner` is a scale-snapped vocal pitch corrector: YIN detection on a decimated
copy, a note state machine, and PSOLA resynthesis at the original sample rate.
The chain position and the latency budget are in
[`vocal-chain.md`](vocal-chain.md); this document is the law, the constants and
the measured form.

- `crates/oc-dsp/src/tuner.rs` — decimator, YIN detector, period track, note
  state machine, scale snap and the PSOLA stage.

All measurements below are 48 kHz; the device measurements are stereo, the
detector measurements use the mono mid signal. They come from the ignored
harness:

```
cargo test -p oc-dsp --release tuner::tests::measurements_for_docs -- --ignored --nocapture
```

## Detection

The detector runs on a band-limited copy at about 16 kHz. The decimation factor
is `round(sample_rate / 16000)`, so 48 kHz becomes exactly 16 kHz, 44.1 kHz
becomes 14.7 kHz and 96 kHz becomes 16 kHz. A 31-tap Blackman-windowed sinc
with its corner at `0.45 x decimated_rate` is applied before the downsampler;
at 44.1 kHz that is 6.6 kHz, far above the 1 kHz pitch ceiling but low enough
to keep consonant energy out of the difference function.

YIN runs over a 40 ms window (24 ms in low-latency mode) and reports one period
every 5 ms of sample time:

```
d(tau)   = sum over the window of (x[j] - x[j+tau])^2
d'(tau)  = d(tau) * tau / sum(d[1..tau])          # cumulative mean
tau0     = first local minimum with d'(tau0) < 0.15
fallback = global minimum of d', accepted only below 0.55
period   = parabolic interpolation of d at tau0
```

- The absolute-threshold rule keeps a multiple of the period from winning: a
  long lag is also a dip, but it is not the first one.
- Interpolation is on the raw difference, not on `d'`. The `1/tau` weighting
  biases the parabola at high f0; on the raw difference the worst error is
  0.39 cents against 2.76 cents with `d'`.
- The search range is 16..201 decimated samples at 16 kHz, 80–1000 Hz.
- Silence below −80 dBFS of window RMS is unvoiced.

The period track is a five-frame median. Octave repair runs before the median:
a raw period more than 1.75x the track is halved when the half lands within
20 % of the track, and one below 0.57x is doubled under the same test. A gap of
three invalid frames clears the track, so a new phrase does not inherit the
previous note's period; a one- or two-frame dropout keeps it.

### Detector error

Pure tone with a second harmonic at half the fundamental's level; the mean of
the settled track:

| f0 | measured | error |
|---:|---:|---:|
| 80 Hz | 80.000 Hz | +0.002 cents |
| 100 Hz | 100.000 Hz | +0.000 cents |
| 150 Hz | 150.000 Hz | −0.002 cents |
| 220 Hz | 219.999 Hz | −0.007 cents |
| 330 Hz | 330.001 Hz | +0.003 cents |
| 440 Hz | 440.012 Hz | +0.047 cents |
| 660 Hz | 660.070 Hz | +0.184 cents |
| 880 Hz | 880.197 Hz | +0.387 cents |
| 1000 Hz | 1000.000 Hz | +0.000 cents |

The same 220 Hz tone at 44.1 and 96 kHz is within 0.01 cents, so the decimation
factor is not the error term. The residual grows toward 1 kHz because a 16 kHz
frame only holds 16 samples per period and the parabola has less curvature to
work with.

### Breathy tone

220 Hz plus white noise, 1.5 s, 290 valid frames each:

| SNR | valid frames | octave-up frames | median error |
|---:|---:|---:|---:|
| 12 dB | 290 | 0 | +0.67 cents |
| 6 dB | 290 | 0 | +2.75 cents |

No frame in either run reports a period near half the true one, so the
octave-up failure mode is guarded by the first-dip rule plus the median and not
merely by the confidence threshold. At 6 dB the track widens to a few cents,
which is the expected detector jitter, not an octave error.

## Note state machine

`onset / steady / release`, updated at the 5 ms detection rate:

- `Idle` to `Onset` needs two valid frames; `Onset` to `Steady` needs six
  (30 ms).
- `Steady` keeps its target until the pitch leaves a **60 cent** band around
  it. Inside the band the target does not change even when the nearest scale
  note would; that is the hysteresis that a vibrato must not cross.
- Leaving the band switches the target to the nearest scale note and starts a
  new `Onset`. A legato jump of **150 cents** held for three frames starts a
  new note without an unvoiced gap.
- Two invalid frames enter `Release`; ten more (50 ms) reach `Idle`. The wet
  gain fades over 3 ms, so a release passes the dry (delayed) signal.

The 60 cent band is wider than the intended vibrato (±25 cents is a 50 cent
peak-to-peak excursion, inside the band) and the 150 cent retrigger is wider
still, so a 6 Hz vibrato cannot re-trigger a note. Measured with the state
machine fed a +30 cent note carrying ±25 cents of 6 Hz vibrato: 200 frames,
target unchanged, phase `Steady` throughout.

### Scale set and how it is chosen

The scale is chosen explicitly by the engineer with `key` and `scale`; the
device does not infer key from the audio or read the project key (a device has
no project context). `key` is one of the twelve pitch classes (sharps and
flats are both accepted) and `scale` is one of:

```
Chromatic, Major, Natural Minor, Harmonic Minor, Dorian, Mixolydian, Lydian,
Phrygian, Locrian, Major Pentatonic, Minor Pentatonic, Blues
```

`Chromatic` is the no-assumption default. Snapping is a note-level decision:
the continuous MIDI pitch is compared with every allowed note within two
semitones, and the nearest one becomes the note target. It is recomputed only
on onset or when the 60 cent band is left, never per sample, so the correction
is a constant per note plus the retune glide. The largest gap in the shipped
sets is three semitones (pentatonic and blues), so the snap asks for at most
300 cents of correction.

**The exact-semitone limitation.** A chromatic snap cannot correct a pitch that
is exactly one semitone away from the intended note: the input *is* the
neighbouring note, in tune. The same is true at an exact half-semitone tie in
any set. The 100 cent acceptance case therefore uses A major pentatonic, where
G and G# are absent and A is the only candidate within 300 cents. A pitch
100 cents below A (which is exactly G#) is therefore pulled up to A. The price
is that a singer who intended G# would also be pulled up; with the Chromatic
set the same input is in tune and is left alone. Both behaviours are a
consequence of note-level scale snapping, not of the shifter.

## Correction

The shifter is time-domain PSOLA at the original rate. It copies grains of the
input waveform; it never resamples, so the formant envelope is preserved by
construction rather than by an envelope correction.

```
R        = round(period)                       # one analysis period, samples
window   = 0.5 - 0.5*cos(pi*(k + 0.5)/R), k = 0..2R
p_in     = tracked period                      # original-rate samples
p_out    = p_in / 2^(correction/1200)          # epoch spacing, output samples
```

An analysis epoch is a positive peak of the 5-tap smoothed mid signal, found in
the last period and refined by a three-point parabola. Epochs sit on a grid
`anchor + n*p_in`, so consecutive grains sample the same phase of the waveform.
For each output epoch the grain is the grid point nearest `epoch - delay`; when
`p_out` is shorter than `p_in` some grid points are skipped, and when it is
longer some are repeated. That is what changes the period: the output peaks sit
at the epoch spacing, while each grain is a copy of the input around a real
peak.

Grains are accumulated in an output ring and normalised by the accumulated
window sum, so a change of overlap ratio does not change the level. The delay
line is `D` samples:

- full range: **1920 samples (40.0 ms at 48 kHz)**, 40 ms analysis window,
  80–1000 Hz;
- low latency: **960 samples (20.0 ms at 48 kHz)**, 24 ms analysis window,
  120–1000 Hz.

`D` is reported through `latency_samples()` and therefore to the graph's PDC.
The dry path is the input delayed by exactly `D`, measured at 1920 samples in
both modes when the detector reports unvoiced. The wet path's features sit at
`D + delta` where `delta` is the grain-grid offset, bounded by half an analysis
period: measured 1949 samples (40.6 ms) at 220 Hz, i.e. 0.6 ms beyond the
reported number and inside the half-period bound. The device does not report a tail; the delayed input is the
latency, and PDC aligns the other tracks.

The correction itself is a one-pole on the cents error, evaluated once per
detection frame:

```
desired   = amount * (target_midi - tracked_midi) * 100     # cents
state    += (desired - state) * (1 - exp(-dt/tau))          # tau = retune
p_out     = p_in / 2^(state/1200)
```

`amount` scales the correction, not the wet/dry mix. At `amount = 0` (and at
`enabled = false`) the device is a bit-exact copy with zero reported latency;
the delay line is bypassed, so there is a latency step when it is engaged, the
same contract as `resonance`'s zero-depth bypass. `retune` is the one-pole time
constant, 10–300 ms, default 50 ms; a longer constant leaves more of a fast
vibrato and slows the pull-in of a static error.

The correction is clamped to ±1200 cents. The scale sets never ask for more
than 300 cents. Down-shifts near −1200 cents sit at the edge of the two-period
grain overlap and modulate; the specified working range is ±300 cents.

### Pitch-shift range

| range | behaviour |
|---|---|
| ±300 cents | all scale sets; grains keep at least 40 % overlap |
| +1200 cents | four grains overlap; level normalised, no gaps |
| −600 cents | about 25 % overlap; still supported |
| −1200 cents | grains just touch; the window sum reaches zero at the junction, so the wet path modulates |

## Acceptance, measured

The input is a 48 kHz stereo tone; output pitch is measured from interpolated
positive zero crossings after the reported delay.

| item | requirement | measured |
|---|---|---|
| 100 cent flat from 220 Hz, amount 1.0 (A major pentatonic) | within 2 cents | **220.000 Hz, −0.000 cents** |
| amount 0 | bit-identical to input | **bit-identical, latency 0** |
| C major run, each note −40 cents | within 5 cents | **worst 0.233 cents** |
| 6 Hz ±25 cent vibrato, amount 0.5 | preserved, depth reported | **24.97 → 18.52 cents** |
| hard 200/150 cent steps, retune 10 ms | no click | **max step 1.012x the input's** |
| latency, full range | 35–45 ms | **1920 samples = 40.0 ms** |
| latency, low latency | ~20 ms | **960 samples = 20.0 ms** |

The vibrato loss is the retune one-pole doing its job: at amount 0.5 the
applied correction is half the tracking error and the one-pole lags a 6 Hz
signal, so the depth falls from 25 to 18.5 cents rather than disappearing.

The click metric is the largest sample-to-sample step in the output against the
largest step in the input, over a phase-continuous tone that steps 200 cents up
and 150 cents down. It is 1.012x, so the hard-tune path has no spectral stage
and does not need one at the 10 ms retune floor. A step that bypasses the
one-pole would be a different device; the retune control is the guard.

## Parameters

| id | range | default | meaning |
|---|---|---|---|
| `amount` | 0–100 % | 100 % | correction depth; 0 % is a bit-exact bypass |
| `retune` | 10–300 ms | 50 ms | one-pole time constant on the correction |
| `key` | C … B | C | scale root |
| `scale` | 12 sets | Chromatic | note-level snap set |
| `mode` | Full Range / Low Latency | Full Range | full range has an 80 Hz floor and 40 ms; low latency a 120 Hz floor and 20 ms |
| `enabled` | on/off | on | off is a bit-exact bypass with zero latency |

`amount`, `retune`, `key`, `scale` and `mode` reach the device in the declared
units: percent is divided by 100, time is milliseconds, names are resolved by
`key_name`, `scale_name` and `mode_code`. Unknown names, wrong units and
unknown ids are rejected by the factory rather than silently clamped.

## Real-time safety

`new` allocates the decimator, two YIN detectors (full and low-latency, so a
mode change does not allocate), the dry delay ring, the smoothing ring and the
output accumulation ring. `process` touches those buffers and scalar state
only: no allocation, no locks, no threads. The detector runs every 5 ms; the
grain scheduler and the OLA run per sample. Processing is block-size invariant
(97-frame and 480-frame blocks produce the same samples).

The measured cost in the release build is **0.0182 s per audio second (55x
realtime)**, 144,672 bytes of state. The debug build is 0.279 s per audio
second (3.6x realtime); the difference is the unoptimised YIN difference loop.
`amount = 0` costs 0.00030 s per audio second because it only maintains history
and the decimator. The stage is not optional: at 55x realtime it is affordable
on the audio thread, and it is the only stage that changes pitch.

## Tests

`tuner.rs`:

- detector: `pitch_error_across_the_range`, `pitch_error_holds_at_other_sample_rates`,
  `breathy_noise_never_reports_an_octave_up`,
  `a_single_octave_jump_is_filtered_out`,
  `a_short_gap_keeps_the_track_and_a_long_gap_clears_it`
- note state: `scale_snap_uses_the_whole_scale`,
  `c_major_run_snaps_each_note_within_five_cents`,
  `vibrato_never_retriggers_a_note`, `a_legato_step_starts_a_new_note`,
  `the_hysteresis_band_holds_a_note_before_switching`,
  `release_needs_two_invalid_frames_and_returns_to_idle`
- correction: `a_hundred_cent_detuned_220_hz_is_corrected_within_two_cents`,
  `amount_zero_is_a_bit_exact_bypass`, `a_c_major_run_lands_on_the_scale_notes`,
  `vibrato_survives_half_amount`, `hard_tune_steps_do_not_click`,
  `reported_latency_matches_the_measured_delay`,
  `the_dry_path_delays_by_exactly_the_reported_latency`,
  `breathy_notes_do_not_jump_an_octave`,
  `processing_is_block_size_invariant`

`oc-engine`:

- `dsp::tuner_factory_reports_the_psola_delay`,
  `dsp::tuner_parameters_reach_the_dsp_in_declared_units`,
  `graph::tests::tuner_chain_reports_the_psola_delay`.

## Deliberately out of scope

- **No spectral stage.** Hard-tune steps are a retune ramp, not a phase-locked
  spectral resynthesis. At the 10 ms floor the measured step ratio is 1.012x;
  a shorter ramp is not exposed because it would click.
- **No key inference.** The key and scale are controls. A project-wide key
  could be wired in at the graph level later; the device has no project
  context to read.
- **No formant control.** PSOLA keeps the source envelope; there is no
  formant shift or vocal-tract knob.
- **Monophonic.** One period and one note target at a time, as a vocal take
  should be; polyphony would need per-note detection and is not a device.
- **Unvoiced audio is not corrected.** Consonants and breaths pass through the
  dry path, delayed by the same D, so they are not gated or pitch-shifted.
- **A note's first detected window is dry.** The detector needs 40 ms (24 ms)
  of window plus two onset frames; the first ~50 ms of a phrase is the delayed
  input, then the wet path fades in. This is the price of a 40 ms detector and
  is why the low-latency mode exists.
