# Voice modulation: LFO shapes, mod-matrix curve/slew, envelope attack

This document defines the modulation laws of the OpenSynth voice engine, the
patch parameters that control them, and values measured from the engine's test
suite. Everything here runs on the audio thread without allocation, locks or
extra threads. Per-LFO state is a fixed array inside `OpenSynth`; the three shape
arrays live behind an `Arc` in the patch, so loading or editing a patch is the
only place that allocates (the JSON reader builds a temporary vector).

Source: `crates/oc-synth-core/src/synth.rs` (`LfoShapeType`, `LfoShape`,
`LfoSettings`, `ModRoute`, `update_lfo_runtime`, `curve_shape_value`,
`path_shape_value`, `chaos_step`, `shape_mod_value`, `slew_step`,
`EnvelopeStageSteps`).

## 1. LFO shapes

All LFOs (LFO 1..LFO 10) share one curve evaluator. LFO 1 keeps the flat
`lfo.*` patch fields for ABI stability; LFO 2..LFO 10 use `lfo2.*`..`lfo10.*`
and are stored in `extra_lfos`. Every LFO owns a shape; the patch stores it as
three fixed `[f32; 480]` arrays (`x`, `y`, `curve`) plus the active breakpoint
count, so the audio path reads a preallocated buffer and never allocates.

The phase `p` is a 64-bit accumulator over one cycle, so `p` is in `[0, 1)` and
the rate is independent of the shape. A free-running LFO never applies its
start phase: `lfo.phase` is ignored unless the LFO is set to retrigger, in
which case a note-on restarts the accumulator at `phase / 360`.

### Shape types

| Type | Value | Evaluation |
| --- | --- | --- |
| `Curve` | 0 | Breakpoint segments with the per-segment bend (the default) |
| `Path` | 1 | Cyclic Catmull-Rom through the same breakpoints; ignores bend |
| `ChaosA` | 2 | Our logistic-plus-echo attractor |
| `ChaosB` | 3 | Our folded-tent attractor |
| `RandomHold` | 4 | One random value held per cycle |

The shape source is **unipolar 0..1**. A mod slot's `bipolar` flag re-centres it
on zero (`2v - 1`); the direct `lfo.pitch` vibrato uses the centred value.
`RandomHold` and the chaotic maps never leave 0..1: the logistic map is bounded
for growth rates up to 4, the echo and the fold are convex maps of bounded
states, and the output is clamped as a last guard.

The two chaotic generators are ours, not textbook Lorenz/Rossler systems:

- `ChaosA` runs `a' = 3.99 a (1-a)`, then `b' = 0.86 b + 0.14 a'`, and returns
  `0.65 a' + 0.35 b'`. The growth rate is chaotic above 3.57 and still maps
  `[0,1]` into itself; the echo adds a slow second time scale.
- `ChaosB` folds `m = 0.65 a + 0.35 b` through the tent map
  `1 - |2m - 1|`, then follows with `b' = 0.5 b + 0.5 a'` and returns the fold.
  The fold maps `[0,1]` onto itself; the unequal weights stop the two states
  from synchronising, which would collapse the map to a fixed point.

Both generators iterate `LFO_CHAOS_STEPS = 32` map steps per LFO cycle and
interpolate linearly between the two most recent values, so at high rates the
contour stays continuous instead of stepping once per sample. Their starting
states are fixed per LFO index, so renders are reproducible.

### Curve bend (per segment)

Each breakpoint stores the bend of the segment that starts at it:

```
u' = u ^ (4 ^ (2*bend - 1))          bend in [0, 1], 0.5 = straight
```

`bend = 0.25` rises early and flattens (convex); `bend = 0.75` stays low and
rises late (concave). Because the local position `u` is still monotonic
`0 -> 1`, the segment endpoints and the cycle rate are unchanged.

The retired whole-cycle phase skew (`lfo.bend`, -1..1) is still applied before
the shape lookup, so old documents keep their phase asymmetry.

### Legacy waveform aliases

Old documents name one of the retired waveforms and, for the drawn variants, a
shared 256-point curve. Both are folded into LFO 1's shape on load:

| Legacy name | Folds into |
| --- | --- |
| `Sine` | `Curve`, 480-point sampled sine |
| `Triangle` | `Curve`, the default triangle (bit-identical source) |
| `Saw` | `Curve`, a rising ramp |
| `Square` | `Curve`, two flat halves |
| `SampleAndHold` | `RandomHold` |
| `Curve` / `Smooth` / `Stepped` | `Curve`, the legacy 256-point curve |

`Smooth` and `Stepped` have no special shape law of their own (the `Smooth`
parameter covers smoothing); the data is kept so those patches still load and
move.

## 2. LFO parameters

| Parameter | Range | Unit | Default | Patch field |
| --- | --- | --- | --- | --- |
| `lfo.rate` | 0..100 | Hz | 5.2 | `lfo_rate` |
| `lfo.rate_x10` | 0/1 | bool | 0 | `lfo_rate_x10` |
| `lfo.shape` | 0..4 | enum | 0 (Curve) | `lfo_shape_type` |
| `lfo.shape.points` | 2..480 | count | 3 | `lfo_shape.points` |
| `lfo.shape.x` | array | 0..1 | `[0, 0.5, 1]` | `lfo_shape.x` |
| `lfo.shape.y` | array | 0..1 | `[0, 1, 0]` | `lfo_shape.y` |
| `lfo.shape.curve` | array | 0..1 | `[0.5, 0.5, 0.5]` | `lfo_shape.curve` |
| `lfo.phase` | 0..360 | degrees | 0 | `lfo_phase` |
| `lfo.smooth` | 0..1 | amount | 0 | `lfo_smooth` |
| `lfo.rise` | 0..5 | s | 0 | `lfo_rise` |
| `lfo.delay` | 0..5 | s | 0 | `lfo_delay` |
| `lfo.bend` | -1..1 | exponent | 0 | `lfo_bend` |
| `lfo.sync_div` | 0..42 | ladder index | 22 (1/4) | `lfo_sync_div` |
| `lfo.division` | 0..5 | retired selector | 2 | `lfo_division` |
| `lfo.retrigger` | 0/1 | bool | 0 | `lfo_retrigger` |
| `lfo.sync` | 0/1 | bool | 0 | `lfo_sync` |
| `lfo.pitch` | 0..200 | cents | 0 | `lfo_pitch_cents` |

`lfo2.*`..`lfo10.*` mirror every row except `lfo.pitch`. Array values are
`ParamValue::Array` in the DSL (`lfo.shape.x: [0, 0.5, 1]`); the document
importer and the patch document exporter both use that form.

`lfo.division` is the retired six-step selector: old documents keep exactly
their old synced rates. New documents use `lfo.sync_div`, an index into the
43-step ladder in beats per cycle (`LFO_SYNC_DIVISIONS`); entry 22 is one beat,
and the ladder covers 32 bars down to 1/512 note (three variants per octave).
`rate_x10` multiplies the free-running rate only.

## 3. Fade and Smooth

`Delay` and `Rise` are real seconds. The gain scales the shape around the 0.5
midpoint, so a starting LFO sits at neutral rather than at the bottom:

```
fade(t) = 0                                   t < delay
fade(t) = (t - delay) / rise                  delay <= t < delay + rise
fade(t) = 1                                   t >= delay + rise

out = 0.5 + (shape - 0.5) * fade
```

`t` restarts on note-on only when the LFO retriggers; a free LFO measures from
synth creation. `Smooth` is a one-pole on the shaped output:

```
tau   = smooth^2 * 0.5 s
alpha = 1                                  smooth = 0
alpha = 1 - exp(-1 / (tau * fs))           otherwise
y[n]  = y[n-1] + alpha * (out[n] - y[n-1])
```

The one-pole state starts at 0.5 and is reset on a retrigger.

## 4. Mod matrix curve and slew

Each of the 64 routes can shape its signal after the source, auxiliary scaling
and amount are combined:

### Curve

```
v' = sign(v) * |v| ^ (4 ^ curve)          curve in [-1, 1]
```

`curve = 0` is the identity. Negative values expand small signals (soft knee),
positive values compress them (hard knee). The exponent range is 0.25..4.0 and
the map is odd, so polarity and zero crossings are preserved.

### Slew (one-pole, separate rise and fall)

```
tau_dir = slew_up or slew_down (seconds, 0..0.5)
alpha   = 1                                  tau_dir = 0
alpha   = 1 - exp(-1 / (tau_dir * fs))       otherwise
s[n]    = s[n-1] + alpha * (v'[n] - s[n-1])
```

`alpha` is chosen from the sign of `v' - s`, so rising and falling edges can have
different times. The step is a convex combination, so the state is monotonic
toward the target and never overshoots. Slew state is per voice and per compiled
route, initialised to 0 at note-on; it is never allocated.

## 5. Envelope minimum attack

The amplitude envelope's attack length is floored at 1 ms:

```
A_effective = max(A, 0.001 s)
```

When the requested attack is below the floor (including 0), the attack phase is
a straight linear ramp of the floored length instead of the exponential curve;
this guarantees a silent first sample even when the curve parameter is extreme.
The table-driven attack for `A >= 1 ms` remains the same curve as before and is
monotonic by construction. The filter, Env 3 and Env 4 envelopes keep immediate
zero attacks because they are modulation sources, not the amplitude stage.

## 6. Parameter table (routes and legacy ids)

The LFO parameters are listed in section 2. These round-trip through
`SynthPatch::set_param` as well:

| Parameter | Range | Unit | Default | Patch field |
| --- | --- | --- | --- | --- |
| `lfo.curve.N` | -1..1 | legacy point | triangle | `lfo_curve[N]` |
| `lfo.waveform` | 0..7 | retired enum | (unset) | `legacy_lfo_waveform` |
| `lfoN.waveform` | 0..7 | retired enum | (unset) | `extra_lfos[N-2].legacy_waveform` |
| `mod.N.curve` | -1..1 | exponent | 0 | `mod_routes[N-1].curve` |
| `mod.N.slew_up` | 0..0.5 | s | 0 | `mod_routes[N-1].slew_up` |
| `mod.N.slew_down` | 0..0.5 | s | 0 | `mod_routes[N-1].slew_down` |

Notes:

- Host automation is normalised 0..1. `lfo.waveform` keeps its retired
  normalised map (0..1 -> 0..4); `lfo.shape` and `lfoN.shape` use `4u`.
  `lfo.sync_div` uses `u * 42`, `lfo.phase` uses `u * 360`, `lfo.rate` uses
  `0.01 * 10000^u` (0.01..100 Hz).
- `lfo.curve.N` writes the retired 256-point curve, which rebuilds LFO 1's
  shape; `lfo.waveform` selects a built-in shape and LFO 1's curve when the
  legacy drawn variant is named.
- `mod.N.curve`, `lfo.bend`, `lfoN.bend` and `lfo.curve.N` normalise from
  -1..1 as `2u - 1`. `lfo.rise`, `lfoN.rise`, `lfo.delay`, `lfoN.delay`
  normalise as `u^2 * 5`; `mod.N.slew_up`/`slew_down` as `u^2 * 0.5`.
- Route `curve`/`slew_up`/`slew_down` are omitted from JSON when zero, so
  default routes keep their original three-key shape.
- The retired fields are skipped when writing JSON; a document that carries
  them is folded into the shape on load.

## 7. Measured values

Measured by `synth::tests::modulation_law_measurements` and the shape tests with
the engine built from this tree (48 kHz).

### Default triangle and bend

The default shape is `x = [0, 0.5, 1]`, `y = [0, 1, 0]`, every bend 0.5. Its
value at phase 0.25 is exactly 0.5; the retired triangle source
(`1 - 2*|p - 0.5|`) matches it sample for sample after the f32 cast over 48,000
phases, and a legacy triangle patch renders bit-identically to the default
shape (checked over 2,048 samples with vibrato routed). An imported preset
whose default curve stores its straight bends as 0.49999997 is recognised and
canonicalised to this exact triangle.

A two-point ramp (`y = 0 -> 1`) with bend 0.25 evaluates to 0.500000 at phase
0.25 instead of 0.25, and with bend 0.75 to 0.062500; inside a bend of 0.5 the
value is 0.250000.

### Rate accuracy and x10

The phase accumulator's measured rate (`increment / 2^64 * fs`) at 48 kHz:

| Knob | Measured |
| --- | --- |
| 1 Hz | 1.000000 |
| 8 Hz | 8.000000 |
| 16 Hz | 16.000000 |
| 100 Hz | 100.000000 |
| 8 Hz with `rate_x10` | 80.000000 |

Errors are below 1e-6 Hz. The free knob stops at 100 Hz; `rate_x10` multiplies
only the free-running rate.

### Phase identity

A free-running LFO renders the same 2,048 samples at `phase = 0, 90, 180, 270`
degrees (exact equality). With retrigger enabled, a note-on at phase 90 degrees
starts a saw shape at 0.25 and phase 270 degrees starts it at 0.75.

### 43-step sync ladder

`LFO_SYNC_DIVISIONS` has 43 entries, strictly descending, with entry 22 equal to
one beat. At 120 BPM, entry 22 is 2 Hz and the retired selector value 4 (four
beats) is still 0.5 Hz.

### Chaotic generator bounds

Over 20,000 iterations each generator stays finite and inside `[0,1]`; no exact
period of 1..512 steps appears in the first 2,048 values. Two initial states
1e-12 apart diverge by more than 0.1 within 256 steps for both generators.

### Slew step response (one-pole, tau = 10 ms, 48 kHz)

`alpha = 0.002081165`; unit step from 0:

| Time | Value |
| --- | --- |
| 10 ms | 0.632121 |
| 50 ms | 0.993262 |
| 1 s | 1.000000 |

With `slew_up = 0.01 s` and `slew_down = 0.05 s`, after 10 ms the rising edge is
further from 0 than the falling edge is from 1 (asymmetric slew works as
intended). The state never overshoots, checked sample by sample.

### Smooth parameter (one-pole, amount 0.1)

The time constant is `0.1^2 * 0.5 s = 5 ms`, so at 48 kHz the coefficient is
`1 - exp(-1/240) = 0.004158`. A flat 1.0 shape started from the 0.5 midpoint
reaches 0.5 + 0.5*(1 - (1 - 0.004158)^512) > 0.9 after 512 samples, rising
monotonically.

### Rise / delay fade (delay 240 samples, rise 480 samples, top of a square)

| Sample | Source |
| --- | --- |
| 0 | 0.500000 |
| 240 | 0.500000 |
| 241 | 0.501042 |
| 719 | 1.000000 |
| 720 | 1.000000 |

### Mod curve at |v| = 0.5

| Curve | Value |
| --- | --- |
| -1 (soft) | 0.840896 |
| 0 | 0.500000 |
| +1 (hard) | 0.062500 |

### Attack ramp

Requested attack 0 ms becomes `0.001 s` = 48 samples at 48 kHz (44 samples at
44.1 kHz). First amplitude value is `1/48 = 0.020833`; the ramp is monotonic and
reaches 1.0 at sample 48. For comparison, the unfloored engine produced 1.0 on
the first sample.

## 8. Deliberate choices

- The shape and the Smooth parameter are independent: Smooth is a lag on the
  final unipolar source, at any shape type.
- Fade-in scales the source around the 0.5 midpoint, so a fading LFO starts at
  neutral for both unipolar and bipolar routes.
- The two chaotic generators are ours: logistic-plus-echo and folded-tent. They
  are bounded by construction rather than by trusting the ODE; the bounds are
  pinned by tests.
- The 43-step ladder keeps `lfo.division` as a separate retired selector, so old
  synced patches keep their exact rates.
- `lfo.curve.N` and the retired waveform names are compatibility inputs; new
  documents write the `lfo.shape.*` arrays.
- Only the amplitude envelope gets the attack floor; Env 2/3/4 keep immediate
  attacks so existing modulation timings are unchanged.
- Route curve and slew are not filled by the external preset importer; that
  mapper does not have verified scaling for them yet, so imported patches get
  the neutral defaults.
