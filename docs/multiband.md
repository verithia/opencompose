# Multiband compander

The `multiband` device is a mode-dependent multiband compander: a selectable
crossover feeds two to four per-band gain stages, and each stage has a bipolar
range clamp and a two-stage program-dependent ballistic.

This note describes the design, the measured behaviour, and the deliberate
choices that differ from a generic multiband compressor. It does not describe
any other product's implementation.

## Signal path

```
input ──> crossover ──> band 1 delay ──> band 1 compander ──┐
             │  └────> band 2 delay ──> band 2 compander ──┤
             │  └────> band 3 delay ──> band 3 compander ──┤──> sum ──> dry/wet ──> output
             └───────> band 4 delay ──> band 4 compander ──┘
```

The crossover is progressive: split 1 separates band 1 from the rest, split 2
separates band 2 from the rest, and so on. Every split carries an allpass
compensation for the bands below it, so the sum of the bands is a cascade of
allpasses and is flat in magnitude. The per-band delay stages are the lookahead
line: they are all the same length, so the sum stays flat and only moves in
time (see [Lookahead](#lookahead)).

## Crossover

### Slopes

All slopes are built from our own complementary Butterworth sections:

| Slope | IIR construction | High-band sign |
|---|---|---|
| 6 dB/oct | one first-order Butterworth section | + |
| 12 dB/oct | two cascaded first-order sections (LR2) | − |
| 24 dB/oct | two cascaded second-order Butterworth sections (LR4) | + |
| 36 dB/oct | two cascaded third-order Butterworth sections (LR6) | − |
| 48 dB/oct | two cascaded fourth-order Butterworth sections (LR8) | + |

The square/cascade produces the Linkwitz-Riley alignment: each band is −6 dB
at the crossover. The sign is the polarity that makes `LP ± HP` an allpass. For
orders where the base order is odd (12 and 36 dB/oct) the high branch is
inverted; the other slopes add in phase.

The compensation allpass is the complementary sum `LP ± HP` of the same split.
For a base Butterworth order `m` it reduces exactly to a cascade of `m`
Butterworth-pole allpass sections (one first-order allpass plus second-order
allpasses with `Q = 1/(2 cos θ)`). No measured or copied coefficient tables are
used.

### Phase modes

| Mode | Latency | Pre-ringing | Behaviour |
|---|---|---|---|
| Minimum | 0 frames | none | IIR split with the exact allpass compensation. |
| Dynamic | 0 frames | none | The same split, plus a program-dependent allpass blend on each low-band compensation path. |
| Linear | 1024 frames at 48 kHz (21.33 ms) | ±1024 frames | Per-band linear-phase FIRs designed from the same magnitude shapes. |

**Minimum** is the default. It is the cheapest and has no latency.

**Dynamic** keeps the IIR split and adds a first-order allpass at 90 Hz after
each compensation allpass. The blend amount comes from the ratio of a fast and a
slow mean-square detector (1 ms / 80 ms attack, 80 ms / 500 ms release). A
1.5 dB deadband suppresses the control on stationary program, and a 6 dB
fast/slow difference maps to full alignment. On a transient the low band's
group delay bends toward the high band's leading edge; on stationary program the
control is exactly zero, so the steady-state sum equals Minimum. The deliberate
trade is that the band sum is not perfectly flat while the control is moving;
this is the phase-alignment behaviour the mode exists for.

**Linear** designs one FIR per band from the same Linkwitz-Riley magnitude
masks. The masks sum to one at every frequency, so the sum of the FIRs is an
exact delta. The implementation uses a 2049-tap Kaiser-windowed design
(β = 10) and overlap-add with a 4096-point FFT; blocks up to 2048 frames are
processed without re-planning, and the overlap-add state makes the result
independent of the caller's block size. Because the FIR sum is a delta, the
band sum is the input delayed by exactly 1024 frames after latency compensation.
The cost is 21.33 ms of latency and ±21.33 ms of pre-ringing at 48 kHz.

### Flatness and slope measurements

Measured at 48 kHz with three bands, steady tones from 60 Hz to 16 kHz, after
the mode's latency:

- Worst band-sum deviation across every slope × phase mode: **0.0046 dB**.
- Stated tolerance: **±0.01 dB** for stationary program in all three modes.
- Dynamic mode has a separately bounded transient deviation while its control
  is active; it settles back to the same ±0.01 dB steady-state tolerance.

Slope measured over one octave after the transition region:

| Slope | Minimum | Dynamic | Linear |
|---|---|---|---|
| 12 dB/oct | 11.72 | 11.72 | 11.66 |
| 24 dB/oct | 24.69 | 24.69 | 24.09 |
| 48 dB/oct | 48.84 | 48.84 | 48.21 |

All three are within ±1 dB/oct of nominal. The IIR numbers are slightly steep
at 48 dB/oct because of bilinear frequency warping; the linear numbers are
slightly shallow because of the windowed FIR transition. Both are inside the
acceptance window.

## Per-band compander

### Detector

`detector` selects the detector law of every band. The default, `Peak`, is the
law every earlier render used; `Programme` changes only the transient
behaviour, so a sustained level settles on the same reduction in both modes.

#### Peak

A stereo-linked peak follower with a fixed 0.1 ms attack feeds the gain
computer. Its release is the band release, and the user attack and release also
shape the gain-change envelope (see [Ballistics](#ballistics)). The detector
therefore holds a transient's peak for the release time, and the fast stage
carries 35 % of the change through the user attack.

#### Programme

The sustained stage keeps the peak follower and the gain law, and carries the
sustained reduction with the user attack and release. A second, fast stage
carries the part of the clamped reduction the sustained stage has not delivered
yet, with a 1 ms attack and a 40 ms release:

```
slow_down   = follow(slow_down, down_target, attack, release)
undelivered = max(0, down_target - slow_down)
fast_down   = follow(fast_down, undelivered, 1 ms, 40 ms)
```

On a transient the fast stage delivers the reduction before the first peak of a
low note, and lets go in tens of milliseconds; on a sustained level the
sustained stage has already delivered everything, so the fast stage sits at zero
and the reduction is exactly the `Peak` value. The stage is therefore active
only when the programme outruns the attack, which is what a transient-dense
programme does and a steady tone does not.

Measured at 48 kHz, band 1 of the `low-end-grip` law (threshold −26 dB,
range −6 dB, ratio 3:1, knee 6 dB, attack 15 ms, release 220 ms):

| Material and measurement | `Peak` | `Programme` | Static `gain_law` |
|---|---:|---:|---:|
| 60 Hz tone bursts (40 ms on / 160 ms off), output crest | 13.28 dB | 9.91 dB | 9.69 dB |
| 55 Hz bed with 30 ms hits, output crest | 16.05 dB | 12.55 dB | 11.46 dB |
| Reduction 10 ms into a 40 ms burst | 1.96 dB | 6.00 dB | 6.00 dB |
| Reduction 60 ms after the burst ends | 5.86 dB | 6.00 dB | 0.00 dB |

The material's own crest is 9.95 dB for the burst train and 12.75 dB for the
bed: `Programme` keeps it, `Peak` adds 3.3 dB. The static column is the clamp of
`gain_law` applied to the band signal with no ballistics at all. `Programme` removes the crest inflation the peak law adds
(+3.3 dB over the input) and puts the onset reduction on the static value; it
does not remove the release hold, which is the band release control.

A steady tone settles within 0.01 dB of the `Peak` law at every level from
−30 dBFS to −10 dBFS, so the mode never changes the level of sustained
material. Below `threshold_below` the boost is unchanged: a −40 dBFS band with
`threshold_below = −30 dB`, `ratio_below = 2` gains 5.02 dB in both modes.

### Gain law and Range

The static gain law is symmetric around the threshold:

```
slope = 1 - 1/ratio
over  = level_db - threshold_db
gain  = -slope * over          (with a quadratic soft knee of `knee` dB)
```

Positive gain means a boost below threshold; negative gain means a cut above
threshold. `range` is a bipolar clamp on that gain change:

| Range | Clamp | Behaviour |
|---|---|---|
| `+N` | `[-N, +N]` | Compander: quiet content may be boosted by up to `N` dB, loud content cut by up to `N` dB. |
| `0` | `[0, 0]` | Neutral: no dynamic gain change. |
| `-N` | `[-N, 0]` | Downward only: loud content may be cut by up to `N` dB; quiet content is not boosted. |

`range = 0` is the default, so a freshly inserted band is neutral. A non-zero
range is required for the threshold and ratio to do anything; the UI description
says so on every band. `makeup` is a static gain added after the clamp.

### Ballistics

The clamped gain change is split into attenuation and boost envelopes. Each is
handled by a fast and a slow stage that share the attack coefficient:

- Fast stage: 35% of the change; the user attack and release times.
- Slow stage: 65% of the change; the same attack time, release 3× longer.

On release the fast stage lets go first and the slow stage holds the tail; on a
short transient the reduction is therefore shorter than on sustained program.
This split is the `detector = peak` path; `detector = programme` replaces it
with the fast catch-up stage described above.
The constants are our own: 35% keeps the measured release close to the knob
while still giving a program-dependent tail, and 3× gives a long but audible
recovery without pumping.

Measured at 48 kHz, band 1, a 12 dB gain change:

- Attack to 63% of the final change with a 5 ms knob: **5.46 ms**.
- Release to 63% of the initial change with a 100 ms knob: **91.15 ms**.

The attack spec is ±30% of the knob; the release spec is 0.6–1.5× the knob
because the slow stage and the fixed detector release both contribute.

### Enable and makeup

`bandN_enabled` bypasses the band's dynamic gain change and makeup; the band
still passes through the crossover, so the sum stays flat. `bandN_makeup` is a
static trim after the clamp.

Because the trim is inside the band, several bands' makeups add in the sum: two
bands at +0.5 dB lift the output by 1 dB. Use `bandN_makeup` to place one band
in the mix and not as the device's output gain; a preset that compensates its
compression band by band shifts the band balance instead of the level.

## External key

The device declares an audio `key` input that does not sum into the output. A
band whose `bandN_detector` is `Key` reads its detector level from that input,
so another track's level can duck one band while the others keep their in-band
behaviour:

```ocp
@ocp "0.4.0"

@project "Keyed band"
  tempo: 120
  time_signature: 4/4

@preset bass
  source: builtin
  plugin: "builtin.open_synth"

@track kick | preset: bass, instrument: synth/bass
  @phrase hits [1:1 - 2:1]
    C2 q mf

@track bass | preset: bass, instrument: synth/bass
  effects:
    - multiband: crossover_1=300Hz, band1_detector=1, band1_threshold=-35dB, band1_ratio=8, band1_range=-18dB
  route: kick -> multiband
  @phrase line [1:1 - 2:1]
    C2 w mf
```

The enum is written by index in that line: `band1_detector=1` is `Key`, `0` is
`In band`.

A measured probe with a 100 Hz + 5 kHz signal and a 1 kHz key: with the key
loud, the 100 Hz band drops by more than 60 % while the 5 kHz band stays within
10 % of its unkeyed level.

## Solo and mute

`bandN_solo` and `bandN_mute` decide which bands reach the sum:

| Solo anywhere | Mute | Band reaches the sum |
|---|---|---|
| no | off | yes |
| no | on | no |
| yes | off | only when this band is soloed |
| yes | on | no: mute always wins |

The band still runs its detector and ballistics while it is out of the sum, so
the meter and the recovered gain stay current when it comes back. Because a
soloed or muted band is dropped rather than replaced, the sum is no longer
magnitude-flat: that is the point of the control, and it is why the default
state is every band audible.

Measured at 48 kHz with three bands at 500 Hz and 4 kHz, 24 dB/oct, all bands
neutral, 0.2-peak tones:

| Setting | 100 Hz | 1 kHz | 8 kHz |
|---|---:|---:|---:|
| neutral | 0.00 dB | 0.00 dB | 0.00 dB |
| band 2 soloed | −55.94 dB | −0.56 dB | −27.06 dB |
| band 1 muted | −55.94 dB | −0.52 dB | — |

The residual at 1 kHz is the band's own magnitude, not a leak: a soloed band is
the only signal left, and an LR4 band is not unity at its centre. The
neighbouring-band figures are the crossover slope, so a 24 dB/oct split leaks
about 27 dB one octave above the mid band.

## Lookahead

`lookahead` allocates a shared line in front of every band and is the device's
reported latency. Every band's audio is delayed by the same number of frames, so
the band sum still reconstructs the input, only later. `bandN_lookahead` chooses
how far ahead that band's detector reads inside the line: 0 reads the sample
that is about to leave the line (no anticipation), the full value reads the live
input. A per-band value above the shared line is clamped to it, so a project
that sets only `bandN_lookahead` without `lookahead` gets no anticipation and no
latency.

The detector reads the band after the crossover and before the line's output,
so the gain is already moving when the delayed audio arrives. Measured at
48 kHz, band 1 with a 10 ms attack and a 100 Hz burst 10 ms into the buffer:

| `lookahead` / `band1_lookahead` | First 5 ms of the burst at the output |
|---|---:|
| 5 ms / 0 ms | 0.27047 |
| 5 ms / 5 ms | 0.17317 (**−3.87 dB**) |

## Parameters

Global:

| Id | Unit | Range | Default | Meaning |
|---|---|---|---|---|
| `band_count` | enum | 2 / 3 / 4 | 2 | Active bands; uses the first N−1 crossovers. |
| `slope` | enum | 6/12/24/36/48 dB/oct | 24 | Slope of every split. |
| `phase_mode` | enum | Minimum / Dynamic / Linear | Minimum | Crossover phase mode. |
| `crossover_1` | Hz | 20–20000 | 1000 | Lowest split. |
| `crossover_2` | Hz | 20–20000 | 4000 | Second split (3–4 bands). |
| `crossover_3` | Hz | 20–20000 | 10000 | Third split (4 bands). |
| `mix` | % | 0–1 | 1 | Dry/wet; the dry path is delay-aligned with the wet path. |
| `lookahead` | ms | 0–20 | 0 | Shared audio delay and reported latency. |
| `detector` | enum | Peak / Programme | Peak | Detector law of every band (see [Detector](#detector)). |

Per band (`N` = 1..4):

| Id | Unit | Range | Default | Meaning |
|---|---|---|---|---|
| `bandN_enabled` | bool | 0/1 | 1 | Bypass this band's dynamic gain change and makeup. |
| `bandN_threshold` | dB | −60–0 | 0 | Compander pivot. |
| `bandN_range` | dB | −60–+60 | 0 | Bipolar gain-change clamp (see above). |
| `bandN_ratio` | ratio | 1–20 | 1 | Gain-law slope. |
| `bandN_attack` | ms | 0.1–200 | 10 | Fast-stage 63% time. |
| `bandN_release` | ms | 10–2000 | 100 | Fast-stage release; slow stage is 3×. |
| `bandN_knee` | dB | 0–24 | 6 | Soft-knee width. |
| `bandN_makeup` | dB | −12–+24 | 0 | Static gain after the clamp. |
| `bandN_lookahead` | ms | 0–20 | 0 | Detector anticipation inside the shared line; clamped to `lookahead`. |
| `bandN_solo` | bool | 0/1 | 0 | Drop the other active bands from the sum. |
| `bandN_mute` | bool | 0/1 | 0 | Drop this band from the sum; mute wins over solo. |
| `bandN_detector` | enum | In band / Key | In band | `Key` takes this band's detector level from the audio key input instead of the split signal. |

### Id compatibility

`band_count`, `crossover_1..3`, `mix`, and every
`bandN_threshold/ratio/attack/release/knee/makeup` are stable ids with fixed
names and units, so stored projects keep loading. `detector` defaults to
`Peak`. `bandN_ratio` is the compander slope inside the range clamp, so a
non-zero `bandN_range` is needed for any gain change. `slope`, `phase_mode`, `bandN_range`,
`bandN_enabled`, `lookahead`, `bandN_lookahead`, `bandN_solo` and `bandN_mute`
default to the plain split-and-sum path: `lookahead = 0` and no solo or mute
keep the render bit-identical to the stored digest, which
`defaults_render_the_pre_lookahead_digest` pins.

## Latency

| Mode | Latency | Compensation |
|---|---|---|
| Minimum | `lookahead` frames | reported; dry path delayed by the same amount |
| Dynamic | `lookahead` frames | reported; dry path delayed by the same amount |
| Linear | 1024 + `lookahead` frames | reported through `latency_samples()`; dry path delayed by the same amount |

`lookahead` is truncated to whole frames at the current sample rate, and the
reported latency uses that same frame count, so 5 ms at 48 kHz is exactly 240
frames and 1264 frames in Linear mode. Changing `lookahead` moves the reported
latency, so the graph must be prepared again; a project that never sets it keeps
the zero-latency IIR path.

The linear reconstruction error after compensation is below 1e-9 in the
crossover test and below 1e-6 through the full device (f32 output). With a
0.99 FS input the output peak is 0.99; the declared ceiling is 1.0 and no sample
overshoots it.

## Deliberate differences from a generic multiband

- The 6 dB/oct linear mode uses the complementary magnitude pair
  `1/(1+x)` / `x/(1+x)`. A zero-phase implementation cannot use the first-order
  Butterworth magnitudes directly because their magnitudes do not sum to one;
  the chosen pair has the same 6 dB/oct asymptote and sums exactly to one.
- Dynamic mode trades momentary band-sum flatness for transient phase
  alignment. It is not a static allpass and it is not intended to be measured
  during the transient.
- Range is direction-selecting, not a symmetric magnitude: `+N` is a symmetric
  compander clamp, `−N` is a downward-only clamp. The UI descriptions say this
  on every band.
- One audio key input feeds any band whose `bandN_detector` is `Key`; the other
  bands stay on their split signal. A band set to `Key` with no route falls back
  to its in-band level, so a project without a key behaves as before.
- No per-band pan, oversampling, or auto-release programs. The device is a
  compander, not a full channel strip.
- Solo and mute drop bands from the sum instead of replacing them with silence
  in the crossover. The sum is therefore not flat while a solo is active, which
  is the intended audition behaviour.
- The lookahead line is shared by every band and its length is the reported
  latency; only the detector tap is per band. A per-band audio delay would
  smear the band sum and break the flat reconstruction.
- The linear crossover is windowed-FIR overlap-add, not a zero-padded FFT mask
  with a different latency; the 2049-tap design was chosen so the 48 dB/oct
  slope stays within ±1 dB/oct after windowing.

## Real-time safety

The audio path allocates nothing: all filter states, FFT twiddles, bit-reversal
tables, FIR responses, overlap buffers, and the linear dry delay are allocated
at construction. The lookahead lines are allocated at construction too, sized
for 20 ms at the device's sample rate. `process` takes no locks and starts no
threads. The `multiband_alloc` integration test runs four bands in Minimum and
Linear modes with the counting allocator, lookahead, per-band lookahead and a
solo, and asserts zero allocations.

## Tests

- `crossover_sums_flat_for_every_slope_and_phase_mode`
- `slope_matches_the_nominal_value_for_12_24_and_48`
- `linear_sum_is_the_delayed_input`
- `block_splitting_does_not_change_the_linear_result`
- `latency_is_per_phase_mode`
- `neutral_setting_is_level_neutral_for_every_mode_and_slope`
- `positive_range_allows_boost_and_cut`
- `negative_range_clamps_downward_and_blocks_boost`
- `attack_and_release_are_measured_against_the_knob`
- `disabled_band_applies_no_gain_change`
- `a_tone_in_band_one_only_moves_band_one`
- `latency_is_reported_and_linear_impulse_lands_on_it`
- `linear_mode_does_not_overshoot_the_declared_ceiling`
- `reset_is_deterministic_for_every_mode`
- `extreme_settings_stay_finite`
- `solo_and_mute_select_which_bands_sum`
- `mute_wins_over_solo`
- `lookahead_is_the_reported_latency_and_delays_the_output`
- `lookahead_delays_the_dry_path_in_minimum_mode`
- `per_band_lookahead_cannot_exceed_the_shared_line`
- `per_band_lookahead_anticipates_that_bands_detector`
- `defaults_render_the_pre_lookahead_digest`
- `four_bands_do_not_allocate_in_minimum_or_linear_mode` (oc-engine integration)
