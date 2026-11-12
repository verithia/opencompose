# Vocal chain

A vocal is the one source that gets a dedicated chain. This document fixes the
**designed order** and the rationale for it. The laws of the devices that shipped
live with their measurements in `docs/vocal-comp.md`, `docs/vocal-tone.md` and
`docs/vocal-fx.md`; this file links to them instead of restating formulas that
would drift.

Two rules up front:

- Every device here is a **law**, not a preset. The law is the character; the
  knobs are positions on it.
- Crest factor is the thing a vocal chain must not eat. A compander shortens it,
  a leveler does not. That is why the leveler comes first.

## Order

```
tuner -> leveler -> clean-up EQ (HPF) -> plosive -> breath split -> opto
      -> de-esser -> passive EQ -> iron -> resonance suppressor -> doubler
      -> exciter (parallel send) -> space (ducked_send + delay)
```

Every stage in the diagram ships: `tuner`, `leveler`, `opto`, `fet`, `iron`,
`passive_eq`, `doubler`, `exciter`, `ducked_send`, `resonance`, `plosive` and
`breath_split`, plus the de-esser and the plate/room sends. The diagram is the
order the devices are meant to be used in, not a required serial chain.

The order is not arbitrary:

- The tuner sees the raw take; correcting after compression makes the note
  transitions jump because the gain moved under the shifter.
- The leveler is pre-compressor. Riding after compression means the compressor's
  threshold decides the tone and the rider only moves the result.
- The clean-up EQ is a 30–40 Hz high-pass: rumble into a transformer becomes
  intermodulation, not bass.
- The plosive is after the clean-up EQ and before the breath split and opto: the
  low thump is removed before the compressor can pump on it, and the device is
  zero latency so it costs nothing in the monitoring path.
- The breath split is after the clean-up EQ and before the opto: the compressor
  should not pump on breaths, and the split needs the full-band take before the
  transformer and saturation stages add harmonics.
- The opto is the leveling stage. It compresses the phrase, not the syllable, so
  it stays after the HPF and before the de-esser.
- De-esser before the passive EQ: a shelf boost at 10 kHz would otherwise be
  de-essed back down.
- Iron last among the tone stages, because saturation produces harmonics that
  the resonance suppressor would otherwise chase.
- Doubler and space are parallel in spirit but serial in the chain; the doubler
  is mono-safe by construction (see below).
- The exciter is a parallel send, not a serial stage: it returns only the
  synthesized air band, so tapping it after the iron keeps the dry vocal intact.
- The ducked send is the space return itself; it reduces the reverb the track
  already sends, so it belongs on the bus and not in the channel strip.

## Devices

### Leveler (`leveler`)

Control-rate gain riding, no peak detector and no attack/release.

The law as shipped — K-weighted short-term loudness, a rate-limited gain and a
gate on the 10 ms **block** level rather than on the slow loudness, so the first
few hundred milliseconds of a phrase gap are not lifted — is in
`docs/vocal-comp.md`. What matters here is the shape: control rate 10 ms,
interpolated to the sample rate.
- Zero latency: it is a gain, not a delay.
- The gate is not optional. Without it the rider lifts breaths and room tone
  between phrases, which is the one audible failure mode of riding.
- The rate limit **is** the character: faster sounds like compression, slower
  cannot catch a verse.
- Expected result on a 60 s take with a 20 dB range: verse and chorus within
  ±1 dB of target on 30 s RMS, while the per-syllable peak-to-RMS ratio moves
  less than 1 dB.

### Opto compressor (`opto`)

A shunt light-dependent resistor in a divider. One control (peak reduction) sets
the drive into the cell; there is no ratio knob because the ratio is a property
of the cell.

```
R(E)  = R0 * (E/E0)^(-gamma)
GR_dB = 20*log10(1 + K*10^(gamma*L/20)),  K = Rs/R0,  L = 20*log10(E/E0)
local ratio = 1/(1 - gamma*sigma),  sigma = K*10^(gamma L/20)/(1 + K*10^(gamma L/20))
```

Properties that define the feel:

- GR = 6.02 dB where `K*10^(gamma*L/20) = 1`; that point is the "knee" and it does
  not move with `gamma`.
- Knee width is `38.2/gamma` dB. At `gamma = 0.7` that is ~55 dB — the whole
  useful control range is the knee, which is why the unit has no threshold feel.
- Asymptotic ratio `1/(1-gamma)`; at 50 % engagement `1/(1-gamma/2)`. With
  `gamma = 0.7` that is 1.54:1 in normal use and 3.3:1 only when working hard.
- Two-state release: a 60 ms state and a 0.5–5 s state charged by a 150 ms
  coupler while the fast state is active, mixed with `lambda = 0.3..0.5`. A
  transient gets a clean fast recovery; a sustained note gets the long bloom.
- Attack is the cell rise (~10 ms) and is not adjustable. Steady state adds no
  distortion: the cell is a resistor, not a nonlinearity.
- Peak reduction is modelled as **sidechain drive**, not as a threshold.

Constants to measure, not copy: `gamma` (the one number that decides the whole
feel), `K`, `lambda`, the release positions.

Listening target: one phrase with 8–10 dB peaks at 10 dB GR — the last word must
bloom back over 1–2 s, and on a sustained vowel the processed take must be
indistinguishable from the unprocessed one at matched RMS.

### FET compressor (`fet`)

The one device here with a feedback topology.

```
y = (sqrt(1 + 4*beta*x) - 1) / (2*beta)      # linear feedback loop, exact
```

A linear feedback loop asymptotes to exactly 2:1 regardless of `beta`. Ratios
above 2:1 can therefore only come from the FET's superlinear conductance — which
is why "the ratio knob is a bias change" is literally true, and why the
all-buttons position (smallest quiescent bias, largest `beta`, near pinch-off)
raises the ratio and the distortion with the same element. Both come out of one
model or the model is wrong.

The shipped channel law is `g = beta*((b + e + kappa*|v|)^q - b^q)` with `q = 6`,
so the loop ceiling is 7:1 and the bias moves both the knee and the measured
slope. The four bias positions measure 1.86:1 to 4.81:1 at -6 dBFS, and THD rises
from 2.79 % to 10.14 % with them; disabling the instantaneous channel term drops
THD below 0.2 %, which is the proof that the distortion is not an added
saturator. There is no clipping ceiling: the conductance is smooth and monotone.
Law, constants and the measured tables: `docs/vocal-fet.md`.

### De-esser (`de_esser`)

One device, six choices: absolute or program-relative threshold, wide-band or
split-band gain, detector before or after the split, an Auto or 5–9 kHz
sibilance band, a fixed band or a per-take estimate, and a continuous or
segment-gated reduction. The shipped defaults are absolute, wide, sidechain,
Auto, no estimation and no segment gate, so old projects render unchanged. The
split-band law, the relative-threshold window, the estimator and segment
detector thresholds and the measured tables are in
[`deesser-modes.md`](deesser-modes.md).

The two placements differ in one line each: the sidechain detector reads the
band-limited program before the split, so the applied gain never feeds back into
detection; the in-band detector reads the band after the split, so it responds to
the signal it attenuates (with Wide Band, the body itself triggers reduction).

### Iron stage (`iron`)

Asymmetric saturation plus the transformer it sits behind. The existing
saturator is symmetric and stays symmetric; this is a different device.

The shipped shaper is `s = (tanh(k*(x+b)) - tanh(k*b)) / k` — note the `/k`, which
is what makes zero drive an exact bypass — followed by the flux-driven core and a
DC block on the generated offset. The full law and its measured harmonic table are
in `docs/vocal-tone.md`; the design intent here is only the ordering: bias sets
the even order, level sets the odd order.

Transformer: flux is the integral of voltage, so core saturation gives

```
H3/H1 ∝ (V / (2*pi*f*Phi_s))^2     # +2 dB/dB of level, -12 dB/oct of frequency
```

That relation is the whole point: a 100 Hz note thickens and a 5 kHz note does
not, and no static waveshaper reproduces it. Band limit is a second-order
high-pass at `f_L` (`Q` 0.7–1.2, above 0.707 for the low bump) and a second-order
low-pass at `f_H` (`Q` 0.5–0.6, no peaking).

Reuse the saturator's 4x FIR oversampler. Do not add a second oversampler: two
anti-alias paths means two latency numbers that must stay in step.

At zero drive the device must null below −90 dBFS.

### Passive EQ (`passive_eq`)

One corner shared by a boost and a cut, and the asymmetry is the point.

```
low boost  = bell at f0,  +G at f0, 0 dB at DC and HF, Q ~ 1.0
low cut    = 2nd-order shelf down, -C at DC, ~-C/2 at f0, Q ~ 0.7
both       = +G - C/2 at f0, -C at DC
```

So boost 5 dB and cut 5 dB at 100 Hz do **not** cancel: the result is about
+2.5 dB at 100 Hz and ≈−C at DC. A shelf-vs-shelf pair cancels almost exactly and
produces none of this. Measured at 5/5 with the shipped device: +2.50 dB at
100 Hz, −3.11 dB at 50 Hz, −4.44 dB at 30 Hz, −4.77 dB at 20 Hz.

High band: boost is a bell (`f0` 3–16 kHz, `Q` 0.7 broad to 1.5–2 sharp), cut is a
shelf at 5/10/20 kHz. Tilt is one first-order shelf with the centring term

```
H = 10^(-G/40) * (1 + s/wz) / (1 + s/wp)
wz = w_pivot * 10^(-G/40),  wp = w_pivot * 10^(+G/40)
```

Without the `10^(-G/40)` factor a tilt spans 0…+G instead of ±G/2.

Air is a high shelf at 8–12 kHz, +2–6 dB, after the 30–40 Hz high-pass.

Listening target: a 50 Hz tone drops ~3 dB while a 100 Hz tone rises ~2.5 dB at
5/5, and neither setting moves 1–4 kHz by more than 0.3 dB.

### Resonance suppressor (`resonance`)

Spectral dynamics: many narrow bands, each reduced only when it stands above the
local spectral envelope.

```
P[k]     = |X[k]|^2
env[k]   = smoothed P over a ~1/3-octave window          # the "expected" spectrum
prom[k]  = 10*log10(P[k] / env[k])                       # prominence, dB
g[k]     = -clamp(prom[k] - threshold, 0, depth)         # per-band reduction
g[k]     = slope-limit(g[k])                             # neighbour diff cap
```

- Reduction curve is smoothed across frequency before it becomes a filter,
  otherwise the result is a vocoder.
- The gain curve is turned into a minimum-phase FIR through the cepstrum; the
  same engine will later serve a spectral dynamic EQ.
- Latency: 2048-point FFT with a 256-sample hop is 2304 samples (48 ms at
  48 kHz). A zero-latency variant needs a biquad bank driven by the same
  detection — that is the harder form and is not part of the first cut.
- Prefer the short form: see the latency ledger.
- Shipped as `resonance`. The analysis window, the leave-one-out envelope, the
  cepstral FIR and the measured depth/cost tables are in
  [`resonance.md`](resonance.md).

### Doubler (`doubler`)

Two to four voices of short modulated delay, detuned by delay-time modulation
rather than by resampling. Law, constants and measurements: `docs/vocal-fx.md`.

```
delay_i = 8..30 ms + depth_i * LFO_i(t),  depth 1..3 ms, rate 0.2..0.5 Hz
detune  = +-5..20 cents (optional, on top of the delay modulation)
focus   = high-pass at 200..400 Hz before the taps     # keeps the low end mono
```

Mono compatibility is mandatory: sum the wet path to mono in a test and require
no more than 1 dB change in the 100–300 Hz band.

### Tuner (`tuner`)

Shipped as `tuner`. Detection is the hard part, not the shifter.

```
1. YIN at 16 kHz on a decimated copy, 40 ms window (24 ms low-latency)
2. median filter over the period track; octave-jump repair
3. note state machine: onset / steady / release, with a 60 cent hysteresis band
4. PSOLA resynthesis at the original rate (preserves formants by construction)
5. amount + retune: a one-pole on the correction, 0..1 amount
```

- 1920 samples (40 ms) of reported latency in full-range mode, 960 samples
  (20 ms) in low-latency mode; `amount = 0` is a bit-exact bypass that reports
  0. PDC aligns the track, but it is still 20–45 ms of monitoring delay.
- Scale snap is a note-level decision, not a sample-level one: the target is
  recomputed only on onset or when the pitch leaves the 60 cent band.
- Breathy tones must not produce an octave-up period, and hard-tune steps must
  not click without a spectral stage. Both are measured in
  [`tuner.md`](tuner.md); the detector error table, the vibrato depth at half
  amount and the 1.012x step ratio are there.

### Exciter (`exciter`)

Air is synthesized, not boosted: harmonic generation of 10–16 kHz from 5–8 kHz.
Law, constants and measurements: `docs/vocal-fx.md`.

- Even-order laws are alias-safe at the source band (2nd harmonic of 8 kHz is
  16 kHz); odd-order laws need 4x oversampling. Both laws share the existing FIR
  and its 16 samples.
- Follow with a high-pass at ~9 kHz and a band limit, or it reads as fizz.
- The device returns only the synthesized band, so it is a parallel send fed
  after the iron stage, not a serial insert.

### Space (`ducked_send`, `delay`)

- Plate/room sends already exist; the vocal-specific part is the duck: the send
  return is reduced by the dry vocal envelope, so the reverb never fights the
  lyric. `ducked_send` is the device; it sits on the return bus after the
  existing reverb. Law, constants and measurements: `docs/vocal-fx.md`.
- Wet-only width: widen the return, keep the dry centred.
- Pre-delay 20–40 ms keeps the onset clean.
- The duck depth and release must be slow enough not to pump on syllables.

### Breath handling

Split the take into a voice path and a breath path with complementary filters,
process them separately, and **sum back to the original** when nothing is
reduced. A gate cannot do this without chopping the breath; a split can. This is
an offline-capable device with a real-time control path.

Shipped as `breath_split`: a linear-phase low-pass and its exact complement, a
30 ms/10 ms periodicity detector weighted by low/high band energy, and a
one-pole gain transition. At unity the split sums to the delayed input exactly;
the engaged latency is 128 samples (2.7 ms) and the default is a bit-exact
bypass. Law, detector, measured tables and limits:
[`breath-split.md`](breath-split.md).

### Plosive (`plosive`)

The transient case: a 30 ms broadband burst with most of its energy below
120 Hz. The device detects it and takes the low band down without touching the
vowel that follows.

```
low   = 24 dB/oct low-pass at `frequency` (default 120 Hz)
high  = 12 dB/oct high-pass at 400 Hz
fast  = 1/15 ms power envelope; slow = 40/250 ms power envelope
fire  = rise(fast/slow) >= threshold && low excess share >= 0.5
hold  = 20 ms, then a 1.5 ms attack / `release` one-pole on the shelf depth
shelf = RBJ low shelf at `frequency`, S = 1, gain = -12 dB * amount * engagement
```

- The **excess share** is the discriminator: a vowel onset also rises fast, but
  its excess energy is in the harmonics (measured 0.26 at 110 Hz, 0.04 at
  220 Hz); a plosive's excess is in the low band (0.99). The 400 Hz band is
  what makes the rise visible over a sustained note.
- The shelf is retargeted once per millisecond and its coefficients are
  interpolated across the block, so the action is block-size invariant and has
  no coefficient step.
- Zero latency: a minimum-phase biquad, no lookahead, no split FIR. `amount = 0`
  is a bit-exact bypass.
- Measured at amount 100 on the S8 synthetic: < 120 Hz **−7.94 dB**, vowel body
  500 Hz–2 kHz **−0.000 dB**, 110 Hz sung note f0 **0.000 cents**, added
  per-sample step 0.0064, `process` allocations 0. Thresholds, band shape and
  the full table: [`plosive.md`](plosive.md).

### Comping and take alignment (offline)

Aligning takes needs cross-correlation or energy-based onset matching plus a
time-stretch to close the gap. Both halves exist: clips stretch offline through
the derived-media cache (WSOLA or the phase vocoder,
`docs/audio-clip-stretch.md`), and `ocp clip align` analyses each take against a
reference and writes the affine correction to the same `offset`/`stretch`
fields. Polarity and the seam are built too: `ocp clip align` reports the
normalised correlation over the aligned overlap, `--invert` writes the clip's
`invert:` flag when a take is out of phase, and `crossfade:` gives each clip an
equal-power head and tail so a comp seam does not bump. Use the GUI's Take
lane to audition takes and choose the comp. See [Comping](comping.md).

## Latency ledger

| Device | Latency | Why |
|---|---|---|
| leveler | 0 | gain only |
| opto / passive EQ | 0 | nonlinearity and minimum-phase filters only |
| fet | 16 samples (0.33 ms) | the shared 4x FIR oversampler, same path as `iron` |
| iron | 16 samples (0.33 ms) | the shared 4x FIR oversampler; reported to PDC |
| doubler | 0 | wet delay is not reported latency |
| de-esser (existing) | 0 ms by default | the lookahead defaults to 0; the split FIR is 512 samples (10.7 ms), the lookahead tops out at 15 ms, and the analog modes add no delay |
| exciter | 16 samples (0.33 ms) | the shared 4x FIR oversampler, same number as `iron` |
| ducked send | 0 | the 20–40 ms wet pre-delay is not reported latency |
| breath split | 128 samples (2.67 ms at 48 kHz) when engaged; 0 bypassed | linear-phase complementary split; the delay is the FIR group delay, not lookahead |
| plosive | 0 | minimum-phase low shelf; no lookahead, no split |
| resonance | 2304 samples (48 ms at 48 kHz) | 2048 FFT + 256 hop; a bypass reports 0 |
| tuner | 1920 samples (40 ms at 48 kHz) full range, 960 (20 ms) low latency | PSOLA delay line; a bypass reports 0 |

Track inserts are aligned by PDC inside the graph, so unequal track latencies do
not smear the mix. The offline renderer also trims the master chain's own latency
by rendering the extra frames and shifting the result back. What is untrimmed is
the **live monitor path**: during playback the audible result runs ahead of the
ruler by the chain's total latency, which is ~0.3 ms today and would become
visible with a lookahead device. Keep a single vocal chain under ~50 ms total,
and do not put the tuner and the spectral suppressor on the same track without
deciding which one the latency budget belongs to.

## Deliberately out of scope

- AI denoise and dereverb, source separation: cost without a law we can state.
- Linear-phase spectral dynamics: needs a linear-phase engine and doubles the
  latency; revisit when the spectral engine is shared.
- A graph-mode tuning editor: a full pitch-editing UI, not a device.

## Listening targets

Each device gets one regression target, all mono-summed and level-matched:

1. leveler — 20 dB range take lands within 2 dB verse-to-chorus, breath levels
   unchanged, steady tone moves ±0.1 dB.
2. opto — longest note blooms back over 1–2 s; steady vowel nulls at matched RMS.
3. fet — ratio above 2:1 without the distortion rising faster than the ratio.
   Measured 1.86:1 to 4.81:1 and 2.79 % to 10.14 % THD at -6 dBFS; the
   distortion is the channel's instantaneous term, not a saturator.
4. iron — 100 Hz sine gains 2–4 % H3, 2 kHz sine stays under 0.5 %.
5. passive EQ — the 5/5 corner behaviour above.
6. resonance — a 3 kHz ring is reduced without the neighbouring 2.5 kHz moving.
7. doubler — mono sum within 1 dB below 300 Hz.
8. exciter — air above 9 kHz is present, nothing below 5 kHz, alias below −60 dBc,
   and zero amount is a true null.
9. ducked send — the return drops by the depth within the attack, returns within
   1.5× the release, and a steady dry tone does not modulate it.
10. tuner — a scale run stays within 5 cents and breathy notes do not jump an
    octave. Measured worst 0.233 cents and zero octave-up frames at 6 dB SNR;
    a 6 Hz ±25 cent vibrato falls from 25 to 18.5 cents at amount 0.5.
11. breath split — a 4–8 kHz breath drops by the requested amount, a vowel is
    untouched, and the split nulls at unity.
12. plosive — an S8 burst loses at least 6 dB below 120 Hz, the vowel body stays
    within 0.3 dB, and a plosive over a sung note keeps its f0. Measured
    −7.94 dB, −0.000 dB and 0.000 cents; a vowel with no plosive is
    bit-identical.
