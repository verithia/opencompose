# Live MIDI I/O

`oc_engine::midi_io` adds controller input, MIDI output (thru, generated
notes, and 24-PPQN clock), and a deterministic fake backend for tests.

## Backend choice

- **`midir` 0.11** for platform I/O: CoreMIDI on macOS, WinMM on Windows, ALSA
  on Linux. Virtual-port APIs are available on Unix and are used by the CoreMIDI
  loopback test.
- **`rtrb` 0.3** for the preallocated SPSC queue between the MIDI thread and the
  audio callback.
- `FakeBackend` implements the same `MidiBackend` trait for tests and for
  hardware-free monitoring.

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

Errors are typed (`MidiError`): unavailable backend, missing port, port already
open, permission denied, disconnected, invalid message, and generic I/O. Backend
errors are mapped at the boundary; no path panics on a missing device.

## Input path

1. The `midir` input callback copies one 1-3 byte message into a 1024-slot
   preallocated ring. It never parses, locks, or allocates.
2. A MIDI thread drains that ring in `MidiBackend::poll` and parses note on/off,
   poly/channel pressure, CC, program change, pitch bend, clock, and
   start/continue/stop into fixed-size `LiveEvent` values.
3. The same thread pushes events into `midi_io::event_queue`, a bounded
   `rtrb` SPSC queue owned by the `LiveEventProducer`.
4. The audio callback owns the `LiveEventConsumer`, drains at most 256 events per
   block, and applies them to `OpenSynth` through `send_midi`. The callback does
   no allocation, no locking, and no unbounded work. A full queue drops events
   instead of blocking.

Note-on with velocity zero becomes note-off before it reaches the instrument.

## Output path

- **Thru**: `MidiSession::forward` resends polled input events to the selected
  output byte-for-byte (`pack_into`).
- **Generated notes**: `OutputScheduler` consumes the interpreter's `MidiEvent`
  list and sends every event whose sample time is due at the playback cursor.
  14-bit CC is split into two wire events.
- **Clock**: `MidiClock` emits 24 ticks per quarter note from the playback
  position. `set_clock_enabled` turns it off/on without restarting the session.

The scheduler runs on the MIDI thread, where CoreMIDI/WinMM/ALSA sends are
allowed to block. Clock jitter is bounded by the polling interval (1 ms in the
CLI), not by the audio callback.

## CLI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp --midi-in "IAC Driver Bus 1"
ocp play song.ocp --midi-in keys --midi-out synth
ocp play song.ocp --midi-in keys --midi-out synth --midi-clock --midi-thru
```

- `midi list` prints input and output ports with indices; names or indices are
  accepted as selectors.
- `midi monitor` prints timestamped events (`[ 12.345678] note on ch=1 C4 vel=100`).
- `play` renders the project offline and streams it through the default audio
  device. Live input plays the first OpenSynth preset in the project (or a
  default patch when none exists). With `--midi-out`, project events are sent as
  they become due; `--midi-clock` enables 24-PPQN clock and `--midi-thru`
  forwards the controller to the output.

`play` uses the per-track render path, so built-in/OpenSynth and SoundFont
tracks work; CLAP/VST3 instrument tracks need the offline/plugin render path and
are not yet part of live play. Tempo-map output follows the project's base
tempo only.

## GUI wiring

The Tauri preferences popover owns live input selection and monitoring. The
commands live in `crates/oc-gui/frontend/src-tauri/src/midi_live.rs`:

- `list_midi_ports` — input/output ports with index, name, and opaque id. A
  missing backend returns `supported: false` plus an inline message; it never
  raises a dialog.
- `get_midi_config` / `set_midi_config` — read or apply `{ input, monitor }`.
- `get_midi_activity` — last channel event, event count, thread status, and the
  current error, polled by the UI at 4 Hz while monitoring.

State lives in `AppStateInner::midi_live` (selection, runtime thread, and the
lock-free activity probe), not in `AudioState`'s audio settings. Preferences are
persisted to `~/.opencompose/midi.json` as
`{ "input": "Keys", "monitor": true }`. Missing or corrupt files fall back to
defaults; write failures are shown inline.

Routing: the MIDI thread pushes parsed `LiveEvent`s into the active OpenSynth
audition. Input is audible only while an audition is open (the patch editor's
keyboard or a preset audition); otherwise the UI shows a hint and activity still
updates. This keeps one output stream owner and avoids interrupting transport
playback.

## Threading contract

One MIDI thread per enabled input owns `MidirBackend` + `MidiSession`, polls
every 1 ms into a fixed 64-event buffer, parses events on that thread, and
updates the activity probe with atomics. It then pushes `Command::Live` into
`AudioState`'s shared producer slot (`Arc<Mutex<Option<Producer<Command>>>>`) —
a short mutex held only for the push or for an audition start/stop swap. The
audio callback owns the consumer, drains at most 256 commands per block, and
applies them to `OpenSynth`. The callback never allocates, locks, creates a
thread, or blocks; a full ring drops the event on the MIDI side. Starting,
stopping, or changing devices joins the MIDI thread while the GUI holds the
`AppState` lock, which is safe because the MIDI thread never takes that lock.

## GUI behaviour

- Ports are re-enumerated when the preferences popover opens and on the refresh
  button. There is no background hot-plug watcher; a saved device that vanished
  stays selected and is labelled "(unavailable)" until changed.
- If opening the device fails (missing port, permission, no backend), the
  configuration is still persisted and the error is shown inline. The monitor
  toggle is disabled when no input is selected.
- The frontend store (`stores/midiLive.ts`) restores the saved selection on
  startup and resumes monitoring when `monitor` was true, re-validating the
  device against the ports present at that moment.
- Activity shows the last note and velocity (or the last CC/program/bend event)
  and never shows realtime clock ticks as the last event.

## Tests without hardware

Run `CARGO_TARGET_DIR=... cargo test -p oc-engine midi_io` for the core
and `cargo test -p oc-gui --lib midi_live` for the GUI bridge. The GUI tests use
`FakeBackend` and a recording sink, so they need no device.

- `midi_live::tests::port_entries_preserve_index_name_and_id`
- `midi_live::tests::preferences_round_trip_and_missing_file_uses_defaults`
- `midi_live::tests::corrupt_preferences_fall_back_to_defaults`
- `midi_live::tests::selection_persists_without_opening_a_device`
- `midi_live::tests::absent_backend_is_reported_without_panicking`
- `midi_live::tests::input_thread_routes_events_and_records_activity`
- `midi_live::tests::missing_port_is_reported_by_the_runtime`
- `midi_live::tests::activity_ignores_clock_and_keeps_the_last_note`
- frontend: `stores/midiLive.test.ts`, `components/ToolbarMidiLive.test.tsx`

Core `midi_io` (unchanged): message round-trip and malformed-input tests, queue
bounds/ordering/budget, clock 24-PPQN, scheduler due events, `FakeBackend`
through `session::tests::*`, `midir_backend::tests::missing_port_is_a_typed_error`,
and the macOS-only `coremidi_virtual_loopback` (skips when virtual ports are
unavailable).

## What needs real hardware

- Physical controller enumeration, hot-plug/disconnect, and permission prompts.
- Audible input through the audition synth and latency/jitter measured through
  a real audio interface and DIN/USB MIDI.
- WinMM and ALSA behavior (the loopback test is CoreMIDI-only).
- External gear clock sync and thru fidelity with a real synth.
