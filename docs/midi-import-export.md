# MIDI import / export — core contract

> The application and dataset profiles share the engine-owned SMF importer.
> The application behavior below remains the default; dataset-specific notation
> and acceptance are described at the end.

## 1. Import: SMF → project

Input can be any Standard MIDI File (type 0, 1 or 2). Import is a **dialog**, not
a silent conversion: the user sees what will be created before it lands.

### 1.1 What is read

| SMF | Becomes |
|---|---|
| `set_tempo` (all events) | project tempo map (piecewise constant beats) |
| `time_signature` | project meter map |
| `key_signature` | project key map |
| track name / instrument name | track name, and a suggested instrument |
| `program_change` | instrument suggestion (GM name +, if a plugin preset matches by name, that preset) |
| note on/off | phrase notes (beat/velocity/duration), channel preserved |
| CC 1, 7, 10, 11, 64, 74, 71 | automation lanes on the track (`mod`, `volume`, `pan`, `expression`, `sustain`, `brightness`, `resonance`) |
| pitch bend | `pitch_bend` automation lane (−8192..8191 mapped to −2..+2 semitones unless RPN 0 says otherwise) |
| aftertouch (channel/poly) | `pressure` automation (channel) / per-note pressure where the model supports it |
| `end_of_track` | track length hint |

Anything else (sysex, text, markers, SMPTE offsets in a musical project) is
listed in the import summary as **ignored**, never silently dropped.

### 1.2 Track mapping

The dialog proposes a mapping and lets the user override it:

- default: one SMF track → one project track; when an SMF track carries several
  channels, offer **split by channel** (default on for channel 10).
- channel 10 → drum track with GM note map, unless the user maps it to a kit.
- empty tracks are dropped by default (toggle to keep).
- name collisions get a suffix, never silently merged.
- the user can map an SMF track onto an **existing** project track (append) or
  create a new one. An append starts after the target's last written content
  (a phrase's end, a clip's end, or a pattern's written bars, whichever is
  later) and keeps the file's internal timing from that point; every unit that
  lands on one track shares the same start, so a multi-track file keeps its
  parts in sync. The import writes the appended material as one undo entry and
  leaves the existing content untouched.

### 1.3 Timing

- Internal resolution is 480 PPQ; the source PPQ is preserved in the summary and
  the conversion is exact when 480 is a multiple of it, otherwise the residual is
  reported (max tick error, and how many events moved).
- Tempo changes are written at their exact tick; a tempo change inside a note
  does not split the note (documented limitation).
- Overlapping same-pitch notes: the later note-on ends the previous note (a
  deterministic, documented policy) and the summary counts them.
- Sustain pedal (CC64 ≥ 64) extends note ends to the pedal release; this is
  applied at import and also kept as an automation lane so the pedal data is not
  lost.

### 1.4 Import summary (shown in the dialog)

Per source track: name, channel, program, note count, CC lanes found, first/last
bar, tempo changes, and a list of ignored event types. After import the summary
stays available in the UI (e.g. a "last import" panel) so the user can audit it.

## 2. Export: project → SMF

Export is per **selection**: whole project, a track range, or a single track.
The dialog lists the chosen format, PPQ and channel assignment, plus the **loss
list** (below) before writing.

### 2.1 What is written

- format 1 with a conductor track (tempo map, meter map, key map, track names);
  format 0 available as an option for legacy gear.
- PPQ 480 by default (96/960 selectable).
- one SMF track per project track, channel assigned round-robin from the track
  order starting at 1 (channel 10 reserved for the drum track), user-overridable.
- notes: on/off with velocity 1..127, duration in ticks (zero-length notes are
  widened to 1 tick and counted).
- program change on the first event if the track's instrument maps to a GM
  program; plugins without a GM mapping export no program change (counted).
- automation: CC lanes exported for the controllers that have a CC equivalent
  (volume, pan, expression, mod, sustain, brightness, resonance); pitch bend
  exported as such. Curves with no CC equivalent are dropped and listed.
- tempo/meter/key changes exported at their ticks.

### 2.2 Loss list (always shown, never hidden)

MIDI cannot carry: articulations and keyswitches (exported as plain notes unless
the user maps them), per-note microtuning, plugin parameters, automation of
non-CC parameters, audio clips, clip fades/gain envelopes, sends/buses, mute/solo
beyond the selection, and tempo ramps finer than the discrete tempo events. A
note or hit that falls outside a clip's slip, loop or trim window is also lost;
the report counts it per placement. Content of a named pattern that no clip
places is never exported, so it is counted the same way, once per object. The
dialog states exactly which of these exist in the current selection and how many
objects are affected.

## 3. Guarantees

- `import → export → import` preserves notes, tempo map, meter map, programs and
  the CC lanes listed in §1.1 within 1 tick.
- Output is deterministic: same project + same options → byte-identical SMF.
- Malformed or truncated files produce a typed error with the byte offset; no
  panics, no partial project.
- The apply reads the document revision it plans against. An edit committed
  while the file is parsed makes the import stale: it is refused with a revision
  message and nothing is applied, so the edit is never lost. A track index that
  the mappings name must exist in the document the import applies to.
- Large files (100k+ events) import without quadratic behaviour; the dialog
  reports monotonic progress and can be cancelled.
- Progress carries a stage (`parse`, `tracks`, `clock`, `notes`, `params`,
  `commit`) plus a 0.0–1.0 fraction. The cancel flag is polled per event and per
  note, so a cancelled import aborts promptly; it returns a typed `Cancelled`
  outcome instead of an error string, leaves the project untouched, and creates
  no temp files.

## 4. UI surface

- **Import**: file picker → mapping table (source track / channel / name /
  notes / program / → target track) → options (split by channel, drop empty,
  pedal expansion, tempo map) → summary → confirm. While applying, the dialog
  shows the stage label and a percentage; cancelling shows a neutral
  "cancelled" state, not an error.
- **Export**: scope selector (project / track range / track) → format, PPQ,
  channel map → loss list → confirm.
- Both are reachable from the menu, from the track context menu, and via the
  agent tool surface (`midi_import_preview`, `midi_import_apply`,
  `midi_export`).

## Program identity and instrument paths

General MIDI program numbers are zero-based and come from the shared music
catalog. Imported exact program numbers remain in their presets and MIDI
metadata even when several programs suggest the same broad instrument path.
For tracks without an explicit program, MIDI export uses the same instrument
mapping as playback: for example, `guitar/acoustic_steel` is program 25 and
`bass/electric_finger` is program 33. Existing instrument path keys, preset
program numbers and preset-picker `gm:N` identities remain unchanged.

## Dataset conversion profile

`oc-data::midi2dsl` uses `oc-engine::midi::import_smf_dataset`; it contains no
second MIDI parser, clock implementation or note/percussion mapper. Dataset
import retains source PPQ and exact gate/timing metadata, quantized notation
labels, humanization offsets and drum grid offsets. Same-key overlapping onsets
share an explicit release. Source tracks are kept separate instead of merging
all tracks that happen to use the same channel. Controller lanes and source
program identity remain in the common output model.

The existing `midi_to_project` facade applies strict dataset acceptance: missing
releases, zero-length repairs and clock warnings are refused. `midi_to_import`
and `midi_bytes_to_import` return the shared repair/loss summary for callers
that need to review the result. File input is bounded to 64 MiB; type-2
asynchronous sequences require separate imports. Application import continues
to use its existing 480 PPQ, retrigger and optional sustain-expansion rules.

## Shared import resource limits

Application, GUI, CLI and dataset imports use the same `ImportLimits` defaults.
The byte-oriented APIs check limits before `midly` materializes event vectors;
file entrypoints use a bounded reader and check the opened handle is a regular
file. Symlinks to regular MIDI files remain supported. Directories and FIFOs are
refused, and Unix opens use nonblocking mode to avoid a FIFO path-swap wait.
Growing files cannot bypass the byte limit, and GUI cancellation is checked
during file reads as well as event import.

| Budget | Default |
|---|---:|
| Input bytes | 64 MiB |
| Source tracks | 4,096 |
| Source events and chunk count | 2,000,000 |
| Note onsets | 500,000 |
| Candidate output notes, automation and clock points | 2,000,000 |
| Candidate output track units | 8,192 |
| Work units, including repeated scans, sorts and name collisions | 200,000,000 |
| Source extent in quarter notes | 1,000,000 |
| Metadata and supplied names | 4 MiB |
| One encoded event body, excluding delta (including ignored SysEx/Escape) | 64 KiB |

`MidiError::LimitExceeded` records its typed limit kind, bound, observed count
and byte offset. `MidiError::code()` exposes stable codes such as
`midi_bytes_limit`, `midi_events_limit`, `midi_payload_limit` and `midi_extent_limit`;
`midi_nonregular_input` and `midi_input_io` distinguish file failures.
Library callers can inject smaller limits through `*_with_limits` APIs.
Exceeding a limit returns an error rather than a truncated or partially committed
project. These bounds do not change application 480 PPQ or dataset source-PPQ
interpretation, note gates, controller conversion or program identity.
