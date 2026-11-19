# Live MIDI

`oc_engine::midi_io` handles controller input and MIDI output: thru, generated
notes and a 24-PPQN clock. It also provides a deterministic fake backend for
tests.

## Backends

- `midir` 0.11 does the platform I/O: CoreMIDI on macOS, WinMM on Windows,
  ALSA on Linux. Virtual ports are available on Unix, and the CoreMIDI loopback
  test uses them.
- `rtrb` 0.3 provides the preallocated single-producer, single-consumer queue
  between the MIDI thread and the audio callback.
- `FakeBackend` implements the same `MidiBackend` trait, for tests and for
  monitoring without hardware.

`MidiBackend` is the only platform boundary:

```rust
fn list_inputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
fn list_outputs(&self) -> Result<Vec<MidiPortInfo>, MidiError>;
fn open_input(&mut self, port: &MidiPortInfo) -> Result<InputId, MidiError>;
fn open_output(&mut self, port: &MidiPortInfo) -> Result<OutputId, MidiError>;
fn close_input(&mut self, input: InputId);
fn close_output(&mut self, output: OutputId);
fn send(&mut self, output: OutputId, message: &[u8]) -> Result<(), MidiError>;
fn poll(&mut self, input: InputId, out: &mut [MidiInputEvent]) -> Result<usize, MidiError>;
```

Errors are typed as `MidiError`: backend unavailable, port not found, port
already open, permission denied, disconnected, input buffer overflow, invalid
message and generic I/O. Backend errors are mapped at the boundary, and a
missing device never causes a panic.

## Input path

1. The `midir` input callback copies each message of 1 to 3 bytes into a
   preallocated ring of 1024 slots. It does not parse, lock or allocate. If the
   ring fills up, the callback stops queueing for that connection, because a
   gap could lose a note release. `poll` then returns the intact events
   already queued, followed by an input buffer overflow error.
2. A MIDI thread drains the ring in `MidiBackend::poll`. It parses note on and
   off, poly and channel pressure, CC, program change, pitch bend, clock and
   start, continue and stop into fixed-size `LiveEvent` values.
3. The same thread pushes the events into `midi_io::event_queue`, a bounded
   `rtrb` queue whose producer end is the `LiveEventProducer`.
4. The audio callback owns the `LiveEventConsumer`. It drains at most 256
   events per block and applies them to `OpenSynth` through `send_midi`. The
   callback does not allocate or lock, and its work per block is bounded. A
   push into a full queue returns the event at once and never blocks.
   `ocp play` then stops live input with "Live MIDI synth queue overflowed".

A note-on with velocity zero becomes a note-off before it reaches the
instrument.

## Output path

- Thru: `MidiSession::forward` sends the polled input events to the selected
  output byte for byte (`pack_into`).
- Generated notes: `OutputScheduler` reads the interpreter's `MidiEvent` list
  and sends each event whose sample time is due at the playback cursor. A
  14-bit CC is split into two wire events.
- Clock: `MidiClock` sends 24 ticks per quarter note, derived from the playback
  position. `set_clock_enabled` turns it off and on without restarting the
  session.

The scheduler runs on the MIDI thread, where CoreMIDI, WinMM and ALSA sends
may block. Clock jitter depends on the polling interval (1 ms in the CLI); the
audio callback does not affect it.

## CLI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp --midi-in "IAC Driver Bus 1"
ocp play song.ocp --midi-in keys --midi-out synth
ocp play song.ocp --midi-in keys --midi-out synth --midi-clock --midi-thru
```

`ocp midi list` prints the input and output ports with their indices. Wherever
a port is expected, you can give its name or its index.

`ocp midi monitor` prints each event with a timestamp:

```text
[ 12.345678] note on   ch=1 C4   vel=100
```

`ocp play` renders the project offline, then streams it through the output
device and period saved in the application's audio settings
(`~/.opencompose/audio.json`). Live input plays the first OpenSynth preset in
the project, or a default patch if there is none. With `--midi-out`, project
events are sent as they fall due. `--midi-clock` turns on the 24-PPQN clock and
`--midi-thru` forwards the controller to the output.

`ocp play` uses the per-track render path, so built-in, OpenSynth and
SoundFont tracks play. CLAP and VST3 instrument tracks need the offline plugin
render path and are not part of live play. The MIDI clock follows the
project's base tempo only, not its tempo map.

## In the application

Choose the MIDI input on the Audio page of [Preferences](preferences.md). The
commands are in `crates/oc-gui/frontend/src-tauri/src/midi_live.rs`:

- `list_midi_ports` returns the input and output ports with index, name and an
  opaque id. Without a MIDI backend it returns `supported: false` and a message
  that the page shows inline. It never opens a dialog.
- `get_midi_config` and `set_midi_config` read or apply `{ input, monitor }`,
  plus the optional live MPE zones (`mpe`).
- `get_midi_activity` returns the last channel event, the event count, the
  thread status and the current error. The page polls it at 4 Hz while
  monitoring.

The state lives in `AppStateInner::midi_live`: the selection, the input thread
and the lock-free activity probe. It is kept apart from the audio settings in
`AudioState`. The selection is saved to `~/.opencompose/midi.json`, for example
`{ "input": "Keys", "monitor": true }`. A missing or corrupt file falls back to
the defaults. A failed write is shown inline.

The MIDI thread pushes the parsed `LiveEvent`s into the open OpenSynth
audition, and into the MIDI take when one is recording. You hear the input
only while an audition is open, from the patch editor's keyboard or a preset
audition. Otherwise the page shows **Open a synth patch to hear live input**
and the activity display still updates. This keeps a single owner for the
output stream and leaves transport playback alone.

A MIDI take opens the selected input even while **Live input monitor** is off,
and releases it again when the take ends.

### Threading

Each enabled input gets one MIDI thread. The thread owns `MidirBackend` and
`MidiSession`, polls every 1 ms into a fixed buffer of 64 events, parses on
that thread, and updates the activity probe with atomics.

The thread then pushes `Command::Live` into the audition's shared producer
slot, `live_synth::Route` (`Arc<Mutex<Option<CommandProducer>>>`). The mutex is
held only for the push, or for the swap when an audition starts or stops. The
command ring has 256 slots. Every note-on it admits keeps one slot free for the
matching release, so a full ring drops new presses on the MIDI side and still
delivers releases.

The audio callback owns the consumer, drains at most 256 commands per block and
applies them to `OpenSynth`. The callback never allocates, locks, creates a
thread or blocks.

When the input stops, the thread sends a note-off for every note still held and
releases a held sustain pedal, so nothing keeps sounding on the audition synth.

Starting, stopping or changing the device joins the MIDI thread while the GUI
holds the `AppState` lock. This is safe because the MIDI thread never takes
that lock.

### Behaviour

- The port list is read again each time you open the Audio page and when you
  press the refresh button (↻). There is no background hot-plug watcher. A
  saved device that has gone stays selected and is labelled "(unavailable)"
  until you change it.
- If the device fails to open (missing port, permission, no backend), the
  selection is still saved and the error is shown inline. **Live input
  monitor** is disabled while no input is selected.
- At startup the frontend store (`stores/midiLive.ts`) restores the saved
  selection. If `monitor` was true it resumes monitoring, after checking the
  device against the ports present at that moment.
- The activity display shows the last note and velocity, or the last CC,
  program or bend event. Realtime clock ticks never replace the last event.

## Tests without hardware

Run `cargo test -p oc-engine midi_io` for the core and
`cargo test -p oc-gui --lib midi_live` for the GUI bridge. The GUI tests use
`FakeBackend` and a recording sink, so they need no device:

- `midi_live::tests::port_entries_preserve_index_name_and_id`
- `midi_live::tests::preferences_round_trip_and_missing_file_uses_defaults`
- `midi_live::tests::corrupt_preferences_fall_back_to_defaults`
- `midi_live::tests::selection_persists_without_opening_a_device`
- `midi_live::tests::absent_backend_is_reported_without_panicking`
- `midi_live::tests::input_thread_routes_events_and_releases_after_stop_or_overflow`
- `midi_live::tests::missing_port_is_reported_by_the_runtime`
- `midi_live::tests::activity_ignores_clock_and_keeps_the_last_note`
- frontend: `__tests__/stores/midiLive.test.ts` and
  `__tests__/components/ToolbarMidiLive.test.tsx`

The core `midi_io` tests cover:

- message round trips and malformed input;
- queue bounds, ordering and the per-block budget;
- the 24-PPQN clock and the scheduler's due events;
- `FakeBackend` through `session::tests::*`;
- `midir_backend::tests::missing_port_is_a_typed_error`;
- the macOS-only `coremidi_virtual_loopback`, which skips when virtual ports
  are unavailable.

## What needs real hardware

- Enumerating physical controllers, hot-plug and disconnect, and permission
  prompts.
- Hearing input through the audition synth, and measuring latency and jitter
  through a real audio interface and DIN or USB MIDI.
- WinMM and ALSA behaviour (the loopback test runs on CoreMIDI only).
- Clock sync with external gear, and thru fidelity with a real synth.
