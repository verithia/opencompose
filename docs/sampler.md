# `builtin.sampler` (SFZ subset)

`builtin.sampler` is the engine's file-based sampler. Project presets use it
directly, and imported sample and multisample oscillator layers in
`builtin.open_synth` play through it.

## Supported input

- WAV (8-, 16-, 24- and 32-bit PCM, and 32-bit float).
- FLAC.
- The decoder is chosen by the file extension (`.wav`, `.wave`, `.flac`). A
  file with another extension is read as FLAC if it starts with `fLaC`, and as
  WAV otherwise.
- A file with more than two channels keeps its first two.
- One decoded sample may hold at most 64 Mi sample values, and an SFZ file may
  be at most 4 MiB.
- A sample can have a different sample rate from the project. Playback
  resamples with linear interpolation, using one ratio that combines the note,
  the region's tuning and the file's rate against the output rate.
- All files are decoded when the sampler is built. `Sampler::next_stereo`
  neither allocates nor decodes.

## Supported SFZ subset

Headers:

- `<control>`: only `default_path`, a folder prefix for the sample paths that
  follow;
- `<global>` and `<group>`: defaults that the following regions inherit, group
  values over global ones;
- `<region>`: one sample zone.

| Opcode | Meaning |
|---|---|
| `sample` | Sample path, relative to the SFZ folder or a search root. Paths with spaces need no quotes. |
| `key`, `lokey`, `hikey` | Key zone, as a MIDI number or a note name (`C4` is 60). `key` also sets the root when `pitch_keycenter` is absent. |
| `lovel`, `hivel` | Velocity zone. |
| `pitch_keycenter` | Root note. Without it and without `key`, the root is 60. |
| `tune` | Tuning in cents. |
| `volume` | Gain in dB. |
| `pan` | −100 to 100. |
| `amp_veltrack` | How much velocity changes the level, 0 to 100 percent; default 100. |
| `offset` | First frame to play. |
| `end` | Last frame to play. |
| `loop_start`, `loop_end` | Loop points in frames. Without them, a loop covers the whole sample. |
| `loop_mode` | `no_loop` (default), `one_shot`, `loop_continuous`, `loop_sustain` or `loop_pingpong` (`pingpong` is also accepted). |
| `seq_position`, `seq_length` | Round-robin position and cycle length; the sampler has one sequence for all regions. |
| `group`, `off_by` | Choke groups. |
| `ampeg_attack`, `ampeg_decay`, `ampeg_release` | Amplitude envelope times in seconds. Release defaults to 0.05 s. |
| `ampeg_sustain` | Sustain level as a fraction from 0 to 1. Larger values count as 1, so an SFZ percentage such as `50` gives full sustain. |
| `ampeg_release_shape` | Bend of the release curve, −20 to 20; default 0, a straight fade. |

The parser collects every header and opcode it does not support, and every
region it cannot use (no `sample`, a file that is not found, an inverted zone),
in `SamplerPatch::diagnostics`, and logs them when the sampler is built. It
never reads an unsupported opcode as a different feature.

## Velocity, tuning and pitch bend

In a project preset, velocity sets the level as
`1 − (1 − velocity / 127) × amp_veltrack / 100`. Imported oscillator layers use
the velocity law of the format they came from.

`pitch_keycenter` sets a region's root and `tune` moves it in cents. The note,
the tuning and the file's rate combine into one playback ratio, so a sample
recorded at another rate still plays at the written pitch.

Pitch bend covers ±2 semitones and applies to the whole channel: a bend moves
every voice held on that channel, including layered regions. Put a per-note
detune in `tune`, not in a second bend on the same channel.

The sampler also responds to the sustain pedal (CC 64), All Sound Off (CC 120)
and All Notes Off (CC 123).

## Loops, release and round-robin

- `no_loop` plays to `end` and releases on note-off.
- `one_shot` ignores note-off and plays to `end`, or to the end of the sample.
- `loop_continuous` loops between the loop points until the release ends.
- `loop_sustain` loops while the key is held. After note-off the loop stops,
  the release starts and the sample plays on from its position.
- `loop_pingpong` plays forward to the loop end, backward to the loop start,
  and forward again. Like `loop_continuous`, it keeps looping after note-off
  until the release ends.

A loop can end on the sample's last frame, and leaving out `loop_end` ends it
at the region's end. A running loop wraps or turns around there instead of
ending the voice; a released sustain loop can play to its end.

All modes except `one_shot` start the release (`ampeg_release`) on note-off,
or when the sustain pedal is lifted.

`ampeg_release_shape` bends the release. At 0 the fade is a straight line;
negative values fall faster at first and positive values slower. Over the
release the level follows `1 - expm1(shape × progress) / expm1(shape)`, which
becomes the straight line at 0. It can be set in a region, group or global
header.

Round-robin uses one counter for the whole sampler, which every note-on
advances. Among the regions that match a note, the cycle length is the largest
`seq_position` or `seq_length`. The region whose `seq_position` equals the
counter's place in that cycle plays, together with every matching region that
has no `seq_position`.

## Choke and polyphony

`off_by` chokes: a note whose region has `off_by = N` stops every voice whose
region has `group = N`. The stop is immediate, with no fade, so it can click. Regions triggered by one
note start in order, so a region can choke an earlier layer of the same note
when both share a choke group; check layered maps before a region chokes its
own group. The drum kit's `choke` field does fade; see [Drum Kit](drum-kit.md).

The sampler has 64 voices. When all are in use, a new note replaces the oldest
voice.

When playback rewinds, for example on a seek, the sampler resets: every voice
stops, and the round-robin counter, pitch bend and sustain return to their
start values. The decoded samples stay loaded.

## Project parameters

```ocp
@preset pad
  source: builtin
  plugin: "builtin.sampler"
  params: {
    sampler.sfz_file: "pad.sfz",
    sampler.root: "samples",
    sampler.gain: 1.0
  }
```

A preset names its source with one of three parameters. When several are set,
the first in this list wins:

| Parameter | Source | Sample search folders |
|---|---|---|
| `sampler.sfz_text` | Inline SFZ text, kept in the saved patch. | The project folder, then `sampler.root`. |
| `sampler.sfz_file` | An SFZ file. | The SFZ file's folder, then `sampler.root`. |
| `sampler.sample_file` | One sample across the whole keyboard. | None: the path itself is used. |

With `sampler.sample_file`, these parameters also apply: `sampler.root_note`
(default 60), `sampler.loop_start`, `sampler.loop_end` and `sampler.loop_mode`.
With any source, `sampler.gain` (linear, default 1.0) and `sampler.pan`
(−1 to 1) apply to the whole patch.

A relative `sampler.sample_file`, `sampler.sfz_file` or `sampler.root` is
resolved against the project folder. The rule and its refusals are under
Presets in [The .ocp file](manual/dsl.md). A `sampler.sample_file` that does not
exist is an error. The sampler reports its longest release as its tail, at most
30 seconds.

An imported preset stores its SFZ text in `SamplerPatch::sfz`, so the mapped
patch can be rebuilt without the original preset file. Its sample paths resolve
against the imported preset's content folder and its multisample folders.

## Play a marked WAVE file as slices

Generate an editable SFZ beside the WAVE file, then use it as the preset's
`sampler.sfz_file`.

```sh
ocp sample slices media/loop.wav --first-key 60 > media/loop.sfz
```

The command assigns a key to each cue in frame order. Each slice ends at
the next cue or the end of the file. Use `--gated` to release slices on
note-off or `--json` to inspect the key and frame map. Edit each SFZ
`offset` and `end` to refine the cut without modifying the audio.

## Known import gaps

- OpenSynth supports [granular and spectral oscillators](osc-granular-spectral.md)
  and [sample-layer unison](synth-levels.md). A preset import needs access to
  the sample files: an oscillator that holds only embedded audio, with no file
  path, is not supported.
- Sample layers follow the owning voice's filter routing and amplitude
  envelope, and then feed the synth effects rack.
- SFZ filters, `pitch_keytrack`, keyswitches, release samples, the `<master>`
  and `<effect>` headers, round-robin beyond one shared counter, and
  crossfades between velocity layers are not implemented.
- Apart from `ampeg_release_shape`, the `ampeg_*` envelope segments are
  linear, not the SFZ curve shapes.
