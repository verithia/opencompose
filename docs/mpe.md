# MPE: channel model, bend law and routing

Status: implemented for OpenSynth (built-in instrument), the interpreter, offline
render, streaming playback and live MIDI input. Existing v0.2 projects and
non-MPE controllers keep the legacy path.

Source: `crates/oc-synth-core/src/synth.rs` (`MpeConfig`, `MpeMemberRange`, `Voice`,
`mpe_note_on_channel`, `set_pitch_bend`, `set_timbre`, `set_channel_pressure`),
`crates/oc-engine/src/interpret.rs`, `crates/oc-engine/src/interpret/pattern.rs`,
`crates/oc-engine/src/interpret/articulation.rs`,
`crates/oc-engine/src/mpe.rs` and `crates/oc-dsl/src/ast.rs`.

## 1. Channel model

MPE separates one **master** channel per zone from a block of **member**
channels. The master carries zone-wide messages; each member channel carries at
most one sounding note plus that note's pitch bend, channel pressure and CC74.

| Zone | Master | Typical members | Member storage |
| --- | --- | --- | --- |
| Lower | 1 | 2–16 (alone) | 0-based channels 1–15 |
| Upper | 16 | 15–2 (alone) | 0-based channels 14–1 |

When both zones are configured the member ranges must be split so they do not
overlap and do not contain the other zone's master, for example
`lower: 2-8, upper: 15-9`.

A project records zones in the `@project` header:

```ocp
@project
  tempo: 120
  mpe: { lower: 2-8, upper: 15-9, bend_range: 48 }
```

- `lower` / `upper` are inclusive 1-based MIDI channel ranges and are optional.
  Omitting a zone disables it.
- `bend_range` is the pitch-bend range in semitones (0–96). When omitted it
  defaults to 48. Validation rejects a lower range that includes channel 1, an
  upper range that includes channel 16, and out-of-range values.
- The whole `mpe` block is optional and is skipped when serialising JSON, so
  existing v0.2 files are byte-for-byte unchanged.

The same fields are available on the live-input settings
(`MidiPreferences.mpe`, persisted in `~/.opencompose/midi.json`) and are pushed
to an open audition through `Command::Mpe`.

### Note-on assignment

`OpenSynth` owns the assignment table:

- A note-on on a zone master allocates the first member channel whose current
  voice is no longer held (`key_down == false`). Allocation is round-robin from
  the last used channel, so repeated notes do not all retrigger the same voice.
- The `(zone, note)` → member channel mapping is stored in a fixed
  `[[u8; 128]; 2]` table. A note-off on the master releases that mapping and the
  voice; a note-off on the member channel also clears the mapping.
- If every member channel is held, the allocator falls back to the round-robin
  channel and normal voice stealing applies.
- A note-on directly on a member channel is accepted as-is. This is what the
  interpreter emits for project-recorded MPE notes.

The interpreter assigns project-recorded notes round-robin over the configured
zone (`MpeConfig::channel_for_note`), so offline render, cached render and
streaming playback place every note on the same member channel. A preset-level
`mpe_enabled` / `mpe_first_channel` / `mpe_last_channel` profile is still honoured
when the project has no `mpe` block.

## 2. Bend-range law

MIDI pitch bend is a 14-bit value `b` in `0..=16383`:

```
normalized = (b - 8192) / 8192        -> [-1, 1)
semitones  = normalized * range
```

`OpenSynth::set_pitch_bend` stores the normalized value per channel and copies
it into every matching voice. The voice path multiplies by the effective range
once per sample. The effective range is:

- `mpe.bend_range` when the patch has a range;
- otherwise 48 semitones when an MPE zone is enabled;
- otherwise 2 semitones, which is exactly the legacy
  `value / 8192 * 2` law.

The range also scales `cents_to_pitch_bend` in the interpreter, so `bend=`,
`expression` and vibrato events produced for an MPE project land at the right
pitch. A range change invalidates the cached voice frequency and takes effect on
the next sample.

## 3. Per-voice expression and routing

Each `Voice` stores fixed state created at note-on:

| State | Range | Updated by |
| --- | --- | --- |
| `pitch_bend` | -1..1 | `PitchBend` on the voice channel |
| `channel_pressure` | 0..1 | `ChannelPressure` on the voice channel |
| `pressure` | 0..1 | poly pressure / note-expression pressure |
| `timbre` | 0..1 | CC74 on the voice channel, note-expression timbre |

Events are dispatched between samples, so a bend/pressure/timbre value is
applied on the exact frame of the event; the voice loop evaluates the values
per sample. No allocation, lock or thread is added to the audio path.

The matrix sources are:

| Source | Value | Polarity |
| --- | --- | --- |
| `Aftertouch` (8) | `max(poly pressure, channel pressure)` | unipolar |
| `PitchBend` (27) | per-voice normalized bend | bipolar |
| `Timbre` (28) | `2 * CC74 - 1` | bipolar (centre is neutral) |

The numeric ids are appended after the existing `mod.*.source` table, so older
patches keep their meaning. Pressure is also hard-wired to the cutoff as before;
timbre only reaches the filter through the matrix, matching the “route it as a
modulation source” model. A route such as:

```ocp
@preset mpe_lead
  source: builtin
  plugin: "builtin.open_synth"
  params: { mod.1.source: 27, mod.1.destination: 1, mod.1.amount: 0.25 }
```

adds up to 3 semitones of per-note bend on top of the channel bend. The FX rack
uses the newest active voice for these per-voice sources, like velocity.

## 4. Recorded per-note expression

Projects can record expression with the existing optional note attributes:

```ocp
@track lead | instrument: synth/lead_square, preset: mpe_lead
  @phrase notes [1:1 - 1:3]
    C4 q mf bend=100ct expression={pressure: 20%->90%, timbre: 10%->80%}
```

- `bend=` records a pitch curve in cents. Its value is converted with the
  project bend range.
- `expression={pressure: a->b}` emits `ChannelPressure` at the note edges.
- `expression={timbre: a->b}` emits CC74 (the interpreter already maps
  `brightness`/`timbre` to CC74).
- Multiple `expression` modifiers on one note are merged rather than
  overwriting each other.
- All of it is plain AST data: `fmt`/parse/JSON round-trip with no extra keys
  for projects that do not use it.

## 5. Offline and streaming

Offline render and streaming playback consume the same interpreted events and
the same OpenSynth patch params (MPE params are appended by
`oc_engine::mpe::patch_params` / `builtin_track_params`). The streaming path
dispatches events at 64-frame sub-chunk boundaries before rendering up to the
next event, which is frame-accurate.

`render_cache::tests::mpe::project_expression_matches_between_offline_and_streaming`
parses a project with `bend=` plus pressure/timbre curves, renders it through
`render_builtin_project` and through a `BlockSource` fed in 64-frame chunks, and
requires the two buffers to be bit-identical.

## 6. Tests

| Test | Covers |
| --- | --- |
| `oc-synth-core::synth::tests::mpe_bend_range_scales_channel_bend` | ±48 semitone MPE range |
| `oc-synth-core::synth::tests::legacy_bend_keeps_the_two_semitone_law` | non-MPE compatibility |
| `oc-synth-core::synth::tests::mpe_member_bends_do_not_cross_voices` | two notes, isolated bends |
| `oc-synth-core::synth::tests::pressure_and_timbre_reach_pitch_routes` | Aftertouch/Timbre matrix routing |
| `oc-engine::render_cache::tests::mpe::project_expression_matches_between_offline_and_streaming` | same samples in both paths |
| `oc-dsl/tests/suite/mpe.rs::mpe_config_and_expression_round_trip_dsl_and_json` | `fmt`/parse/JSON round-trip |
| `oc-dsl/tests/suite/mpe.rs::project_without_mpe_is_unchanged` | v0.2 files keep their shape |

## 7. Not covered

- MIDI file import does not infer MPE zones from RPN 0 / MCM messages; imported
  files stay on the legacy per-track channel model unless the project header is
  edited.
- RPN 0 and MCM (zone configuration over MIDI) are not parsed at input time; the
  zone comes from project settings or live-input preferences.
- `bend=` accepts nonnegative cents only, so a negative static note bend is not
  expressible through that attribute (use an expression/automation lane).
- No dedicated MPE editor in the GUI preferences popover yet; the persisted
  `mpe` settings can be written by the settings JSON, and the project `mpe`
  block is the durable path.
- Arpeggiated OpenSynth patches keep their existing per-channel arpeggiator
  path and do not allocate member channels.
- The MPE bend range is configuration only, not an RPN exchange with external
  hardware.
