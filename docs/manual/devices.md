# Built-in devices

OpenCompose ships a library of mixer devices. The engine, the GUI device picker
and `ocp devices list` all read the same registry
(`crates/oc-device-schema/src/lib.rs`), so a device listed in one place is
available in all of them.

To add a device in the GUI, press `+` in the device rack. In the DSL, add an
entry under `effects:`:

```ocp
effects:
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
```

## Writing parameters

A parameter value is a bare number or a number with its unit (`dB`, `%`, `Hz`,
`ms`, `s`, `ct`). A parameter name the device does not define is a render
error.

`bypass=true` on any entry takes that device out of the chain and leaves its
controls as they are; see [Effects](dsl.md#effects).

`sidechain=<name>` feeds the detector from another channel on `compression`,
`dynamic_eq` and `ducked_send`. A track effect may key from a track. A bus or
master effect may key from a track or a bus. The key reads the source's
post-insert, pre-fader, pre-mute signal and never reaches the audio output.

### Fractions and percentages

Fraction-valued controls take bare physical numbers. `compression: mix=0.4` and
`multiband: mix=0.4` both mean 40 % wet, and the GUI shows them as `40%`.
Controls that are native percentages, such as the reverb mix, are written
`mix=40%`. OpenSynth's fractional controls use the same display rule, including
bipolar amounts and values above unity.

Static mixer effects also accept a typed percentage on a fraction control:
`mix=40%` means `0.4` for compression and multiband. `mix=0.4%` therefore means
`0.004` (0.4 % wet), and it is never read as 40 % wet. Write bare fractions in
new edits, so the stored number matches the catalog.

A few contexts read percentage literals their own way:

- Effect automation on these fraction controls reads `0.4%` as the physical
  value `0.4`.
- OpenSynth static patch parameters keep the raw number of a percentage.
- Instrument automation has its own normalized percentage conversion.

In all of these, write bare physical values for fractional device and synth
controls, and check the target's catalog.

## Which device to use

| Device | DSL keyword | What it is | Use it when |
|---|---|---|---|
| Gain | `gain` | Fixed linear gain (`factor`). | You need a level trim and nothing else. |
| Parametric EQ | `eq` | Three-band EQ: low shelf, mid bell, high shelf. | You want quick tone shaping with a fixed layout. |
| Passive EQ | `passive_eq` | Coupled bell and shelf low and high bands, with tilt and air shelves. | You want a program EQ with coupled controls. |
| Dynamic EQ | `dynamic_eq` | Twelve bands plus low and high cuts, each band optionally dynamic. | A frequency range only needs changing when it gets loud. |
| Compression | `compression` | General compressor with a sidechain input and a filtered detector. | You control dynamics, glue a bus or duck against a kick. |
| Multiband Compander | `multiband` | Two to four bands with per-band compression and crossover control. | A single compressor would pump or dull one band. |
| De-esser | `de_esser` | Band-split or wide-band sibilance control. | Vocals or cymbals have harsh `s` and `sh` sounds. |
| Leveler | `leveler` | Control-rate loudness riding with a gate. | You want an even performance without flattened transients. |
| Opto Compressor | `opto` | One-knob optical compressor. | You want smooth, program-dependent leveling on vocals or bass. |
| FET Compressor | `fet` | Field-effect feedback compressor whose `bias` sets the ratio. | A vocal or bass should take on more colour as it compresses. |
| Limiter | `limiter` | Lookahead peak limiter with true-peak detection. | You catch peaks on the master or a bus. |
| Gate | `gate` | Downward expander with threshold, hold and range. | You remove bleed or tighten a noisy source. |
| Saturator | `saturator` | Drive and mix. | You add weight or harmonics without changing the level much. |
| Iron | `iron` | Transformer-style asymmetric saturation with a band limit. | You thicken the lows and add harmonic density. |
| Distortion | `distortion` | Drive, tone and output. | You want audible colour and edge. |
| Reverb | `reverb` | Algorithmic room with damping and filters. | You want a usable space from a short set of controls. |
| FDN Reverb | `reverb_fdn` | Long-form reverb with space, decay rate, brightness, character and distance. | You want to shape the space as well as its length. |
| Delay | `delay` | Stereo delay with independent left and right times. | You want echoes and rhythmic space. |
| Chorus | `chorus` | Modulated doubling. | You add width and motion to pads, guitars or vocals. |
| Flanger | `flanger` | Short modulated delay with feedback. | You want sweeping jet tones. |
| Phaser | `phaser` | Modulated allpass notches. | You want gentler motion than a flanger gives. |
| Vocoder | `vocoder` | Stereo filter bank driven by the envelopes of its input or a sidechain. | You shape a synth with speech, percussion or its own spectrum. |
| Stereo Width | `stereo_width` | Mid/side width; 100 % leaves the signal unchanged. | You widen a stereo source or fold it to mono. |
| Stereo Matrix | `stereo_matrix` | Four signed channel gains and up to 500 ms of offset between channels. | You cross-mix, invert, swap or offset the two channels. |
| Doubler | `doubler` | Short detuned delay voices with a focus high-pass. | You thicken a vocal or guitar and keep the low end mono. |
| Air Exciter | `exciter` | Synthesised air band from existing upper-mid content. | You add openness without an EQ shelf. |
| Ducked Send | `ducked_send` | Reverb return reduced by the dry signal. | You want a space that stays clear of the lyric. |
| Resonance Suppressor | `resonance` | Spectral dynamics that cut narrow resonances. | A ring, whistle or boxy tone follows the notes. |
| Tuner | `tuner` | Scale-snapped pitch correction with PSOLA resynthesis. | A vocal take needs tuning without a formant shift. |
| Breath Split | `breath_split` | Linear-phase split with a periodicity detector. | You reduce or lift breaths without gating the take. |
| Plosive | `plosive` | Low-band burst detector driving a dynamic low shelf. | You remove `p`, `b`, `t` and `k` thumps without thinning the vowel. |

`ocp devices list --json` prints the full parameter list of every device.

Place the vocoder on the carrier track or bus. Assign a sidechain to use
another track as the modulator. With no sidechain, the input supplies both
signals. Choose between 12 and 48 bands and use Attack and Release to control
how closely the envelopes follow the modulator. Mix blends in the dry
carrier. Output Gain trims the wet signal.

Stereo Matrix feeds each output from both inputs. The two direct gains default
to 100% and the two cross gains default to 0%. Negative gains invert the
contribution. A negative `offset` delays the left channel and a positive offset
delays the right. Use `delay_before_matrix=true` to delay the input before mixing.
The default delays the selected output. Offsets are rounded to the nearest
sample frame and do not add latency compensation to the other channel.

## Dynamics

Compression is the general-purpose compressor. Its controls are `threshold`,
`ratio`, `attack`, `release`, `knee`, `range`, `makeup`, `lookahead`,
`max_lookahead`, `detector` (accepted; the detector is always peak),
`detector_hpf`, `stereo_link`, `hold`, `auto_release`, `auto_makeup` and `mix`.
Write `ratio` as `3:1`.

Six optional detector bands (`sidechain_eq1_on` to `sidechain_eq6_q`) filter
the signal the detector reads and leave the audio alone. For example, set
`sidechain_eq1_type` to `high_shelf` at 6 kHz with `sidechain_eq1_gain` at
12 dB and the compressor acts like a de-esser. A `low_cut` band keeps bass out
of the detector. The bands are off by default, so a project that does not set
them renders unchanged.

Multiband splits the signal into `band_count` bands (`two_bands`,
`three_bands`, `four_bands`) at `crossover_1..3`, with `slope` (`6db` to
`48db`) and `phase_mode` (`minimum`, `dynamic`, `linear`). Each band has
`enabled`, `threshold`, `range`, `ratio`, `attack`, `release`, `knee`, `makeup`,
`lookahead`, `solo` and `mute`. `mix` blends the whole device.

- With the device `lookahead` at zero, the signal path has no added delay and
  the device reports no latency. A non-zero value delays every band through the
  same line and reports that latency. A band's own `lookahead` only moves where
  that band's detector reads.
- `solo` auditions one band by dropping the others from the sum. `mute` always
  wins over `solo`.

The band detectors and the external key are described in
[Multiband compander](../multiband.md).

De-esser uses `threshold`, `range`, `attack`, `release`, `high_frequency`,
`low_frequency`, `band_processing` (`wide_band` or `split_band`), `stereo_link`
and `lookahead`. It also has these analog modes:

- `threshold_mode` (`absolute` or `relative`) with `relative_threshold`.
  Relative reads the band against a slow program level.
- `detector` (`sidechain` or `in_band`). In-band moves the detector after the
  split.
- `sibilance_band` (`auto`, `5_khz` to `9_khz`). A fixed band moves the
  sidechain corners and the split together.
- `band_estimation` (`off` or `learn`) and `band_override` (Hz). `learn` finds
  the band from the take.
- `segment_detection` (`off` or `on`). `on` limits the reduction to sibilant
  segments.

The defaults match the shipped device. The modes are documented in
[De-esser modes](../deesser-modes.md).

Limiter has these controls:

- `ceiling`, from −12 to 0 dB.
- `release` and `release_mode` (`fast`, `auto`, `punch` or `sustain`).
- `lookahead`, from 0 to 20 ms.
- `true_peak`, on by default.
- `oversampling`: `4x`, `8x` or `16x`. 4x is the default and keeps the
  original detector.
- `link`, from 0 to 100 %. At the default of 100 % one detector drives both
  channels; at 0 % each channel is limited on its own level.

The delay alignment compensates the lookahead, so a limited master stays in
time. Release styles and measurements are in [Mastering limiter](../limiter.md).

Gate is a noise gate with `threshold`, `attack`, `hold`, `release` and `range`.

Leveler rides gain at control rate. Its controls are `target` (the loudness to
aim for), `speed` (dB/s), `range`, `window` (the loudness integration time),
`gate` and `gate_release`. It keeps the crest factor of the source.

Opto is the optical compressor. `peak_reduction` sets how hard it works, and
`release` and `makeup` complete it. See [Vocal compression](../vocal-comp.md).

FET takes `drive`, `attack`, `release`, `output` and `bias` (0 to 100 %). The
bias sets the ratio position; the law is in
[FET compressor](../vocal-fet.md).

## EQ

Parametric EQ (`eq`) has a fixed three-band layout: `low_freq`, `low_gain`,
`mid_freq`, `mid_gain`, `q`, `high_freq` and `high_gain`.

Passive EQ (`passive_eq`) has these sections:

- A low boost bell and a low cut shelf that share one corner (`low_freq`,
  `low_boost`, `low_cut`).
- A high bell (`high_freq`, `high_boost`, `high_q`).
- A high shelf cut (`high_cut_freq`, `high_cut`).
- A `tilt` around 1 kHz.
- An `air` shelf (`air_freq`, `air`).

Dynamic EQ (`dynamic_eq`) has twelve identical band slots plus dedicated cuts.

- Each band has `enabled`, `solo`, `channel` (`stereo`, `mid`, `side`, `left`,
  `right`), `type` (`bell`, `low_shelf`, `high_shelf`, `low_cut`, `high_cut`,
  `notch`, `band_pass`), `freq`, `q` and `gain`. It can also set `dynamic`,
  `threshold`, `range`, `attack`, `release` and `sidechain`.
- One `sidechain=` source feeds every band that turns on its `sidechain`
  switch. The band's `channel` picks the component it processes.
- The cuts have `low_cut_enabled`, `low_cut_freq` and `low_cut_q`, and the same
  three for `high_cut`.
- `auto_gain` compensates the level.

In the GUI this device opens as a full curve editor (see
[The window](gui.md)); **Add band** turns on another slot.

The retired `eq: high_shelf=…`/`low_cut=…`/`notch=…` shorthand expands onto
this device's static bands and cuts; see
[DSL compatibility](../dsl-compat.md).

## Saturation

Saturator has two controls: `drive` and `mix`. Iron adds a transformer stage
with `drive`, `bias`, `ceiling`, `low_freq`, `high_freq` and `output`.
Distortion has `drive`, `tone`, `output` and `mix`.

## Reverb, delay and modulation

Reverb has `decay`, `predelay`, `damping`, `mix`, `modulation`, `early_late`,
`stereo_width`, `low_cut` and `high_cut`. `predelay` runs from 0 to 300 ms, so
the wet signal can enter up to 0.3 s after the dry one.

FDN Reverb (`reverb_fdn`) has six measured decay bands and a six-band output
EQ.

- `space` is the size control. `decay` reads it back in ms (200 ms to 10 s).
- `brightness` (0 to 100 %) tilts the six decay bands. At 0 % the top end is
  dark, at 50 % the bands match the measured reference, and at 100 % the tail is
  flat and fully open.
- `character` carries the modulation and `distance` the early-reflection
  balance.
- `damping` (100 − `brightness`), `modulation` (equal to `character`) and
  `early_late` (100 − `distance`) are compatibility readings of those controls.
  Set one control of each pair. The stored fields are `space`, `brightness`,
  `character` and `distance`; a preset that carries both sides of a pair keeps
  the stored field.
- The other controls are `decay_rate`, `predelay`, `stereo_width` and `mix`;
  the six `decay_band1…6` trims (dB, default 0) with their placement
  (`decay_bandN_freq`, `decay_bandN_q`, `decay_bandN_shape`: `none`,
  `low_shelf`, `bell` or `high_shelf`); and the six-band post EQ
  (`post_eqN_freq`, `post_eqN_gain`, `post_eqN_q`, `post_eqN_shape`: `bell`,
  `low_shelf`, `high_shelf`, `low_cut`, `high_cut` or `notch`).

Design and measurements are in [FDN reverb](../reverb-fdn.md).

Delay has `time_l`, `time_r`, `feedback` and `mix`. Chorus, Flanger and Phaser
each take `rate`, `depth` and `mix`; Flanger and Phaser add `feedback`.

Flanger can average up to eight modulated voices. `delay` sets the low end of
the sweep, and `spread` varies each voice's delay, depth and rate.
`stereo_cross` sends part of the wet signal to the other channel; a negative
value inverts that part. `dry_gain` and `wet_gain` are signed trims on the two
branches, applied after `mix` sets their levels.

Stereo Width takes `amount` as a percentage from 0 to 200 %. At `100 %` the
signal is unchanged, `0` is mono, and `200 %` doubles the side signal. The
device panel shows it like any other control. The curve views for `eq` and
`passive_eq` and the spectrum analyser for `dynamic_eq` are described in
[Parametric EQ curve editor](../eq-editor.md).

## Voice and vocal

Doubler has `voices`, `delay`, `rate`, `depth`, `detune` (cents), `focus` (a
high-pass that keeps the low end mono) and `mix`.

Air Exciter has `amount`, `drive`, `character`, `low_freq`, `high_freq` and
`output`.

Ducked Send has `predelay`, `depth`, `attack`, `release`, `threshold` and
`width`. Add `sidechain=` to choose the dry key.

Resonance Suppressor has `threshold` (prominence in dB), `depth` (maximum
reduction), `selectivity` (a proxy for the band count, 12 to 52 bands),
`attack`, `release`, `mix` and `enabled`. It runs a 2048/256 STFT and reports
48 ms of latency; bypassed, it reports 0. The law and the measured tables are in
[Resonance suppressor](../resonance.md).

Tuner (`tuner`) is a scale-snapped pitch corrector.

- `amount` runs from 0 to 100 %; at 0 the device is a bit-exact bypass.
- `retune` is a one-pole time from 10 to 300 ms.
- `key` and `scale` pick the target notes. There are twelve scales, from
  Chromatic to Blues.
- `mode` trades range for latency. **Full Range** detects down to 80 Hz and
  reports 40 ms. **Low Latency** starts at 120 Hz and reports 20 ms.
- `enabled` switches the device.

Detection runs YIN on a 16 kHz copy of the signal. The target note changes only
on an onset or when the pitch leaves a 60-cent band, and PSOLA resynthesis
keeps the formants. The measured detector error is at most 0.39 cents up to
880 Hz, and a breathy tone at 6 dB SNR produces no octave-up frames. The law,
constants and tables are in [Tuner](../tuner.md).

Plosive (`plosive`) has these controls:

- `amount`, 0 to 100 %; 0 is a bit-exact bypass.
- `threshold`, a rise of 2 to 24 dB (default 6).
- `frequency`, the low-shelf corner from 80 to 150 Hz (default 120).
- `release`, 10 to 300 ms (default 40).
- `monitor`.

The device compares a 24 dB/oct low band and a 400 Hz transient band with slow
envelopes. A burst fires only when its excess energy sits in the low band, so a
vowel onset or a sibilant does not trigger it. A trigger holds for 20 ms and
drives a minimum-phase low shelf with a 1.5 ms attack.

Measured at amount 100, an S8 burst reads −7.94 dB below 120 Hz and the
500 Hz–2 kHz vowel body moves −0.000 dB. A plosive over a 110 Hz sung note
keeps f0 within 0.000 cents. The reported latency is 0, and `process` allocates
nothing. The law, the thresholds and the full acceptance table are in
[Plosive](../plosive.md).

Breath Split (`breath_split`) has `breath_level`, `voice_level`, `threshold`,
`smoothing`, `monitor` and `split_frequency` (1.5 to 6 kHz, default 2 kHz). A
257-tap linear-phase low-pass and its exact complement split the take. A
periodicity detector (30 ms window, 10 ms hop) decides which windows are breath
and applies `breath_level` only there. At unity the split sums to the input
delayed by 128 samples. At the defaults the device is a bit-exact bypass with
zero reported latency. A requested −12 dB measures −11.998 dB on a 4–8 kHz
breath. The law, the detector and the limits are in
[Breath split](../breath-split.md).

The intended vocal order is: tuner, leveler, clean-up EQ, plosive, breath
split, opto, de-esser, passive EQ, iron, resonance suppressor, doubler, exciter,
space. Every device in that order ships. Each device's law and the reasons for
the order are in [Vocal chain](../vocal-chain.md).

## The device rack

Effects run in the order they appear in the rack, top to bottom. A new device
goes to the end of the chain. To change the order, right-click a slot and
choose **Move up** or **Move down**; the rack has no drag-to-reorder. Remove a
device with the `x` on its slot or **Remove effect** in the same menu. Bus
effects are edited in the inspector; a bus has no separate insert rack.
