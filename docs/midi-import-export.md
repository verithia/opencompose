# MIDI import and export

The application and the dataset tools read Standard MIDI Files through the
same SMF importer in the engine. Everything up to
[Dataset conversion profile](#dataset-conversion-profile) describes the
application, which is the default behaviour. The dataset profile and its
acceptance rules come at the end.

## Import: SMF to project

You can import any Standard MIDI File of type 0, 1 or 2. Import opens a dialog
that shows what will be created before anything changes in the project.

### What is read

| SMF | Becomes |
|---|---|
| `set_tempo` (all events) | project tempo map (piecewise constant beats) |
| `time_signature` | project meter map |
| `key_signature` | project key map |
| track name / instrument name | track name, and a suggested instrument |
| `program_change` | instrument suggestion: the GM name, plus a plugin preset whose name matches, if there is one |
| note on/off | phrase notes (beat, velocity, duration), with the channel preserved |
| CC 1, 7, 10, 11, 64, 74, 71 | automation lanes on the track (`mod`, `volume`, `pan`, `expression`, `sustain`, `brightness`, `resonance`) |
| pitch bend | `pitch_bend` automation lane (−8192..8191 mapped to −2..+2 semitones unless RPN 0 sets another range) |
| channel aftertouch | `pressure` automation lane |
| `end_of_track` | track length hint |

Only the first program change on each channel is used. Poly aftertouch, later
program changes and every other event (SysEx, text, markers, SMPTE offsets in a
musical project) appear in the import summary as ignored. Nothing is dropped
without being listed.

### Track mapping

The dialog proposes a mapping, and you can change any part of it:

- By default each SMF track becomes one project track. **Split by channel**
  turns each channel of a source track into its own track. It is off by
  default, but channel 10 always becomes a separate track.
- Channel 10 becomes a drum track with the GM note map, unless you map it to a
  kit.
- Empty tracks are dropped by default. Turn off **Drop empty tracks** to keep
  them.
- A name that is already taken gets a suffix. Tracks are never merged because
  their names collide.
- You can map an SMF track onto an existing project track, which appends to
  it, or create a new track.

An append starts after the target track's last written content: the end of a
phrase, the end of a clip or the written bars of a pattern, whichever is
latest. From that point the file's internal timing is kept. Every unit that
lands on the same track starts at the same point, so the parts of a multi-track
file stay in sync. The appended material is one undo entry, and the existing
content is not touched.

### Timing

- The project resolution is 480 PPQ. The summary shows the source PPQ. The
  conversion is exact when 480 is a multiple of the source PPQ. Otherwise the
  summary reports the residual: the largest tick error and how many events
  moved.
- A tempo change is written at its exact tick. A tempo change inside a note
  does not split the note.
- When two notes of the same pitch overlap, the later note-on ends the earlier
  note. The summary counts these overlaps.
- **Expand sustain pedal** (on by default) extends note ends to the pedal
  release when CC64 is at or above 64. The pedal data is also kept as an
  automation lane.

### Import summary

For each source track the dialog shows the name, channel, program, note count,
the CC lanes found, the first and last bar, the tempo changes and the ignored
event types. After the import the dialog keeps the summary and the list of
created tracks on screen until you close it, so you can check the result.

## Export: project to SMF

You export a selection: the whole project, a range of tracks or one track.
Before writing, the dialog shows the format, PPQ and channel assignment, and
the loss list described below.

### What is written

- Format 1 with a conductor track that holds the tempo map, meter map, key map
  and track names. Format 0 is available for older hardware.
- 480 PPQ by default. The dialog also offers 960, and `ocp midi export --ppq`
  accepts any value from 96 to 960.
- One SMF track per project track. Melodic tracks get channels in track order
  starting at 1, skipping 10 and wrapping after 16. Channel 10 is kept for drum
  tracks. You can override any assignment.
- Notes as note on/off with velocity 1..127 and the duration in ticks. A note
  of zero length is widened to 1 tick and counted.
- A program change at the start of the track if the track's instrument maps to
  a GM program. Plugins without a GM mapping export no program change, and the
  report counts them.
- Automation as CC lanes for the controllers that have a CC equivalent
  (volume, pan, expression, mod, sustain, brightness, resonance). Pitch bend is
  exported as pitch bend. Curves with no CC equivalent are dropped and listed.
- Tempo, meter and key changes at their ticks.

### Loss list

The loss list is always shown. MIDI cannot carry:

- articulations and keyswitches (exported as plain notes unless you map them);
- per-note microtuning;
- plugin parameters and automation of parameters without a CC;
- audio clips, clip fades and gain envelopes;
- sends and buses;
- mute and solo beyond the selection;
- tempo ramps finer than the discrete tempo events.

A note or hit that falls outside a clip's slip, loop or trim window is lost
too, and the report counts it per placement. A named pattern that no clip
places is never exported, and it is counted the same way, once per object. The
dialog lists which of these losses occur in the current selection and how many
objects each one affects.

## Guarantees

- `import → export → import` keeps the notes, tempo map, meter map, programs
  and the CC lanes listed in [What is read](#what-is-read) to within 1 tick.
- Output is deterministic: the same project with the same options gives a
  byte-identical SMF.
- A malformed or truncated file produces a typed error with the byte offset.
  The importer does not panic and does not leave a partial project.
- The import applies against the document revision it was planned for. If you
  commit an edit while the file is being parsed, the import is stale: it is
  refused with a revision message and nothing is applied, so your edit is never
  lost. Every track index named by the mapping has to exist in the document the
  import applies to.
- Large files (100k+ events) import without quadratic behaviour. The dialog
  shows progress that only moves forward, and you can cancel.
- Progress carries a stage (`parse`, `tracks`, `clock`, `notes`, `params`,
  `commit`) and a fraction from 0.0 to 1.0. The cancel flag is checked for
  every event and every note, so a cancel takes effect quickly. A cancelled
  import returns a typed `Cancelled` outcome (not an error string), leaves the
  project untouched and creates no temporary files.

## Where to find it

To import:

1. Choose **Import MIDI…** and pick a file.
2. Check the mapping table: source track, channel, name, notes, program and
   target track.
3. Set the options: **Split by channel**, **Drop empty tracks**, **Expand
   sustain pedal** and **Apply tempo map**.
4. Read the summary and confirm.

While the import runs, the dialog shows the stage and a percentage. If you
cancel, it shows a neutral cancelled state and no error.

To export:

1. Choose **Export MIDI…**.
2. Pick the scope: **Project**, **Track range** or **Single track**.
3. Set the format, PPQ and channel map.
4. Read the loss list and confirm.

Both commands are in the toolbar's file menu and in the track context menu.
Agents use the tools `midi_import_preview`, `midi_import_apply` and
`midi_export`.

## Program identity and instrument paths

General MIDI program numbers are zero-based and come from the shared music
catalog. An imported program number stays exact in its preset and in the MIDI
metadata, even when several programs suggest the same broad instrument path.

For a track without an explicit program, MIDI export uses the same instrument
mapping as playback. For example, `guitar/acoustic_steel` is program 25 and
`bass/electric_finger` is program 33. Instrument path keys, preset program
numbers and the preset picker's `gm:N` identities are unchanged by this.

## Dataset conversion profile

`oc-data::midi2dsl` calls `oc-engine::midi::import_smf_dataset`. It has no MIDI
parser, clock implementation or note and percussion mapper of its own.

A dataset import keeps:

- the source PPQ and the exact gate and timing metadata;
- quantized notation labels;
- humanization offsets and drum grid offsets.

Overlapping onsets on the same key share an explicit release. Source tracks
stay separate, even when several of them use the same channel. Controller
lanes and the source program identity stay in the common output model.

The `midi_to_project` facade applies strict dataset acceptance: it refuses
missing releases, zero-length repairs and clock warnings. `midi_to_import` and
`midi_bytes_to_import` return the shared repair and loss summary for callers
that need to review the result. File input is limited to 64 MiB. The dataset
profile refuses a type-2 file, whose sequences are independent; import each
sequence separately. The application import keeps its own rules: 480 PPQ,
retrigger on overlap and the optional sustain expansion.

## Shared import resource limits

Application, GUI, CLI and dataset imports use the same `ImportLimits`
defaults. The byte-oriented APIs check the limits before `midly` builds its
event vectors.

The file entry points read through a bounded reader and check that the opened
handle is a regular file. A symlink to a regular MIDI file works. Directories
and FIFOs are refused, and on Unix the file is opened in nonblocking mode so a
FIFO swapped in under the path cannot make the open wait. A file that grows
while it is read cannot get past the byte limit, and the GUI checks for
cancellation during the file read as well as during the event import.

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

`MidiError::LimitExceeded` records the typed limit kind, the bound, the
observed count and the byte offset. `MidiError::code()` returns stable codes
such as `midi_bytes_limit`, `midi_events_limit`, `midi_payload_limit` and
`midi_extent_limit`. `midi_nonregular_input` and `midi_input_io` identify file
failures. Library callers can pass smaller limits through the `*_with_limits`
APIs.

Exceeding a limit returns an error. The importer never returns a truncated or
partly committed project. The limits do not change how the application reads
at 480 PPQ or how the dataset profile keeps the source PPQ, and they do not
change note gates, controller conversion or program identity.
