# Streaming preview latency and mute freezes

This page explains how a space press reaches sound quickly and why a mute does
not freeze the window. The figures come from the 8-track / 22 s 4 × VST3
latency project (`compare.ocp`) on the reference machine described in
[Performance targets](performance-targets.md). They show what each path costs;
the budgets, not these numbers, are the contract.

## Constraints

1. **A full project load is expensive.** `RenderState::load_project` rebuilds
   every instrument; on `compare.ocp` that re-instantiates four VST3 instances
   (~0.3–0.9 s). A mixer edit therefore must not send
   `RenderCommand::ProjectChanged`.
2. **A synchronous Tauri command runs on the macOS webview thread.** Any wait
   inside `transport_play` freezes the window, and a user who presses space
   again to retry pays the wait again. `play()` therefore never waits for a
   load or for the first chunk.
3. **A producer that waits for a callback ack without a bound can wedge.** A
   render loop that publishes a chunk only once
   `callback_generation == seek_generation` sleeps forever when the callback
   never acks: a callback that returns early while `active == false`, or a
   stream that is not running at all, never sets `ready`. The handshake below
   is bounded for that reason.
4. **Committed lookahead delays an edit.** A mixer edit is heard only after the
   audio already in the ring, so a ring filled to its full 2 s capacity delays
   the edit by up to two seconds.

## Cost of a full load

Temporary phase timers (`load_project` / `build_project`) on `compare.ocp` at
48 kHz:

| Phase | Cold | Warm |
|---|---|---|
| `preset::resolve` | 0.03–0.1 ms | 0.03–0.1 ms |
| preset parse / `plugin_preset_render_error` | 60–125 ms | 0.05–0.2 ms |
| split lanes + interpret events (×2) | 60–125 ms | 0.2–0.5 ms |
| **instrument instantiation (4 × VST3)** | **310–890 ms** | 310–890 ms |
| graph build | 0.1–1.2 ms | 0.1–1.2 ms |
| **total `load_project`** | **340–960 ms** | 340–960 ms |
| first 4096-frame chunk | 20–45 ms | 20–45 ms |

Instrument instantiation is ~90 % of the load, and a `ProjectChanged` pays it
even when nothing structural changed.

## Design

- **`RenderCommand::MixerChanged`** (`render_thread.rs`) carries the edited
  project. `RenderState::apply_mixer` patches `AudioGraph` routing in place
  (`TrackMix` + `AudioGraph::apply_mixer_state`). Instruments, voices and DSP
  history are untouched. If a track that was muted (and therefore pruned) at
  load becomes audible, the lane is built on a dedicated source worker
  (`RenderThreadHandle` spawns `ocp-source-build`) and the render thread only
  splices the finished source in at the current DSP position; no instrument is
  ever instantiated on the producer and the other lanes are never rebuilt.
  `ProjectChanged` remains for structural edits.
- `commands.rs` splits `resync_with_undo` into `AudioSync::Full` (structural)
  and `AudioSync::Mixer` (`update_track_mix`, `set_track_mute`,
  `set_track_solo`).
- Solo resolves to an effective mute in the graph, at build time and when
  patched, so streaming and offline agree.
- **Short first chunk.** The first chunk of a playback generation is 1024
  frames (`FIRST_CHUNK_FRAMES`), then steady state is 4096. The producer keeps
  at most ~250 ms queued (`LOOKAHEAD_FRAMES = 12000`), so a mixer edit is
  audible within one lookahead window instead of up to 2 s.
- **Bounded handshake.** The producer waits at most 250 ms
  (`ACK_TIMEOUT`) for the callback to acknowledge a seek generation, then
  publishes anyway. The callback checks the generation *before* the
  `active` flag, so a paused-but-running stream still acks and a later resume
  does not have to wait. `Play` invalidates before enabling output.
- **Non-blocking play.** `play()` dispatches `ProjectChanged`/`Play` and
  returns; `TransportSnapshot` reports `starting` and, after a 5 s deadline,
  `status` while the transport pauses instead of spinning. A `Play` resume flag
  lets the worker keep the intact lookahead instead of seeking (and reloading)
  when only cursor drift separates the UI and worker positions.
- VST3 projects preload in the background; only CLAP projects with live
  editor state still defer to `play()`.

## Live playhead and chain latency

The streaming position is `StreamingPlayback::play_cursor_frames`: the callback
adds the frames it hands to the device. Two constant delays separate that count
from the sound, and a picture drawn from the raw count carries neither of them.

1. **Device period.** A callback fills the buffer that plays one period later.
   cpal's CoreAudio backend defines the timestamp as `playback = callback + one
   callback buffer`, so the presentation delay of a block is the callback block.
2. **Live chain latency.** The offline mix trims the master chain's reported
   latency at the head of the aligned buffer (`render_cache/cache.rs`) and the
   graph's PDC delays every lane to the slowest path before the master. The
   streaming mixer (`AudioGraph::mix_with_transport`) does neither: it leaves the
   shift in the audio and the cursor keeps the old count, so a master tuner
   (1 920 frames, 40 ms) or spectral suppressor (2 304 frames, 48 ms) moves the
   sound later while the playhead and the meters keep going.

So a picture drawn from the raw count runs ahead by `device period + pre-master
PDC + master chain`, which is a property of the loaded project, not a fudge
factor.

### Measured

`render_thread::tests::review_cursor_to_sound_latency` (ignored; run it with
`--ignored --nocapture`) builds a project whose only event is a sixteenth of
`builtin.test_tone` at bar 1 beat 3 (project frame 48 000 at 48 kHz, 120 bpm),
plays it through the real render worker with the test thread standing in for the
device callback, and finds the click's first sample in the delivered stream.
The audio shift is measured, the chain column is the graph's own report, and the
two lead columns are the raw cursor and the compensated position read at the
instant that block is handed to the device.

| Project (48 kHz) | Measured audio shift | Reported chain | Raw cursor lead | Compensated lead |
|---|---|---|---|---|
| builtin, no effects | +1 frame (0.02 ms) | 0 | 8.00 ms | +2.67 ms |
| master limiter, 20 ms lookahead | +961 frames (20.02 ms) | 960 frames (20.00 ms) | 29.33 ms | +4.00 ms |
| tuner as a track insert | +1 922 frames (40.04 ms) | 1 920 frames (40.00 ms) | 50.67 ms | +5.33 ms |
| master tuner + suppressor | +4 226 frames (88.04 ms) | 4 224 frames (88.00 ms) | 98.67 ms | +5.33 ms |

The audio shift is the reported latency to within two frames: one sample because
the tone's first sample is exactly zero and PSOLA places its first grain within
half a period. The lead columns are read at block boundaries, so both carry up to
one device block (5.33 ms at 256 frames) of slack: the cursor only advances when
a callback returns. The compensated column is therefore not an error figure but
the residual of a block-stepped clock, and the frontend interpolates between
polls.

The headline is the two ends of that table: the raw cursor leads the sound by
8.00 ms with no effects at all, 29.33 ms behind a 20 ms master limiter, 50.67 ms
with a tuner insert and 98.67 ms with the tuner plus the suppressor on the
master; the compensated position trails by at most one device block in every
case.

A track insert proves the PDC half: the tuner's 1 920 frames are applied to one
lane, PDC delays the other lanes to match, so the whole mix and the picture
disagree by 40 ms even though the plugin is not on the master.

### What is compensated

`StreamingPlayback::monitor_latency_frames()` is the single quantity:

```
DEVICE_LATENCY_BLOCKS (= 1) * device_buffer_frames + chain_latency_frames
```

- `device_buffer_frames` is measured by the audio callback on every block
  (`output.len() / 2`).
- `chain_latency_frames` is published by the render thread after every mixed
  chunk and on project load: `graph.pre_master_latency_samples() +
  graph.master_chain.latency_samples()`.
- `StreamingPlayback::audible_frames()` is `play_cursor_frames` less that
  quantity, saturating at zero. `transport_snapshot` reports it as
  `position_beats`/`position_seconds`, so the playhead, the position readout and
  everything derived from them move together. The end-of-project transition
  still watches the raw cursor.
- Meters (and the analyser overlay) are captured per mixed chunk and held in a
  bounded queue until `play_cursor_frames - device_latency` reaches the chunk, at
  which point they describe audio the device is playing. The chain latency
  cancels in that condition: the chain shifts the meters and the playhead by the
  same amount.
- The 250 ms lookahead (`LOOKAHEAD_FRAMES = 12000`) is untouched, and the offline
  renderer is not touched at all: it already aligns sources and trims the master
  head.

### The metronome reference

The metronome is audition material. The mixer sums it after the master chain, so
it never enters a render or an export and never passes the chain's own
processing. It does have to stand on the same reference as the backing audio: if
it names the timeline while the mix names the timeline minus the chain, the two
disagree by the whole chain latency, and a performer who tracks the click is
written that far away from a performer who tracks the backing track.

The click therefore sounds on the window the block's audio occupies.
`mix_click` reads its events from `start_frame - chain_latency_frames` instead
of `start_frame`, and `render_count_in_chunk` shifts the count-in by the same
amount, so the last count-in beat and the first timeline beat stay exactly one
beat apart. A run that begins inside the delay reads its window from frame zero
and moves the events to the head by the missing amount, which is what the first
chunk of a play does. With no latency in the chain the shift is zero, and a
bypassed or empty master chain sounds the click on the frame it always did.

The record path uses the same reference. An audio take trims the count-in,
one capture block and `input_latency_frames()`, which is
`monitor_latency_frames() + one capture block`; that monitor quantity is the
device period plus the chain. A MIDI take trims the roll wait and the same
monitor latency, because its playhead is `audible_frames()`. A performer who
plays with the click is therefore written on the beat within one device block,
and a performer who plays with the backing track gets the same answer, because
the click and the backing carry the same delay.

The level and the timbre are untouched: the reference decides when a click
sounds, never how it is generated or summed.

### Where the latency is reported

`transport_snapshot` carries `monitor_latency_seconds`: the same quantity above
divided by the sample rate, so a client can show or log what the position is
being held back by instead of re-deriving it from a constant. The position
fields are already compensated, so a client that adds the field to the cursor
would double-count it.

The number and the position both come from the engine, not the UI. The frontend
draws `smoothPositionBeats`, which is anchored to the polled `position_beats` at
20 Hz and interpolated with `performance.now()` between polls; the engine's
sample clock wins, and the interpolation only removes the poll's stair-step (up
to one device block). The anchor is one command round-trip old, which is well
under a video frame.

### What was derived rather than measured

- **One block of presentation delay.** The simulator has no DAC, so the harness
  prices the audio at the handoff. The one-block offset comes from cpal's
  CoreAudio contract (the playback timestamp is the callback plus one buffer).
  Device safety offsets and converter group delay are not in the number; a real
  device can only make the sound later, never earlier.
- **The chain number is the graph's own report**, validated against the audio to
  within two frames for these devices rather than re-derived per plugin. A
  third-party plugin that misreports its latency is still misreported here; the
  graph's PDC has the same exposure offline.
- **Hardware was not available** in this measurement: everything above ran on the
  production callback with the simulated sink. Confirming the absolute offset on
  a DAC needs the app, which a headless task must not open.

### Consequences of the compensation

- The position is continuous across play/pause: a play from zero sits at 0 for
  the first `monitor` frames, and a pause freezes the audible position, which is
  `monitor` frames before the transport resumes from. Edits anchored at the
  playhead (paste, split) therefore land where the sound was.
- Not compensated: a buffered preview (the A/B pair, which is rendered as two
  aligned takes). It plays an offline render, which is aligned at the head, but
  its one device period is not held back; the snapshot reports
  `monitor_latency_seconds = 0` there. The exported file keeps its existing
  alignment.

Regression tests without hardware:

- `render_thread::tests::monitor_latency_is_the_device_block_plus_the_chain`
- `render_thread::tests::audible_position_trails_the_ring_by_the_monitor_latency`
- `render_thread::tests::live_playhead_does_not_lead_the_click`
- `render_thread::tests::meters_are_held_until_the_device_reaches_them`
- `render_thread::tests::the_click_and_the_backing_share_the_master_chain_reference`
- `render_thread::tests::a_click_aligned_midi_note_records_on_the_beat_through_a_chain`
- `render_thread::tests::the_click_keeps_its_frame_wherever_the_block_starts`
- `render_thread::tests::a_bypassed_master_chain_sounds_the_click_where_an_empty_one_does`
- `render_thread::tests::the_count_in_and_the_first_timeline_beat_stay_one_beat_apart`
- `audio::tests::transport_position_lags_the_engine_clock_by_the_monitor_latency`

The offline path does not apply this compensation, so the render gate holds: two
renders of each fixture at 48 kHz with `--no-loudness-normalisation` are
byte-identical, 4,248,432 bytes, primary
`2f662624361a3cd429112b70eb27fe6bf049ba9afb6d3d897545357296aa5eb7` (md5
`d6dd552a5a33391bd5c1459e0dd00f61`) and hot
`91a737607d4cef8f74450fb470db5e28e886198107c5f347573145e148954633` (md5
`52207cb9304446634a8538ea62d3f5bb`).

## Warm-up on project open

Opening a project starts the warm-up, because a press that finds nothing built
pays for all of it: the plugin lookup (60–81 ms on a cold inventory), the
instrument instantiation and graph (308–771 ms for 4 × VST3), the first chunk
(10–12 ms), and then the device itself. Staging rendered audio in the worker is
not enough on its own. A press that still has to start the audio stream,
handshake with a fresh callback and copy staged chunks into an empty ring waits
for hardware, and a simulated sink prices none of that: it reports 2.7–4.4 ms
for a press the app does not deliver. The warm-up therefore covers four things:

- **The device is open.** `open_streaming_output` and the CoreAudio start run at
  open, not inside the press.
- **The ring holds audio.** The opening chunks are written into the ring, not
  into a staging queue that the press copies behind an invalidation and a
  generation ack.
- **Plugin lookup is memoized,** so neither the press nor a later build rescans
  the inventory.
- **Preset→VST3 state is parsed once per file version,** not for every instance
  and every build (the 60–125 ms "preset parse" line in the breakdown above).

### What the preload does

1. `preload_project` starts the streaming output **before** dispatching the warm
   build. The callback runs but `active` is false, so it outputs silence; nothing
   is audible until a press. A stream that cannot start is logged and the press
   path falls back to starting it on the press.
2. The worker builds sources and the graph, then writes the opening
   `LOOKAHEAD_FRAMES` (12 000 frames, ~250 ms at 48 kHz) **into the ring**, one
   chunk per loop iteration. It waits for the consumer to acknowledge the current
   seek generation first; a project change bumps the generation, the callback
   flushes, and the refill starts only after the ack. With no consumer at all
   (no output device) it falls back to staging in the worker's DSP queue.
3. Pressing play with a warm ring at the same position only sets `active` and
   increments `play_applied`: no seek, no invalidation, no render, no device
   start. A seek or a stopped transport still goes through the normal invalidate
   path.
4. `find_plugin_info` memoizes resolved bundles per normalized id + format;
   `clear_resolved_plugin_cache()` runs on an explicit rescan. The parsed
   Preset→VST3 state is cached per file version (path + mtime + length).
5. `TransportSnapshot` reports `warming`/`prepared`; the frontend keeps its
   loading chip up while `warming || starting` and clears it when the worker has
   audio. A press during the build returns immediately and the chip stays.

### New and recovery follow the same path

File ▸ New installs the document, answers, and starts this warm-up behind the
response exactly as Open and Recover autosave do. The workspace's **New project**
route is one command: it names the project after the chosen file, writes it, and
warms once, instead of New followed by a rename and a save, each of which rebuilt
the graph. The warm-up re-checks the document revision under the state lock, so a
second New or an edit that lands in between wins and the queued warm-up does
nothing. A press issued while the warm-up runs queues behind it for the device
open and the dispatch, then streams; the build itself continues on the worker.

### Measurement harness

Ignored test `audio::tests::review_open_to_first_sound_latency` drives the real
`AudioState` path: parse → `preload_project` (the same call `replace_project`
makes) → wait for `WARM_READY` → press → pause → second press, plus a press
issued immediately after open. `OCP_PROFILE_PLAYBACK=1` prints the per-phase
timers; the harness prints `[measure]` lines. The sink is the existing simulated
consumer: same worker, same ring and the production callback, no audio hardware.

```sh
export CARGO_TARGET_DIR=target/ui-first-play
export OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-ui-first-play.json
export OCP_STREAM_PROJECT=/path/to/compare.ocp
export OCP_PROFILE_PLAYBACK=1
cargo test -p oc-gui --lib review_open_to_first_sound_latency -- --ignored --nocapture
```

`compare.ocp`, 48 kHz, 4 × VST3. Ranges cover repeated runs.

| Phase | Measured |
|---|---|
| project read + parse (open) | 2.3–5.7 ms |
| `preload_project` prepare (resolve paths) | 0.03–0.12 ms |
| preload dispatch to worker | 0.01 ms |
| preload starts the output stream | 0.01–0.02 ms (simulated) |
| worker `preset::resolve` | 0.03–0.05 ms |
| worker plugin lookup / render check | 59–81 ms cold, then 0.02 ms |
| split lanes + interpret events | 0.2–0.5 ms |
| build sources (4 × instantiate + state) | 274–362 ms |
| graph build | 0.1–0.3 ms |
| warm first chunk | 24–27 ms |
| warm prefill total (~250 ms audio) | 61 ms |
| **open → ready** | **0.42–0.47 s** |
| ring holds audio at press | **yes** |
| consumer running at press | **yes** |
| press return (UI thread) | 0.14–0.17 ms |
| **space → first audible sample** | **5.0–7.6 ms** |
| second press: return / audible | **0.12–0.17 ms / 1.3–2.5 ms** |
| press immediately after open: return / audible | 0.10–0.15 ms / 300–311 ms |
| transport while that press waits | warming + starting, then clears |

The first press is the same order as the second (ms, not hundreds of ms).
The remaining difference is callback scheduling in the simulated sink (2 ms
sleep, 256-frame blocks); on hardware the first audible sample lands on the next
device interrupt, which is why starting the device at load matters more than the
5 ms the simulator shows.

### Honest limits

- **4 × VST3 instantiation cannot move earlier than load.** The worker spends
  284–362 ms constructing the instances and applying preset state, already off
  the UI thread at open. A press issued *before* that finishes still waits for
  the remaining build (300–311 ms in the harness): the transport reports
  `warming`/`starting`, the UI shows the loading chip, and nothing freezes. It
  is not possible to sound a VST3 instrument before the plugin exists.
- **CLAP projects still defer.** Their live editor state is captured from the
  running instance by `play()`, so `preload_project` returns early for them
  rather than warm a state the press would have to replace. VST3 presets carry
  their state in the params and are preloaded.
- **The real CoreAudio stream starts at open**, so the harness cannot price
  it (the simulated sink has no device). What is proven here is that the press
  path does not open a stream, render a chunk, look up a plugin or copy
  staging; the phase table's 5–7.6 ms is an upper bound for the callback to pick
  up already-committed ring audio.
- A project with no plugins warms on the same path: `opensynth_showcase.ocp`
  builds in 97–169 ms, and the warm-up adds one device start (a few ms) and the
  write of the lookahead into the ring.

## Edit and play paths

`compare.ocp`, 48 kHz, simulator output (same worker, ring and callback as the
device), measured by the ignored test
`audio::tests::review_edit_to_first_chunk_latency`.

| Measurement | Measured |
|---|---|
| cold play(), UI thread | **0.2–0.3 ms** |
| cold first chunk ready | 439–595 ms (4 × VST3) |
| mute while playing | **3–4 ms dispatch, 0 loads, 0 rebuilds** |
| mute → play (resume) | **2 ms return, audible 0 ms, 0 rebuilds** |
| first unmute of a pruned track | **one lane built, 21–75 ms on the source worker, no reload** |
| structural edit → play | **1–3 ms return**, 353–1142 ms background build |

Success target for an 8-track project with 4 VST3 instances: first audible
chunk ≤ 600 ms cold and ≤ 100 ms warm/after a mixer edit. Cold is met
(439–595 ms, of which 310–500 ms is unavoidable plugin instantiation); warm and
mixer-edit paths are met at 0–4 ms.

The offline renderer does not take these paths: the debug build of `ocp render
opensynth.ocp --sample-rate 48000 --no-loudness-normalisation` hashes
`97ea098eaca5473ac1a90c345808e6b000caa9744811cbb251621eb045c492a4` (two renders
identical; 4,248,432 bytes).

## Residual costs

- Cold plugin instantiation (~0.3–0.5 s) is unavoidable and happens in the
  background; the first chunk follows it.
- The first unmute of a track that was muted when the project loaded builds
  that track's instrument only, on the dedicated source worker. The producer
  keeps rendering the lanes it owns while the worker builds, so the ring does
  not drain and the callback does not underrun; the unmuted lane stays silent
  until the finished source is spliced in (debug-build OpenSynth patches that
  import presets can take seconds). Splice time is the bounded graph rebuild a
  structural edit already pays; later mute/unmute cycles are free.
- The 250 ms lookahead (`LOOKAHEAD_FRAMES = 12000`) is the
  accepted mixer-edit bound, not a budget for building instruments.
- A mixer edit is heard after the current lookahead (≤ 250 ms), because chunks
  already in the ring are committed. The warm ring has the same ordering: a
  mute/volume/pan change made *before* the first play patches the graph, and the
  prefilled ≤ 250 ms keeps the mix it was rendered with, so the edit lands as the
  producer continues. It is not re-mixed, because that would need the per-source
  history the warm-up replaced.
- Structural edits rebuild instruments and preroll to the playhead in the
  background; the UI does not block, but audio may gap.
- A project that cannot stream (rejected by `supports_block_sources`) is refused
  with the offending tracks named, and the press returns that error. There is no
  whole-project buffer fallback: every playback path either streams or
  refuses, so no press can pre-render the song first. The export path still
  renders the whole project, and the A/B compare path still renders both
  candidates before it plays them (it needs two aligned takes to switch between),
  but neither is on a transport start.
- The full-song export follows the same shape. `render_offline` is `async`: the
  snapshot is taken on the command's task, the render and the staged WAV write
  go to Tauri's blocking pool, and the invoke resolves with the path once the
  file is published. The webview thread stays free, the status bar shows the
  `rendering...` chip, and the toolbar's render button stays enabled; a second
  export started meanwhile supersedes the first. The remaining cost is the
  render itself.

## Reproduce

```sh
export CARGO_TARGET_DIR=target/preview-latency
export OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-preview.json
export OCP_STREAM_PROJECT=/path/to/compare.ocp
cargo test -p oc-gui --lib review_edit_to_first_chunk_latency -- --ignored --nocapture
cargo test -p oc-gui --lib review_open_to_first_sound_latency -- --ignored --nocapture
```

Set `OCP_PROFILE_PLAYBACK=1` for the per-phase timers used by the tables above.

Regression tests that run without plugins:

- `render_thread::tests::mixer_change_patches_graph_without_reloading_sources`
- `render_thread::tests::unmuting_a_pruned_track_defers_the_build_to_the_source_worker`
- `render_thread::tests::unmuting_a_pruned_track_does_not_underrun_the_callback`
- `render_thread::tests::stream_that_never_acks_cannot_wedge_the_producer`
- `first_block::tests::*` and `first_block_tests::*` (the first-block contract)
- `audio::tests::mute_while_playing_patches_graph_without_reloading`
- `audio::tests::play_returns_promptly_and_reports_a_bounded_first_chunk_failure`
- `audio::tests::warm_up_opens_the_stream_and_prefills_the_ring`
- `graph::tests::mixer_state_patch_updates_gain_pan_and_solo_without_rebuilding_chains`

The underrun claim is measured, not inferred: `StreamingPlayback::underruns`
counts callbacks that found the ring empty after audio had started for the
generation, and the production callback is the only writer. The regression test
runs that callback against the real ring at realtime, delays the source worker
by 1.5 s (well past the 250 ms lookahead) and asserts zero new underruns while
`active_sources` stays at the pre-unmute count; a build performed on the render
thread fails the same assertion with tens of underruns. `active_sources`
tracks spliced lanes so the test can also prove the worker delivered the lane
instead of the producer building it.

## Preset audition off the main thread

Clicking a preset in the preset panel does real work: it ensures CLAP
instances, resolves the presets (60–125 ms cold) and, for a VST3 preset,
instantiates the plugin (310–890 ms) and applies its state before the arpeggio
renders. That work must not run on the app main thread or under the state lock,
or the window freezes and every other command waits.

### Threading model

- `preview_preset` is `async` and only records a generation plus the `loading`
  status, then queues the work on Tauri's blocking pool. The invoke returns
  immediately; the webview thread never resolves, loads or renders.
- The worker snapshots the project, resource root, output device and sample
  rate under `lock_inner`, releases it, and resolves presets with a cache keyed
  by project revision. It takes or loads one VST3 instance per plugin id,
  applies the preset and renders outside the lock. It re-takes the lock only to
  publish the prepared playback stream. Every audition path, including builtin,
  sample and SoundFont presets, publishes through the dedicated audition
  playback (`AudioState::audition_playback`) instead of the transport's render
  thread. The rendered-buffer path builds its CoreAudio stream before the commit
  lock.
- Auditioning leaves the main transport alone: it never calls `AudioState::stop`,
  never replaces `playback`, and never rewrites `pending_seek_frame`. The
  timeline cursor, playing state and render-thread project stay as they were.
  Starting the main transport stops the audition stream instead.
- A newer click bumps an atomic generation. The older worker finishes the
  plugin call it is already in, keeps the instance and rendered buffer, and
  does not publish; it never queues behind the new request.
- The cache holds one VST3 instance per plugin id and up to eight rendered
  preset buffers. A project revision change (including load/reload) bumps an
  epoch that drops both, a sample-rate mismatch drops the instance, and
  `replace_project` clears the audition cache explicitly. CLAP auditions borrow
  the existing preset instance and put it back.
- The panel stays responsive and shows `Loading <preset>…`; readiness arrives
  through the existing `get_transport_state` poll as
  `audition {preset, status, ready_ms, instances, loads, error}`.

The audition does not run on the streaming render thread: that thread is the
playback producer, and a 0.3–1 s state load there would starve the ring.

Regression tests without plugins: `audition::tests::preview_preserves_the_stopped_cursor`
and `audition::tests::preview_does_not_stop_a_playing_transport` (headless,
builtin `test_tone`, simulated output).

### Measured

In-process harness `audition::tests::review_preset_audition_latency` on
`/tmp/audition-bench.ocp` (10 × a VST3 instrument, `builtin.test_tone`,
`builtin.open_synth`) at the default 44.1 kHz, with simulated output so no
device is opened. A monitor thread samples `lock_inner` every 100 µs and
records the longest continuous hold.

| Measurement | Measured |
|---|---|
| ten clicks, worst main-thread lock hold | **0.1–0.2 ms** |
| ten clicks, worst per-click wall time | 93–1312 ms (worker only) |
| second click on the same preset → sound | **0.1 ms** (cached buffer) |
| A → B → A, third click → sound | **0.1 ms** |
| VST3 instances after ten clicks | **1 cached, 1 load** |
| `builtin.test_tone` | plays, non-silent buffer |
| `builtin.open_synth` | plays, streamed from a private render thread |

The UI-gap bound is the maximum lock hold (0.2 ms or less), not the worker
time: the invoke returns before the work starts. A JS `setInterval` watchdog
cannot observe this stall, because WebKit runs JS in a separate process from
the app main thread; the lock-hold monitor is therefore the relevant
instrument. Switching to a different preset on the same instance pays the
instrument's state load (and a render if that preset's buffer is not cached),
off the UI thread and without re-instantiating.

The gate render is independent of the audition: `ocp render
opensynth.ocp --sample-rate 48000` hashes
`97ea098eaca5473ac1a90c345808e6b000caa9744811cbb251621eb045c492a4`.

## The first press does not scan plugins

The `transport_play` command runs two steps before it reaches the audio state:
it collects plugin render warnings and calls `ensure_clap_instances_loaded`.
`oc_host::cached_plugins()` runs the one-time crash-isolated scan of every
installed CLAP/VST3 bundle, which costs 0.7–1.1 s on a host with 9 built-ins
and 24 isolated third-party plugins. A press that triggers that scan pays the
whole inventory, and the audio harness does not see it, because the scan is not
in the audio path the harness times. The press path therefore keeps the scan
out of reach:

- `ensure_clap_instances_loaded` collects the CLAP presets first and returns
  before touching the inventory when there are none. A plugin-free
  (VST3/builtin/SoundFont) project never triggers a scan from the press path.
- `open_file` starts a background `warm_transport_plugins` for projects that
  reference plugins. It resolves exactly the CLAP/VST3 ids the press will look
  up and initializes the full inventory only when the press needs it (plugin
  effects or CLAP instance lookup), overlapping the audio warm-up and never
  holding the app lock. A press that arrives before it finishes performs the
  lookup itself.
- `oc_host::warm_plugin_inventory()` exposes the one-time scan so the UI layer
  can move it off the press path.

### Measured

`cargo test -p oc-gui --lib commands::tests::review_cold_first_press_latency`
in a cold process with the built scan helper:

| Project | press: warnings | press: CLAP prep | press: `AudioState::play` |
|---|---|---|---|
| `examples/opensynth_showcase.ocp` (builtin only) | 0.03 ms | **0.00 ms** | 0.11 ms |
| `compare.ocp` (4 × VST3) | 0.04 ms | **0.00 ms** | 0.13 ms |

Combined with the audio harness (`press1_first_audible_ms` 1–9 ms in the
simulated sink), the first press pays only the audio path.

Budget: once the project's warm-up reports ready, press → first sample is
≤ 20 ms for a plugin-free project and ≤ 50 ms for a VST3 project. A press issued
while the load warm-up is still running is bounded by the remaining build: 4 ms
per builtin lane (see §First block on every start path; 15 ms end to end for a
three-lane project), and ≈0.3–0.7 s for four VST3 instances. CLAP projects
construct their instances on the press (the documented state-capture
ordering), with the inventory already warm.

The regression test is
`commands::tests::first_press_does_not_scan_plugins_for_a_plugin_free_project`:
it runs the press-path plugin preparation and asserts `scanned_plugins` stays
empty.

Reproduce the benchmark:

```sh
export CARGO_TARGET_DIR=target/space-first
export OCP_PLUGIN_SCAN_HELPER=$CARGO_TARGET_DIR/debug/opencompose-plugin-scan-helper
export OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-space-first.json
export OCP_STREAM_PROJECT=$PWD/examples/opensynth_showcase.ocp
cargo test -p oc-gui --lib commands::tests::review_cold_first_press_latency -- --ignored --nocapture
```

## Preview budget: the audition streams its phrase

A preset audition plays a 2.2 s arpeggio. Rendering the whole phrase before the
device opens costs 0.26–1.24 s for an OpenSynth preset in a debug build, an
order of magnitude more than any other stage, so the builtin audition streams
the phrase instead.

### Budget

| Preview | Cold budget | Warm budget |
|---|---|---|
| Builtin preset (`builtin.open_synth`) → first sample | **≤ 120 ms** | ≤ 40 ms |
| Plugin preset (VST3/CLAP) → first sample | **≤ 400 ms** | ≤ 100 ms (cached instance/buffer) |
| Track preview → first sample | ≤ 150 ms | ≤ 40 ms (debug OpenSynth project) |

The builtin budget is the one a normal project hits: OpenSynth is the default
instrument, so every builtin preset audition is one instrument build plus one
short chunk. The plugin budget allows for the unavoidable instance
construction; the warm-up table above measures 274–362 ms for four VST3
instances (≈70–90 ms each) plus state application.

### Streaming the builtin audition

`prepare` builds the one-track audition project and returns it; `commit` starts
a **private render thread** (`AudioState::open_audition_render`) and opens the
audition stream before dispatching the project, then installs it under the app
lock. The worker renders the phrase in chunks into its own ring, so the first
1024-frame chunk is audible while the rest of the phrase is still rendering.
The transport's render thread, project and ring are untouched, and starting the
main transport stops the audition (`play_prepared` → `stop_audition`).

Sample/SoundFont and plugin auditions use the rendered-buffer path: for
VST3/CLAP the plugin instance is the dominant cost and the buffer cache makes
repeats ~0.1 ms, so streaming would not pay for itself there.

The first-chunk size is the same `FIRST_CHUNK_FRAMES` (1024) the transport
uses; the audition's 250 ms lookahead is producer-side only and does not gate
the first sample.

### Measured stages

`commands::tests::review_preview_latency`, debug build, 48 kHz, simulated sink
(the production callback on the production ring). Preset rows are
`examples/opensynth_showcase.ocp` presets; track rows are the same project's
tracks.

| Stage | Measured |
|---|---|
| click → `audition.begin` + worker dispatch | 0.1 ms |
| project snapshot + preset resolve | 0.02–0.18 ms |
| audition project prepare | 0.2–0.6 ms |
| audition phrase render before playback | **none** |
| builtin preset → first audible sample (click total) | **24.9–36.5 ms** |
| track preview → first audible sample (click total) | 29–34 ms |
| track preview project loads | +1 per press |

Within the builtin click total: dispatch 0.3–1.9 ms, device/stream open
0.1 ms, then the private worker's `build_sources` 18.7–19.0 ms and the first
1024-frame chunk before the first audible block. The default one-track
benchmark project lands in the same 24–27 ms band.

### Interaction with the first-press path

- Preset preview never touches the plugin inventory: the audition worker
  resolves plugin bundles directly (`find_plugin_info`, memoized), and its
  private render thread never calls `ensure_clap_instances_loaded`. The
  `open_file` background warm-up is not repeated.
- Track preview calls `ensure_clap_instances_loaded`, which returns before
  the inventory for projects without CLAP presets and otherwise uses the
  inventory `open_file` already warmed. It does not trigger a scan on a
  plugin-free project.
- Preview reuses the transport's warm output **device** (the stream stays open),
  but not its graph: a preset audition is a different phrase, and a track
  preview is a solo mix that starts at frame 0. The transport's DSP state has
  already advanced through the opening lookahead, and a preview is a different
  mix, so that pass is a load (the row above shows the per-press project-load
  counter). A backward seek in the transport itself does not take that pass:
  it rewinds the existing lanes and keeps their instances.

### Regression and benchmark

- `audition::tests::builtin_synth_preview_streams_instead_of_rendering_its_phrase_first`
  asserts the builtin path publishes a stream (no buffer), leaves the transport
  cursor untouched, and reaches the device without a full render.
- `commands::tests::review_preview_latency` prints the stage table above. It
  uses a one-track OpenSynth project by default; set `OCP_PREVIEW_PROJECT` to a
  real project, and `OCP_TEST_PLUGINS=1` to include VST3 presets (third-party
  instances).
- `audition::tests::the_streamed_preview_plays_at_or_below_the_ceiling` runs the
  whole press path on the simulated device and asserts the loudest sample the
  device heard, so the level ceiling is checked on playback rather than on a
  re-render of the same project.

```sh
export CARGO_TARGET_DIR=target/preview-latency
export OCP_PROFILE_PLAYBACK=1
cargo test -p oc-gui --lib review_preview_latency -- --ignored --nocapture
```

### The level ceiling costs no figure render

The instrument carries its own level ceiling. Every stage that can raise the
voice path has its own normalisation and the master output approaches -6.2
dBFS, so the preview starts without rendering the figure to measure its peak;
see `docs/synth-levels.md`. A measuring pass would cost 125–171 ms at 48 kHz in
a debug build before the first block, and a SoundFont audition would add a font
load on top. The render thread computes at most `LOOKAHEAD+CHUNK` frames
before the first block, and the press-to-first-sample budget includes no
preview render. A SoundFont or hosted plugin preview sits outside the
instrument's contract and is not trimmed either.

### Residual

- Plugin preset auditions render their whole arpeggio after the instance
  has been built. At ~70–90 ms instance construction plus state application they
  sit inside the 400 ms cold budget, and repeat clicks reuse the cached instance
  and buffer. Streaming that path needs a plugin-instance-aware audition
  producer; the VST3 harness also needs a working plugin resolution for a VST3
  id to measure it end to end.
- A track preview is a structural pass over the transport graph, so it reloads
  sources: ~30 ms for a three-track native project, but a four-VST3 project
  pays the same ~0.3–0.9 s build as a structural edit on every preview. Avoiding
  that needs a solo-mix graph patch that can restart at frame 0: the transport's
  backward seek rewinds its existing lanes, but a preview is a different mix.

## Smart disable for silent plugins

A hosted plugin that has rendered silence for four seconds of render time while
it receives no input stops being called. The saving is CPU in the streaming
callback, so the rule applies to the streaming path only: the offline renderer
never enables it, and its output is unchanged.

### The rule

The decision is made per lane and per hosted plugin instance, once per render
block. A plugin pauses only when **all** of these hold:

- its last block peaked at or below `SILENCE_PEAK` (-80 dBFS, `1.0e-4`);
- that block had no input: no MIDI event for an instrument, audio below the same
  threshold for an insert effect;
- the two conditions have held for `IDLE_SECONDS` (4 s) of render time;
- its declared tail is shorter than that window (`tail_samples()`).

The constants live in `crates/oc-engine/src/idle_gate.rs`. They are the only
place the window, the threshold and the fade length are written down.

### Tail rule

A plugin whose reported tail is at least as long as the idle window is never
paused, and neither is a plugin whose format reports no tail at all:

| Format | Tail source | Pauses |
|---|---|---|
| CLAP | `clap.tail` extension | only when the extension exists and reports less than 4 s |
| VST3 | `getTailSamples()` | only when it reports less than 4 s; an infinite tail never pauses |
| AU | `kAudioUnitProperty_TailTime` | only when the unit answers and reports less than 4 s |
| VST2 | `getTailSamples` opcode | never: `0`/`1` means unknown or unbounded |
| LV2 | — | never: the format has no tail signal |

This fails closed towards correctness: the host keeps paying for a plugin it
cannot bound rather than truncating a tail it cannot see.

### Opting out

The switch is per plugin and defaults to enabled:

- a track's instrument: `mix: smart_disable=false`;
- one hosted insert effect: `smart_disable=false` in the effect's parameter
  list, for example `- plugin:"vendor.reverb": smart_disable=false`.

The effect key is reserved and never reaches the plugin. Pausing a track keeps
that track's other plugins running; the switch is not a solo/mute style gate.

### Pause and resume without a click

The last processed block is faded out over a raised-cosine ramp
(`RAMP_SAMPLES`, 256 samples, 5.3 ms at 48 kHz), so the first skipped block
follows a block that already ends at zero. The resumed signal comes from the
plugin's own state, which is never recreated, but its first block is processed
and discarded; the next block starts the fade-in from zero. Processing therefore
resumes one block before the fade-in starts, which gives a delay line or a
release envelope one block to refill before its output is heard.

The host does not allocate, lock or spawn a thread in this path: the gate is
two frame counters, a state enum and one pending `f32` ramp, applied in place.

### Measured

Fake in-process VST3 instances count `process` calls. Blocks are 128 frames at
48 kHz; four seconds is 1 500 blocks.

| Case | Process calls | Result |
|---|---|---|
| insert effect, 1 500 silent blocks | 1 500 | the fade-out block still runs |
| same effect, 8 more silent blocks | 1 500 | processing stopped |
| first block with input after that | 1 501 | resumed within one block |
| next block (fade-in) | 1 502 | output starts at zero |
| instrument, held note then release | 2 + 1 500 | silence after release pauses it |
| instrument, transient after 6 s | +1 | resumed within one block |
| 10 s reported tail, 1 508 silent blocks | 1 508 | never paused |
| `smart_disable=false`, 1 508 silent blocks | 1 508 | never paused |
| offline entry, 1 508 silent blocks | 1 508 | every block processes |

The seam step (the difference between the last sample before and the first
sample after each of the pause and resume splices) measures `0.0`, below the
-80 dBFS threshold; the fade-in's first sample is exactly zero and the block
reaches the plugin's own level by its last sample. The unit test
`idle_gate::tests::the_seam_steps_below_minus_80_dbfs` asserts the step and that
the resumed block actually rises.

### What it does not cover

- Offline renders, stems, range renders, clip previews and auditions never
  enable the gate.
- A plugin that modulates its own output without input (a free-running LFO, an
  oscillator, a noise source) is silent only if its own modulation is silent;
  when it is below the threshold for four seconds it pauses like any other, and
  its modulation resumes from the state it had when it stopped.
- Parameter automation aimed at a paused plugin is dropped while it is paused.
  The last value is applied when it resumes, so a sweep that happens entirely
  inside the paused window is not heard.
- A paused lane still reports its declared latency to PDC; only its output stops.

### Reproduce

```sh
export CARGO_TARGET_DIR=target/smartoff-verify
cargo test -p oc-engine --lib -- idle_gate
cargo test -p oc-engine --lib -- smart_disable
cargo test -p oc-engine --lib -- streaming_gate_tests
cargo test -p oc-dsl --lib -- smart_disable
```

`crates/oc-engine/src/graph/streaming_gate_tests.rs` holds the effect cases and
the offline-entry assertion; `block_source::tests::smart_disable` holds the
instrument cases; `idle_gate::tests` holds the gate arithmetic and the seam.


## First block on every start path

The earlier sections cover individual paths. This one states the contract for
every path that starts audio, the instrumentation that measures it, and what each
path costs.

### The contract

- **First block ≤ 50 ms.** The moment a command starts audio (a press, a seek, a
  preview click, a record start, an unmute) to the moment the first block for
  that start leaves the device sink.
- **A start path streams.** It computes at most the 250 ms lookahead window plus
  one chunk before that first block: `LOOKAHEAD_FRAMES + CHUNK_FRAMES` =
  16 096 frames, stated as 16 384. Anything longer means the path rendered a
  range first. The transport therefore never renders a window, a section or a
  song before it can sound.

The bound is measured on the production worker, ring and callback, with the
simulated sink standing in for the device (128-frame blocks). The debug profile
is the gate: a number under the bound here is under it in the `perf` build too.

### Instrumentation

`first_block.rs` holds one `FirstBlock` per playback object. Every field is an
atomic, so the audio callback only reads and writes counters: no allocation, no
lock, no thread spawn.

| Field | Written by | Meaning |
|---|---|---|
| `start_ns` | the command that starts audio | monotonic press instant |
| `rendered_frames` | the producer | frames computed since that press |
| `delivered_ns` | the device sink | when the first block left |
| `frames_computed` | the device sink | producer frames at that instant |
| `block_frames`, `queued_frames` | the device sink | block size and ring depth |
| `spliced_ns` | the render thread | when a rebuilt lane joined the mix |

A start is stamped at the earliest point of the command (`AudioState::play`,
`seek`, `apply_mixer_change`, the preview and take commands), so everything the
command does before audio is inside the number. `FirstBlock::observe` is called
from `streaming_audio_callback` and from the buffered callback; it records the
first non-empty block only.

The numbers reach the UI through `TransportSnapshot::first_block` and
`AuditionSnapshot::first_block`, and one debug line per start
(`first_block path=… latency_ms=… frames_computed=…`) is written by the
transport poll, because the callback must not log.

### Measured

`first_block_tests::review_first_block_latency` (ignored; run it with
`--ignored --nocapture`), debug build, 48 kHz, 128-frame device blocks,
`n` repetitions per path. `frames` is the producer work that happened before the
first block left.

| Path | n | p50 | p95 | worst frames | Verdict |
|---|---|---|---|---|---|
| play from stop, small project (warm) | 12 | 1.25 ms | 2.52 ms | 0 | pass |
| play from stop, 20-track session | 3 | 1.65 ms | 2.26 ms | 0 | pass |
| press while the open warm-up is still building | 12 | **15.05 ms** | 15.12 ms | 1 024 | pass |
| seek to a new position while playing | 12 | 41.93 ms | 42.72 ms | 9 216 | pass |
| seek backward while playing, 4 VST3 instruments (bar 8 → bar 1) | 4 | **7.56 ms** | **7.66 ms** | 1 024 | pass |
| seek backward while playing, 4 native lanes (bar 8 → bar 1) | 6 | 35.69 ms | 38.66 ms | 5 120 | pass |
| resume after pause | 12 | 1.27 ms | 1.82 ms | 0 | pass |
| preset audition, cold | 12 | **6.55 ms** | 6.64 ms | 1 024 | pass |
| pattern preview | 12 | **6.47 ms** | 6.57 ms | 1 024 | pass |
| clip-range preview | 12 | **6.44 ms** | 6.55 ms | 1 024 | pass |
| start an audio take | 4 | 2.28 ms | 2.29 ms | 0 | pass |
| start a MIDI take (transport part) | 12 | 1.26 ms | 2.52 ms | 0 | pass |
| unmute a pruned lane | 6 | **12.47 ms** | 12.51 ms | 0 | pass |
| structural edit, then play immediately | 6 | **17.10 ms** | 17.49 ms | 1 152 | pass |
| the same edit, press after the rebuild | 6 | 6.34 ms | 7.59 ms | 1 664 | pass |

Latency numbers move with machine load, so repeat a run before reading a row
that sits near the bound. Repeated runs put the backward-seek VST3 row at p50
7.6–10.7 ms and the native row at p50 33.0–35.7 ms. The VST3 row uses four
in-process fake components, which instantiate far faster than a real plugin;
the row therefore shows the rewind path, not a plugin's 0.3–0.9 s instance
build. The native row pays four OpenSynth state rebuilds and stays under the
bound.

Every path is streaming: the worst `frames_computed` is 9 216 (two chunks, the
seek it abandoned), and a warm press computes nothing at all because the warm-up
already filled the ring.

### What keeps each path inside the bound

1. **The seek preroll is bounded.** Rendering the skipped range of a forward
   seek costs time proportional to the distance (575 ms and 229 024 computed
   frames for a 4.8 s jump), so a jump across a song would hold the device
   silent for seconds. A seek that arrives while the transport plays therefore
   spends at most `SEEK_PREROLL_BUDGET` (20 ms) on exact history. Past that it
   starts at the destination with its voices released: the notes that were held
   across the jump are released, the last program/controller/pitch value before
   the destination is re-applied, and the skipped events are dropped. A paused
   seek keeps exact history, because nothing is waiting for sound; that is the
   behaviour the parity tests pin.
2. **Playback requires streaming support.** A project that cannot stream is
   refused with its affected tracks named. Playback never renders a whole song
   before starting the device.
3. **Audition uses streaming sources.** Offline figure rendering remains a
   test reference for comparison with the streaming path.
4. **A backward seek rewinds instead of reloading.** Rebuilding the project when
   the target moves behind the render cursor would re-instantiate every
   instrument: 0.3–0.9 s on four VST3 lanes and a flushed ring on the press.
   The render thread instead returns each lane to its construction state in
   place: OpenSynth, sampler, drum kit, performance, SoundFont and built-in
   sources reset their voices and DSP, a hosted plugin gets its format's reset
   (`setProcessing` cycle for VST3, `clap_plugin::reset` for CLAP,
   suspend/resume for VST2, `AudioUnitReset`, LV2 activate), and the graph and
   lane chains reset their effect state. A lane whose source reports that it
   cannot reset is rebuilt on its own; the other lanes keep their instances. The
   event and performance cursors return to zero, so the bounded preroll replays
   the gap and the position starts with no voice left over from before the seek.
5. **An instrument builds only the IR banks it uses.** `SynthFxChain` reserves
   two convolution slots per OpenSynth source, and generating and FFT-loading an
   IR bank costs 8.6 ms per slot in a debug build, 17.6 ms per source. A press
   queued behind a build would wait for that work whether or not the patch has a
   convolution module. `ConvState::new` therefore only allocates;
   `SynthFxChain::sync_from_patch`, which runs on every load path and calls
   `prepare_convolutions`, generates the bank for the slots that actually hold a
   Conv module. Per-lane construction is 1.4 ms, and `build_sources` for a
   three-lane project is 4.3 ms. The generation stays on the load path and never
   enters the audio callback.

### Tests that pin the property

- `first_block::tests::*` — the trace arithmetic: press, block, frames, the
  splice gate, a silent callback and a superseded start.
- `first_block_tests::play_from_stop_streams_and_reaches_the_device` — p95 under
  50 ms and at most 16 384 frames before the first block.
- `first_block_tests::press_during_the_warm_up_reaches_the_device` and
  `first_block_tests::press_right_after_a_structural_edit_reaches_the_device` —
  the two paths that wait behind a build.
- `first_block_tests::resume_after_pause_reaches_the_device`
- `first_block_tests::backward_seek_reaches_the_device` and
  `first_block_tests::backward_seek_on_vst3_lanes_keeps_the_instances` — the
  backward rows above, with the VST3 case asserting that no instrument was
  instantiated.
- `render_thread::tests::a_backward_seek_keeps_the_instruments_and_starts_clean`,
  `a_backward_seek_rebuilds_only_a_lane_that_cannot_rewind` and
  `a_forward_seek_keeps_the_fast_path` — instance identity, the single-lane
  rebuild, released voices, and the forward path.
- `first_block_tests::pattern_and_clip_previews_stream`
- `first_block_tests::a_ten_minute_project_starts_without_computing_its_timeline`
  — the project reports 28 704 000 frames (598 s), the press computes none, and
  the first block is bounded.
- `oc_synth_fx::tests::a_rack_without_a_convolution_module_generates_no_ir` —
  building a rack leaves every convolution slot unprepared and only the module
  that exists prepares its own.
- `first_block_tests::review_first_block_latency` — the table above.

Run them with:

```sh
export CARGO_TARGET_DIR=target/firstblock-verify
cargo test -p oc-gui --lib -- first_block_tests::
cargo test -p oc-gui --lib -- first_block_tests::review_first_block_latency --ignored --nocapture
```

### What still costs time

No path on the table fails 50 ms. The press that lands during a build still
pays for the build, and that is the honest shape of it: a lane that does not
exist cannot be mixed, so the remaining cost is one build plus one chunk rather
than a range render.

- **A press during the open warm-up**: 15 ms, of which ~4.3 ms is the first
  1 024-frame chunk and ~4.3 ms is the build of three OpenSynth lanes.
- **A structural edit followed immediately by a press**: 17 ms, the same build
  plus the seek bookkeeping and the flushed ring (extras `frames_computed`
  1 152, nine device blocks).

Both scale with the instruments in the project, not with the length of the song.
A project whose lanes are hosted plugins still pays the plugin's instantiation
inside the press, and that is the plugin's number, not the graph's: a VST3 lane
is 90–300 ms to construct, so a press during that rebuild can
still exceed the budget. The measured builtin path is what this section covers.

A press during the warm-up also still computes one first chunk even though the
warm-up is about to fill the ring: `Play` only takes the prefilled ring when a
chunk has already landed at the requested frame. Cutting that last chunk is a
splice-ordering change to `load_project`, not an allocation change; it is not
needed for the contract.

The MIDI take row measures the transport half only: no MIDI input was available,
so `midi_record::start_inner` refuses before the transport rolls. The press
stamp is taken before the take opens its input, so a host with a MIDI port adds
that port's open time to the same row.

## A loop wrap is a fade, not a gap

A pass that ends inside a sustained note must not cost a callback of silence at
the wrap. If the render thread stops at the region end and waits for the ring
to drain before it rebuilds the pass, every callback that arrives during the
rebuild reads an empty ring, and the wrap becomes a dropout on every pass.

### What the wrap does

The wrap does not wait for the drain. As soon as the last chunk of the pass is
in the ring, the render thread rebuilds the pass from the region start while the
callback plays the queued tail. The rebuild — reload the graph, replay the
history before the region start — runs behind audio that already exists: it has
up to the 12 000-frame lookahead (250 ms at 48 kHz) of queued audio to finish in.

Two fades keep the seam click-free. The closing chunk fades out over `RESUME_FADE_FRAMES` (96 frames, 2 ms at
48 kHz), and the chunk that opens the next pass fades in over the same length.
A completed pass plays the tail fade and the head fade with no silence between
them. Holding the old tail back and crossfading it into the new head would
remove the level dip as well, but it would also change the samples a completed
pass plays, so the seam keeps the two fades.

The playhead needs the same seam. A wrap keeps the ring running instead of
flushing it, so the callback cannot keep counting frames past the region end.
When it wraps, the render thread publishes the frame the pass ends on and the
frame the next pass starts on; the callback moves the playhead across that seam
after it advances the cursor. Two atomics: the callback reads the end every
block and the start only at the seam. Disarming the loop or moving the region
clears the seam, so the picture never wraps on its own.

Nothing allocates or locks in the callback. The rebuild is render-thread work;
the callback reads two seam frames and applies the existing fade helper.

### Measured

`render_thread::tests::a_loop_that_ends_inside_a_note_wraps_without_a_silent_callback`
drives the production callback over an armed region that ends inside a held
note. The tone is a 250 Hz sine (a whole 32-frame period at 8 kHz), the region
starts on a period boundary, and the test reads the block in which the playhead
wraps.

| Measurement | Measured |
|---|---|
| callback at the seam | longest run at exactly zero: 1 frame |
| level dip at the seam | 26 frames (3.25 ms) |
| seam step against the program's own slope | 0.06897 against 0.06942 |
| sine phase after the fade-in | 1.3e-4 of 0.354 amplitude |

The step stays under the program's own slope because both fades reach zero at the
seam, so the seam is nearly transparent: at this level a 250 Hz sine never steps
by more than 0.069 per frame at 8 kHz. Phase continuity is measurable for the
same reason: the region starts on a whole period, so the pass after the seam
must be the same sine at the same phase, and it is.

### What it does not cover

- A rebuild that outlives the queued tail still starves the callback. The rebuild
  is bounded by the graph, not by the loop, so a project whose load takes longer
  than the audio in the ring keeps the starve fade on the seam. The early wrap
  removes the structural silence; it cannot remove a rebuild slower than the
  audio the ring holds.
- The seam is a 4 ms level notch, not a crossfade. Two fades in sequence dip
  through zero at the junction, so a sustained note loops with a short soft blip
  instead of a dropout. These numbers are measurements only; the seam has not
  been judged in a listening test.
- Only live playback wraps. Offline renders, exports, stems and the two gate
  fixtures never read a loop region; the fixtures keep their own pinned hashes
  (`8b9c61e9…`, `280c1d3c…`).

### Reproduce

```sh
export CARGO_TARGET_DIR=target/loop-seam-verify
cargo test -p oc-gui --lib -- a_loop_that_ends_inside_a_note --nocapture
```

The test prints its seam numbers on the `[loop-seam]` line and fails if any
callback at the seam is silent.
