# Drum Kit

`builtin.drumkit` is a drum sampler with one pad per MIDI note. It uses the
same sample decoder as `builtin.sampler` and adds a pad grid, velocity layers,
deterministic round-robin, choke groups, per-pad envelopes, reverse playback,
voice stealing, and a synthesised fallback voice for pads without samples.

```ocp
@kit basic
  max_voices: 32
  pad kick        | note: C2, gain: -2dB, decay: 0.35s, one_shot: true, choke: 1
  pad snare       | note: D2, decay: 0.2s
  pad hat_closed  | note: F#2, gain: -3dB, choke: 2
  pad hat_open    | note: A#2, gain: -4dB, choke: 2, release: 0.08s
  layer kick  | low: 1,  high: 127, sample: "samples/kick.wav"
  layer snare | low: 1,  high: 63,  sample: "samples/snare_soft_a.wav"
  layer snare | low: 1,  high: 63,  sample: "samples/snare_soft_b.wav"
  layer snare | low: 64, high: 127, sample: "samples/snare_hard.wav"

@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 2:1]
    kick 1:0 ff
    snare 2:0 mf
```

`instrument: drumkit` is short for the full form `instrument: drums/drumkit`.
An `@kit` block becomes a preset with `source: builtin` and
`plugin: "builtin.drumkit"`, whose `params` hold the pads and layers as
`pad.<name>.*` and `layer.<name>.<index>.*` entries. Every render path (offline,
per track, streaming, JSON export) reads kits the same way. `ocp fmt` writes a
kit back as an `@kit` block.

## Pads

| Field | Range | Default | Meaning |
|---|---|---|---|
| `note` | MIDI number or note name | 36 plus the pad's position in the block | The MIDI note that triggers the pad. |
| `gain` | dB | `0` | Pad level, linear, with no normalisation. |
| `pan` | −1 to 1 | `0` | Balance: centre leaves both sides at full level, and a pan turns down the other side. |
| `tune` | whole semitones | `0` | Transposition. |
| `cents` | cents | `0` | Fine transposition. |
| `decay` | seconds | `0` | Fade to −60 dB over this time. `0` keeps the sample's own envelope. |
| `release` | seconds | `0.005` | Release after note-off, in gate mode. |
| `reverse` | `true` or `false` | `false` | Play the sample backwards. The sample is reversed once, when it is decoded. |
| `one_shot` | `true` or `false` | `true` | Ignore note-off. `false` is gate mode. |
| `choke` | 0 to 127 | `0` | Choke group. `0` means no group. |
| `voice` | `kick`, `snare`, `hat_closed`, `hat_open`, `tom`, `clap`, `rim` | from the pad name, then the note | The synthesised voice the pad plays when it has no playable layer. See [Synthesised drum voices](drum-synthesis.md). |

A pad with no playable layer plays its synthesised voice, so a kit written with
pads alone is already audible. The voice comes from `voice`, then from the pad
name, then from the General MIDI drum note. Write `voice` when the pad name is
not a voice name, for example `pad snare_top | note: D2, voice: snare`. An
unknown `voice` is reported and ignored. A pad that has no playable layer and
no voice stays silent and is reported once.

Each pad has a list of layers:

| Layer field | Range | Default | Meaning |
|---|---|---|---|
| `low`, `high` | MIDI velocity | `1`, `127` | The velocity window. |
| `sample` | path | required | A WAV or FLAC file, resolved against the project folder. |
| `gain` | dB | `0` | Layer trim. |

All samples are decoded when the kit is built. A missing or unreadable file
produces one warning per path, and its layer stays silent; the project still
loads and renders. `DrumKit::warnings()` returns the same list.

A pattern hit names a pad, for example `kick` or `snare`. When the kit has no
pad of that name, the hit uses the General MIDI note for the name (a name that
is not a General MIDI drum uses note 38). The render reports this once per
track and hit name, with the number of hits, and says whether a pad holds that
note or the hit is silent. See [Render warnings](render-warnings.md).

## Velocity layers and round-robin

The choice of layer is deterministic and does not depend on time:

1. Take the pad's playable layers whose `low` to `high` window contains the
   velocity.
2. Each pad keeps a trigger counter. The chosen layer is
   `matching[counter % matching.len()]`, and the counter goes up by one.
3. If no window contains the velocity, use the playable layer with the nearest
   window. On a tie, the first declared layer wins. The counter does not move.

As a result:

- Windows that do not overlap are velocity layers: soft hits play the soft
  sample and hard hits the hard one.
- Several layers with the same window form a round-robin set: repeated hits at
  that velocity cycle through them in the order they are declared.
- The order depends only on the order of the hits. Two renders of the same
  pattern play the same sequence; no clock, random source or note timing is
  involved.
- Only playable layers count, so one missing file does not silence a pad that
  still has other samples.

The level is `velocity / 127` times the pad gain and the layer gain. Layers are
not level-matched: two samples recorded at different levels keep their
difference.

## Choke groups

- Each voice remembers the `choke` group of the pad that started it.
- A note-on on a pad whose `choke` is not `0` fades out every active voice in
  the same group over 4 ms (`CHOKE_FADE_SECONDS`) before the new voice starts.
  A closed hi-hat cuts an open one, and a second hit on a pad in a group cuts
  the first.
- Groups are shared by the whole kit; pad names do not matter.
- A choked voice moves to a fade slot and ramps down linearly to zero, so the
  cut does not click. Voice stealing uses the same slots.
- A pad with `choke: 0` never chokes another pad and is never choked.

## Polyphony and voice stealing

- A kit has at most 64 voices (`MAX_VOICES`). `max_voices` sets the number per
  kit; the default is 32.
- 32 more fade slots hold voices that are being choked or stolen. New notes
  never use them.
- When every voice is busy, the quietest one (envelope level × gain) is
  stolen, and on a tie the oldest one. It fades out over 8 ms
  (`STEAL_FADE_SECONDS`) in a fade slot, and the new note takes its place.
- When every fade slot is busy too, the quietest fading voice is dropped. Only
  then can a fading voice be cut short, and it takes more than 32 chokes or
  steals at once.

## Envelope and level laws

The values below come from the `oc-synth` and `oc-engine` tests at 48 kHz.

| Quantity | Law | Test result |
|---|---|---|
| Attack | linear, 0.5 ms | 24 frames |
| Decay | `level *= 0.001^(1 / (decay × rate))`, −60 dB over `decay` | `decay: 0.1s`: 0.500 at frame 30, −60.0 dB at frame 4,790 |
| Gate release | linear over `release` from the level at note-off | a 10 ms release is silent after 480 frames |
| Choke fade | linear over 4 ms | below 0.001 (−60 dBFS) after 192 frames; largest step between samples below 0.01 |
| Steal fade | linear over 8 ms | largest step between samples below 0.02; the quietest voice (velocity 10) is the one moved to a fade slot |
| End of a voice | the voice stops at `level <= 1e-5` | −100 dB relative to full scale |
| Velocity | `velocity / 127`, linear | layers at 0.1 and 0.4 stay 4× (12 dB) apart at velocities 63 and 64 |
| Pitch | `2^((tune + cents/100) / 12)` × file rate / output rate | one octave up doubles the playback ratio |
| Timing | hits on the project clock | at 120 BPM, the kick lands on frame 0 and the snare on frame 24,000 |

Reverse is applied once when the sample is decoded, so the audio callback does
not branch on it.

## Rendering and export

- Offline rendering (`render_cache::render_builtin_project`) sends
  `builtin.drumkit` tracks to `oc_synth::drumkit::try_render_track_with_warnings`.
- Streaming playback uses `BlockSource::DrumKit`, which decodes once and
  renders blocks without allocating. A seek resets every voice and every
  round-robin counter.
- `RenderCache` records the state of every `layer.*.sample` file, so replacing
  a sample on disk marks the track as changed and renders it again.
- The tail reported to the renderer is the longest sample plus the pad's
  release, or the synthesised voice's tail for a pad without samples, at most
  30 seconds.
- `ocp fmt` keeps `@kit` blocks, and `ocp export --format json` includes the
  preset with its `pad.*` and `layer.*` params.

## The Drum Kit panel

The **Drum Kit** bottom panel (`K`) shows the kit of the selected track. Without
one, it shows the selected kit preset, or else the first kit in the project.
The header shows the kit's name, its number of pads and its voice count.

- The grid shows at least 16 pads, sorted by note. A kit can declare fewer
  (empty slots stay silent and accept a sample) or more (the grid grows).
- Each pad shows its name, the file name of its first sample, its note, its
  number of layers and round-robin samples, and its choke group.
- Clicking a pad plays it at velocity 110 through the kit instrument, flashes
  it and selects it.
- Dropping a WAV or FLAC file on a pad replaces the sample of its first layer.
  Dropping it on an empty slot creates a pad at the next free note.
- The inspector beside the grid edits **Gain dB**, **Pan**, **Tune st**,
  **Cents**, **Decay s**, **Release s**, **Choke group**, **Mode**
  (**one-shot** or **gate**) and **Reverse**, and lists the layers with their
  velocity windows.
- Note-ons arriving from a live MIDI input flash the matching pad.

Every sample drop and inspector edit uses the same preset command as the rest
of the app, so you can undo it.

## Not supported

- Time-stretching or following the tempo. Pads play at their recorded speed,
  transposed by `tune` and `cents`.
- Layers in more than one dimension, such as several microphones
  (round-robin is the single trigger counter).
- Filters, EQ, compression or insert effects per pad; use the track's effect
  chain.
- Streaming samples from disk or decoding in the background; a kit must fit in
  memory.
- Changing pad parameters with MIDI CC or aftertouch, and a separate output per
  pad.
- Importing kit formats from other software.
- Trimming waveforms or setting loop points in the panel, and any sample
  history beyond the project's undo.

## Verification

```sh
cargo test -p oc-synth drumkit
cargo test -p oc-engine drumkit
cd crates/oc-gui/frontend && npx vitest run src/__tests__/drumkit-utils.test.ts \
  src/__tests__/components/DrumKitPanel.test.tsx
```

The tests cover choke timing, one-shot and gate modes, velocity layers,
deterministic round-robin, voice stealing, missing samples, reverse playback,
the decay law, the synthesised fallback voices, DSL round-trip and JSON export,
and a full render with hits on the expected frames.
