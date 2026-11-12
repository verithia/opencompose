# Vocal tone devices

Two tone stages close the shipped tone path after the compressor (the designed chain continues with the resonance suppressor, the doubler and the space send — see `docs/vocal-chain.md`): `iron` saturates
asymmetrically in front of a transformer core, and `passive_eq` couples a boost
and a cut on one low corner. The chain order and the listening targets are in
`docs/vocal-chain.md`; the laws, constants and measurements live here, in the
same form as `docs/vocal-comp.md`.

- `crates/oc-dsp/src/iron.rs` — asymmetric shaper, one-sided ceiling, DC block,
  flux-driven core and band limit.
- `crates/oc-dsp/src/passive_eq.rs` — coupled low band, high band, tilt and air.
- `crates/oc-dsp/src/oversampler.rs` — the 4x FIR oversampler shared with the
  saturator. There is one anti-alias path and one latency number, not two.

All measurements below are 48 kHz, stereo, from the ignored harnesses:

```
cargo test -p oc-dsp --lib measurements_for_docs -- --ignored --nocapture
```

## Iron

The stage is a shaper and the transformer behind it. The shaper makes the
harmonics; the transformer decides which of them a given note gets.

### Law

```
u  = x + b
s  = (tanh(k*u) - tanh(k*b)) / k          # DC-free, linear as k -> 0
s  = s - c*(s - tanh(s))  if s > 0        # one-sided ceiling, c = ceiling*drive gate
r  = dc_block_10Hz(s - x)                 # offset of the generated part only
u' = x + r                                # voltage into the core
Phi[n] = Phi[n-1]*(1 - 2*pi*f_leak/fs) + u'[n]/fs
a  = Phi * FLUX_BASE * k
y  = u' * (1 - tanh(a)^2)                 # sech^2 incremental core gain
out = x + band_limit(y - x)
```

Shaper, small signal:

```
H2/H1 ∝ k^2 * b * A       # +1 dB per dB of level; H2 doubles with the bias
H3/H1 ∝ k^2 * A^2 / 12    # +2 dB per dB of level; independent of the bias
```

The `/k` normalisation is what makes the drive control honest: at `k = 0` the
shaper is exactly `x`, so `drive 0` is a true bypass rather than a quiet
waveshaper. The one-sided ceiling is scaled by `1 - exp(-k)`, for the same
reason.

Transformer, small signal: the flux is the integral of the voltage, so

```
H3/H1 ∝ (V / (2*pi*f*Phi_s))^2    # +2 dB per dB of level, -12 dB per octave
```

That is the part a static waveshaper cannot do: the same input level gives 16x
more third harmonic at 100 Hz than at 400 Hz, and 400x more than at 2 kHz. The
low notes thicken; the high notes do not.

The band limit filters the **generated colour**, not the dry signal: `f_L` is a
second-order high-pass (Q 1.0) and `f_H` a second-order low-pass (Q 0.55) on
`y - x` before it is added back. Filtering the residual keeps the linear path
flat, which is what lets zero drive null exactly instead of nulling "except for
the filters". The DC block is a fixed 10 Hz first-order block on the shaper's
own offset; it does not high-pass the dry signal either.

### Constants

| constant | value | why |
|---|---|---|
| `FLUX_BASE` | 1485 | places a 100 Hz tone at -12 dBFS at about 3 % H3 with drive 4 dB |
| `k` | `10^(drive/20) - 1` | 0 dB drive is exactly linear |
| core gain | `1 - tanh(a)^2` | the incremental permeability of a saturating core; never negative |
| DC block | 10 Hz, first order | removes the bias offset without touching the audio band |
| flux leak | 2 Hz, one pole | bounds the integrator; 0.02 % gain error at 100 Hz |
| `f_L` | 20–120 Hz, Q 1.0 | the transformer's low limit, with the low bump above 0.707 |
| `f_H` | 8–20 kHz, Q 0.55 | the transformer's high limit, no peaking |

The core model is `1 - tanh(a)^2` rather than a hard limiter because a core does
not clip a voltage; its incremental inductance falls as the flux approaches
saturation. That keeps the model stable at any drive and makes the third-order
law exact in the small-signal limit.

### Measured

Drive 4 dB, 1 kHz, two biases and three levels. The shaper laws are read here;
the transformer contributes about 0.01 % at 1 kHz, below the third column's
resolution.

| level | bias 0.1 H2 | bias 0.1 H3 | bias 0.4 H2 | bias 0.4 H3 |
|---|---|---|---|---|
| -24 dBFS | 0.108 % | 0.009 % | 0.423 % | 0.008 % |
| -12 dBFS | 0.425 % | 0.147 % | 1.671 % | 0.124 % |
| -6 dBFS | 0.825 % | 0.578 % | 3.248 % | 0.489 % |

- H2 moves **12.0 dB per 12 dB** of level (+1 dB/dB) and **6.0 dB per doubling
  of bias** (the table's bias 0.1 → 0.4 step is 4x, i.e. +11.9 dB); H3 moves
  **24 dB per 12 dB** (+2 dB/dB) and stays within 1.6 dB when the bias is
  quadrupled. The bias control changes the second/third balance by
  **13.4 dB** at -12 dBFS.
- The level series is the measurement of `H2/H1 ∝ k^2 b A` and
  `H3/H1 ∝ k^2 A^2/12`; the bias series is the measurement of the `b` term.

Drive 4 dB, -12 dBFS, the acceptance pair:

| frequency | H2 | H3 |
|---|---|---|
| 100 Hz | 0.000 % | 2.707 % |
| 200 Hz | 0.000 % | 0.558 % |
| 400 Hz | 0.000 % | 0.012 % |
| 1 kHz | 0.000 % | 0.147 % |
| 2 kHz | 0.000 % | 0.167 % |

The 100 Hz tone gains **2.7 %** third harmonic and the 2 kHz tone stays at
**0.17 %**, under the 0.5 % bound. The 400 Hz dip is the transformer's negative
third harmonic cancelling the shaper's positive one; the transformer table
below shows the two separately.

Transformer alone, shaper held linear, -12 dBFS:

| frequency | H3 | fall from 100 Hz |
|---|---|---|
| 50 Hz | 10.75 % | +11.4 dB |
| 100 Hz | 2.910 % | 0 dB |
| 200 Hz | 0.742 % | -11.9 dB |
| 400 Hz | 0.186 % | -23.9 dB |
| 2 kHz | 0.0074 % | -51.9 dB |

That is **-12 dB per octave** between 100 and 400 Hz and **-52 dB over
4.3 octaves** to 2 kHz, the flux law.

Zero-drive null: the largest sample difference against the input delayed by the
oversampler latency is **0.0** (exact) at drive 0 with bias 0.4 and ceiling
100 %, so the bypass is not level-dependent.

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `drive` | dB | 0–24 | 4 | shaper and core drive; 0 dB bypasses the colour |
| `bias` | none | -0.5–0.5 | 0 | asymmetry before the shaper; H2 ∝ bias |
| `ceiling` | % | 0–100 | 0 | one-sided soft ceiling on the positive half, scaled by drive |
| `low_freq` | Hz | 20–120 | 25 | second-order high-pass on the colour, Q 1.0 |
| `high_freq` | Hz | 8–20 kHz | 18 kHz | second-order low-pass on the colour, Q 0.55 |
| `output` | dB | -12–12 | 0 | output trim |

There is no mix knob and no separate transformer control: zero drive is the
bypass, and the drive is the one colour control. `output` is an exact gain, and
`latency_samples()` reports the oversampler's 16 samples.

## Passive EQ

One low corner feeds a boost bell and a cut shelf. The asymmetry is the point:
the bell returns to 0 dB at DC, the shelf does not, so the pair does not cancel.

### Law

```
low boost  = bell at f0, +G at f0, 0 dB at DC and HF, Q = 1.0
low cut    = 2nd-order shelf down at f0, -C at DC, ~ -C/2 at f0, Q = 0.7
both       = +G - C/2 at f0, -C at DC
```

High band: the boost is a bell at 3–16 kHz with `Q` 0.7 (broad) to 2 (narrow);
the cut is a high shelf at 5–20 kHz. Tilt is one first-order shelf with the
centring term:

```
H = 10^(-G/40) * (1 + s/wz) / (1 + s/wp)
wz = w_pivot * 10^(-G/40),  wp = w_pivot * 10^(+G/40),  w_pivot = 1 kHz
```

Without the `10^(-G/40)` factor a tilt spans 0…+G; with it the span is ±G/2 and
the response crosses 0 dB at the pivot. Air is a high shelf at 8–12 kHz applied
last, after the low cut and the tilt, so no earlier stage can undo it.

### Constants

| constant | value | why |
|---|---|---|
| low boost Q | 1.0 | a bell that still reads as a corner, not a narrow honk |
| low cut Q | 0.7 | the shelf reaches most of `-C` by 30 Hz |
| high cut Q | 0.707 | shelf, not a resonant filter |
| air Q | 0.707 | shelf |
| tilt pivot | 1 kHz | the middle of a vocal; ±G/2 either side |
| tilt order | first | one pole, minimum phase, no ringing |

### Measured

Low 5 dB boost + 5 dB cut at 100 Hz:

| frequency | response |
|---|---|
| 20 Hz | -4.769 dB |
| 30 Hz | -4.439 dB |
| 50 Hz | -3.111 dB |
| 100 Hz | +2.500 dB |
| 200 Hz | +1.232 dB |
| 1 kHz | +0.052 dB |
| 2 kHz | +0.013 dB |
| 4 kHz | +0.003 dB |

The corner gives **+2.50 dB at 100 Hz** and **-4.44 dB at 30 Hz**: 30 Hz is
about a dB short of the full `-C` because the boost bell still contributes
+0.51 dB there and the 0.7 shelf is not fully settled. A shelf-vs-shelf pair at
the same corner cancels to 0.0 dB at every frequency; the bell/shelf pair does
not, which is the device. The midrange moves by at most **0.052 dB** between
1 and 4 kHz.

Tilt +6 dB:

| frequency | response |
|---|---|
| 20 Hz | -2.997 dB |
| 100 Hz | -2.936 dB |
| 1 kHz | +0.008 dB |
| 10 kHz | +2.971 dB |
| 20 kHz | +3.017 dB |

The span is **-3/+3 dB**, not 0/+6; a tilt of -6 dB mirrors it exactly.

High band and air:

| setting | 1 kHz | 4 kHz | corner | top |
|---|---|---|---|---|
| high bell +5 dB at 8 kHz, Q 1.4 | 0.035 dB | 0.782 dB | +5.000 dB at 8 kHz | — |
| high cut -5 dB at 10 kHz | 0.000 dB | -0.077 dB | -2.500 dB at 10 kHz | -4.991 dB at 20 kHz |
| air +6 dB at 10 kHz | 0.000 dB | 0.095 dB | +3.000 dB at 10 kHz | +5.988 dB at 20 kHz |

Neither the high cut nor the air shelf moves 1–4 kHz by more than 0.1 dB at
these settings. The drawn curve is the sum of the six serial stages; the GUI
response module mirrors the DSP coefficients and the frontend tests pin the same
table (`crates/oc-gui/frontend/src/__tests__/devices/passiveEq.test.ts`).

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `low_freq` | Hz | 30–300 | 100 | the corner shared by boost and cut |
| `low_boost` | dB | 0–10 | 0 | bell at `low_freq`, Q 1.0 |
| `low_cut` | dB | 0–10 | 0 | low shelf down at `low_freq`, Q 0.7 |
| `high_freq` | Hz | 3–16 kHz | 8 kHz | high bell centre |
| `high_boost` | dB | 0–10 | 0 | high bell gain |
| `high_q` | none | 0.7–2.0 | 1.0 | high bell width |
| `high_cut_freq` | Hz | 5–20 kHz | 10 kHz | high shelf corner |
| `high_cut` | dB | 0–10 | 0 | high shelf cut |
| `tilt` | dB | -6–6 | 0 | first-order tilt at 1 kHz, span ±G/2 |
| `air_freq` | Hz | 8–12 kHz | 10 kHz | air shelf corner |
| `air` | dB | 0–6 | 0 | air shelf gain, applied last |

The default setting is bit-identical to the input (every gain is 0 dB and every
filter reduces to the identity), so the device has no hidden colour. It is
linear and zero-latency; there is no oversampling.

## Latency

| device | latency | why |
|---|---|---|
| iron | 16 samples (0.33 ms at 48 kHz) | the shared 4x FIR oversampler; reported to PDC |
| passive EQ | 0 | minimum-phase biquads only |

The chain ledger in `docs/vocal-chain.md` records the iron's 16 samples: the
shared FIR oversampler's group delay is real, and PDC must see it even though it
is under half a millisecond.

## Listening targets

From `docs/vocal-chain.md`, with the measured result:

1. **Iron** — a 100 Hz sine gains 2–4 % H3, a 2 kHz sine stays under 0.5 %.
   Measured 2.71 % and 0.17 % at -12 dBFS, drive 4 dB.
2. **Iron bias** — the bias moves the second/third balance at a fixed drive.
   Measured 13.4 dB of balance change between bias 0.1 and 0.4, with H2
   doubling for a doubled bias.
3. **Passive EQ** — 5/5 at 100 Hz is +2.5 dB at the corner and about -5 dB at
   30 Hz, and neither setting moves 1–4 kHz by more than 0.3 dB. Measured
   +2.50 / -4.44 dB and 0.052 dB.

## Real-time safety

Iron allocates in `new` only. `process` touches scalar state, two biquads, a
one-pole DC block, two flux states and the shared FIR rings; it takes no locks,
starts no threads and allocates nothing. The passive EQ is six biquads and no
state beyond their two history words per channel. Both are invariant to the
host block size.

## Tests

Iron:

- `zero_drive_is_a_true_null_at_the_oversampler_latency`
- `harmonic_laws_follow_level_and_bias` (two biases, four levels)
- `a_100_hz_tone_thickens_and_a_2_khz_tone_does_not`
- `the_transformer_flux_law_falls_twelve_db_per_octave`
- `the_ceiling_is_one_sided_and_scales_with_drive`
- `the_output_trim_is_an_exact_gain_and_the_cut_filters_work`
- `block_size_does_not_change_the_output`

Passive EQ:

- `a_shelf_pair_cancels_but_the_coupled_corner_does_not`
- `the_low_corner_boost_and_cut_do_not_cancel`
- `neither_corner_moves_the_one_to_four_khz_band`
- `a_six_db_tilt_spans_minus_three_to_plus_three`
- `the_high_band_is_a_bell_and_the_cut_is_a_shelf`
- `air_is_a_high_shelf_after_the_rest_of_the_chain`
- `the_default_setting_is_an_exact_identity`
- `block_size_does_not_change_the_output`

Schema tests pin the surface: six iron parameters and eleven passive-EQ
parameters. The GUI tests pin the same response table the DSP measures.

## Deliberately out of scope

- A second oversampler. The saturator and iron share one FIR and one latency.
- A mix knob on the iron. Zero drive is the bypass; a mix knob would hide the
  fact that the shaper is normalised to be linear at zero.
- A transformer amount separate from the drive. A transformer that is always on
  is exactly the hidden colour the device is meant not to have.
- A ratio/threshold-style control on the iron; it is a colour stage, not a
  compressor.
- A shelf-vs-shelf option on the passive EQ. The coupling is the device; a pair
  of shelves is a different EQ.
- A high-pass in the passive EQ. The chain's clean-up high-pass is a separate
  device before the compressor, and duplicating it here would make the chain
  order lie.
- Program-dependent Q, dynamic bands and auto-gain on the passive EQ. They are
  not part of the law and would make the drawn curve stop matching the audio.
