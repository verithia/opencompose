# Voice filter character

OpenSynth has four voice-filter models. Each is our own implementation of a
well-known *principle* — a resonant state-variable core, a ladder, or a dirty
ladder — not a reproduction of another instrument's coefficients. This file
records the laws we chose and what they measure, so the next change can tell
deliberate character from an accident.

The goal is a filter that sounds right: a resonance knob that stays musical
over its whole travel, a passband whose loudness does not move when resonance
does, slopes that match their names, and tails that always decay.

## Model map

| Model | Ids | Response |
|---|---|---|
| `Svf` | `L12`/`H12`/`B12`/`N12`/`P12`, morph ids | two-pole state variable, 12 dB/oct |
| `Svf1` | `L6`/`H6` | first-order tap, 6 dB/oct, no resonance |
| `Svf3` | `*18` | two-pole core + first-order section, 18 dB/oct |
| `Svf4` | `*24` | two-pole core + Butterworth two-pole section, 24 dB/oct |
| `Moog6/12/18/24` | `MgL6/12/18/24` | four-pole ladder, tapped after 1/2/3/4 stages |
| `Ladder` | `LadderMg`/`LadderAcid`/`LadderEMS` | four-pole TPT ladder, linear feedback |
| `DirtyLadder` | `DirtyMg` | same ladder with saturating stages |

## Damping and the resonance ceiling

The state-variable damping (reciprocal Q) is

```
g       = sin(pi * fc / fs)
damping = min(2 - 2 * res^0.25, 1 / g - g)      clamped to [0, 2]
```

The first term runs from Q = 0.5 at resonance 0 towards zero. The second is the
prewarped ceiling: as the corner approaches Nyquist it asks for less damping,
which is what keeps the resonant gain from running away at the top of the
cutoff range. `g` is a sine, not a tangent, so the corner follows the reference
law even at 10 kHz instead of drifting 20 % high.

The law reaches zero at resonance 1.0. A linear loop with zero damping has its
poles on the unit circle: it would ring forever after any excitation. The
applied damping therefore has a floor of `1/2`, a **constant ceiling on the
linear resonant gain of +6 dB**. That floor is the resonance normalisation, not
part of the measured law: a two-pole peak adds `1 / damping` at the corner, and
the instrument's headroom contract lets one stage consume at most +6 dB. The
floor acts on the damping, so the passband does not move with it; the peak just
stops growing above resonance 0.316. See `docs/synth-levels.md`.

Above the floor the response is broad rather than sharp, so the highest sample
sits below the nominal corner: at fc = 425 Hz the peak is at 398 Hz for `L12`
and 370-382 Hz for the cascades. That is a property of the capped response, not
a mistuned corner.

## Passband trim

The two-pole response grows a resonant shelf below the corner as damping
falls, so raising resonance also raises the passband. The state-variable family
takes a frequency-independent input trim

```
gain = 1 / (1 + 0.20 * res^0.25)
```

which is 0 dB at resonance 0 and about -1.5 dB at the top. It normalises the
passband to the resonance-free setting; it does not flatten the shelf shape,
because one scalar cannot. Measured over 20 Hz to 0.7 fc, the level stays
within 0.9 dB of the resonance-free level at every tested cutoff. The
first-order tap has no resonance and is not trimmed. Ladders are not trimmed:
their passband loss is part of their character.

## Cascades

The 18 and 24 dB responses cascade the resonant core with one fixed stage at
the same corner: a first-order section for three poles, a maximally flat
(Q 0.707) two-pole section for four. A fixed stage has unity gain at DC and
-3 dB at the corner, so the passband level and the corner position survive the
cascade while the slope steepens. Resonance stays in the core, so the extra
stage does not square the peak: the three slopes peak within 2.7 dB of each
other, and putting resonance into both sections would make 24 dB far hotter
than 12 dB.

Measured at fc = 425 Hz, low-pass (one-octave fit above the corner):

| id | slope, res 0 | slope, res 0.7 | peak @0.7 | peak @0.98 | peak Hz |
|---|---|---|---|---|---|
| `L12` | -11.4 | -12.9 | +4.84 | +4.72 | 398 |
| `L18` | -17.2 | -18.6 | +2.19 | +2.07 | 382 |
| `L24` | -23.6 | -25.0 | +2.62 | +2.51 | 370 |

The fit reads 1-1.5 dB/oct shallow at resonance 0 because the transition is
still bending over that octave; the asymptotes are 18 and 24 dB/oct.

## Moog ladder

Four one-pole stages with a fixed zero at `z = -0.29`, a one-sub-step-delayed
feedback loop and two sub-steps per output sample. Each stage is

```
y = G*x + k*x_prev + (1 - G - k)*y_prev
G = f2 / (1 + f2)          f2 = tan(pi * fc / fs)
k = 0.29 * G
```

The zero is what makes `k` a fixed fraction of the stage gain: the stage
transfer is `G(1 + 0.29 z^-1) / (1 - (1 - 1.29 G) z^-1)`, so its DC gain is one.
The loop subtracts `4 * Reso * y4` from the saturated input. Both sub-steps use
the same coefficients; the output is the halfband decimator
`[0.25, 0.50, 0.25]` over the previous, first and second tap, which sums to one.

Three choices shape the sound:

- **The loop margin is 0.875 of the four-times threshold.** Four times
  resonance is the oscillation threshold of the linear loop; at exactly 4.0 a
  full-resonance tail rings instead of decaying. The same margin as the TPT
  ladder keeps every cutoff strictly damped.
- **The input is scaled by `1 + feedback`** to cancel the loop's DC loss. Raw
  feedback drops the passband by up to 14 dB at full resonance; that is a mix
  problem, not a character we want. Resonance changes the peak, not the level.
- **The output tap is not compensated.** A tap after one stage has less loop
  gain than a tap after four, so `MgL6` peaks hotter than `MgL24` in the linear
  response. The tap is the only difference between the ids.

Measured at fc = 425 Hz:

| id | slope | peak @0.7 | peak @0.98 | peak Hz @0.98 |
|---|---|---|---|---|
| `MgL6` | -5.5 | +18.3 | +29.4 | 523 |
| `MgL12` | -11.0 | +15.8 | +26.6 | 523 |
| `MgL18` | -16.5 | +13.3 | +23.7 | 523 |
| `MgL24` | -21.9 | +10.8 | +20.9 | 523 |

The 24 dB tap reads 21.9 dB/oct over the first octave because the transition is
still bending there; the asymptote is 24. The low-frequency corner is 243 Hz at
res 0, i.e. 0.57 of the nominal cutoff — a four-pole one-pole cascade has its
-3 dB well below the stage corner. The peak rises about 5 dB from 0.7 to 0.98
and sits near the corner. These are **linear small-signal** peaks: the voice
path runs the Moog input through the saturator, so the rendered peak is bounded
well below them (Moog12 rises +6.4 dB from resonance 0 to 1, Moog24 +5.3 dB, at
a fixed input; see `docs/synth-levels.md`). The 0.875 margin is what keeps the
linear loop decaying instead of self-oscillating.

## Ladder feedback margin and the dirty stage

A `poles`-stage one-pole ladder oscillates when the loop gain reaches
`2^(poles/2)`. We use 0.875 of that, which is about +18 dB of resonant gain at
every pole count: the loop is strictly decaying, so a tail drains in
milliseconds instead of ringing at the edge of stability. The dirty model uses
the same loop with gain-normalised saturators; the saturation shapes the
signal, it does not add loop gain. All of 0..1 is stable, and a zero state
stays exactly silent.

Measured at fc = 425 Hz, small signal:

| id | passband @0.98 | peak @0.98 |
|---|---|---|
| `LadderMg` | -12.1 | +8.1 |
| `DirtyMg` | -12.1 | +8.1 |

Both are the same at small signal by design; they diverge under drive, which
is what the dirty stage is for.

## Measuring

`crates/oc-synth-core/src/synth/filter_response.rs` renders an impulse through the
same dispatch the voice uses and evaluates the transfer function at five
points per octave from 20 Hz to the top of the cutoff range. It reports the
-3 dB point relative to the flat part of the passband, the peak and its
frequency (refined around the grid maximum so narrow peaks are not missed),
the slope fitted over one octave above the corner, and the mean passband level
over 20 Hz to 0.7 fc.

The unit tests assert the properties, not the numbers: passband level stays
flat across resonance, the peak is cutoff-independent and under its ceiling,
cascades steepen by 6 dB/oct per pole, Moog taps are slope-correct and
peak-matched, and ladders decay from a zero state and from an impulse.
