# Agent feedback loop

The agent surface is one catalog in
`crates/oc-gui/frontend/src-tauri/src/tool_registry.rs`. The external MCP
server and the built-in AI panel both read it, so every tool below has the same
name, JSON schema and handler on both surfaces (`agent_api::dispatch`). The
panel additionally applies its exclusion policy; the range render, audition,
analysis, compare, transaction and plugin program/state tools are enabled in
the panel.

The MCP catalog has 126 tools. The panel enables 98 of them and adds its local
`edit_subtree` tool, for 99 panel tools.

Use `effects_set_bypass` for a reversible comparison of an existing insert.
Read its original bypass state and chain index with `project_extract`. Pass
the target, zero-based index, bypass state and current revision. Restore the
original state after the comparison. The effect and its parameters stay in
the chain.

After installing a managed separator through `plugin_pull`, pass its catalog
ID as `runtime_id` to `separate_stems`. The call requires that verified
runtime and reports its ID, version, method and output files. An unavailable
runtime or failed separation returns an error so another provider or the
approximate band split cannot be mistaken for the requested runtime.
External separator processes have a 30-minute deadline. A timeout discards
that run's partial files and leaves previous external runs available.
Stop in the AI panel also cancels its active external separator process.
Each split writes a fresh subfolder in the output directory. Use the returned
stem paths to find its files after a retry or an automatic fallback.

## Headless agent host

Set `OPENCOMPOSE_HEADLESS=1` when starting the native application to run its
loopback agent server without windows or WebViews. Headless hosts do not
participate in desktop single-instance forwarding, focus requests or operating
system document-open events. Opening or closing a desktop window therefore does
not control a headless host's session.

Give each concurrent host a separate `OPENCOMPOSE_CONFIG_DIR`. This directory
owns its configuration and private `agent.json` endpoint; it does not
automatically inherit another profile's provider settings or credentials.
`OPENCOMPOSE_AGENT_PORT=0` requests an available port, recorded in that endpoint.
For a saved headless conversation, create a project file and call
`sessions_new` with that file and a title. Send both `session_id` and the
absolute `project_path` on each `POST /ai/complete` request. The file must be
open in that host and the session must belong to its workspace. Requests save
incoming tool results, audition media and the provider reply through the same
conversation store as the AI panel. Keep saving the same project path; copies
for checkpoints do not change the session's working file. Omitting both fields
keeps the request ephemeral.

The store belongs to `OPENCOMPOSE_CONFIG_DIR`. A private profile's sessions do
not appear in the default desktop profile. Use the intended profile when you
want to continue the conversation there.

Stop the host before removing its profile. Headless mode still uses the native
application event loop; it is not a standalone core daemon for systems without
a window-system session. Audio playback tools can use hardware output, so
offline workflows must use render and audition tools rather than transport.

## Import aligned WAV stems and MIDI

Save an empty destination project and stage the exports in a real `handoff`
folder beside its project file. Export each WAV from the same start, including
any leading silence. Read the current revision with `project_get`, then call
`import_aligned_stems` with the relative paths and that revision.

```json
{"paths":["handoff/Drums.wav","handoff/Vocal.wav","handoff/Notes.mid"],"expected_revision":3}
```

The tool accepts 1–64 mono or stereo WAVs, up to 2 GiB total, and one optional
MIDI file up to 8 MiB. WAVs are copied into project media and start at 1:1 at
source speed. Imported stereo tracks use `pan_law=balance` to retain the file
level at center, even when its channels are identical. MIDI sets the tempo
and meter, retains key-release note lengths
and separate CC64 per channel, and starts muted. Without MIDI the destination clock is used. The result reports the revision,
stem count, number of timestamped WAVs, MIDI track count, and any MIDI import
warnings. When Broadcast WAV start timestamps disagree, import stops and asks
for common-start exports. Missing timestamps leave the common origin
unverified. Matching timestamps only describe the headers. Use
`audio_clips_list` and audition common landmarks to verify the alignment.

The tool refuses absolute paths and paths that resolve outside `handoff`.
Use the **File → Import Aligned Stems** picker for files elsewhere. A stale
or failed import cleans its partial copies. A committed import is undoable
and retains its project media for redo. Rolling back the owning transaction
removes its copies and leaves the staged files available.

## Hand off MIDI without stems

In a saved project, read the revision with `project_get` and call
`midi_export_handoff`. The export accepts the standard MIDI track selection
and format options. It returns a relative path such as `handoff/project.mid`
and the export loss report. It creates a new file on each call. Commit any
open transaction before exporting. In the AI panel, start an export turn after
the previous edit turn finishes.

To import, stage one `.mid` or `.midi` file in the receiving project's
`handoff/` folder and call `midi_import_handoff` with its relative path and
the receiving revision. The import limit is 8 MiB. Key-release note lengths
and CC64 automation remain separate, with one imported track per source
track and channel. Empty projects take the MIDI tempo,
meter and key maps. Projects with tracks or sections keep their own clock.
An import joins an open transaction owned by the caller and rolls back with it.

## Applying an edit: a proposal or an application

`edit_subtree` is the one write path that replaces a whole subtree, and it has
two outcomes:

| session | result the model reads | what happens |
|---|---|---|
| a user is there (the panel) | `{"status":"pending_user_confirmation","applied":false}` | the panel shows the diff; **Accept** calls `ai_apply_edit` |
| nobody to ask (an agent session) | `{"status":"applied","applied":true,"path":…,"revision":…}` | the loop calls `ai_apply_edit` itself, in the same turn |

The session sets the capability (`runAgent({autoApply})`), never a guess about
the surface: the panel leaves it off, and a headless driver turns it on. Both
outcomes are one undo entry through `apply_edit_to_project`, and both refuse a
replacement that does not parse or names missing media without touching the
project. An applied edit moves the document the turn is checked against, so a
second replacement in the same turn is validated against what the first one
wrote.

Replaceable paths: `project` (a whole document), `project.properties` or
`properties`, `track:<ref>`, `track:<ref>.phrase:<ref>`, `track:<ref>.mix`,
`track:<ref>.effects`, `track:<ref>.automation`, `preset:<name>`,
`pattern:<name>` (created when absent), `bus:<name>`, `master`, and `sections`
(every `@section` at once). Extract and replace take the same path language, so
`project_extract("pattern:groove")` reads exactly what `edit_subtree` replaces.

Both outcomes validate the candidate before anything is written: it must parse
into the addressed subtree, the media it declares must exist under the project
directory, and it must not add an error `oc_dsl::validator` would make a render
refuse (a `gain_automation` point in dB, a bus route that names nothing). The
error text is the validator's own, so a replacement the renderer would reject
never reaches the project or the undo stack.

This document is the contract for the feedback-loop pieces: render a window,
measure it, compare two mixes, edit atomically, and export a full song.

## The preview bar's one-line summary

A proposal opened as a preview prints one line above the side hint that says
what the edit does. It comes from the proposal's own diff — `diff_dsl` over the
extracted subtree — and never from the model's prose; the classifier is
`crates/oc-gui/frontend/src/ai/previewChange.ts`.

* A changed `@track` or `@pattern` declaration names that track or pattern. The
  same name on both sides of the diff changed in place and reads as a settings
  change; a name on one side only reads as an addition or a removal.
* A `@phrase`, an indented `@pattern` or an `@audio_clip` is a clip. A clip
  whose range moved reads as a move, one that only left or only arrived reads as
  a removal or an addition, and the owning track is named.
* A body line under a `@phrase` or `@pattern` is a note; any other body line is
  a setting. A body under a declaration the diff replaces counts as part of that
  declaration, so adding a track does not also claim to change its notes.
* A diff with no hunks, or one that shares no line with the base document at the
  whole document's level, prints `Replaces the whole project document.` rather
  than guessing. A diff with no changed line prints nothing.

The line names at most three kinds in a fixed order of significance — tracks,
patterns, clips, notes, structure, settings — and the strings live under
`ai.preview.change.*` in all five locales.

## 1. `render_range` — a window of the full mix

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | Output master WAV (required) |
| `start` / `end` | number | Window in seconds, `[start, end)` |
| `start_bar` / `end_bar` | integer | Window in notated bars, `[start_bar, end_bar)` |
| `tracks` | integer[] | Optional track subset; omit for the full mix |
| `stems` | boolean | Also write one WAV per selected track |
| `bus` | string | Write one bus's processed return instead of the master mix |
| `float_wav` | boolean | Write 32-bit float WAV to a `.wav` path |
| `preRoll` | number | Wall-clock plugin warm-up (0–600 s, default 0) |

Provide exactly one of the seconds pair or the bar pair; mixing or omitting
them is an error. A `bus` tap keeps the whole graph running, so a track that
feeds a sidechain key still ducks the return being observed; it cannot be
combined with `tracks`, and an unknown bus name is refused. A bus tap captures
stems for its one render, so it does not publish the range cache: the next plain
window renders again instead of reusing a cached mix. Bars are one-indexed and resolved through the project clock
(tempo and meter maps). Section `repeat` is not unrolled for bar lookups, so
use seconds when the arrangement repeats a section.

Result:

```json
{
  "path": "/tmp/chorus.wav",
  "start_s": 48.0,
  "end_s": 72.0,
  "sample_rate": 48000,
  "frames": 1152000,
  "duration_s": 24.0,
  "tracks": [0, 1, 2],
  "stems": [{
    "track_index": 0,
    "name": "bass",
    "path": "/tmp/chorus.track-00-bass.wav",
    "pre_encode": {"peak_dbfs": -6.0, "true_peak_dbfs": -5.8, "above_full_scale": false, "clipped": false}
  }],
  "pre_encode": {"peak_dbfs": 1.9, "true_peak_dbfs": 2.1, "above_full_scale": true, "clipped": true},
  "cache_hit": true,
  "window": null
}
```

`sample_rate` is the app's configured output rate (`audio_config.sample_rate`;
44.1 kHz on the default build), not a project setting and not the exporter's
rate. It is the rate the window was rendered at, so the WAV header and
`frames / duration_s` always agree with it. The window is the
pre-normalisation mix: `ocp render` and `render_offline` apply a project
`loudness:` target, `render_range` does not.

The mix and each stem report their sample peak and estimated true peak in
`pre_encode`. Silence has null peak values. `above_full_scale` means a sample
exceeded full scale. `clipped` reports whether the selected encoding clamped
those samples. Set `float_wav: true` to preserve them for inspection without
changing the project export settings. The encoder still refuses any output
whose sample peak exceeds +6 dBFS.

`audition_range` accepts the same override for its saved full-rate file, and
its `pre_encode` report describes that full window. The inline listening WAV
keeps its existing integer format and may be shorter than the saved window.

### Semantics

* **Exact slice.** Without `tracks`, the output is sample-for-sample
  `full[start_sample..end_sample]` of the full render, including bus/master
  effect tails and notes that started before the window. Tests pin this for a
  project with a stateful master reverb.
* **Cache reuse.** `render_range` keeps a full render keyed by the project's
  content key and sample rate in `AppState::range_cache`. A clean cache is
  copied and sliced without synthesising or decoding anything
  (`cache_hit: true`); the cached mix is not replaced. A stale or missing cache
  is rendered fresh, so the first window after an edit costs one full render;
  later full-mix windows of the same state are pure slices. A subset window
  cuts the cached timeline instead of re-mixing it (see "True slice" below).
* **The cache follows the state it rendered.** The content key moves on every
  edit, including an edit made inside an open transaction, so a window rendered
  there is the transaction's working state and never the pre-batch mix. The
  commit publishes a new key and the rollback restores the pre-transaction one,
  so a window rendered inside the batch is not reused afterwards and the
  pre-transaction render becomes available again. A window inside a transaction
  therefore costs one full render; ordinary renders keep the cache.
* **Determinism.** The same project, sample rate and cache state produce
  byte-identical windows. Samples beyond the rendered length are zero, never a
  re-render.
* **`cache_hit` means what it says.** It is true whenever an already clean
  cached render answered the request: a full-mix slice, a subset window served
  from post-FX snapshots or re-processed from the cached source buffers, or
  silence past the cached horizon. It is false only for a cold cache, where the
  request had to synthesise its sources. `window` names the finer path.
* **No project lock while rendering.** The project, audio settings, CLAP
  instance forks and content key are captured under the state lock, which is
  released before any audio work. A second mutex serialises range renders so
  they cannot interleave cache updates. A render that overlaps an edit is not
  stored (the content key changed).
* **True slice.** A `tracks` request slices the same timeline with every other
  track muted. Notes that started before the window, bus/master effect tails
  and automation therefore match a full render of that selection, so subset and
  full windows are directly comparable. The cached full mix stays valid: a
  subset request does not invalidate it, and the next full window is still a
  cache hit. The response's `tracks` lists the effective selection.
* **A subset window stops at its end.** On a clean cache the engine keeps the
  selected tracks' post-track-FX buffers as snapshots and runs only the
  routing, bus and master chains, from frame 0 up to the window end plus the
  master chain's latency. The first window for a track re-runs that track's
  chains (its "post-FX snapshot" does not exist yet); every repeat window on the
  same span is served from the snapshot. With no clean cache at all the engine
  renders only the selected tracks, not the whole project. The response's
  `window` field says which path ran: `snapshot`, `recomputed`, `cold` or
  `silent` (a window past the rendered timeline).
* **Shared outputs are one unit.** Tracks that read different output pairs of
  one hosted instrument (`plugin_output_pair`, e.g. a multi-output sampler) are
  a group. Selecting any member expands the selection to the whole group,
  because one process call produces every pair; without that, the slice would
  silently miss the group's other outputs.
* **Stems.** `stems` without `tracks` is captured from the full mix, so stems
  and master are slices of the same render. With `tracks`, each stem is a slice
  of the selected-tracks timeline and unselected tracks are silent.

### Limits

* The window is limited to 600 s and the arrangement render budget is also
  600 s. A subset window costs the routing/bus/master pass from frame 0 to the
  window end, so a window late in a long song costs more than one at the start;
  a warm repeat window on the same span skips the track chains entirely.
* A cold subset runs a window-bounded render only when it provably equals the
  whole-program one: every selected track is classified stereo in the window, or
  silent there, and the master chain's latency fits the render guard. Otherwise
  (mono content in the window, or a master chain with more than a second of
  latency) it renders the selection over the whole program, which is still
  cheaper than rendering every track but is not window-bounded.
* A track whose effect chain reads another track as its sidechain key cannot use
  a snapshot unless that key track is selected too, because the snapshot was
  taken with the key audible; the window recomputes the chain instead.
* **Snapshot memory.** Post-FX snapshots cost 8 bytes per frame — 384 KB per
  covered track-second at 48 kHz — so a 2-track window at 60 s holds about
  47 MB. `RenderCache::set_post_fx_snapshot_budget` caps the store (default
  64 MiB) and evicts least-recently-used tracks; the buffers are dropped when a
  track is marked dirty, when the cache is marked dirty, and when the content
  key no longer matches. A buffer that does not fit the budget is not stored.
* The cache holds the full master (plus track buffers and stems when
  requested); memory scales with the song length. It is dropped when the
  content key no longer matches. While a transaction is open the pre-transaction
  render is also kept, so a batch holds up to two full renders.

### Listening: `audition_range`

A model cannot open a path, so a render is only audible if the clip comes back
with it. `audition_range` takes the same window arguments (and the same
refusals) as `render_range`, renders through the same code path, writes the
same full-rate WAV, and additionally returns a mono 16 kHz 16-bit WAV inline:

```json
{
  "path": "/tmp/opencompose-audition-1793193642930.wav",
  "start_s": 52.0,
  "end_s": 72.0,
  "duration_s": 20.0,
  "tracks": [0],
  "audio": {"mime": "audio/wav", "sample_rate": 16000, "channels": 1, "base64": "..."}
}
```

* **Clip.** `channel` selects what the clip carries: `mono` (default) is the
  equal-weight downmix (`0.5 * (L + R)`), `left` and `right` isolate one side as
  a mono clip, and `stereo` keeps both when the model accepts two channels.
  `clip_sample_rate` overrides the inline rate (8000–48000; default 16000). The
  media-path windowed-sinc resampler at the live rung does the conversion. The provider's own audio pipeline
  documents that multi-channel input is combined to one channel and the
  resolution is downsampled, at 32 tokens per second, so the clip is prepared in
  the shape that survives; a higher rate or a second channel would be discarded
  upstream. OpenAI's `input_audio` documents `wav` and `mp3` but no rate, so it
  resamples on its side as well. The full render is still written and named by
  `path`. The session archive keeps the full render in the format selected in
  Preferences → Storage and preserves the bounded model excerpt as the exact WAV
  delivered to the model. The clip is capped at 30 s and 2 MiB
  of base64; a longer window returns its first 30 s with `"truncated": true`,
  while `path` still holds the whole window. `duration_s` is the length of the
  returned clip.
* **What each provider accepts.** The clip is shaped for the model in front of
  it. The row in the bundled catalog (`ai-catalog.json`) may carry an
  `audio_input` block — `channels` (1 or 2, default 1), `sample_rate` (default
  16000) and `formats` (default `wav` and `mp3`) — and the loop passes it to
  `audition_range` as `channel` and `clip_sample_rate`; a model whose card says
  nothing gets the default. `audio_in: false` skips the delivery entirely.

  | Provider | Audio in | Documented shape | What the clip does |
  |---|---|---|---|
  | Gemini (Argon) | yes | `wav`/`mp3`; multi-channel combined to one channel, resolution downsampled, 32 tokens per second, 9.5 h per prompt | mono, 16 kHz |
  | MiMo (V2.6 Pro) | yes | audio tokenizer config: **24 kHz**, 128 mel bins, 10 ms hop, 300 s per clip; `mp3`, `wav`, `flac`, `m4a`, `ogg` |

  The MiMo shape comes from the model's own audio tokenizer
  (`audio_tokenizer/config.json` in the model repository), not from prose documentation: a clip
  sent at 16 kHz would be resampled up by that encoder, so the catalogue gives the family 24 kHz.
  | OpenAI-compatible | yes | `input_audio` takes `wav`/`mp3`; no rate stated, the service resamples | any rate 8–48 kHz, mono by default |
  | Custom Gemini gateway | depends on the configured route | if its OpenAI chat conversion refuses audio, use a Gemini `generateContent` entry at `/v1beta` with the bare model id | the adapter names the fix and keeps the clip intact |
  | Anthropic | **no** | text, images and PDF only | the loop sends a text note instead of audio |

* **Delivery is not a decision.** The panel's agent loop turns an `audio`
  payload in a tool result into one extra user message carrying an
  `input_audio` part and a one-line text naming the range. It never renders or
  attaches audio on its own; a round whose result carries no `audio` adds no
  message. The tool message keeps the result and its `path`, without a second
  copy of the base64.
* **Model support.** Delivery still passes the existing attachment gate: a
  model whose catalog entry has `audio_in = false` refuses the request instead
  of silently dropping the clip.

### Picture timecode

Use `picture_timecode` to map editorial frame labels to the current project
clock. It uses the saved project clock if both `rate` and `origin` are omitted.
Supply both fields to override the clock for one conversion. For example,
at 24 fps with origin `01:00:00:00`, `01:00:08:00` is eight seconds into the
cue. Each point accepts one of `timecode`, `seconds` or a musical
`position`. The report includes the nearest frame label and the signed
difference from the nearest musical subdivision in milliseconds.

For 29.97 or 59.94 fps, choose `30000/1001` or `60000/1001` and use a
semicolon before the frame field for drop-frame labels. Supported rates are
23.976, 24, 25, 29.97, 30, 47.95, 48, 50, 59.94 and 60 fps. A label may carry
an explicit `D+1 ` or `D-1 ` day prefix (`D+1 01:00:00:00`); labels never wrap
implicitly, and conversions that cross a day boundary return that prefix. A picture label earlier than
the origin returns negative seconds without a musical position.
Use `picture_clock_set` with `clock: {rate, origin}` and the latest
`expected_revision` from `project_get` to save a confirmed setting. Pass
`clock: null` to clear it. The edit is undoable and is saved with the project.
[Timecode reference](https://pub.smpte.org/latest/st12-1/st0012-1-2014.pdf)

Use `preset_bundle_export` to share one existing OpenSynth preset into a new,
numbered `.ocppatch` file under the saved project's `handoff/` folder. Pass the
preset `name` and `expected_revision`. Finish the active transaction first;
export in a new panel turn. Earlier files are never overwritten.

Use `preset_bundle_import` with a project-relative `handoff/` path, destination
preset `name`, and `expected_revision` to replace an existing OpenSynth voice
and its inserts as one undoable edit. It preserves project references and also
stores a reusable library copy. Undo restores the project, while the library
copy remains available. A bundle cannot carry an insert's external track or bus
sidechain; remove that dependency before sharing. The GUI offers the same archive
format through the patch menu's Share preset and Load actions.

## 2. `audio_analyze` — numbers instead of guesses

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | WAV to analyse (required) |
| `top_peaks` | integer | Spectral peaks to return, 0–64 (default 8) |

The report contains:

| Field | Definition |
|---|---|
| `level.peak_dbfs`, `level.rms_dbfs`, `level.crest_db` | Sample peak, RMS and crest from `level_metrics` |
| `level.integrated_lufs` | BS.1770-4 integrated loudness (K-weighting, 400 ms/100 ms blocks, absolute and relative gating), or null when undefined |
| `true_peak_dbfs` | 4× polyphase true-peak estimate, the same detector the limiter uses |
| `octave` | Mean STFT power in the ten octave bands 31.5 Hz–16 kHz, in dBFS-relative mean-square units |
| `spectral_peaks` | Mean-power local maxima: `frequency_hz` (parabolic interpolation), `level_db`, `relative_db` to the loudest peak, and the STFT `resolution_hz` |
| `stereo` | L/R correlation and mid/side ratio (null for mono) |
| `silence`, `sections` | 20 ms RMS windows below −60 dBFS / sounding runs |
| `warnings` | Mono input and other caveats |

The level numbers are reused, not re-derived: `audio_analyze` calls
`level_metrics`, `measure_stereo_interleaved` and `true_peak_dbfs` from
`crates/oc-engine/src/analysis.rs`, which are the same functions behind the
render loudness/limiter report. A test asserts that the analysis levels equal
the meter levels for the same samples.

For raw levels around a splice, call
`audio_analyze({path, level_window: {start: 1.96, duration: 0.04}})`.
The mode returns sample peak and RMS over the specified file frames without
alignment or spectral analysis. RMS includes both channels independently, so
opposite polarity is preserved. The level window accepts up to 30 seconds.

For mono compatibility, call `audio_analyze({path, mono_fold: {start: 0, duration: 2}})`.
This mode compares each octave band's stereo mean channel power with the power
after averaging left and right, without any gain matching. Each band reports
both levels and the loss in dB. A mono reading at the reported floor makes the
loss a lower bound. A stereo reading at the floor makes loss undefined.
The passage must fit in the file and span 0.5–30 seconds. Use only one of
`mono_fold`, `level_window`, `beat_grid` and `transients` per call.

The mono-fold payload contains:

| Field | Definition |
|---|---|
| `resolution_hz` | FFT bin resolution of the band measurement |
| `floor_dbfs` | Measurement floor; a band at this level is a lower bound |
| `bands[].center_hz` | Octave-band center |
| `bands[].stereo_dbfs`, `bands[].mono_dbfs` | Stereo and mono-fold band power |
| `bands[].loss_db` | Stereo minus mono power, or null when stereo is at the floor |
| `bands[].mono_at_floor` | True when the mono reading is at the floor, so the loss is a lower bound |

For attack timing in a source or processed render, call
`audio_analyze({path, transients: {start: 0, duration: 2, min_gap_ms: 5}})`.
The requested passage must fit inside the file and can span up to 20 seconds.
The start defaults to zero. Minimum spacing accepts 1–250 ms and defaults to
35 ms. The result limit accepts 1–128 and defaults to 24. Use either this
mode or `beat_grid`.

The transient payload contains:

| Field | Definition |
|---|---|
| `start_s`, `end_s` | Analysed file window |
| `resolution_seconds` | Time resolution of one 64-frame measurement bucket |
| `effective_gap_ms` | Minimum spacing rounded up to that resolution |
| `attacks[].file_seconds` | Attack time in file seconds |
| `attacks[].strength` | Relative attack strength used for ranking |

The result ranks attacks by strength. For a range render, add the render's
project start to each file time to locate the attack on the timeline. Inspect
the relevant hits and confirm them by listening before changing timing.

For pulse and meter evidence, call
`audio_analyze({path, beat_grid: {start: 0, duration: 8}})`.
The passage must fit in the file and span 4–30 seconds. The result contains
`beat_grid` evidence, `downbeat_confirmed: false` and `time_signature: null`;
the scores rank pulse evidence, they are not probabilities. Check a later
passage and confirm the pulse, meter and downbeat before applying a project
grid. A half- or double-time interpretation can fit the same attacks. The
grid is evidence for a decision, not an edit.

### `analyze_notes` — beat-grid notes

`analyze_notes` transcribes a WAV window into note events. Pass the WAV path and,
when the file is not a `render_range` output, the tempo and meter the beats
should use:

```json
{"path":"/tmp/lead.wav","tempo":120,"time_signature":"4/4","quantize":true}
```

The window defaults to the whole file. `min_confidence` drops weak detector
readings; `max_notes` caps the result earliest-first. A `project` path supplies
tempo and meter when the WAV did not come from a render.

Each note carries:

| Field | Definition |
|---|---|
| `start_s`, `end_s` | File seconds, relative to the analysed window |
| `midi`, `name`, `cents`, `confidence` | Pitch and detector confidence |
| `start_beat` | Quarter-note beats from the window's musical start, 0-based |
| `duration_beats` | Measured length in quarter-note beats |
| `bar` | 1-based bar label of the 1/16-snapped start; a label, not a measurement |
| `suggested_duration` | Nearest DSL duration token to the 1/16-rounded length |
| `start_beat_quantised`, `duration_beats_quantised` | Snapped values, present only with `quantize` |
| `octave_ambiguous`, `octave_runner_up` | The pitch has two octave readings, or the next candidate |

The report also returns `beat_seconds`, `beats_per_bar`, the chosen grid and
warnings. A note more than a 64th off the 1/16 grid keeps its measured
`start_beat`; `bar` and `suggested_duration` are the nearest reading, not a
measurement. When no DSL token is close to the length, `suggested_duration` is
a guess; check it by ear or write a tie. Read `octave_ambiguous` before
committing a pitch: the lower octave can be a harmonic, not the sung note.

## 3. `audio_compare` — A/B two renders

| Argument | Type | Meaning |
|---|---|---|
| `reference` / `candidate` | string | WAV paths (required) |
| `stems` | boolean | Narrow the alignment search to ±300 ms instead of ±1500 ms |
| `allow_mismatch` | boolean | Resample and trim unequal formats instead of refusing (default false) |
| `listening_only` | boolean | With `listen: true`, prepare the matched listening pair without alignment or similarity metrics |

Result highlights:

* `level.delta` — peak, RMS, crest and integrated-LUFS differences
  (candidate − reference).
* `tilt.octave_delta_db`, `body` — per-octave and low/mid band deltas.
  `tilt.sub_gate_tilt_db` reports the tilt of the bands below the −60 dB gate
  (reported-only, not scored) with `tilt.gated_octaves` and the sub-gate means.
* `third_octave`, `mfcc`, `lsd` — the v1.3.0 spectral-shape terms: a mean band
  table with deltas, MFCC c1–c13 cosine distances, and broadband/level-matched
  LSD. All three are score terms, not just diagnostics.
* `sample` — `max_abs_difference` on the aligned overlap, the time and channel
  where it happens, and the Pearson `correlation` of the mono mix.
* `envelope_correlation`, `log_mel`, `alignment` — the existing similarity
  metrics and the detected lag.
* `dtw` — banded DTW over the aligned overlap: `mean_abs_warp_ms`,
  `max_abs_warp_ms`, `median_signed_warp_ms`, `path_cost`, `band_ms`,
  `hop_ms`, `frames` and a `note`. The band is bounded (400 ms with `stems`,
  1100 ms without) and the note explains that silence can inflate mean warp;
  treat it as a path-shape diagnostic, not a precise timing error.
* `pitch` — YIN-style f0 diagnostic for a monophonic stem: `status`
  (`ok` / `not_applicable`), `reason`, median f0 per side, median signed and
  absolute cents error, p90 absolute cents and `fraction_within_25_cents`.
  A full mix (no `stems`), a near-silent side or weak periodicity reports N/A
  with the numeric fields null instead of a bogus cents number.
* `transient`, `onset` — the legacy global flux ratio
  (`transient.flux_ratio_db`, diagnostic only) and the v1.3.0 split:
  `onset.reference_count`, `candidate_count`, `reference_density_per_s`,
  `candidate_density_per_s`, `density_ratio_db`, `paired_count`,
  `onset_strength_median_db` and the best-lag flux correlation. The onset term
  is alignment-invariant per-attack strength; density is events/s and is N/A
  below 5 attacks per side.
* `score` — the 0–100 heuristic and its components. `value` is the v1.3.0
  formula, `previous_value` the v1.2.0 formula on the same measurements, and
  `legacy_value` the pre-1.1 three-term formula. Each component in the result reports its weight.

When no output path is supplied, the WAV goes to bounded audition scratch
storage. Keep a durable explicit output path if another workflow depends on the
full render. Session replay uses the archived excerpt independently of scratch
retention.

To compare the original recording, call `audition_clip_source` with a clip
index from `audio_clips_list` and a window in source seconds. The tool reads
the file before clip processing and mixer effects, and returns a bounded inline
excerpt for the session. Use `audition_range` for the corresponding edited
timeline window.

With `listen: true`, `listening` carries a loudness-matched pair:

| Field | Definition |
|---|---|
| `status` | `matched` within 0.1 LU, otherwise `unmatched`; `unmeasured` when one side has no LUFS |
| `duration_s` | Shared excerpt length after trimming |
| `reference_lufs`, `candidate_lufs`, `delta_lu` | Measured loudness and candidate − reference |
| `reference_gain_db`, `candidate_gain_db` | Applied matching gain, including the peak ceiling trim |
| `reference_audio`, `candidate_audio` | `mime`, `sample_rate`, `channels` and base64 WAV |

`listening_only: true` returns only the pair and window, without alignment or
similarity metrics. It requires `listen: true` and a duration of 0.4–8 seconds.

### Refusal rules

Both files must share a sample rate. `render_range` renders at the app's
configured output rate (44.1 kHz by default), so render the reference and the
candidate at the same rate before comparing; a mismatch is refused rather than
resampled.

Comparing silently across formats hides whether a difference comes from the
mix or the file. By default `audio_compare` returns an error when the two files
have different sample rates or different frame counts. Passing
`allow_mismatch: true` opts into a lenient comparison (resample the candidate
to the reference rate, compare the aligned overlap) and records a warning.

## 4. Transactions — one revision, one undo entry

`transaction_begin` snapshots the project, DSL text, dirty flag, file path and
undo mark, and returns the base revision. `transaction_commit` keeps the batch,
publishes exactly one revision and squashes the batch into one undo entry
labelled `Transaction: <label>`. `transaction_rollback` restores the snapshot,
drops the batch's undo/redo entries and moves the revision forward so stale
`expected_revision` guards fail.

* **Edits do not publish revisions individually.** Inside an open batch the
  first mutation marks the transaction; the revision stays at the base until
  commit. `expected_revision` accepts the base for every call in the batch.
* **A refused mutation changes nothing and leaves the batch open.** `dispatch`
  wraps every mutation; if the handler returns an error, or the revision guard
  refuses the call first, the error is the result and the open transaction keeps
  the edits before it. The caller reads that error the way it reads any other
  tool result and may adapt, so a stale `expected_revision` costs one call
  rather than the batch. Only an explicit `transaction_rollback` undoes a batch;
  a cancel, a stop or a failed commit leaves it open for its owner, and an empty
  batch commits nothing, so a turn whose writes were all refused publishes no
  revision and leaves no undo entry.
* **No partial persistence.** `project_save` and `redo` refuse while a batch is
  open, so a crash cannot leave a half-applied edit on disk or replay a redo
  under the batch. `project_open` / `project_new` cancel the batch because they
  replace the whole session.
* **`undo` works inside the batch.** It rewinds the newest edit: the batch's own
  last step, or the edit before the batch when the batch has not edited yet. The
  batch keeps recording, its commit still publishes one entry, and a rollback
  puts back the history it found.
* **Undo depth is one per transaction.** After commit, one `undo` restores the
  pre-batch project and one `redo` re-applies the whole batch.
* **Renders see the working state.** Every tool reads the project the batch has
  edited so far, and `render_range` renders it: a model can audition its own
  in-turn edit. The pre-batch render is kept while the batch is open, so
  `transaction_rollback` restores it and the next window is the same bytes with
  `cache_hit: true`; a commit drops it.

`transaction_begin` is itself a mutation tool (it takes `expected_revision`)
but is not counted as an edit.

* **One owner per batch.** A batch records the caller that opened it: a panel
  turn is `panel:<conversation>`, and an HTTP or MCP client is `http`. A
  mutation from any other caller is refused before it writes, and only the
  owner may commit or roll it back. An external `/call` therefore cannot join a
  turn's batch, and the turn's commit never includes it.
* **A reloaded panel recovers its own orphan.** The panel calls
  `ai_batch_recover` once on mount; it rolls back an open `panel:` batch and
  returns the restored document, so `project_save`, `redo` and preview work
  immediately. A batch an external client owns is left alone, and a call with
  no batch open is a no-op.

### The panel turn is one transaction

The panel loop wraps a turn in the same commands an external session calls, so a
turn is one undo step. No stop discards it: cancel, provider failure, timeout and
the round cap all keep the batch and report how many edits stayed. The stopped row
offers Continue (and Retry when the last request failed), and the transcript
points at the user message's revert action.

* **Opening the batch.** Every panel tool carries a `mutation` flag, read from
  `is_mutation_tool`. The loop opens the batch before the first call with that
  flag; the panel-local `edit_subtree` opens it too when the session applies the
  replacement itself (`autoApply`). A turn that only reads opens no batch, so it
  cannot disturb the undo or redo history.
* **One entry.** The answer commits the batch with the turn's label (the panel
  passes `AI edit`; a batch the model opened keeps the model's label). One undo
  restores the pre-turn project, and the commit publishes one revision.
* **The model cannot commit early.** While the turn's batch is open, the loop
  answers a model `transaction_commit` with `committed: false`: the batch stays
  open until the turn ends, so calls after the model's commit stay in the same
  entry. A model `transaction_begin` is adopted when no batch is open and
  answered with `active: true` when one is.
* **A refused mutation does not stop the turn.** The backend returns the error
  and keeps the batch, so the loop hands it back as an ordinary tool result and
  the model may read it, retry with the current `revision`, or do something else
  in the same turn. What already landed still commits once at the end, and a
  refused call is not counted as an edit.
* **Every stop keeps the batch.** Cancel, a provider or transport failure, a
  timeout and the round cap all commit the open batch and report how many edits
  were kept, so a stop cannot discard completed work. The row offers Continue,
  which resumes the wire where the loop stopped; it offers no Retry, because
  resending the turn would apply the same edits twice. A commit that fails
  leaves the batch open and says so; it is never rolled back automatically.
  The notice points at the user message's context menu for a deliberate revert.
* **The round cap is the same rule.** The cap is a budget stop, not a failure:
  when the loop reaches its last round it commits the open batch like an answer
  and says how many edits were kept, so an over-budget turn keeps the work that
  landed as its one undo entry.
* **A model rollback asks, then discards the batch.** `transaction_rollback`
  asks first: it throws away the edits the turn already applied. Once it is
  approved the loop drops its own record of the batch, so the turn end neither
  commits nor rolls back again, and a mutation after it opens a fresh batch that
  commits as the turn's new undo entry. A declined rollback never ran, so the
  open batch and its edits stay and still commit once at the end.

### A destructive call needs the user

Some calls cannot be made right by reading the transcript. Deleting a track, bus,
section, preset or effect removes an object the user built; replacing the whole
project goes further; a rollback discards the batch the turn already applied.
One undo restores the bytes, but not the object the user meant to keep, so these
calls ask before they run.

A conversation's permission has three values, and it decides what an automatic
write may do:

| mode | an automatic write |
| --- | --- |
| `require_approval` | asks before it runs |
| `autoreview` | validates the change, records its diff in the transcript, then applies it in the same turn |
| `write` | applies directly, without a prompt or a diff row |

A conversation with no stored mode reads as `write`. The mode governs automatic
writes only: a destructive call is a floor under all three.

* **The policy is data.** `tool_registry::approval_policy` classifies every tool
  as `Automatic` (apply inside the turn's batch, as one undo entry) or `Confirm`
  (ask first). The panel wire carries the answer as `approval`; the loop reads
  that field and never infers the class from a tool name. A test pins the whole
  `Confirm` set, so a new tool has to be classified on purpose.
* **The `Confirm` set.** `tracks_remove`, `buses_remove`, `sections_remove`,
  `presets_remove`, `effects_remove`, `transaction_rollback`, plus `project_new`
  and `project_open`, which the panel already excludes. Everything else that
  changes the project — adding, renaming, mixing, moving, applying a preset or a
  plugin state, undo and redo — stays `Automatic`.
* **A `Confirm` call asks in every mode.** The three modes decide whether an
  automatic write is announced; none of them is a reviewer that has seen this
  deletion, so the card is shown under `write` as it is under
  `require_approval`. Only an answer on the card lifts it, and only for the
  conversation that gave it.
* **The card.** The panel shows one card naming the tool, the target resolved
  against the open project (track name, bus, section, preset, device, file) and
  the consequence. The turn is paused, not cancelled: no other call runs while
  the card is open. Approve applies the call through the turn's batch and
  continues; Always allow records the tool for this conversation and continues
  the same way; Reject answers the call with `declined: true`, names the object,
  and says nothing changed, so the turn continues and the model can propose
  something else.
* **Always allow belongs to one conversation.** The panel keeps the tools a
  conversation allowed as its own preference, beside the theme and the locale,
  not in the project or the saved session. A later turn of that conversation
  runs the tool without a card and still commits one undo entry, unless the
  conversation asks for every change (`require_approval`), which keeps asking.
  A fresh conversation starts from the default and asks again, and a choice
  made in the panel never widens a headless session.
* **A declined call opens no batch.** The approval comes before
  `transaction_begin`, so a turn whose only call was rejected leaves the project
  byte-identical and no undo entry behind.
* **Nobody to ask means no.** In a session with nobody to approve (`autoApply`,
  or a caller that supplies no approver) the call is refused with a readable
  result naming the tool and its arguments, and the turn continues. Silence is
  never an approval.
* **Cancel while the card is open** settles the card as a decline and stops the
  turn at the loop's own check: the batch keeps the edits that already landed,
  like any other cancel, and the transcript's notice says what landed.

### A cancelled turn

Cancel stops the loop at the next call boundary. A call that was already
dispatched cannot be recalled, so it returns first and is counted; a call the
loop had not dispatched yet never runs. The model's remaining calls in the
interrupted round do not run. The batch keeps the edits that already landed and
commits them as the turn's one undo entry.

The panel writes one transcript line built from the loop's counts: the calls the
model had requested, the calls that returned, and the applied edits that were
kept. A cancel before any edit ran says the project is unchanged. The line points
at the user message's context menu for a deliberate revert; the project keeps the
edits until that action runs.

The line is transcript-only: the saved conversation keeps the turn's user
message (see `docs/ai-sessions.md`).

### Retry and continue after a stop

A stopped turn leaves one actionable row. It offers **Retry** when the last
request failed in a way a resend can fix — a transport error, a timeout, or a
reply the loop could not use (malformed tool arguments, an empty or cut-off
reply, a stalled read). Retry sends that request again over the wire the loop
held; it does not replay the calls that already ran, so a kept edit is safe. It
offers **Continue** whenever there is a wire to carry on from: a reportable
stop, a cancel, a kept failure, or a tool result the model never answered. A
refused mutation is not one of those: it ends no turn, so it leaves no row. A
cancel and the round cap offer Continue only: the user stopped one, and the cap
is not a request failure. A configuration or authentication failure offers no
Retry either, because the same request cannot start working; the panel's
provider layer decides that and the error carries `retryable`.

* **The row keeps the wire.** The loop hands the panel the messages it last held,
  system prompt included, plus the edits a stop kept. Retry sends exactly
  that array through the same loop entry as Send, so the model sees the tool
  results it already earned and nothing is retyped.
* **The wire stays inside the request budget.** Before every provider request
  the loop counts its messages and prompt-text bytes against the bounds `ai_chat`
  enforces: 100 messages and 2 MiB, with a margin so one round of results cannot
  reach them. When either bound is near, the oldest part of the wire is folded
  into one summary row: the turn's first user message and the newest six
  call/result pairs stay whole, and a call is never separated from its result.
  The fold is reported as its own transcript line, the summary row is marked
  `summary`, and a Retry resends the folded wire rather than the oversized one.
* **No second question.** The store already holds the turn's user message; a
  request whose tail the conversation has appends nothing (`absorb` in
  `ai/sessions.rs`), so `sessions_load` keeps one user row for the turn.
* **One entry per resumed turn.** Retry and Continue start a turn, not a
  continuation of the stopped one: the loop opens a fresh batch before the first
  mutating call and commits it when the resumed turn answers. A call the model
  asks for again runs inside that batch, never outside it.
* **A kept turn says so.** A failed or cancelled turn that applied edits commits
  them before its row appears, and the row states how many edits were kept and
  points at the message menu for a deliberate revert. A kept failure or timeout
  still offers Retry: it resends only the failed request, with the applied
  results already in the wire.
* **A stop can land mid-round.** The model may have asked for more calls than
  returned. Before resuming, the panel closes that round with one `not run` tool
  result per missing call, so the request never carries an unanswered call and
  the model is not told a call landed.
* **An empty reply is a failure.** A provider that answers with no text and no
  tool call still ends the turn's batch like any answer, but the loop reports the
  turn as failed with `Provider returned no text.` instead of passing the
  fallback on as prose. The panel then paints a failed row with Retry, not an
  answer bubble that reads like finished work.
* **The loop names the call it dispatches.** `onTool` reports each call before it
  runs, so the panel's status strip can say which tool is in flight instead of
  showing the previous round's text. It is a progress hint only: it changes no
  dispatch, no approval and no transaction.
* **Where it resumed.** Continue names what it continues from, both on its row
  (`Continue resumes after 2 of 5 requested calls`) and in the transcript line
  the click writes.
* **A failure does not eat the composer.** The draft and its attachment chips go
  back to the composer when a turn fails or stops, and Retry and Continue leave
  them there, so a failed turn costs the typing once.

## 5. `render_offline` — full-song export

| Argument | Type | Meaning |
|---|---|---|
| `path` | string | Output master WAV (required) |
| `preRoll` | number | Wall-clock plugin warm-up (0–600 s, default 0) |
| `sample_format` | string | `i16`, `i24` or `f32`; defaults to the project's export setting |
| `dither` | boolean | TPDF at one LSB; integer formats only, refused for `f32` |

Result: `{"path": "/tmp/song.wav"}`. The tool is excluded from the built-in
panel for cost reasons but remains in the HTTP/MCP catalog. The schema declares
all four arguments, so a model can discover the format and dither overrides the
dispatcher already accepts.

### Lock policy

A render never holds the project lock while it computes.

* **Snapshot under the lock.** The project, sample rate, SoundFont path,
  project resource root, forked CLAP instances, pre-roll, the content key and
  a render generation are captured, then the lock is released.
* **Render off-lock.** The full mix is rendered from the snapshot, and the WAV
  is staged in a hidden file beside the target. Neither step touches `AppState`.
* **Publish under the lock.** The staged WAV is renamed into place and the
  clean per-track cache is stored in `AppState::range_cache` only after two
  checks: the content key still equals the snapshot's, and this render is
  still the newest one started. The WAV write happens off-lock; the rename and
  the cache store are the only work under the lock.

A second tool call therefore runs while a render is in flight; only the short
publish step is serialised. A render started later supersedes this one, and a
committed edit makes it stale.

### UI thread

Both exports are `async` Tauri commands — the full mix (`render_offline`) and
the toolbar's stems export (`render_stems`) — so the webview thread never runs
either render. Each takes the same shape: the snapshot is taken on the command's
own task (it copies the project and forks the CLAP instances), the render and
the staged writes run on Tauri's blocking pool, and the invoke resolves only
once the result is published — `{"path": ...}` for the mix, the stems summary
(`directory`, `manifest`, `stems`, `frames`, `loudness`) for the stems.

The stems export writes several files, so it stages the whole set — mix WAV,
every stem WAV and `stems.json` — in a hidden `.stems.oc-render-<generation>.tmp`
directory inside the target and moves the files into place only at publish, the
manifest last. A superseded or failed export removes the staging directory, so
it leaves no half-written set of stems behind; a publish that fails part-way
removes the files it already moved. The stems export does not seed `range_cache`.

The window stays responsive while it renders: the status bar shows the existing
`rendering...` chip. Both toolbar buttons are disabled for the duration of the
invoke (`exportBusy`), because a second export would supersede the first; the
command itself runs off the IPC thread, which is what keeps the window
responsive, not the buttons. There is no per-frame progress channel. A second
export started by an agent meanwhile follows the generation rule below — it
wins, and the older export returns the superseded error without publishing.

### Staleness and cancellation

* **Content key, not a content hash.** `AppStateInner::content_epoch` is the
  key the shared range cache uses (`take_range_cache` / `store_range_cache`
  compare it), so a content hash would accept a render the cache would then
  discard. Every edit bumps it, including an edit inside an open transaction,
  and a rollback restores the pre-transaction value. A render whose snapshot key
  no longer matches is dropped with
  `project changed during render (content key A -> B); result not published`.
* **Generation.** `AppStateInner::render_generation` is bumped when a render
  starts. Publishing requires the current value to equal the render's ticket;
  an older render that finishes later returns
  `render superseded by a newer render; result not published` and cannot
  overwrite the newer render's output or cache.
* **No stale cache entry.** A stale or superseded render publishes nothing: its
  staged WAV — or, for the stems export, its whole staged set — is removed. An
  edit that lands mid-render therefore cannot become the cached render for the
  new state, and a superseded render cannot replace a newer file on the same
  path.

The arguments and the `{"path": ...}` result are unchanged, and a clean full
export seeds `range_cache` under the same content key that `render_range` reads,
so a following window of the same state is a cache hit.

## 6. Plugin programs and state

`plugins_program_capabilities` is data, not a promise: `enumerate` is `native`
(VST3 reads the plugin's own program list) or `none`, and `select` lists the
mechanisms the format supports. An empty program list therefore means "this
format exposes no list", never "this plugin has no patches".

* `plugins_programs {preset}` lists the native programs and the format's
  capability. It builds a temporary instance when the preset is not loaded.
* `plugins_select_program {preset, index | name}` writes the chosen program's
  state into the project (embedded by default, or a sidecar with `mode=file`).
  An unknown index or name is refused with the programs that were offered.
  `state_file` is the state-injection fallback for formats with no native list.
* `plugins_read_state {preset, max_bytes?}` captures the live state as a value:
  `state_hex`, `bytes`, `sha256` and `truncated`. The default cap is 256 KiB
  and `max_bytes` may rise to 3 MiB (the hex of a larger blob does not fit one
  agent call); over the cap `state_hex` is null and `truncated` is true, never
  a fragment.
* `plugins_apply_state {preset, state_hex}` verifies the envelope's format,
  plugin identity, version and SHA-256, then writes the value into the preset's
  project parameter and clears any `state_file`. Both apply and select are
  `PluginState` mutations: revision guard, one undo entry.

`plugins_save_state` (embedded/file) stays the live-save path and is excluded
from the panel because of the sidecar path; `plugins_read_state` /
`plugins_apply_state` are the value round-trip the panel offers. A state larger
than the inline ceiling goes through `plugins_save_state mode=file` and
`plugins_select_program state_file`.

## 7. Producer operations — tempo, meter, copies, history, stems

A producer asks for these directly, and before this section each one meant
either a whole-subtree `project_edit` or a refusal. They are ordinary tools on
the same surface, so the revision guard, undo and transaction machinery apply
unchanged; six of the seven are panel-enabled.

| Tool | Mutation | Panel | Set | Returns |
|---|---|---|---|---|
| `tempo_map_set` | yes (Transport) | enabled | `project-edit` | the written points, the project tempo, revision |
| `time_signature_set` | yes (Transport) | enabled | `project-edit` | the written points, the base meter, revision |
| `tracks_duplicate` | yes (Project + Preset) | enabled | `project-edit` | new index, name, preset, revision |
| `presets_rename` | yes (Preset) | enabled | `project-edit` | new name, the tracks and patterns that moved |
| `presets_duplicate` | yes (Preset) | enabled | `project-edit` | new name, whether references moved |
| `undo_history` | no | enabled | `project-edit` | the stack entries plus applied/redo counts |
| `render_stems` | no (writes files) | excluded | — | the manifest path and every file with its levels |

* **Tempo and meter maps.** Both carry the map, not one point, and hand back
  what the project now holds, so a call either lands the change or explains it.
  An empty list clears the map (back to the project tempo / base meter). The
  engine's clock validates the map — ascending unique positions, tempo
  1–400 BPM, meter numerator 1–32 over 1/2/4/8/16/32/64 — and the tool appends
  the points that were sent to the refusal, e.g.
  `Invalid tempo map: tempo map must have unique ascending positions and valid tempos (points: 13:1 = 140 BPM, 9:1 = 90 BPM)`.
  A refused map leaves the project, the undo stack and the revision untouched.
  Meter edits remap arranged content to the same absolute musical time, as the
  GUI's meter lane does.
* **Track copy.** `tracks_duplicate` is the toolbar's duplicate: the copy lands
  after the last track, keeps the instrument, engine and output routing, and
  owns a preset copy (`<preset> copy`). Mix, clips, effects and automation are
  not copied — the copy is a sound, not a part — and the description says so,
  so a model asked to duplicate a *part* follows up with `project_edit`.
* **Preset rename.** One `Preset` undo entry renames the preset and rewrites
  every reference (track presets, top-level and inline pattern presets, and
  `extends`), then reports which tracks and patterns moved. A taken name, an
  unknown preset or an empty name is refused before anything changes.
* **Preset copy.** `reassign` (default true) is what the patch editor's
  Duplicate does: the references move to the copy, so the original stays free
  to edit or delete.
* **History read.** `undo_history` is read-only and never moves the cursor; it
  exists so a model can describe what happened. `undo`/`redo` are the only
  cursor movers, and neither is hidden behind the history tool.
* **Stems.** `render_stems` is the toolbar's Export stems: mix + every track
  and bus + `stems.json` into an existing directory. The reply includes the set's
  format, sample rate and channel count, plus each file's frame count, peak,
  RMS and integrated LUFS. It refuses a directory that does not exist
  rather than creating a tree from an agent-supplied path, and the panel keeps
  it out for the same unbounded full-song cost that excludes `render_offline`;
  the panel's windowed alternative is `render_range` with `stems: true`.

`meter_snapshot {}` reads the same published live snapshot as the mixer. It is
read-only, available over HTTP/MCP and in the panel's **Ears** toolset, and never
starts playback, opens an audio device, enables an analyser or renders a project.
Check `available`, optional `reason`, and `playing` before interpreting readings:
no streaming graph means unavailable, and a paused snapshot may retain the last
published chunk. It does not measure edits that have not reached playback.

The result preserves master channels, requested/applied mono state, track/bus
identities, peak/RMS levels in dBFS, gain reduction and effect-band values in dB,
clipping latches and optional ceiling margins. Master readings also include
optional short-term LUFS over `short_term_window_seconds` (3 seconds), stereo
correlation before the mono fold, and interleaved left/right goniometer points.
Spectrum bands appear only while an analyser is already enabled. Optional fields
are omitted when their owner has no measurement; absence is not a numeric zero.
An unavailable snapshot may still contain retained values: use its availability
state rather than treating those as current measurements. LUFS is not an
integrated full-song loudness result. This live snapshot
is not an atomic measurement of every graph node at one instant. For a
reproducible headless measurement use `render_range` + `audio_analyze`.

## 8. Device presets

The device preset library exposes the two operations the device panel offers:
read the bank, and apply one entry to an effect insert.

| Tool | Mutation | Panel | Set | Returns |
|---|---|---|---|---|
| `device_presets_list` | no | enabled | `project-edit` | every preset row with `source`, `shadows` and `deletable`, plus `warnings` |
| `device_preset_apply` | yes (Preset) | enabled | `project-edit` | the project snapshot after the one undo entry |

* **Listing.** `device_presets_list {device?}` merges the shipped factory bank
  with the user file under `~/.opencompose`. `device` narrows the answer to one
  catalog device; an unknown id is refused, and omitting it lists the whole
  library. Every row carries `source` (`factory` or `user`), `shadows` (a user
  row replaced a factory row with the same id) and `deletable` (false for
  factory rows). A broken user file lands in `warnings` while the factory rows
  still list, exactly as the panel sees it.
* **Applying.** `device_preset_apply {id, target, effect_index, device}` applies
  one preset to the effect insert at `effect_index` on `target` (`track:N`,
  `bus:name`, `master`, `preset:name`) as one `Preset` undo entry. The insert
  must already hold the preset's device: a cross-device apply is refused with
  the same sentence `apply_device_preset` returns, and an unknown id is refused
  by name. All four arguments are declared because the dispatcher reads all
  four; `expected_revision` is added to every mutation tool.
* **`intent` is authored data.** A preset may carry `intent`, one short
  sentence saying what it is for; the 25 factory presets all do. The panel
  shows it as a quiet line under the picker. It is stored as written and never
  translated, like a preset name: it is authored text, not UI copy, so it does
  not go through `i18n`. An empty or whitespace-only `intent` is absent.
* **Saving and deleting are deliberately not exposed.** A device preset save or
  delete mutates the operator's own library, outside the project and outside
  undo. That needs an approval model of its own, so the agent surface stops at
  listing and applying; `save_device_preset` and `delete_device_preset` stay
  Tauri commands the panel calls.

## Tests

* `crates/oc-engine/src/render_cache/tests/range.rs` — full-slice equality
  (including a stateful master reverb), cache reuse without re-rendering,
  deterministic windows, stem slices, zero padding, subset-as-slice equality
  (including a note that rings in from before the window), the cached full mix
  staying valid after a subset request, and the window-only preview renderer.
  `tests/plugins.rs` pins shared-output group expansion.
* `crates/oc-engine/src/analysis.rs` (tests module) — analysis levels equal the
  meter, true peak, spectral peaks, known gain change detection, max sample
  difference, refusal of unequal lengths/rates.
* `crates/oc-gui/frontend/src-tauri/src/state.rs` — byte-identical rollback,
  one undo entry per transaction, one revision per commit, no-edit rollback.
* `crates/oc-gui/frontend/src-tauri/src/agent_api.rs` — panel round trip
  (seconds + bars + cache hit + analysis + compare), failed-batch rollback,
  transaction guards; and the producer tools: a tempo map written and read back
  (plus clear), an out-of-order/duplicate/out-of-range tempo map refused with
  the points that were sent, a meter map written and read back, four invalid
  meters refused, a duplicated track with its own preset, an unknown track or
  empty name refused, a preset rename that rewrites its track, a taken name
  refused with the project unchanged, a preset copy that moves references by
  default, a taken copy name refused, the undo history listing labels and
  scopes without moving the cursor, and a stem export whose every returned path
  exists (plus a missing directory refused without creating it).
* `crates/oc-gui/frontend/src-tauri/src/tool_registry.rs` — pinned counts,
  surface partition, schema validation, one dispatch arm per enabled tool, and
  one schema property for every argument a dispatch arm reads.
* `crates/oc-gui/frontend/src-tauri/src/device_presets.rs` — every factory
  preset carries a one-line intent, an intent round-trips through the document,
  a blank intent is absent, and a user save without one writes no intent key.
* `crates/oc-gui/frontend/src-tauri/src/commands/tests/device_presets.rs` —
  applying writes every parameter as one undo entry, the cross-device refusal
  equals the command's sentence, an unknown id is refused by name, and the
  listing answers factory plus user rows with `source`, `shadows` and
  `deletable`, filtered by device.
* `crates/oc-gui/frontend/src/__tests__/components/DevicePresets.test.tsx` —
  the picker shows the intent of a factory preset and nothing for a preset
  without one.
* `crates/oc-gui/frontend/src-tauri/src/commands/tests/render.rs` —
  `audition_range` returns a 16 kHz mono clip whose full-rate file is byte-equal
  to `render_range`'s, truncates a long window, repeats byte-identically and
  refuses the same windows as `render_range`; a second
  tool call completes during an in-flight render, an edit and a newer render
  are not published (no WAV, no cache entry); the export renders on the blocking
  pool rather than the command thread while the app lock stays free; a second
  export supersedes one in flight instead of corrupting either output. The same
  holds for the stems export, whose staged set is removed as a whole when a
  render fails, a publish fails part-way, or a newer export supersedes it.
* `crates/oc-gui/frontend/src-tauri/src/ai/sessions.rs` — `prepare` closes a
  round whose calls never got a result (a window reloaded mid-turn) with one
  `not run` result per call, saves it once, and leaves a round that got its
  results alone.
* `crates/oc-gui/frontend/src-tauri/src/ai_service.rs` — `ai_active_turn`
  reports the one request in flight, names its conversation only once that is
  resolved, refuses a second claim, and reaches the request it names on cancel.
* `crates/oc-gui/frontend/src/__tests__/ai-panel-reload.test.tsx` — a reloaded
  window keeps the draft, the chips and the queue per conversation, sends from
  the restored queue in order, keeps the turn marker only while the backend
  reports that turn, clears it (painting nothing as running) when the backend
  reports none or another conversation's, and paints an answer that arrives
  after the reload into the conversation the marker named.
