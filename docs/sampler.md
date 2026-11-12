# `builtin.sampler` (SFZ subset)

`builtin.sampler` is the engine's file-backed sampler. It is used directly by
project presets and by imported `SampleOsc*` / `MultiSampleOsc*` layers
into `builtin.open_synth`.

## Supported input

- WAV via `hound` (PCM 8/16/24/32-bit and float).
- FLAC via `claxon`.
- Assets may have a different sample rate than the project. Playback resamples
  with the combined note/tuning and asset/output rate ratio; no per-callback
  allocation or decoding happens in `Sampler::next_stereo`.

## Supported SFZ subset

Headers: `<group>` (defaults inherited by following regions) and `<region>`.

| Opcode | Meaning |
|---|---|
| `sample` | sample path, relative to the SFZ directory or a search root |
| `key`, `lokey`, `hikey` | key zone (numeric or note name; `key` also acts as default root) |
| `lovel`, `hivel` | velocity zone |
| `pitch_keycenter` | root note |
| `tune` | cents |
| `volume` | dB |
| `pan` | -100..100 |
| `amp_veltrack` | velocity-to-amplitude percentage |
| `loop_start`, `loop_end` | loop points in sample frames |
| `loop_mode` | `no_loop`, `one_shot`, `loop_continuous`, `loop_sustain`, `loop_pingpong` (`pingpong` is accepted) |
| `end` | playback end frame |
| `seq_position` | round-robin position (single global sequence) |
| `group`, `off_by` | choke groups |
| `ampeg_attack`, `ampeg_decay`, `ampeg_sustain`, `ampeg_release` | linear amp envelope |

Unsupported headers/opcodes are collected in `SamplerPatch::diagnostics` and
logged. They are never silently interpreted as a different feature.

## Tuning and pitch bend

`pitch_keycenter` sets a region's root and `tune` offsets it in cents; the note,
the region's tuning and the asset/output rate combine into one playback ratio,
so a sample recorded at another rate still lands on the written pitch. Pitch
bend is channel-wide: a bend moves every voice held on that channel, including a
layered region sharing it. Per-note detune belongs in `tune`, not in a second
bend on the same channel.

## Release and choke

`loop_sustain` follows the key: `note_off` starts the amp release and the loop
stops. `loop_pingpong` scans forward to the loop end, then backward to the loop
start, and forward again; it keeps running after `note_off`, like
`loop_continuous`. `one_shot` ignores `note_off` and plays to `end`, or to the end of the
sample when no `end` is set; any other loop mode releases on `note_off` over
`ampeg_release`.

`off_by` chokes: a new note whose region carries `off_by = N` cuts every voice
whose region has `group = N`, over a short fade rather than an instant cut. The
drum kit's `choke` field follows the same rule with its own fade. Pausing or
stopping the transport silences the audio callback, so a stop does not leave a
tail playing.

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

Also accepted: `sampler.sfz_text` (inline SFZ, preserved in the saved patch),
`sampler.sample_file` with `sampler.root_note`, `sampler.loop_start`,
`sampler.loop_end`, `sampler.loop_mode`, and `sampler.pan`.

A relative `sampler.sample_file`, `sampler.sfz_file` or `sampler.root` resolves
against the project directory. The rule and its refusals are in Presets in the
DSL manual.

Preset imports set `SamplerPatch::sfz` to the embedded SFZ text, so the mapped
patch remains reproducible without the original preset container. Sample paths
are resolved against the preset content root (`Tables` or the preset content directory,
with `Multisamples/...` and the `sfzPathRelative` parent tried as well).

## Known import gaps

- OpenSynth supports [granular and spectral oscillators](osc-granular-spectral.md)
  and [sample-layer unison](synth-levels.md). Preset imports require access to
  the sample source. A `SampleOsc` containing only `embedded_audio` bytes
  without a file path is unsupported.
- Sample layers follow the owning voice's filter routing and amp envelope,
  then feed the synth effects rack.
- SFZ filters, `pitch_keytrack`, keyswitches, release samples, `effect` /
  `master` / `global` sections, round-robin banks beyond one global sequence,
  and multi-velocity-layer crossfades are not implemented.
- `ampeg_*` envelopes are linear, not the SFZ curve variants.
