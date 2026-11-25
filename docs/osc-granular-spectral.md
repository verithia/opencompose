# Granular and spectral oscillators

OpenSynth has two source oscillators that play a decoded sample instead of a
wavetable. `Waveform::Granular` and `Waveform::Spectral` were appended after
`Wavetable`, so every existing patch keeps its waveform id. Both engines own a
fixed amount of state per voice and neither allocates, locks or starts a thread
while rendering.

Oscillator slots A, B and C select their engine through the normal
`osc.a/b/c.waveform` field (`5` = granular, `6` = spectral via
`set_param`/JSON). The source patch lives in `SynthPatch::granular[slot]` and
`SynthPatch::spectral[slot]`; a slot whose engine has no patch or whose sample
failed to decode renders silence, never a substituted wavetable.

## Granular engine

`crates/oc-synth-core/src/synth/granular.rs`

A voice keeps a fixed pool of 64 grains, a 64-bit xorshift stream seeded from
the patch and the note, a spawn accumulator and one playhead. Rendering is
grain-local: no FFT, no look-ahead and no interpolation state beyond the source
sample's linear read.

- Grains spawn at `density` grains/second. The spawn accumulator is bounded by
  the pool, so a density change cannot grow memory or cost.
- Each grain reads the source at the playhead with a per-grain offset, length,
  pitch, direction, pan and gain jitter. `pitch_ratio` tracks the played note
  against `root_note` when `pitch_track` is set.
- The playhead advances at `scan_rate` source durations per second and folds
  inside `loop_start..loop_end` (wrap or ping-pong). `scan_rate = 0` holds the
  position.
- Output is normalised by `1 / sqrt(max(density * grain_length, 1))`: below
  unity overlap the level follows `sqrt(density)`, above it the level stays
  flat. Six windows are available (Hann, Tukey, exponential decay, triangle,
  Blackman-Harris, Gaussian); the Gaussian window is edge-corrected so it ends
  at silence.

### Granular parameters

| Field | Unit | Meaning |
|---|---|---|
| `sample` | path | decoded source (WAV or FLAC) |
| `channel_mode` | `mono` or `stereo` | `mono` is the default source mix; `stereo` retains independent source channels |
| `read_mode` | `linear` or `bandlimited` | `bandlimited` reduces aliasing when transposition or sample-rate differences speed up source reads |
| `root_note` | MIDI note, 0..127 | note that plays the source at its recorded rate |
| `density` | grains/s | spawn rate, validated in 0..4096 |
| `grain_length` | seconds | base grain length, 0.002..10 |
| `position` | seconds, 0..86400 | start position of the playhead |
| `loop_start`, `loop_end` | seconds, 0..86400 | loop window; an empty window uses the whole sample |
| `loop_grains` | bool | wrap the read position of each active grain inside the source loop |
| `ping_pong` | bool | reflect at the loop edges instead of wrapping |
| `scan_rate` | source durations/s, -4096..4096 | playhead speed; 0 holds |
| `window` | enum | grain window shape |
| `envelope` | object | optional linear attack and release fractions, with a separate first-grain attack |
| `window_param` | 0..1 | shape control (shoulder, decay, width) |
| `window_skew` | -1..1 | moves the window peak |
| `random_offset` | fraction of grain, 0..1 | position jitter |
| `random_length` | fraction of grain, 0..1 | length jitter |
| `random_pitch` | semitones, 0..48 | per-grain pitch jitter |
| `random_direction` | 0..1 | probability of a reversed grain |
| `random_pan` | -1..1 | signed pan jitter |
| `random_gain` | 0..1 | gain jitter |
| `reverse` | bool | force every grain backwards |
| `pitch_track` | bool | follow the played note |
| `pitch_mode` | `spawn` or `follow` | sample note pitch at grain onset (default), or follow it throughout active grains |
| `seed` | u64 | stream seed, derived from the preset name on import |

Omitting `channel_mode` keeps the mono source mix and its existing PCM. In
`stereo` mode, grains read both source channels at the same position through the
same window. The existing linear grain pan gains apply independently: left is
multiplied by `1 - pan`, right by `1 + pan`, with no crossfeed. Centre preserves
both channels at unity. A mono file remains mono in either mode. Stereo mode
requires a mono or stereo input; it refuses a multichannel input rather than
silently dropping channels. Spectral file loading keeps its mono source mix.

Set `read_mode: "bandlimited"` in the same patch to prepare half-rate
source levels before playback. Each grain selects a level from its current
source rate. Forward and reverse reads use the same filtering. Fast reads
between powers of two may use a lower bandwidth than strictly necessary.
Unity and slower reads use the original interpolation. Omitting the field
keeps the existing linear reader. Both source options require preparing the
patch again.

An envelope such as `{"attack": 0.25, "release": 0.25, "first_attack": 0.0}`
starts the first grain at full level. Each following grain rises linearly
during its first quarter, sustains in the middle and releases linearly over
its last quarter. The first grain uses the same release. Each attack and
release must fit within the grain duration. Omitting the envelope keeps the
existing window and its shape controls.

Set `loop_grains: true` to keep each active grain reading within the same
sample loop. The bounds are rounded to source frames and the end is exclusive.
A grain plays through the loop without restarting its envelope. A reversed
grain wraps at the lower boundary. `ping_pong` controls the scan playhead.
With the default `loop_grains: false`, grain reads stop at the edge of the
sample. The bandlimited reader prepares levels from the whole source.


Choose `pitch_mode: "follow"` in a granular slot of the native patch JSON to
apply note-relative pitch curves, channel bends and vibrato to active grains.
The same patch document serves agent edits, playback and export. `spawn` samples
note pitch when each grain starts. Omitting the field selects `spawn` and omits
it again when saving. Neither mode changes grain position, window age, random
pitch or direction when note pitch changes. Constant note pitch produces the
same PCM in both modes, including stereo sources. With `pitch_track: false`,
notes do not change the source rate in either mode. The mode is a prepared patch
choice, rather than a numeric automation control or a dedicated GUI picker.
Note-relative `pitch_curve` points use quarters:cents and the shared finite
range of -9600..9600 cents. They retain the original note key and target its
voice identity, including release tails; they do not create another note.
Curves contain at most 1024 ordered points and an explicit horizon of at most
256 quarters. Values outside the range, including interpolation overshoot,
are refused rather than clamped.

Use `pitch_curve_mode="step"` to hold each point's value until the next authored
point. For example, `pitch_curve="0:0,0.125:700,0.25:-500,1:0"` with that mode
changes only at those quarter-note positions. Each boundary uses the project
clock, including tempo changes, and is floored once to an output frame. Points
that share a frame keep their written order; the last point on that frame wins.
The note starts before its initial pitch update. An update at note-off targets
the same voice's release tail. A final value holds without an extra update at
the horizon. Reset and replay reconstruct the same events and audio.

A uniform step ramp can use `pitch_curve="steps(16,5,0,-1200)"` with the same
mode. The arguments are ticks per quarter note, step count, initial cents
before the first step, and final cents. The example advances by −240 cents
per step. The first advance is at note-on and the last is 0.25 quarter notes
later. The same clock, horizon, and point limit apply.

Omit the mode, or choose `linear`, for the continuous curve. Its existing
200 Hz event raster and optional `pitch_curve_tension` Hermite shaping stay
unchanged. Step mode schedules only authored points and refuses a tension
modifier; it does not approximate their timing with that raster. All modes use
the same 1024-point bound, pitch range, voice identities and horizon validation.

All numeric grain settings must be finite and inside their physical domains.
Patch admission rejects invalid settings before opening the source file. A direct
runtime constructor disables an invalid source through the same logged failure
path as a source that cannot decode. Playback folds the playhead in constant
time, including when one sample crosses many loop boundaries. Nonfinite or
overflowing travel leaves its previous finite position unchanged.

## Spectral engine

`crates/oc-synth-core/src/synth/spectral.rs`

The source is analysed once at load time: a 1024-point FFT with a sqrt-Hann
window and a 256-sample hop stores magnitude, phase and the instantaneous phase
advance per bin, plus a spectral-flux transient flag per frame. Analysis is the
only place that allocates.

Each voice keeps a phase accumulator, a 1024-sample overlap-add ring and one
preallocated inverse-FFT scratch buffer. Every 256 samples it rebuilds the
magnitude spectrum from the current playhead, applies the two warp slots and
the band edges, advances each bin's phase by the analysed advance, and performs
one inverse FFT. Two sqrt-Hann windows with a 75 % overlap sum to 2, so the
ring is scaled by `1 / (2 * 1024)`. The engine reports 512 samples of latency.

Phase lock is applied on the first block (and on detected transients): after
that the accumulator tracks the instantaneous frequency. The earlier behaviour
of snapping every block back to the absolute analysis phase cancelled the
advance whenever the playhead was held, so it was removed.

### Spectral warps

`SpectralWarp` operates on the magnitude axis before resynthesis.

| Warp | Effect |
|---|---|
| `Off` | no change |
| `Comb` | periodic comb over the bin axis |
| `Gate` | alternating pass/stop bands |
| `Smear` | moving-average magnitude blur |
| `Spread` | stretch partials away from the spectrum centre |
| `Detune` | sum two copies shifted in opposite directions |
| `PitchShift` | scale the frequency axis by `2^morph` |
| `AddHarmonics` | add an octave above every partial |
| `AddSubharmonics` | add an octave below every partial |
| `Shepard` | log-frequency comb |
| `PeakShift` | move spectral peaks up by up to two octaves |
| `Mirror` | blend towards the mirrored spectrum |
| `PhaseTwist` | quadratic phase offset, magnitudes unchanged |

### Spectral parameters

| Field | Unit | Meaning |
|---|---|---|
| `sample` | path | decoded source (WAV or FLAC) |
| `root_note` | MIDI note, 0..127 | note that plays the source at its recorded rate |
| `position` | seconds | playhead position |
| `loop_start`, `loop_end` | seconds, 0..86400 | loop window; an empty window uses the whole sample |
| `ping_pong` | bool | reflect at the loop edges |
| `scan_rate` | source durations/s, -4096..4096 | playhead speed; 0 holds |
| `pitch_track` | bool | follow the played note |
| `warp`, `morph` | enum, 0..1 | first warp and its depth |
| `warp2`, `morph2` | enum, 0..1 | second warp and its depth |
| `bend` | semitones | frequency-axis bend, clamped to ±48 |
| `phase_lock` | 0..1 | 1 locks the initial phase to the analysed phase |
| `transients` | bool | re-seed the phase accumulator on transients |
| `low_hz`, `high_hz` | Hz | resynthesis band edges; 0 disables an edge |
| `band_smooth` | bool | one-octave edge slope instead of a brick wall |
| `seed` | u64 | per-voice seed (reserved) |

## Modulation

The source controls are appended to `ModDestination` without renumbering the
original variants. Their normalized ids are 23..34.

| Destination | Ids (A/B/C) | Scale per route |
|---|---|---|
| `SourceDensityA/B/C` | 23/24/25 | 512 grains/s |
| `SourcePositionA/B/C` | 26/27/28 | 2 seconds |
| `SourceMorphA/B/C` | 29/30/31 | 1 (0..1 depth) |
| `SourceBendA/B/C` | 32/33/34 | 24 semitones |

Granular slots use density and position; spectral slots use morph (first warp)
and bend. The offsets are added to the patch value and clamped by the engine.

## Preset import

The importer resolves file-backed samples relative to the preset's content
roots and maps the supported grain and spectral controls. The root note and
source position come from the preset when provided. The sampler reports a
warning when it cannot load a referenced source.

Some stored settings have no corresponding control, including embedded
sample data, per-grain warp, custom position curves, and certain warp
variations. Choose a local sample file if a preset needs an embedded source.
Replacing the sample rebuilds its oscillator instance. Modulating its
existing controls continues to work on sounding notes.
