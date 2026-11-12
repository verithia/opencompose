# Vocal compression devices

Two dynamics devices share the vocal chain: the leveler rides the take before
the compressor, and the opto compresses the phrase. Their laws, constants and
measured behaviour live here; the chain order and the listening targets are in
`docs/vocal-chain.md`. Both are gain-only devices: zero latency, no delay line,
no oversampling.

- `crates/oc-dsp/src/leveler.rs` — control-rate loudness rider.
- `crates/oc-dsp/src/opto.rs` — shunt-cell optical compressor.
- `crates/oc-dsp/src/kweight.rs` — the BS.1770 K-weighting pair shared with the
  dynamic EQ's auto-gain.

All measurements below are 48 kHz, stereo, from the ignored harnesses:

```
cargo test -p oc-dsp measurements_for_docs -- --ignored --nocapture
```

## Leveler

The rider moves a gain, not a peak. It measures short-term loudness, compares it
with a target, and slews the gain toward the difference at a bounded rate. There
is deliberately no peak detector and no attack/release pair, because those are
what shorten the crest factor.

### Law

```
L(t)   = 10*log10(one-pole(K(x)^2, tau = window))       # K-weighted RMS, dBFS
G_des  = target - L(t)
G(t)   = G(t-1) + clamp(G_des - G(t-1), -r*dt, +r*dt)   # r = 2..6 dB/s
G      = clamp(G, -range, +range)                        # range = 12..24 dB
if block_level < gate for > 300 ms: hold G, then G -> 0 with tau = gate_release
```

- Control rate is 10 ms. The gain is interpolated to the sample rate over each
  control period, so a block boundary does not step the gain.
- `L` is the K-weighted RMS level in dBFS; the K-weighting is part of the
  measurement, not a display offset.
- `block_level` is the mean K-weighted power of the current 10 ms control block,
  not the long `L`. The rider's one-pole is intentionally slow (0.5–3 s); using
  it for the gate would keep the gate from opening until the smoothed level had
  decayed, and the rider would lift the first few hundred milliseconds of every
  phrase gap. The block detector sees silence immediately, the rider keeps its
  long window, and the 300 ms hold still comes from the law.
- The gate is not optional: without it the rider lifts breaths and room tone
  between phrases. The release position is exposed because it decides how much
  of a long breath is still riding the previous phrase.

### K-weighting

The analytic BS.1770 pair (high shelf at 1682 Hz, high pass at 38.1 Hz) is a
shared module, so the leveler and the dynamic EQ use the same coefficients. A plain squared envelope is not enough for a rider: a bass-heavy
take would read louder than it sounds, the rider would cut it, and the vocal
would lose low end relative to a thin take. K-weighting makes the target a
perceptual level rather than a flat one. The opto does **not** use it — its cell
responds to the rectified program, and that difference is part of why the two
devices behave differently.

### Measured

Rate limit, 500 ms window, a 20 dB step, peak 1 s movement:

| speed knob | measured |
|---|---|
| 2 dB/s | 2.00 dB / s |
| 4 dB/s | 4.00 dB / s |
| 6 dB/s | 6.00 dB / s |

Gate, 5 s of program then silence, target −20 dB, gate −45 dB, release 1 s:
the rider held **8.38 dB** of boost; it was 1 dB down **0.41 s** after the
program stopped (300 ms hold plus the start of the drift), and below 1 dB of
gain **2.41 s** after.

Steady tone (1 kHz, 12 s, 200 ms windows after 10 s): **0.000 dB** peak-to-peak;
the test bound is 0.1 dB.

A 60 s take with a 20 dB verse-to-chorus step (30 s each, target −20 dB):
verse **−20.65 dBFS**, chorus **−20.65 dBFS**, difference 0.00 dB; the test bound
is 2 dB. The absolute offset is the K-weighting response at 1 kHz.

Crest factor on a 2 s program with 20 dB spikes: per-100 ms worst change
**0.081 dB**; global crest **20.07 → 20.92 dB**. The rider does not reduce it.
Without peak detection the spike passes at whatever gain the program has
reached; only the program level moves the gain.

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `target` | dB | −40–0 | −20 | K-weighted short-term level to aim for |
| `speed` | dB/s | 2–6 | 3 | gain slew limit; the character |
| `range` | dB | 12–24 | 18 | symmetric gain clamp |
| `window` | ms | 500–3000 | 1000 | loudness integration time constant |
| `gate` | dB | −70–−20 | −45 | block level below which the gate holds |
| `gate_release` | ms | 1000–2000 | 1500 | return-to-unity time constant after the hold |

`gain_reduction_db()` reports the current attenuation and
`max_gain_reduction_db()` its peak. A boost is not a reduction and reads 0 dB.
`latency_samples()` and `tail_samples()` are 0.

### Baking the decision into a project

`ocp level` runs the same law offline over a track's dry signal and writes the
decision into the track's `gain_automation` lane, then removes the device. The
lane stores linear gain factors on the project clock's `ppq=4800` grid, so the
control points follow the tempo map. The dry pass renders the track with the
leveler removed but with every effect before it, so the leveler must be the
track's last effect. The bake reads the device's six parameters from the
project; it does not take a separate setting.

One point lands on every 10 ms control boundary and thinning drops the points a
linear gain interpolation reproduces within 0.05 dB. A 22 s take falling 10 dB
(4 dB/s, 800 ms window) kept 16 of 2200 points and reproduced the device to a
worst per-100 ms RMS difference of **0.048 dB** (mean 0.028 dB, 220 windows).

Limits:

- the lane encodes one law; changing `target`, `speed`, `window`, `gate`,
  `range` or `gate_release` needs a re-bake;
- the bake replaces the track's existing gain clips; a track whose phrases
  carry hairpins is refused, because the hairpin expands into its own gain clip
  and would overlap the baked lane;
- the device ramps its gain linearly in amplitude, so the lane interpolates the
  same way and the 0.05 dB thinning bound is measured against that
  interpolation;
- bake at the sample rate you will render with. The law's time constants are
  rate-independent, but the K-weighting coefficients are not.

## Opto

A shunt light-dependent resistor in a divider. The cell resistance falls with
the program level; the series resistor turns that into gain reduction. There is
one drive control and no ratio control, because the ratio is a property of the
cell.

### Law

```
R(E)   = R0 * (E/E0)^(-gamma)
GR_dB  = 20*log10(1 + K*10^(gamma*L/20)),  K = Rs/R0,  L = 20*log10(E/E0)
sigma  = K*10^(gamma*L/20) / (1 + K*10^(gamma*L/20))        # engagement
ratio  = 1 / (1 - gamma*sigma)                               # local compression
```

`E` is the peak of the current 10 ms control block. A periodic program has the
same peak in every block that contains a full period, so the cell current is
constant in steady state and the device cannot add waveform distortion. `L`
already includes the drive: peak reduction is sidechain drive, so it shifts `L`
and the cell's exponent compresses the shift (0.7 dB of extra reduction for each
dB of drive, asymptotically).

Our constants:

| constant | value | why |
|---|---|---|
| `gamma` | 0.7 | the one number that sets the feel: 1.54:1 in normal use, 3.3:1 only when working hard, and a 55 dB knee |
| `K` | 2.5 | puts the 6 dB knee at −5.7 dBFS with no drive, so normal program sits below the hard part of the knee instead of always being pinned |
| `lambda` | 0.4 | slow-state share of the release; high enough for the bloom, low enough that the fast state is audible on a transient |
| cell rise | 10 ms | attack, not adjustable |
| fast release | 60 ms | lets go first |
| coupler | 150 ms | charges the slow state while the fast state is active |
| detector block | 10 ms | the program level the cell sees |

Consequences, all pinned by tests:

- **Knee point** `L = -20*log10(K)/gamma = -5.68 dBFS` at zero drive. It does not
  move with `gamma`.
- **Knee width** `38.2/gamma = 54.53 dB`, defined between 10 % and 90 %
  engagement (`sigma = K*10^(gamma L/20)/(1 + ...)`). The whole useful control
  range is the knee; that is why there is no threshold feel.
- **Local ratio** `1/(1 - gamma*sigma)`: **1.538:1** at 50 % engagement and
  **3.333:1** asymptotically. Measured from the law as
  `1/(1 - dGR/dL)` around the half-engagement point: 1.538.

### Static curve, measured through the DSP

A 1 kHz tone, 3 s, gain reduction from the settled output/input ratio. Drive is
the `peak_reduction` parameter.

| input peak | GR at drive 0 | GR at drive 6 |
|---|---|---|
| −50 dBFS | 0.38 dB | 0.60 dB |
| −35 dBFS | 1.21 dB | 1.88 dB |
| −25 dBFS | 2.50 dB | 3.75 dB |
| −15 dBFS | 4.84 dB | 6.89 dB |
| −5 dBFS | 8.53 dB | 11.39 dB |

The drive-0 row is the law with `K = 2.5`; the drive-6 row is the same law at
`L + 6 dB`.

### Two-state release, measured

A 2 s burst at −6 dBFS peak with drive 6, then silence. The table is the mix
`(1 - lambda)*fast + lambda*slow` after the burst:

| slow release | 60 ms | 150 ms | 1 s | fitted tail tau |
|---|---|---|---|---|
| 500 ms | 6.12 dB | 3.97 dB | 0.65 dB | 0.50 s |
| 1500 ms | 6.29 dB | 4.50 dB | 2.30 dB | 1.50 s |
| 5000 ms | 6.35 dB | 4.71 dB | 3.59 dB | 5.00 s |

The fast state supplies the first drop; the slow state is what is left at 1 s.
The fitted tail is the slow release position to within 1 %.

### Attack

The cell rise is 10 ms. With a 10 ms control block the mixed output is **70 %**
of the final reduction 50 ms after a step. It is not a knob.

### Steady state

A periodic vowel (100 Hz plus 20 harmonics), processed for 3 s and compared with
the same input scaled to matched RMS over the last second: residual
**−158.9 dBFS**. This is the no-distortion identity — the cell is a resistor, so
a settled gain multiplies the waveform and adds nothing. Test bound: −80 dBFS.

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `peak_reduction` | dB | 0–20 | 6 | sidechain drive into the cell |
| `release` | ms | 500–5000 | 1500 | slow release state |
| `makeup` | dB | −12–12 | 0 | output gain after the cell |

There is no `ratio`, `threshold` or `attack` id, and tests assert their absence.
`gain_reduction_db()` reports the current reduction, `max_gain_reduction_db()`
its peak. `latency_samples()` and `tail_samples()` are 0.

## Listening targets

From `docs/vocal-chain.md`, with the measured result:

1. **Leveler** — a 20 dB take lands within 2 dB verse-to-chorus, breaths are not
   lifted, a steady tone moves ±0.1 dB. Measured 0.00 dB, 0.20 dB of residual
   boost after 4 s of silence, 0.000 dB.
2. **Opto** — the longest note blooms back over 1–2 s; a steady vowel nulls at
   matched RMS. Measured tail tau 1.50 s at the default release; residual
   −158.9 dBFS.
3. **Crest** — the pair must not eat it. The leveler leaves the per-syllable
   crest within 0.081 dB. The opto is a gain, so it cannot shorten a waveform's
   crest within a block; it only changes the level of the block.

## Latency

| device | latency | why |
|---|---|---|
| leveler | 0 | gain only |
| opto | 0 | gain only |

No lookahead, no oversampling and no reported tail. The chain latency comes from
the de-esser and the spectral suppressor, not from these two.

## Real-time safety

Both devices allocate in `new` only. `process` touches scalar state and two
biquads per channel; it takes no locks, starts no threads and allocates nothing.
The control tick runs every 10 ms of sample time and is independent of the host
block size; tests process the same signal at 97-frame and 480-frame blocks and
require the same output.

## Tests

Leveler:

- `rate_limit_moves_the_gain_at_most_speed_db_per_second`
- `gate_holds_for_300ms_then_returns_to_unity`
- `a_steady_tone_moves_less_than_a_tenth_of_a_db`
- `crest_factor_is_preserved`
- `a_20db_program_lands_within_two_db`
- `breaths_are_not_lifted_after_the_hold`
- `zero_latency_impulse_passes_at_its_own_frame`
- `block_size_does_not_change_the_output`

Opto:

- `static_curve_matches_the_law` (five points)
- `knee_is_about_55_db_wide_and_the_ratios_are_properties_of_the_law`
- `release_has_a_fast_state_and_a_slow_tail`
- `the_slow_release_position_sets_the_tail`
- `a_steady_vowel_leaves_no_residual`
- `attack_is_the_cell_rise_and_is_not_adjustable`
- `makeup_is_an_exact_output_gain`
- `block_size_does_not_change_the_output`
- `reset_returns_the_cell_to_unity`
- `zero_latency_impulse_passes_at_its_own_frame`

Schema tests pin the exposed surface: the leveler has six parameters, the opto
three, and neither exposes a ratio, threshold or attack.

## Deliberately out of scope

- A ratio knob, a threshold knob and an attack knob. The laws do not have them;
  exposing one would turn the device into a generic compressor.
- External sidechain for either device. The vocal chain uses the track itself.
- Oversampling. Neither device is nonlinear in the audio path.
- Auto-makeup. Makeup is an explicit output trim; guessing it would fight the
  leveler in front.
- Program-dependent detector EQ, stereo linking controls and a mix knob. One
  vocal, one path; a mix knob on a rider hides a broken range setting.
