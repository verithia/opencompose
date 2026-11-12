# Built-in devices

OpenCompose ships a mixer device library. The engine, the GUI device picker and
`ocp devices list` all read the same registry
(`crates/oc-device-schema/src/lib.rs`), so a device is available everywhere it is
listed.

Add a device in the GUI with `+` in the device rack, or in the DSL:

```ocp
effects:
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
```

Parameter values take their unit (`dB`, `%`, `Hz`, `ms`, `s`, `ct`) or a bare
number. A parameter name the device does not define is a render error. The
`sidechain=<name>` key feeds the detector from another channel on
`compression`, `dynamic_eq` and `ducked_send`. A track effect may key a track;
a bus or master effect may key a track or a bus. The key reads the source's
post-insert, pre-fader, pre-mute signal and never enters the audio output.
`bypass=true` on any entry takes that device out of the chain without editing
its controls; see [Effects](dsl.md#effects).

Fraction-valued controls use bare physical numbers: `compression: mix=0.4`
and `multiband: mix=0.4` mean 40% wet. Their GUI readouts show `40%`, while
native percentage controls such as reverb mix use `mix=40%`. OpenSynth's
fractional controls follow the same display rule, including bipolar amounts
and values above unity.

Static mixer effects also accept conventional typed percentages: `mix=40%`
means `0.4` for compression and multiband. Existing `mix=0.4%` therefore
continues to mean `0.004` (0.4% wet); it is not reinterpreted as 40% wet.
Use bare fractions for new edits so the stored number matches the catalog.

Percentage literals have historical context-specific behavior. Existing effect
automation on these fractional controls treats `0.4%` as physical `0.4`, and
OpenSynth static patch parameters also preserve their raw numeric percentage
value. Instrument automation has its own normalized percentage conversion.
These existing contexts are unchanged; use bare physical values when writing
fractional device or synth controls, and inspect the target's catalog.

## Which device when

| Device | DSL keyword | What it is | Reach for it when |
|---|---|---|---|
| Gain | `gain` | Fixed linear gain (`factor`). | You need a level trim with no other colour. |
| Parametric EQ | `eq` | Three-band EQ: low shelf, mid bell, high shelf. | Fast tone shaping with a fixed layout. |
| Passive EQ | `passive_eq` | Coupled bell/shelf low and high bands, tilt and air shelves. | You want a musical program EQ with coupled controls. |
| Dynamic EQ | `dynamic_eq` | Six bands plus low/high cuts, each optionally dynamic. | A frequency range only needs changing when it gets loud. |
| Compression | `compression` | Flexible compressor with peak/RMS detection and sidechain. | Control dynamics; glue a bus; duck against a kick. |
| Multiband Compander | `multiband` | Two to four bands with per-band compression and crossover control. | A single compressor would pump or dull one band. |
| De-esser | `de_esser` | Band-split or wide-band sibilance control. | Harsh `s` and `sh` on vocals or cymbals. |
| Leveler | `leveler` | Control-rate loudness riding with a gate. | Even out a performance without flattening transients. |
| Opto Compressor | `opto` | One-knob optical compressor. | Smooth, program-dependent leveling on vocals or bass. |
| FET Compressor | `fet` | Field-effect feedback compressor with four bias positions. | A vocal or bass that should colour more as it compresses. |
| Limiter | `limiter` | Lookahead peak limiter with true-peak detection. | Catch peaks on the master or a bus. |
| Gate | `gate` | Downward expander with threshold, hold and range. | Remove bleed or tighten a noisy source. |
| Saturator | `saturator` | Simple drive and mix. | Add weight or harmonics without changing level much. |
| Iron | `iron` | Transformer-style asymmetric saturation with a band limit. | Thicken lows and add harmonic density. |
| Distortion | `distortion` | Drive, tone and output. | Audible colour and edge. |
| Reverb | `reverb` | Algorithmic room with damping and filters. | A usable space with a short control set. |
| FDN Reverb | `reverb_fdn` | Long-form reverb: space, decay rate, brightness, character, distance. | You want to shape the space, not just its length. |
| Delay | `delay` | Stereo delay with independent left/right times. | Echo and rhythmic space. |
| Chorus | `chorus` | Modulated doubling. | Width and motion on pads, guitars, vocals. |
| Flanger | `flanger` | Short modulated delay with feedback. | Sweeping jet tones. |
| Phaser | `phaser` | Modulated allpass notches. | Softer motion than a flanger. |
| Stereo Width | `stereo_width` | Mid/side width, 100 % = unchanged. | Widen or mono a stereo source. |
| Doubler | `doubler` | Short detuned delay voices with a focus high-pass. | Thicken a vocal or guitar while keeping the low end mono. |
| Air Exciter | `exciter` | Synthesised air band from existing upper-mid content. | Add openness without an EQ shelf. |
| Ducked Send | `ducked_send` | Reverb return reduced by the dry signal. | A space that stays out of the lyric's way. |
| Resonance Suppressor | `resonance` | Spectral dynamics that cut narrow resonances. | A ring, whistle or boxy tone that follows the notes. |
| Tuner | `tuner` | Scale-snapped pitch correction with PSOLA resynthesis. | A vocal take needs tuning without a formant shift. |
| Breath Split | `breath_split` | Linear-phase split with a periodicity detector. | Reduce or lift breaths without gating the take. |
| Plosive | `plosive` | Low-band burst detector driving a dynamic low shelf. | Remove `p`/`b`/`t`/`k` thumps without thinning the vowel. |

The full parameter list per device is `ocp devices list --json`.

## Dynamics

**Compression** is the general-purpose compressor: `threshold`, `ratio`,
`attack`, `release`, `knee`, `range`, `makeup`, `lookahead`,
`max_lookahead`, `detector` (accepted; always peak), `detector_hpf`, `stereo_link`,
`hold`, `auto_release`, `auto_makeup`, `mix`. `ratio` is written `3:1`. Six
optional detector bands (`sidechain_eq1_on` … `sidechain_eq6_q`) filter the
signal the detector reads without touching the audio: a `sidechain_eq1_type` of
`high_shelf` at 6 kHz with `sidechain_eq1_gain` = 12 dB turns the device into a
de-esser-shaped compressor, and a `low_cut` band keeps bass out of the detector.
The bands are off by default, so an existing project renders unchanged.

**Multiband** splits the signal into `band_count` bands
(`two_bands`, `three_bands`, `four_bands`) at `crossover_1..3`, with `slope`
(`6db`…`48db`) and `phase_mode` (`minimum`, `dynamic`, `linear`). Each band has
`enabled`, `threshold`, `range`, `ratio`, `attack`, `release`, `knee`, `makeup`,
`lookahead`, `solo` and `mute`; `mix` blends the whole device. A zero
`lookahead` keeps the previous signal path and reports no latency; a non-zero
value delays every band by the same line and reports it, while each band's own
`lookahead` only moves that detector's tap. `solo` auditions one band by
dropping the others from the sum, and `mute` always wins over solo.

**De-esser** uses `threshold`, `range`, `attack`, `release`, `high_frequency`,
`low_frequency`, `band_processing` (`wide_band` or `split_band`), `stereo_link`
and `lookahead`, plus the analog modes: `threshold_mode` (`absolute` or
`relative`), `relative_threshold`, `detector` (`sidechain` or `in_band`),
`sibilance_band` (`auto`, `5_khz` … `9_khz`), `band_estimation` (`off` or
`learn`), `band_override` (Hz) and `segment_detection` (`off` or `on`). Relative
reads the band against a slow program level; in-band moves the detector after
the split; a band step moves the sidechain corners and the split together;
`learn` finds the band from the take and `on` gates the reduction to sibilant
segments. Defaults are the shipped device, and the modes are documented in
[de-esser modes](../deesser-modes.md).

**Limiter** exposes `ceiling` (−12…0 dB), `release`, `release_mode` (`fast`,
`auto`, `punch` or `sustain`), `lookahead` (0–20 ms), `true_peak` (on by
default), `oversampling` (`4x`, `8x` or `16x`; 4x is the default and keeps
the original detector) and `link` (0–100 %, default 100 = one detector drives
both channels; 0 % limits each channel on its own level). The lookahead is
compensated by the delay alignment, so a limited master stays in time. Release
styles and factor measurements are in [the limiter note](../limiter.md).

**Gate** is a noise gate: `threshold`, `attack`, `hold`, `release`, `range`.

**Leveler** rides gain at control rate: `target` (loudness to aim for), `speed`
(dB/s), `range`, `window` (loudness integration), `gate` and `gate_release`.
It keeps crest factor instead of shortening it.

**Opto** is the optical compressor with a drive-style control: `peak_reduction`,
plus `release` and `makeup`. See [Vocal compression](../vocal-comp.md).

## EQ

**Parametric EQ** (`eq`) is a fixed three-band layout: `low_freq`, `low_gain`,
`mid_freq`, `mid_gain`, `q`, `high_freq`, `high_gain`.

**Passive EQ** (`passive_eq`) couples a low boost bell and a low cut shelf on one
corner (`low_freq`, `low_boost`, `low_cut`), a high bell (`high_freq`,
`high_boost`, `high_q`), a high shelf cut (`high_cut_freq`, `high_cut`), a
`tilt` around 1 kHz, and an `air` shelf (`air_freq`, `air`).

**Dynamic EQ** (`dynamic_eq`) has six identical band slots plus dedicated cuts:
per band `enabled`, `solo`, `channel` (`stereo`, `mid`, `side`, `left`, `right`),
`type` (`bell`, `low_shelf`, `high_shelf`, `low_cut`, `high_cut`, `notch`,
`band_pass`), `freq`, `q`, `gain`, and optionally `dynamic`, `threshold`,
`range`, `attack`, `release` and `sidechain`. One `sidechain=` source feeds every
band that sets its `sidechain` switch; the channel target picks the component it
processes. Cuts have `low_cut_enabled`, `low_cut_freq`, `low_cut_q` and the same
for `high_cut`. `auto_gain` compensates level. In the GUI this device is a full
curve editor (see [the window](gui.md)).

The retired `eq: high_shelf=…`/`low_cut=…`/`notch=…` shorthand is expanded onto
this device's static bands and cuts; see
[DSL compatibility](../dsl-compat.md).

## Saturation

**Saturator** is two controls: `drive` and `mix`. **Iron** adds a transformer
stage: `drive`, `bias`, `ceiling`, `low_freq`, `high_freq`, `output`.
**Distortion** has `drive`, `tone`, `output`, `mix`.

## Reverb, delay and modulation

**Reverb**: `decay`, `predelay`, `damping`, `mix`, `modulation`, `early_late`,
`stereo_width`, `low_cut`, `high_cut`. `predelay` runs from 0 to 300 ms, so a
wet entrance can sit a quarter-second behind the dry signal.

**FDN Reverb** (`reverb_fdn`) keeps six measured decay bands and a six-band
output EQ. `space` is the size control and `decay` is its readback in ms
(200 ms–10 s); `brightness` (0–100 %) tilts the six decay bands from a
dark top end at 0 % through the measured reference at 50 % to a flat,
fully open tail at 100 %; `character` and `distance` carry the modulation
and the early-reflection balance, and
`damping` (100 − `brightness`), `modulation` (equal to `character`) and
`early_late` (100 − `distance`) are compatibility readings of them. Pin one
control per pair: `space`, `brightness`, `character` and `distance` are the
stored fields, and a preset that carries both sides keeps the stored field. The
other controls are `decay_rate`, `predelay`, `stereo_width`, `mix`, the six
`decay_band1…6` trims (dB, default 0) with their placement
(`decay_bandN_freq`, `decay_bandN_q`, `decay_bandN_shape`: `none`, `low_shelf`,
`bell` or `high_shelf`), and the six-band post EQ
(`post_eqN_freq`, `post_eqN_gain`, `post_eqN_q`, `post_eqN_shape`: `bell`,
`low_shelf`, `high_shelf`, `low_cut`, `high_cut` or `notch`). Design and
measurements: [the FDN reverb note](../reverb-fdn.md).

**Delay**: `time_l`, `time_r`, `feedback`, `mix`. **Chorus**, **Flanger** and
**Phaser** each take `rate`, `depth`, `mix`; flanger and phaser add `feedback`.

**Stereo Width** takes `amount` as a percentage (0–200 %; `100 %` is unchanged,
`0` is mono, `200 %` doubles the side signal). The device panel exposes it like
any other control. Curve views for `eq` and `passive_eq` and the spectrum
analyser for `dynamic_eq` are described in [the EQ editor](../eq-editor.md).

## Voice and vocal

**Doubler**: `delay`, `rate`, `depth`, `detune` (cents), `focus` (high-pass to
keep the low end mono), `mix`. **Air Exciter**: `amount`, `drive`, `character`,
`low_freq`, `high_freq`, `output`. **Ducked Send**: `predelay`, `depth`,
`attack`, `release`, `threshold`, `width`, plus `sidechain=` to choose the dry
key. **Resonance Suppressor**: `threshold` (prominence in dB), `depth` (maximum
reduction), `selectivity` (band-count proxy, 12–52 bands), `attack`, `release`,
`mix`, `enabled`. It is a 2048/256 STFT and reports 48 ms of latency; a bypass
reports 0. Law and measured tables: [the resonance suppressor](../resonance.md).

**Tuner** (`tuner`) is a scale-snapped pitch corrector: `amount` (0–100 %; 0 is
a bit-exact bypass), `retune` (10–300 ms one-pole), `key`, `scale` (twelve
scales from Chromatic to Blues), `mode` and `enabled`. `mode` trades range for
latency: **full range** detects down to 80 Hz and reports 40 ms, **low latency**
starts at 120 Hz and reports 20 ms. Detection is YIN on a 16 kHz copy; the note
target changes only on an onset or when the pitch leaves a 60 cent band, and
PSOLA resynthesis keeps the formants. The measured detector error is at most
0.39 cents to 880 Hz and a breathy 6 dB SNR tone produces no octave-up frames.
Law, constants and tables: [the tuner](../tuner.md).

**Plosive** (`plosive`): `amount` (0–100 %; 0 is a bit-exact bypass),
`threshold` (2–24 dB rise, default 6), `frequency` (80–150 Hz low-shelf corner,
default 120), `release` (10–300 ms, default 40) and `monitor`. A 24 dB/oct
low-band rise and a 400 Hz transient band are each compared with slow envelopes;
a burst must carry its excess energy in the low band, so a vowel onset or a
sibilant does not fire. A trigger holds for 20 ms and drives a minimum-phase low
shelf with a 1.5 ms attack. Measured at amount 100: an S8 burst reads −7.94 dB
below 120 Hz, the 500 Hz–2 kHz vowel body moves −0.000 dB, a plosive over a
110 Hz sung note keeps f0 to 0.000 cents, the reported latency is 0 and
`process` allocates nothing. Law, thresholds and the full acceptance table:
[the plosive](../plosive.md).

**Breath Split** (`breath_split`): `breath_level`, `voice_level`, `threshold`,
`smoothing`, `monitor` and `split_frequency` (1.5–6 kHz, default 2 kHz). A
257-tap linear-phase low-pass and its exact complement split the take; a
periodicity detector (30 ms window, 10 ms hop) decides which windows are breath
and applies `breath_level` only there. At unity the split sums to the input
delayed by 128 samples; the defaults are a bit-exact bypass with zero reported
latency. A requested −12 dB measures −11.998 dB on a 4–8 kHz breath. Law,
detector and limits: [the breath split](../breath-split.md).

The intended vocal order is tuner → leveler → clean-up EQ → plosive → breath
split → opto → de-esser → passive EQ → iron → resonance suppressor → doubler →
exciter → space. Every device in that order ships; what each device's law is
and the design rationale are in [the vocal chain](../vocal-chain.md).

## A note on the rack

Effects run in the order you add them, and the rack shows them top to bottom.
Adding appends to the end; removing is available on the slot (or its context
menu). There is no drag-to-reorder in the current GUI, so build the chain in the
order you want it. Bus effects are edited in the inspector; buses have no
separate insert rack.
