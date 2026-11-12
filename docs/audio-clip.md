# Audio clips — core import / decode / playback contract

> Scope: DSL, decoding, resampling, offline rendering and the live/streaming
> clip lane. Waveform drawing and timeline editing live in the GUI.

## Import aligned stems in the GUI

Export mono or stereo WAV stems from the same start in the source DAW. Keep
any leading silence, pickups, and effect tails. Save a destination project with
no tracks or sections, then choose **File → Import Aligned Stems**. Select the
WAVs and, if available, one MIDI file from the same origin.

The import copies the WAVs into the project's `media/` folder and starts each
one at bar 1, beat 1 at its original playback rate. Imported stereo stems use
stereo balance at unity when centered, including files with identical left
and right channels. An optional MIDI file
supplies tempo and meter changes and creates muted, editable tracks. With WAVs
alone, the destination clock stays in effect. Save the project after import
and keep its `media/` folder beside it when sharing the project.

Send source routing, plug-in settings, and mixer automation separately.

## Precise placement and source trim

Right-click a clip, open its gain/fade editor and use the Basic tab's **Start
time (s)** or **Nudge (ms)** controls. Earlier/Later moves the entire clip;
it retains the source attack, fades, warp mapping and elapsed duration.
An absolute start is measured from project zero, including tempo changes.
Musical dragging retains the fine offsets relative to its musical anchors.

Agents use `audio_clips_list` for effective `start_seconds`, `end_seconds`,
clip index and revision, then `audio_clip_set_placement` with exactly one of
`start_seconds` or signed `nudge_ms`. The same undoable operation serves the GUI.
This controls artistic timing; it does not change host latency compensation.

Source `offset:` trims media instead. It cannot delay a complete attack.
Splitting retains the two outside placement offsets and snaps the new seam to
the requested grid boundary. Trimming clears the changed edge's fine offset;
stretching clears the tail offset. Source advances at splits/trims are measured
in frames at the current live output rate, preserving the seam at that rate.
Rendering at a different rate can quantize a boundary to a neighboring sample.
Unchanged legacy endpoints retain their existing sample flooring; fine endpoints
round the combined anchor-plus-offset once at the render rate.

## 1. DSL syntax

An audio clip is a project-level placement that names a destination track. The
track's mix, sends, mute/solo state and effect chain apply unchanged, exactly as
for a note-driven track.

```ocp
@ocp "0.2.0"
@project "Radio edit"
  tempo: 120

@audio_clip intro [1:1 - 5:1] | track: tape
  file: "media/intro.flac"
  offset: 250ms
  loop: 2s
  gain: -3dB
  gain_envelope: 0:0dB, 0.5:-6dB, 1:0dB
  fade_in: 5ms
  fade_out: 1.5s
  pitch: -2

@audio_clip take2 [1:1 - 3:1] | track: tape
  file: "media/take-vox-2.wav"
  take_group: "vox take 1720000000000"
  take: 2
  muted: true

@track tape | instrument: audio/clip
  mix: vol=-6dB, output=stems
```

| Property | Meaning | Default |
|---|---|---|
| header range | absolute placement, same `[bar:beat]` grammar as `clip:` | required |
| `| track:` | destination track name | required |
| `file:` | media path relative to the `.ocp` file | required |
| `placement_start:` / `placement_end:` | signed seconds added to the musical start/end anchors; move both equally to retain elapsed duration | `0` |
| `offset:` | seconds trimmed from the start of the (stretched) media (`ms`/`s`) | `0` |
| `loop:` | seconds of media the clip repeats from `offset`; shorter than the clip | `0` |
| `gain:` | clip gain in dB (same unit as `mix: vol`) | `0 dB` |
| `gain_envelope:` | comma-separated `position:gain` points, normalised to the clip; relative to `gain:` | absent |
| `fade_in:` | linear fade from silence, in seconds | `0` |
| `fade_out:` | linear fade to silence, in seconds | `0` |
| `invert:` | reverse polarity at mix time | `false` |
| `crossfade:` | equal-power fade at the head and tail, in seconds | `0` |
| `stretch:` | time-stretch factor; `1.0` is a bit-exact bypass | `1.0` |
| `mode:` | `off`, `percussive` or `musical` | `musical` |
| `tempo_follow:` | derive `stretch` from the range and the media length | `false` |
| `pitch:` | transposition in semitones, fractional allowed (`24` = +2 octaves) | `0` |
| `take_group:` | identifier shared by the passes of one loop-record session | absent |
| `take:` | one-based pass number inside `take_group` | absent |
| `muted:` | keep the clip in the document but contribute no audio | `false` |

Rules enforced by `oc_dsl::validate`:

- names are unique; the destination track must exist;
- `gain_envelope` is either absent or has at least two points, starting at
  `0` and ending at `1`, with strictly ascending positions and finite gains;
- `file` is non-empty and relative to the project (absolute paths are rejected
  in DSL documents; the engine API still accepts them);
- offset, loop, fades and crossfade are finite and non-negative, gain is finite;
- a loop is at least 100 ms, ends inside the media, and is shorter than the
  clip; the engine enforces the same three rules at validate/render time;
- `stretch` is finite and positive; the engine additionally refuses ratios
  outside `0.25..=4.0` at validate/render time;
- `pitch` is finite and within `-24..=+24`; the engine refuses anything wider
  with the clip name, and refuses any non-zero pitch on `mode: off`;
- `take_group` is non-empty and `take` starts at 1;
- the effective start is finite and non-negative, and the effective end is after it.

Clips shorter than their media play the trimmed prefix; longer ones hold silence
after the media ends. `offset` beyond the media is silence, not an error.

A `loop:` repeats the region that starts at `offset` for the clip's whole range.
The region wraps sample-exactly, so a looped clip fills its length instead of
holding silence, and the fades still run over the clip rather than the loop. The
loop reads only the media that exists: a region past the file end is refused
rather than played as silence.

Splitting or trimming a looped clip re-times the window with the edit: the new
head plays the media at `offset + delta mod loop`, and a half that still crosses
a seam keeps the loop. A half that repeats nowhere inside its length plays one
pass of the same media. An edit that would cut a seam the right half cannot
repeat, or repeat media past the file end, is refused instead of writing a
document the engine drops. Both fades are clamped to the length they now cover.
Stretching is described in `docs/audio-clip-stretch.md`; the crossfade law and
the comp workflow are in `docs/comping.md`.

### Muted takes

`muted: true` removes a clip from the mix without removing it from the
document. The engine treats it exactly like a clip that is not there: it does
not decode the media, does not reserve stretched-media memory, does not extend
the render length, and does not fail validation when the file is missing. The
clip keeps its name, range, gain, fades and take fields, so unmuting it later
restores the same performance.

Take recording writes every pass into the same `take_group`. When the transport
stops, the newest pass is unmuted and every earlier pass is muted, so the track
sounds like the last pass until a comp chooses another. A muted take stays in
the DSL so the performance is never lost; the GUI take rows show the mute state
and offer session-only solo, which changes the live render without writing
`muted:`.

## 2. Decoding

`oc-engine::audio_clip` uses `symphonia` with `wav`, `pcm`, `flac`, `mp3`,
`aiff`, `ogg` and `vorbis`. Enabled by default, like the existing `hound` /
`claxon` sampler dependencies; the render path is the only consumer.

- Decode loops over packets of the default track; partial packets are rejected
  with the clip name and path in the message.
- Every channel layout is downmixed to stereo. Mono is duplicated; stereo is
  passed through; >2 channels use the standard film matrix (centre and surrounds
  at −3 dB, LFE at −6 dB), keyed by symphonia's `Channels` bit flags.
- Decoding is bounded to 30 minutes of audio per clip, matching the existing
  audio-region media bound.
- Not enabled: OGG Opus, AAC/MP4, CAF, MKV. Adding a symphonia feature is the
  only change needed for the codecs that symphonia supports.

## 3. Resampling

Rates are converted once, at render time, before the clip is placed on the
timeline. The implementation is a deterministic windowed-sinc interpolator:

- 48 taps (24 per side), Kaiser window with β = 8.6, and a per-output-frame
  kernel normalisation so DC gain is exactly unity;
- when downsampling, the cutoff is moved to `0.97 × dst/src` of the source
  Nyquist, so content above the destination Nyquist is filtered rather than
  aliased; when upsampling the cutoff stays at the source Nyquist;
- edge samples clamp to the first/last frame rather than fading to zero.

This is a "decent offline" resampler (≈90 dB stopband with the chosen window),
not a mastering-grade polyphase bank. The 30-minute bound and the decode cost
are intentional: a clip is decoded/rendered on the worker/offline path, never in
the audio callback.

### Transposition and formants

`pitch:` reuses this converter instead of adding a pitch shifter. The media is
resampled to a rate `2^(semitones/12)` times the project rate, then time-stretched
back by the same factor, so the clip keeps its duration. A +12 clip therefore
reads its media twice as fast and stretches it 2×, and the stretch algorithm of
`mode:` decides how well that survives.

By default the resampling step moves pitch and formants together. Set
`preserve_formants: true` to compensate with an approximate spectral-envelope
correction. Set `formant:` in semitones for an independent change in colour.
The two settings can be combined, with a total correction of up to ±12
semitones. Both run in the derived-media bake. Clip corrections wider than
three semitones preserve unvoiced spans and crossfade the filter at the edges
of detected voiced regions. The envelope analysis uses the local pitch period
to keep the excitation harmonics out of the shifted envelope. At standard
recording rates, wide corrections taper from 5 to 10 kHz so the original
high-frequency roll-off is retained.

For a natural vocal, start with a small pitch change and audition the same
phrase with preservation enabled and disabled. Compare at matched listening
levels and leave headroom in the mixer. Formant correction changes the
spectral balance and can raise peaks even when the average level falls.
If a wide shift softens consonants or makes a vowel unstable, reduce the shift
or apply it to a shorter chop. For a stereo vocal, listen to the stereo and
mono versions before settling on the edit.

### Warp anchors

Write `warp: 0.5s -> 0.65s` to place a source event at a different processed
time. Add further `warp:` lines in increasing source and target order. The
origin is implicit and both coordinates precede any clip offset. The chosen
stretch mode processes each segment and the explicit ratio applies after the
last anchor. Turn off tempo follow before setting anchors.

## 4. Playback

`RenderCache` renders each track's instrument first, then mixes that track's
audio clips into the same stereo buffer:

1. `ProjectClock` converts the clip's bar/beat range to an exact sample start
   and length, so tempo maps and meter changes are honoured.
2. Decoded media is resampled to the project rate; a non-bypass stretch plan
   swaps in the baked variant from the derived-media cache (see
   `docs/audio-clip-stretch.md`). The buffer is then trimmed by `offset`, scaled
   by `gain`, and multiplied by linear fade envelopes.
3. The summed track buffer goes through the normal `AudioGraph`: track effects,
   a balance pan law for stereo clip sources (constant power for mono ones),
   fader, mute/solo, aux sends, buses and master.

Because the mix happens before the graph, audio clips inherit routing and FX
without a parallel code path. They also extend the render length: the last clip
end participates in the total-duration calculation.

The existing `render_to_buffer` / `render_to_buffer_per_track` legacy paths have
no resource root, so they return an explicit error for projects with audio clips
instead of silently dropping them. `ocp render` always selects the resource-root
per-track path when clips are present. Live playback uses the separate clip lane
described in section 7; both paths feed the same track buffer and graph.

In the timeline, right-click an audio clip and choose **Preview clip** to hear
it through its track and mix processing. The preview starts at the selected
clip's placement, excludes overlapping clips on that track, and leaves the
arrangement cursor parked. Press Space to pause or resume the preview.

## 5. Errors

The project directory is the directory of the `.ocp` file. Every load resolves a
clip's relative `file:` against it, including the first load after opening a
project, because the load request carries the directory instead of reading it
back from panel state. An absolute `file:` is used as written and needs no
directory; a relative one with no directory is refused. Preset assets follow the
same rule; Presets in the DSL manual lists the fields it covers.

A file that is not on disk is refused rather than skipped (a `muted:` clip is
the exception, section 1). Validation and render errors always name the clip
and the resolved path:

```
audio clip 'intro': file not found: /project/media/intro.flac
audio clip 'intro': cannot decode /project/media/intro.flac: unsupported or invalid container: ...
audio clip 'intro': pitch 25 is outside -24..=24 semitones
audio clip 'intro': pitch needs a stretch mode other than off
audio clip 'intro': stretch ratio 8.0000 is outside 0.25..=4
```

The last message reports the composed ratio `stretch × pitch`, so a pitch that
lands inside the range on its own can still be refused next to a large stretch.

`ocp validate <project>` decodes every referenced clip, so missing or corrupt
media fails before any render starts. `ocp render` repeats the check on the
tracks it renders.

## 6. Verification

```sh
cargo test -p oc-engine --test all -- audio_clip
```

Covered by tests: parse → format → parse equality, old projects format
unchanged, validation errors, mono→stereo decoding, 44.1→48 kHz length/RMS,
noise RMS, anti-aliasing on 48→44.1 kHz, sample-accurate placement with gain,
offset and fades, missing/corrupt media messages, and `ocp render` output.
`relative_media_paths_resolve_against_the_project_file` loads a project file
whose clip names `media/samples/hit.wav`, renders it through the project
directory, then repeats with an absolute `file:` and with a missing one; the
streaming load is covered by
`a_relative_clip_media_path_resolves_on_the_first_project_load` and
`a_missing_clip_media_file_is_refused_with_the_path_it_tried` in
`render_thread`.
Pitch coverage (a 440 Hz tone at ±12 and +5 within 1 cent, unchanged duration,
bit-exact `pitch: 0`, cache keys, missing media) is listed in
`docs/audio-clip-stretch.md`. Stretch coverage (bit-exact 1.0×, onset/pitch
regressions, cache keys, the derived budget) is listed in the same document.
Streaming coverage is listed in section 7.

## 7. Streaming playback

Live playback has an audio-clip lane alongside the per-track instrument
sources. Clips are deliberately not a `BlockSource`: an instrument source
renders a fresh block from MIDI state, while a clip lane is a placement over
immutable decoded samples addressed by absolute project frame. `ClipLanes` is
built on the render thread when the project loads and mixes each clip into its
track buffer before the track's graph processing, so mix, sends, mute/solo,
effects, buses and master apply exactly as they do offline.

- **Placement and chunk boundaries.** A lane stores the start frame, playable
  length, media offset, gain and fade lengths in project-rate frames. Mixing a
  chunk that starts at absolute frame `F` intersects `[start, start+frames)`
  with `[F, F+chunk)` and reads `offset + (absolute - start)`; a clip starting
  inside a 4096-frame chunk therefore begins on its exact frame. The lane keeps
  no transport cursor, so seek, replay and loop restart all resume the same
  source sample instead of retriggering the clip.
- **Lookahead rule.** There is no partial lookahead window: the whole clip is
  decoded and resampled to the project rate once, on the worker/render thread,
  at project load (or when a backward seek rebuilds the source graph). Chunk
  rendering then only copies from that buffer and never touches the file. The
  existing DSP block is 4096 frames, and the ring buffer pre-queues up to
  12,000 frames (250 ms at 48 kHz) ahead of the audio callback, unchanged.
- **Project directory.** The load carries the directory the clips resolve
  against (`RenderCommand::ProjectChanged.root`), and the render thread keeps it
  for a later mixer edit that has to reload the graph. The worker holds no panel
  state, so the first load after opening a file binds its clips without waiting
  for an "open project" path to be stored, and a project whose `.ocp` file names
  `media/samples/x.wav` finds that file beside itself.
- **Memory cost.** Project-rate stereo f32 is 8 bytes per frame: 384 kB/s
  (0.366 MiB/s), 23.0 MB/min (21.9 MiB/min), and 659 MiB at the 30-minute
  decode bound. When the media rate differs, the cache also keeps the
  native-rate decode, so a clip can cost up to about twice that. A loaded
  project pins its clips: the lane holds the `Arc` even if cache LRU eviction
  drops the entry, so the cost lasts until the project is reloaded. A stretched
  clip pins the baked variant instead of the decoded buffer, and the lane load
  refuses a project whose variants exceed the derived-media budget (see
  `docs/audio-clip-stretch.md`).
- **Cache interaction.** Lanes use the cache identity `(absolute path, mtime,
  size)`. A file that changes while the project is open keeps the decode loaded
  with the project; it is re-read only when the project is (re)loaded (project
  edit, reopen, or a backward seek that rebuilds the graph). That reload logs
  one line, `audio clip '<name>' media changed; reloading its decode`.
- **Threading.** Decoding, resampling, stretching, cache locking and buffer
  construction all happen at load. `ClipLane::mix_into` only does bounds arithmetic and `+=` on
  pre-decoded slices: no allocation, no lock and no I/O; the audio callback
  still only drains the ring buffer. `tests/clip_lane_alloc.rs` counts
  allocations while mixing a two-minute clip and requires zero.
- **Clip-only tracks.** A track whose audio comes only from clips (no preset
  and no musical events) is served by `BlockSource::Silent` instead of falling
  back to a SoundFont, so it does not depend on an installed font.
- **Duration.** The last clip end participates in the streaming duration, so a
  project whose last content is a clip is not cut at the 2 s instrument tail.

Covered by streaming tests: chunked mixing equals a one-shot offline clip mix
exactly; one clip with offset, gain and fades renders sample-identically in the
offline and streaming paths; a looped clip renders the same samples chunked,
one-shot and against the offline render; a seek into a clip resumes the same source sample;
rewind/replay does not double the clip; an off-grid clip starts on its exact
frame; a long clip mixes with zero allocations; and unmuting a clip-only track
plays the clip exactly once.

### Remaining gaps

- **Clip editing**: the selected clip's stretch handle changes its length
  and playback speed together. The Speed and pitch tab sets speed, pitch,
  formant shift, preservation, and warp markers. See `docs/audio-clip-stretch.md`
  for supported ranges and processing modes.
- **Resampler**: reverse playback is unavailable.
- **Codecs**: Opus/AAC are not compiled in; no video-container audio.
- **Caching**: decoded media is cached by absolute path + mtime + size, see
  section 8; stretched variants have their own derived key and budget
  (`docs/audio-clip-stretch.md`). A file changed while the project is open is
  picked up on the next load, not live.
- **Sample-rate mismatch of the offset**: offset is applied after resampling in
  project-rate frames, so it can differ from an exact source-frame trim by less
  than one output sample.
- **Track rename**: clips reference the destination track by name; the GUI rename
  path does not rewrite them yet, so validation reports the stale reference.

## 8. Peaks and the media cache

Peaks are a min/max/rms pyramid computed from the decoded samples:
256-frame base bucket, each level halving the resolution, at most 20 levels.
The encoded form (`OCPK` v1, 30-byte header, one f32 triple per bucket) is
written to disk and rebuilt only when the source file's mtime or size changes.

The cache is keyed by `(absolute path, mtime, size)` and holds decoded samples
and their resampled variants. Memory cap 512 MiB (`OCP_MEDIA_CACHE_BYTES`),
disk cap 2 GiB, LRU eviction; the default disk directory is
`~/Library/Caches/OpenCompose/media` (platform cache dir elsewhere). Peak
computation and decoding run on a worker thread — the UI never decodes.
Stretched variants live in a separate derived layer in the same directory
(`.ocd` files, its own key and 256 MiB default budget); see
`docs/audio-clip-stretch.md`.

The timeline draws the pyramid for the visible zoom level and supports
trim and split through the normal mutation/undo path; the GUI warms the peaks
for every clip when a project opens.

## 9. Timeline gain and fade handles

The timeline block exposes the envelope the DSL carries, so a gain or fade edit
does not need a trip to the project text. The clip menu offers the
same values as sliders and a point editor (section 10) when a line is too small
to hit.

- **Gain line.** A horizontal line across the block sits at the clip's gain
  level, with unity on the block's middle. Drag it up or down; the readout
  follows the pointer.
- **Fade corners.** One square at each top corner. Drag the left one right for
  `fade_in:` and the right one left for `fade_out:`; the ramps are painted over
  the waveform, and the block's waveform columns shrink inside them.
- **Range and floor.** Clip gain is clamped to **-60 dB … +24 dB**; the handle
  label states the range and the readout appends `limit` when a drag reaches
  one. A fade is clamped at the clip length. A fade shorter than **20 ms** is
  refused — `A fade needs at least 20 ms.` — because a shorter ramp clicks
  instead of fading. Zero clears the fade.
- **One history entry per gesture.** Each completed drag or key press writes one
  `set_audio_clip_envelope` edit inside the normal undo gesture, so a drag is
  one undo step, not one per pointer move.
- **Keyboard.** With a clip selected, `Alt+Up` / `Alt+Down` nudge its gain by
  0.5 dB and `Alt+0` returns it to 0 dB.
- **Missing media.** A clip whose file cannot be resolved has no waveform to
  draw a ramp on, so its handles stay inert and report
  `This clip has no audio media, so its gain and fades cannot be edited.`

`set_audio_clip_envelope` takes the clip index plus any of `gain_db`,
`fade_in_seconds`, `fade_out_seconds`, `crossfade_seconds` and
`gain_envelope`. Only the fields present are written, so one edit cannot
clobber the others, and the command returns the project snapshot like its
neighbours (`move_audio_clip`, `trim_audio_clip`).

## 10. Gain envelope window

The clip menu carries the precise controls, so a gain or envelope edit does not
depend on hitting a one-pixel line.

- **Clip gain.** The `Clip gain` row is a slider from -60 dB to +24 dB; release
  commits one undoable edit.
- **Gain envelope.** The row reads `Add gain envelope` when the clip has none
  and `Edit gain envelope` otherwise. It opens one non-modal floating window;
  opening it for another clip re-points the same window instead of creating a
  second one.
- **Basic tab.** Sliders for `fade_in:`, `fade_out:` and `crossfade:`. Values
  are clamped to the clip length; a non-zero fade shorter than 20 ms is
  refused.
- **Envelope tab.** The curve is relative to `gain:`. Drag a point, double-click
  the line to add one, and double-click or right-click a point to remove it. The
  first point stays at position 0 and the last at position 1; interior points
  stay ordered. Each drag commits one undoable edit.
- **Window.** Drag the title bar to move it. It never blocks the arrangement and
  closes with its close button or Escape.
- **Edits keep the curve.** Trimming or splitting remaps the points to the new
  clip length, adding a seam endpoint so the two halves join without a jump.

The envelope is a point curve, not an effect insert: the engine evaluates it per
sample inside the clip mix, after `gain:` and the fades/crossfade, before track
effects.

## 11. Bounce and consolidate

Two edits turn rendered audio back into a clip. Both render offline, write one
media file, and replace their source in a single undo entry. Both refuse with
the reason in front of the change instead of publishing a half-applied edit.

### Bounce

`bounce_track_to_audio(track, from, to, mode)` renders one track's
**post-insert, pre-fader** signal over `[from, to)` at the project rate, writes
`<project>/media/bounce-<track>-<n>.wav`, and adds an `@audio_clip` at `from`
with `offset_seconds = 0` and a range ending at `to`. `ocp bounce` runs the same
engine entry point (`oc_engine::render::render_track_post_fx_range`) without the
GUI.

- **What is rendered.** The track tap, after its insert chain and before its
  fader, polarity, mute, sends and pan. A send or bus return, and the master
  chain, never reach the bounce; the clip inherits its routing from the track
  it lands on.
- **Alignment rule.** The render is a slice of the same timeline the mix
  renders: the graph runs from the project start, so insert state and latency
  handling are the ones the full mix uses, and sample 0 of the file is the
  track tap at `from`. The clip covers exactly the rendered span, so it plays
  back on the same samples. Destination latency compensation is applied once by
  the graph when the clip plays, exactly as it was for the source track; a
  bounce never carries a second copy of it. An insert chain that reports
  latency keeps that latency inside the waveform, so a bounced track whose
  source had a lookahead device sits later than the source did by that amount.
- **The fader is not baked.** Volume, pan, polarity, sends and routing stay on
  the destination track or are copied to the new one, so a bounce never applies
  them twice.
- **Modes.** `mode: "new_track"` adds `<track> (bounced)` with the same mix,
  sends and output routing, no insert effects, and the source track muted (not
  cleared). `mode: "in_place"` removes the track's blocks that overlap the
  range, adds the clip on the same track, and bypasses the baked chain: every
  insert gets `bypass=true` and a post-insert gain envelope is cleared. The
  report names the bypassed devices.
- **FX-baking rule.** Effects are baked exactly once. `new_track` bakes them
  into the file and adds no chain, so the source's chain is irrelevant because
  the source is muted. `in_place` bakes them and then takes them out of the
  path. If bypassing the chain would change a block outside the range, the
  command refuses and names that block; bounce the whole track, or move the
  block first.
- **Media.** `<project>/media/bounce-<track>-<n>.wav`, 32-bit float, stereo, at
  the project rate (no dither and no clipping: a hot track keeps its headroom).
  `<track>` is the track name reduced to file-safe characters, and `n` is the
  first free number. The file is written to a dot-prefixed temporary
  name in `media/` and renamed into place; a failed render, a refused edit or a
  project that changed during the render removes it.

### Consolidate

`consolidate_selection(track, clip_indices)` renders the union of the selected
audio clips on one track, at clip level: the clips' own gain, fades and
crossfades are applied and the track's insert chain is **not**, because the new
clip stays on that track and runs through the chain once. The selection is
replaced by one clip at the union start, named `<track> consolidate` in
`media/consolidate-<track>-<n>.wav`.

The command refuses a selection that is empty, that spans more than one track,
that names an out-of-range clip, or whose span also contains content the new
clip would duplicate: a pattern clip, a phrase, a performance event, or an
unselected audio clip. The refusal names the first offending item.

### Shared behaviour

- **One undo entry.** Each command pushes one `bounce track` or
  `consolidate clips` entry with the whole edit in it.
- **Snapshot publishing.** The render runs with the project lock released. The
  edit publishes only if the project's content epoch is unchanged; otherwise the
  command refuses and removes the staged media, so a concurrent edit is never
  overwritten.
- **Unsaved project.** Both refuse with a save-first message, because a relative
  media path needs the project directory.
- **GUI.** The track menu carries "Bounce track to audio…" and the audio-clip
  menu carries "Consolidate selection". Both prompts default to the track's
  content bounds, show a pending state, keep a refusal inside the prompt, and
  put the report line in the status bar.

### Verification

```sh
cargo test -p oc-engine --lib -- bounce
cargo test -p oc-gui --lib -- bounce
cargo test -p oc-cli -- bounce
npx vitest run src/__tests__/bounce.test.tsx
```

The engine tests prove the alignment rule: a bounce over `[from, to)` equals the
full render's track tap at the same samples, sample 0 included, and a project
with the bounce substituted for its source renders the same track stem within
1e-6 per sample. The command tests cover the undo entry, the media file, the
staging cleanup, the refusals, the new-track routing and the in-place bypass.
