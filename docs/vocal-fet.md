# FET compressor

A feedback compressor whose gain element is a superlinear channel conductance.
The ratio is not a knob: the bias moves the operating point along the channel
law, and the ratio, the knee and the distortion all follow from that one state.
The chain order and the DESIGNED target live in `docs/vocal-chain.md`; this file
records the law, the constants and what the shipped device measures.

- `crates/oc-dsp/src/fet.rs` — the loop, the peak detector and the controls.

All measurements below are 48 kHz, stereo, from the ignored harness:

```
cargo test -p oc-dsp --lib fet::tests::measurements_for_docs -- --ignored --nocapture
```

## Law

The element is a shunt channel whose conductance is a power of its control
voltage `u`. The series resistor closes the loop, normalized to one:

```
u  = b + e + kappa * |v|                 # quiescent bias, envelope, terminal voltage
g  = beta * (u^q - b^q)                  # channel conductance, zero at zero signal
x  = v + g * v                           # input drive, output v
```

`v` is the output; `e` is the peak envelope of `|v|` (the sidechain); `b` is the
quiescent bias; `beta` is the loop gain; `q` is the channel exponent and `kappa`
is the share of the instantaneous terminal voltage that adds to the control.
The subtraction of `b^q` makes the conductance zero at zero signal, so silence
is silent at every bias.

The linear-channel case is `q = 1`: `g = beta*(e + kappa*|v|)`. At the peak of a
steady sine `e = v`, so

```
x = v + beta*(1 + kappa)*v^2
v = (sqrt(1 + 4*beta'*x) - 1) / (2*beta')      beta' = beta*(1 + kappa)
```

with `beta' = beta*(1+kappa)`. That is the exact 2:1 asymptote: a linear
conductance can never exceed it, at any `beta`, `b` or `kappa`. Ratios above 2:1
exist only when `q > 1`; the loop then asymptotes to `q + 1`. The shipped
exponent is `q = 6`, so the ceiling is **7:1**, reached only at high level. The
device uses the odd extension `v(-x) = -v(x)`, so both polarities compress
symmetrically and the element adds no DC.

### Bias: the ratio position

One state, `bias` in 0..100 %, maps to the quiescent bias and the loop gain:

```
b    = 0.02 + 1.98*(1 - position)^2
beta = 0.03*(2 / b)^3
```

| position | b | beta | meaning |
|---|---|---|---|
| 25 % | 1.134 | 0.165 | most quiescent bias, lowest ratio |
| 50 % | 0.515 | 1.757 | |
| 75 % | 0.144 | 80.8 | |
| 100 % | 0.020 | 30 000 | all buttons: nearest pinch-off, largest beta |

At 100 % the element is nearest pinch-off: the smallest quiescent bias, the
largest loop gain, the highest ratio and the most distortion. It is the same
law, not a mode. Threshold does not exist: the bias sets where the channel sits,
and the signal itself does the rest.

### Constants

| constant | value | why |
|---|---|---|
| `q` | 6 | the channel exponent; the loop ceiling is 7:1. With `q = 4` the measured ratio stayed under 3.5:1 at -6 dBFS; `q = 8` raised THD without raising the measured ratio much. |
| `kappa` | 0.25 | instantaneous channel share. With `kappa = 0` the envelope-only loop is a pure gain and THD falls below 0.2 %, so the distortion is the channel's, not a saturator's. |
| `beta` map | `0.03*(2/b)^3` | loop gain grows as the bias approaches pinch-off; the exponent puts the all-buttons position at about 10 dB of gain reduction at -6 dBFS. |
| detector | peak of `|v|`, one pole | attack and release set the two coefficients; there is no separate RMS window. |
| oversampler | shared 4x FIR | 16 samples of reported latency; the same path as `iron` and the exciter. |

## Measured

### Static curve

1 kHz tone, 3 s, drive 0 dB, output 0 dB. Gain reduction is read from the
settled output peak. The law column is the same table solved from `x = v + g*v`
with `e = v`; every measured point is within **0.05 dB** of it.

| input peak | 25 % | 50 % | 75 % | 100 % |
|---|---|---|---|---|
| -30 dBFS | 0.61 dB | 0.15 dB | 0.02 dB | 0.01 dB |
| -24 dBFS | 1.19 dB | 0.35 dB | 0.08 dB | 0.21 dB |
| -18 dBFS | 2.23 dB | 0.88 dB | 0.42 dB | 2.03 dB |
| -12 dBFS | 3.92 dB | 2.24 dB | 2.11 dB | 5.88 dB |
| -6 dBFS | 6.35 dB | 4.82 dB | 5.55 dB | 10.47 dB |
| 0 dBFS | 9.43 dB | 8.35 dB | 9.83 dB | 15.31 dB |

The bias positions are not four parallel curves with the same knee; the bias
moves the knee as well as the slope, which is what makes the control read as a
bias rather than a ratio.

### Ratio and 2:1 proof

Local ratio at -6 dBFS, measured through the device from two 1 dB steps and
compared with the law at the same operating point:

| position | measured ratio | law |
|---|---|---|
| 25 % | 1.862 | 1.864 |
| 50 % | 2.093 | 2.097 |
| 75 % | 2.976 | 2.980 |
| 100 % | 4.808 | 4.800 |

The ratio is above 2:1 from 50 % up. The linear-element test sweeps `beta` and
`b` and requires every local ratio to stay at or below 2.000001; the exact
closed form above is also compared with the solver to 1e-6 relative. The ceiling
is `q + 1 = 7`, so no measured point can exceed it.

### THD versus bias at a fixed level

1 kHz at -6 dBFS (0.5 peak), 3 s, harmonics 2-10 relative to the fundamental:

| position | THD |
|---|---|
| 25 % | 2.79 % |
| 50 % | 3.35 % |
| 75 % | 5.59 % |
| 100 % | 10.14 % |

The distortion rises monotonically with the ratio position and is the channel's
instantaneous term. Setting `kappa = 0` keeps the same envelope loop and drops
THD to **0.036 %** at 25 %, **0.042 %** at 50 %, **0.077 %** at 75 % and
**0.169 %** at 100 %, so the harmonics are not added after the compressor and
are not an artifact of the gain movement.

### Ballistics

The detector is a peak follower with independent attack and release
coefficients:

```
e += (|v| - e) * (1 - exp(-1 / (tau * fs)))     tau = attack if |v| > e else release
```

Detector time constants from a DC step (the 5-95 % rise divided by 2.944) and a
release fit after the step:

| set | measured attack | measured release |
|---|---|---|
| 0.2 ms / 50 ms | 0.198 ms | 50.0 ms |
| 2 ms / 200 ms | 1.925 ms | 200.0 ms |
| 10 ms / 1 s | 9.511 ms | 1000.0 ms |

The attack is fast by design; the loop's envelope feedback adds lag when the
bias is high, which is why the all-buttons position grabs a transient harder
than its detector alone would suggest.

### Null and stability

- Silence in, silence out: the largest output over 0.5 s is **0.0** at every
  bias, attack and release extreme. The `b^q` subtraction makes the channel
  conductance exactly zero at zero signal.
- An impulse through a linear-channel setting appears at the oversampler
  latency and decays to the FIR floor; there is no feedback state that can
  sustain it.
- A control-space sweep (three biases x two attacks x two releases x two
  drives) feeds 0.2 s of loud tone then 1.5 s of silence; the last 0.5 s stays
  below -80 dBFS. The loop equation is monotone, so there is no solution branch
  that can oscillate.

## Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `drive` | dB | 0-24 | 0 | pre-gain into the loop; shifts the whole static curve |
| `attack` | ms | 0.05-20 | 0.3 | peak-detector attack; the FET's fast attack |
| `release` | ms | 20-2000 | 120 | peak-detector release |
| `output` | dB | -12-12 | 0 | exact output trim |
| `bias` | % | 0-100 | 25 | quiescent bias and loop gain; 100 % is all buttons |

There is no `ratio`, `threshold` or mix id. `gain_reduction_db()` reports the
current frame's peak reduction, `max_gain_reduction_db()` its maximum.
`latency_samples()` is 16 and `tail_samples()` is 32.

## Latency

| device | latency | why |
|---|---|---|
| fet | 16 samples (0.33 ms) | the shared 4x FIR oversampler; reported to PDC |

The distortion is generated at 4x and the shared FIR removes the aliases. The
`v` path is wet-only: there is no dry blend to delay separately.

## Listening target

From `docs/vocal-chain.md`: **ratio above 2:1 without the distortion rising
faster than the ratio.**

Measured at -6 dBFS: the ratio runs 1.86:1 at 25 % to 4.81:1 at 100 %, and THD
runs 2.79 % to 10.14 %. The all-buttons position is the most compressed and the
most distorted; at -6 dBFS the ratio rises 2.6x and THD 3.6x, so the distortion
is the part of the all-buttons character that outruns the ratio. It is bounded by
the channel's instantaneous term, not by a saturator after the compressor; at
`kappa = 0` the same loop is clean. The 25-75 % range is the everyday setting:
there the ratio rises 1.6x and THD 2.0x. The sound target: at 25 % the vocal
keeps its edge and only the peaks are held; at 100 % the channel adds the
hardening and the low-end thickening that the all-buttons position is for.

## Real-time safety

`Fet::new` builds fixed arrays only. `process` touches scalar state, the
envelope and the shared FIR rings; it takes no locks, starts no threads and
allocates nothing. The envelope ticks per base-rate frame, so the output does
not depend on the host block size.

## Tests

- `a_linear_element_cannot_exceed_two_to_one`
- `the_superlinear_channel_raises_the_ratio_toward_its_exponent`
- `static_curve_matches_the_law_at_four_bias_positions`
- `the_ratio_position_raises_the_ratio_and_the_distortion`
- `the_distortion_comes_from_the_instantaneous_channel_term`
- `attack_and_release_are_pinned_to_the_detector_time_constants`
- `zero_drive_is_a_silent_null`
- `no_self_oscillation_across_the_control_space`
- `block_size_does_not_change_the_output`
- `output_is_an_exact_gain`
- `latency_is_the_shared_oversampler`
- `reset_clears_the_envelope_and_the_loop`

Schema tests pin the five-parameter surface and assert that `ratio` and
`threshold` do not exist. The GUI panel test pins the same five controls and
the gain-reduction meter.

## Deliberately out of scope

- A ratio knob, a threshold knob and a mix knob. The bias is the ratio state;
  adding a ratio knob would put a second law on top of the channel.
- External sidechain. The vocal chain uses the track itself.
- A second oversampler or a second saturator. The distortion is the channel's;
  a saturator after the loop would make the THD-versus-bias table a lie.
- Program-dependent release and auto-makeup. The detector is a peak follower
  with two constants; makeup is an explicit output trim.
- A clipping ceiling. The conductance is smooth and monotone, so it cannot
  self-oscillate and does not need one.
