# Drum Kit

`builtin.drumkit` is a pad-per-note drum sampler built on the same sample
decoder as `builtin.sampler`. It adds a pad grid, velocity layers, deterministic
round-robin, choke groups, per-pad envelopes, reverse playback, and polyphonic
voice stealing.

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

A bare `instrument:` name denotes a drum-kit instrument; the canonical form is
`instrument: drums/drumkit`. `@kit` lowers to a `source: builtin`,
`plugin: "builtin.drumkit"` preset whose `params` carry the pad document, so
every render path (offline, per-track, streaming, JSON export) resolves kits the
same way. `fmt` writes kits back as `@kit` blocks.

## Pad model

| Field | Range | Default | Meaning |
|---|---|---|---|
| `note` | MIDI number or note name | `36 + declaration index` | MIDI note that triggers the pad |
| `gain` | dB | `0` | Pad level; linear, no normalisation |
| `pan` | `-1..1` | `0` | Balance pan: unity per side at centre, attenuates the opposite side |
| `tune` | semitones | `0` | Playback transposition |
| `cents` | cents | `0` | Fine transposition |
| `decay` | seconds | `0` | VCA decay to -60 dB; `0` keeps the sample envelope |
| `release` | seconds | `0.005` | Gate-mode release after note-off |
| `reverse` | bool | `false` | Decode the sample back to front once, at construction |
| `one_shot` | bool | `true` | Ignore note-off; `false` = gate mode |
| `choke` | `0..127` | `0` | Choke group; `0` = no group |
| `voice` | `kick`, `snare`, `hat_closed`, `hat_open`, `tom`, `clap`, `rim` | from the pad name, then the note | Fallback synthesised voice used when the pad has no playable `layer`; see [drum-synthesis.md](drum-synthesis.md) |

A pad with no playable layer falls back to its synthesised voice, so a kit written from the pad table alone is audible. `voice` overrides the name-based guess; write it when the pad name is not a voice (`pad snare_top | note: D2, voice: snare`).

Each pad owns a list of layers:

| Layer field | Range | Default | Meaning |
|---|---|---|---|
| `low`, `high` | MIDI velocity | `1`, `127` | Velocity window |
| `sample` | path | required | WAV or FLAC, resolved against the project directory |
| `gain` | dB | `0` | Layer trim |

The panel always shows at least **16 pads**; a kit may declare fewer (empty
slots stay silent and accept a sample drop) or more (the grid grows).

All samples are decoded at construction. Missing or unreadable files produce one
`log::warn!` per unique path and that layer is silent; the project still loads
and renders. `DrumKit::warnings()` exposes the same list for callers.

A hit that no pad answers falls back to its General MIDI note. When the kit does
not hold that note either, the hit is silent and the render reports it once per
(track, pad) with the number of hits; see
[render warnings](render-warnings.md).

## Velocity layers and round-robin

Selection is deterministic and clock-free:

1. Consider the pad's **playable** layers whose `[low, high]` contains the
   note-on velocity.
2. Keep a per-pad trigger counter. The selected layer is
   `matching[counter % matching.len()]`, and the counter increments once per
   note-on.
3. If no layer matches, use the nearest velocity window (smallest distance,
   first declaration wins).

Consequences:

- Non-overlapping windows are ordinary **velocity layers**: quiet hits select
  the soft sample, loud hits the hard one.
- Several layers sharing the same window form a **round-robin bank**: repeated
  hits at the same velocity cycle through them in declaration order.
- The selection depends only on trigger order. Two runs of the same pattern
  produce the same sequence; no clock, random source, or note timing is used.
- Only playable layers participate, so one missing file does not silence a pad
  that still has samples.

Velocity gain is linear (`velocity / 127`) multiplied by pad and layer gain.
Layers are not level-matched: two samples recorded at different levels keep
their difference.

## Choke groups and sharing

- Voices remember the `choke` group of the pad that started them.
- A note-on on a pad with `choke != 0` fades **every active primary voice in the
  same group** over `CHOKE_FADE_SECONDS` (4 ms) before starting the new voice.
  This is the classic hi-hat behaviour: closed hat cuts open hat; triggering a
  second kick in group 1 cuts the previous kick.
- Chokes are cross-pad: the group id is global to the kit, and pad names do not
  matter.
- The fading voice moves to a preallocated **fade slot**, so the cut is a
  linear amplitude ramp to zero rather than a waveform discontinuity. Fade
  slots are also used by voice stealing.
- Pads with `choke: 0` never choke and are never choked.

## Polyphony and voice stealing

- Hard maximum: **64 primary voices** (`MAX_VOICES`); `max_voices` may lower it
  per kit (default 32).
- 32 extra preallocated **fade slots** hold voices that are choking or being
  stolen. They are never used for new notes.
- When all primary slots are busy, the voice with the lowest current output
  (`envelope level × gain`) is stolen; ties go to the oldest voice. The stolen
  voice moves to a fade slot over `STEAL_FADE_SECONDS` (8 ms) and the freed
  primary slot starts the new note.
- If every fade slot is busy, the quietest fade tail is overwritten. That is the
  only path that can cut a still-fading voice, and it requires more than 32
  simultaneous chokes/steals.

## DSP law and measured numbers

Measured with the `oc-synth` and `oc-engine` tests at 48 kHz:

| Quantity | Law | Measured |
|---|---|---|
| Onset ramp | linear, 0.5 ms | 24 frames |
| Decay | `level *= 0.001^(1 / (decay × sr))` — -60 dB over `decay` | `decay: 0.1s` → frame 30: 0.500, frame 4 790: 0.0005 (**-60.0 dB**) |
| Gate release | linear over `release` seconds from the level at note-off | 10 ms release → silence after 480 frames |
| Choke fade | linear over 4 ms | tail **< 0.001** (-60 dBFS) after 192 frames; largest per-sample jump **< 0.01** |
| Steal fade | linear over 8 ms | largest per-sample jump **< 0.02**; quietest voice (velocity 10) is the one moved to a tail |
| Silence floor | `level <= 1e-5` ends the voice | -100 dB relative to unity |
| Velocity | `velocity / 127`, linear | -2 dB pad + 4× layer = 12 dB peak difference at equal velocity |
| Pitch | `2^((tune + cents/100) / 12)` × asset rate / output rate | one octave doubles the resampling ratio |
| Render timing | pattern hits on the project clock | 120 BPM: kick at frame 0, snare at frame 24 000 |

Reverse is applied once when the sample is decoded, so the audio callback does
not branch on it.

## Render path and export

- `render_cache::render_builtin_project` dispatches `builtin.drumkit` to
  `oc_synth::drumkit::try_render_track`.
- Streaming playback uses `BlockSource::DrumKit`; the block source decodes once
  and renders chunks without allocating.
- `RenderCache` stamps every existing `layer.*.sample` file, so replacing a
  sample on disk dirties the track and re-renders it.
- The kit tail reported to the renderer is the longest decoded sample plus the
  pad release, capped at 30 s.
- `ocp fmt` round-trips `@kit` blocks; `ocp export --format json` includes the
  lowered preset with its `pad.*` / `layer.*` params.

## GUI

The **Drum Kit** bottom panel (`K`) shows the grid for the kit of the selected
track (or the selected kit preset):

- each pad shows its name, first sample file name, MIDI note, layer and
  round-robin counts, and choke group;
- clicking a pad auditions it through the backend kit instrument and flashes it;
- dropping a WAV/FLAC file on a pad loads it into that pad (creating the pad if
  the slot was empty), by calling `set_preset_params`;
- the inspector edits gain, pan, tune, cents, decay, release, choke, one-shot vs
  gate, and reverse, also through `set_preset_params`;
- incoming live MIDI note-ons from the MIDI monitor flash the matching pad.

Every sample load and inspector edit goes through the same preset mutation
command as the rest of the GUI, so it participates in the backend undo stack.

## Deliberately not implemented

- Time-stretching or tempo-following samples. Pads always play at their
  recorded rate, transposed by `tune`/`cents`.
- Multi-mic / multi-velocity-dimension layers (round-robin is one dimension:
  the trigger counter).
- Per-pad filters, EQ, compression, or insert FX; use the track chain.
- Sample disk streaming or background decoding; kits are assumed to fit in RAM.
- MIDI CC / aftertouch modulation of pad parameters, and per-pad output routing.
- A kit authoring workflow outside the DSL/GUI (import of third-party kit
  formats).
- GUI waveform trimming, loop points, or sample replacement history beyond the
  project undo stack.

## Verification

```sh
CARGO_TARGET_DIR=… cargo test -p oc-synth drumkit
CARGO_TARGET_DIR=… cargo test -p oc-engine drumkit
cd crates/oc-gui/frontend && npx vitest run src/__tests__/drumkit-utils.test.ts \
  src/__tests__/components/DrumKitPanel.test.tsx
```

The tests cover choke timing, one-shot vs gate, velocity layers, round-robin
determinism, voice stealing, missing samples, reverse, the decay law, DSL
round-trip and JSON export, and a full render proving hits at the expected
frames.
