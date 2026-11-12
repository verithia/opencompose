# FDN reverb

`crates/oc-dsp/src/reverb_fdn.rs` is the long-form reverb behind the
`reverb_fdn` device and DSL effect. It keeps two independent per-channel networks — a 16-line Hadamard
FDN and a 2-line FDN — with six measured per-band RT30 breakpoints, a six-band
feedback tilt, a 256-tap early-reflection table and two modulated stages. This
note covers the user-facing decay and post EQ controls.

## Signal path

```
L,R -> pre-delay -> early-reflection taps -> one-pole absorption
                   \-> input damping -> diffusers -> 16-line FDN + 2-line FDN
                                                      -> width -> post EQ -> mix
```

The late field is the sum of both tanks; the 2-line tank supplies a small,
longer part of the tail. The six feedback tilt bands are centred at 125 Hz,
1 kHz, 2 kHz, 4 kHz, 8 kHz and 16 kHz and are designed so each band's loop
decays at its own RT30 target. Nothing here allocates, takes a lock or starts a
thread after `new`.

## Brightness

`brightness` is a six-band decay tilt on top of the Space table. The measured
points at 0, 25, 50 and 75 % come from the late-field octave spectrum; 50 % is
the neutral setting where the tilt is unity. Below 50 % the top octaves decay faster
than the 1 kHz band, above 50 % they decay slower:

| brightness | 1 kHz | 4 kHz | 8 kHz | 16 kHz | tank absorption cutoff |
|---|---|---|---|---|---|
| 0 % | 2.739 s | 2.220 s | 1.891 s | 1.461 s | 1 640 Hz |
| 25 % | 2.799 s | 2.350 s | 2.060 s | 1.720 s | 2 350 Hz |
| 50 % | 2.850 s | 2.500 s | 2.230 s | 1.950 s | 5 850 Hz |
| 75 % | 2.881 s | 2.650 s | 2.420 s | 2.190 s | 11 200 Hz |
| 100 % | 2.881 s | 2.881 s | 2.881 s | 2.881 s | 16 000 Hz |

Targets are RT30 at Space 50 % and Decay Rate 100 %; the measurement is
`brightness_tilt_matches_the_measured_t60_table` and
`brightness_extension_raises_the_top_octaves`.

The measured table stops at 75 %. Above that point the tilt is removed
linearly: at 100 % every band takes the slowest band's 75 % RT30, so the six
targets are equal and the in-loop tilt biquads are flat. The endpoint follows
the network: the 1 kHz band is the loop-gain reference, and a full-brightness
tone has no frequency-dependent loss left to tilt the tail. The tank's
one-pole absorption also opens from 11 200 Hz to 16 000 Hz, the top tilt
breakpoint, so it stops cutting the 16 kHz octave at the input.

The 0-75 % segment is unchanged. `brightness_ratios` returns the measured
function before it evaluates any extension arithmetic, and
`measured_brightness_law_is_frozen_below_75` pins render digests at 0, 25, 50
and 75 %.

### Measured effect on Long Hall, Dark

The preset is `space` 74 %, `decay_rate` 100 %, `character` 55 %, `distance`
62 %, `predelay` 35 ms, `mix` 22 % and decay trims +2/+1/0/0/−1/−3 dB. RT30
per octave, with Character bypassed and Mix wet-only so the fit sees the late
field:

| brightness | 125 Hz | 1 kHz | 2 kHz | 4 kHz | 8 kHz | 16 kHz | 500 Hz-8 kHz tail |
|---|---|---|---|---|---|---|---|
| 34 % | 6.943 s | 5.651 s | 4.715 s | 4.222 s | 3.362 s | 2.434 s | −84.28 dB |
| 48 % | 6.947 s | 5.716 s | 4.875 s | 4.341 s | 3.501 s | 2.584 s | −82.57 dB |
| 75 % | 6.952 s | 5.795 s | 5.067 s | 4.680 s | 3.918 s | 2.997 s | −80.46 dB |
| 100 % | 6.967 s | 6.033 s | 5.351 s | 5.342 s | 4.841 s | 4.083 s | −78.56 dB |

The 34 → 48 % step moves 8 kHz by 4.1 % and 16 kHz by 6.1 %; 100 % moves
16 kHz by 68 % over 34 % and lifts the 500 Hz-8 kHz tail by 5.72 dB, above the
3.31 dB run-to-run floor the old comparison carried. With the preset's own
Character and Mix the tail gains are +1.62 dB (34 → 48 %) and +5.55 dB
(34 → 100 %). The measured 16 kHz decay at 100 % is still shorter than the
1 kHz decay because the six-band measurement cannot isolate the top octave
from the neighbouring tank modes; the target table itself is flat.

Harness: `cargo test -p oc-dsp --lib -- reverb_fdn::tests::long_hall_dark -- --nocapture`.

## Decay EQ

Each of the six `decay_bandN` trims (dB, −24…+12, default 0) is placed by
`decay_bandN_freq` (Hz, 20…20000), `decay_bandN_q` (0.1…10) and
`decay_bandN_shape` (`None`, `Low Shelf`, `Bell` or `High Shelf`):

| parameter | unit | range | default |
|---|---|---|---|
| `decay_bandN` | dB | −24…12 | 0 |
| `decay_bandN_freq` | Hz | 20…20000 | the band centre (125 Hz, 1 kHz, 2 kHz, 4 kHz, 8 kHz, 16 kHz) |
| `decay_bandN_q` | — | 0.1…10 | 0.707 |
| `decay_bandN_shape` | enum | None / Low Shelf / Bell / High Shelf | None |

The trim is not applied as an extra filter in the feedback loop. A +6 dB shelf
there would multiply the loop gain above one and the tail would grow instead of
decaying. Instead the shape's magnitude response is folded into the per-band
RT30 targets that the in-loop tilt biquads are designed from:

```
target_band[k] = space_band_rt30(space, brightness, decay_rate)[k]
                 * 10^(trim_db[k] / 20)
trim_db[k]     = sum over bands m of response_m(TILT_HZ[k])
```

`response_m` is the magnitude of band `m`'s biquad (designed with gain = its
trim) at the breakpoint `TILT_HZ[k]`. A band whose shape is `None` contributes
its trim to its own breakpoint only, so `None` reproduces the previous law
exactly. A low shelf at 4 kHz on band 1 therefore lengthens the decay of every
breakpoint below 4 kHz by the trim and leaves the breakpoints above it alone; a
bell changes the breakpoints around its own frequency.

With all trims at 0 dB the multiply is skipped and the render is bit-identical
(`defaults_keep_the_frozen_render`, digest `0x9fc7cf190ebd7637`). The shape
response is evaluated at the six fixed breakpoints, so a narrow bell is
quantised to those points and the tilt chain interpolates between them; a shelf
is exact at the breakpoints. One biquad per band also means the placement is not
a slope cascade.

Because the mid band (1 kHz) is also the loop-gain reference, trimming it moves
the reference and the other bands' tilt targets are recomputed to keep their
absolute RT30 unchanged. Trimming any other band leaves the rest where they
were.

## Post EQ

A six-band EQ on the wet path, after width and before the dry/wet mix:

| parameter | unit | range | default |
|---|---|---|---|
| `post_eqN_freq` | Hz | 20…20000 | 100 / 300 / 1k / 3k / 8k / 16k |
| `post_eqN_gain` | dB | −30…30 | 0 |
| `post_eqN_q` | — | 0.05…20 | 1.0 |
| `post_eqN_shape` | enum | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell |

Each band is one biquad (12 dB/oct for the cuts and the notch). A band is
engaged when its shape is a cut/notch or its gain is non-zero; with every gain
at 0 dB the whole EQ is bypassed, so the default render is bit-identical.
Non-engaged bands are skipped, and their coefficients are never applied.

## Measurements

Method: render a mono impulse through a wet-only instance (character 0,
mix 100 %), band-pass each of the six centres with a Q = 6 biquad, smooth the
band energy with a 20 ms one-pole, fit the −5…−35 dB decay segment and report
RT30 = 60 / |slope|. Harness:
`cargo test -p oc-dsp decay_band_trim_moves_only_that_band -- --nocapture`.

Default (space 50 %, brightness 50 %, decay rate 100 %):

| band | 125 Hz | 1 kHz | 2 kHz | 4 kHz | 8 kHz | 16 kHz |
|---|---|---|---|---|---|---|
| RT30 target | 2.87 s | 2.85 s | 2.72 s | 2.50 s | 2.23 s | 1.95 s |
| measured RT30 | 2.891 s | 2.968 s | 2.807 s | 2.639 s | 2.413 s | 2.282 s |

The high bands measure longer than the breakpoint because the tank's input
damping and the finite band isolation add a small broadband component; the
decay law is pinned by `feedback_eq_hits_the_band_t60_targets`, and the trim
table below is measured against the same method.

With `decay_band1 = +6 dB`:

| band | 125 Hz | 1 kHz | 2 kHz | 4 kHz | 8 kHz | 16 kHz |
|---|---|---|---|---|---|---|
| measured RT30 | 5.759 s | 2.972 s | 2.810 s | 2.638 s | 2.413 s | 2.282 s |
| ratio vs default | 1.992 | 1.002 | 1.001 | 1.000 | 1.000 | 1.000 |

The expected ratio for a +6 dB trim is `10^(6/20) = 1.995`; the measured
1.992 is within 0.2 % of it, and every other band stays within 0.2 % of its
default. The same test fails if any other band moves by more than 5 %.

### Placed decay EQ

Same method. `decay_band1 = +6 dB` with a **low shelf at 4 kHz, Q 0.707**:

| band | 125 Hz | 1 kHz | 2 kHz | 4 kHz | 8 kHz | 16 kHz |
|---|---|---|---|---|---|---|
| measured RT30 | 5.972 s | 5.739 s | 5.237 s | 3.645 s | 2.522 s | 2.331 s |
| ratio vs default | 2.066 | 1.934 | 1.865 | 1.381 | 1.045 | 1.021 |

The 1 kHz breakpoint is below the corner and measures 1.934× its default (the
trim predicts 1.995, −3.1 %); the 16 kHz breakpoint is above the corner and
measures 1.021× (+2.1 %). The below/above contrast, `(1 kHz / 16 kHz)` relative
to the default, is **1.893** against the predicted 1.995 (−5.1 %). The test
asserts 15 %.

`decay_band3 = +6 dB` with a **bell at 2 kHz, Q 2.0**:

| band | 2 kHz | 4 kHz |
|---|---|---|
| ratio vs default | 1.881 | 1.067 |

The bell moves the 2 kHz breakpoint by 1.881× (predicted 1.995, −5.7 %) and the
4 kHz breakpoint by only 1.067× (+6.7 %). Both placed-EQ tests fail on the
previous device, where a trim moved one breakpoint and the placement controls
did not exist.

Post EQ: a `post_eq3_gain = +6 dB` bell at 1 kHz measures **+6.00 dB** of wet
gain on a 1 kHz sine, and the default is transparent. A low cut at 1 kHz
measures below −20 dB at 100 Hz. Tests:
`post_eq_gain_applies_at_its_frequency`, `post_eq_shapes_are_parameterised`,
`post_eq_defaults_are_transparent`.

## Bit-identity

- `defaults_keep_the_frozen_render` pins the default render digest
  (`0x9fc7cf190ebd7637`, character 0).
- `post_eq_defaults_are_transparent` sets all 24 post-EQ parameters explicitly
  to their defaults and compares the render sample for sample with the
  untouched default.
- `decay_bands_are_six_independent_parameters` sets every trim to 0 dB and
  checks the targets come back unchanged.
- `decay_eq_parameters_are_independent` checks the default frequency, Q and
  shape of every band and their clamping.
- `decay_eq_shape_none_matches_the_plain_trim` sets a placement and a `None`
  shape, then compares the six RT30 targets exactly with the bare trim.
- Digest pinning is deliberately avoided for character > 0, which is
  per-instance random by design.

## Tests

`crates/oc-dsp/src/reverb_fdn.rs`:

- `brightness_tilt_matches_the_measured_t60_table`
- `brightness_extension_raises_the_top_octaves`
- `measured_brightness_law_is_frozen_below_75`
- `tank_damping_cutoff_opens_above_75`
- `long_hall_dark_brightness_measurement`
- `long_hall_dark_preset_tail_gains_energy_at_full_brightness`
- `decay_bands_are_six_independent_parameters`
- `decay_band_trim_moves_only_that_band`
- `decay_eq_parameters_are_independent`
- `decay_eq_shape_none_matches_the_plain_trim`
- `decay_eq_low_shelf_lengthens_the_tail_below_its_corner`
- `decay_eq_bell_places_the_trim_at_its_own_frequency`
- `post_eq_defaults_are_transparent`
- `post_eq_gain_applies_at_its_frequency`
- `post_eq_shapes_are_parameterised`
- `defaults_keep_the_frozen_render`
- `character_zero_is_bit_reproducible`, `reset_restores_the_character_zero_render`
  and the existing space/brightness/distance/ER tests.

`crates/oc-engine/src/dsp/mod.rs`:

- `reverb_fdn_controls_reach_the_dsp_in_declared_units` — the new dB, Hz,
  Q and shape values arrive in the declared units.
- `checked_builtin_factory_control_matrix` — the factory forwards the decay-EQ
  placement controls.

## Known approximations

Short-Space (< 37.5 %) modal spread still makes RT30 up to about 2.3× long; the
low-Brightness 8/16 kHz tilt is shallower than the target, and at full
brightness the measured 16 kHz RT30 is still shorter than the 1 kHz target
because the six-band fit cannot isolate the top octave from its neighbours; the
tank input damping and ER/late levels are calibrated. The decay EQ is a placement of the
existing breakpoints, not new topology: the shape response is sampled at the six
fixed breakpoints, and the post EQ remains a six-biquad wet-path EQ.
