---
id: build-a-tempo-and-meter-map
title: Build a tempo and meter map
when: Imported audio drifts against the grid, a project changes meter, or musical bars must follow supplied time anchors.
category: editing
version: 2
tools: [project_get, project_extract, project_set_tempo, tempo_map_set, time_signature_set, audio_clips_list, audio_clip_set_playback, edit_subtree, project_validate, render_range, audio_analyze, audition_range]
---
Choose whether the grid follows the recording or media follows the grid. Save
the source and timing. The GUI Transport maps and agent tempo_map_set and
time_signature_set replace complete point lists; inspect existing maps first.

For imported audio, use audio_analyze with beat_grid: {start: 0, duration: 18}
only when the file contains that passage; supported windows are 4–30 seconds.
Treat tempo candidates and observed attacks as pulse evidence, not a downbeat or
meter decision. Compare half and double the candidate against bass, backbeat,
phrase returns and a confirmed bar count. A pickup does not establish bar one.
Use shorter transients windows and audition uncertain anchors before editing.

When the grid follows a take, preserve its source offset and sample-rate playback.
Inspect audio_clips_list and disable tempo_follow with audio_clip_set_playback
and expected_revision. In a disposable dry control, use stretch: 1, stretch_mode:
off and no warp markers. Do not clear an existing artistic edit without agreement.
Keep enough clip range to retain the source end after the tempo changes.

## Fit two measured segments
Use an 18-second fixed-rate reference whose confirmed 4/4 bar starts are at
0, 8.4 and 17.0 seconds for bars 1, 5 and 9. Each four-bar segment has 16 quarter
notes. Compute BPM as 60 times that count divided by the measured segment seconds.
Set project_set_tempo to 114.285714, then tempo_map_set with
points: [{bar: 5, bpm: 111.627907}]. These rounded values approximate the anchors;
the measured error, not the displayed decimal count, decides whether to refine.
For a real map, retain unrelated points. Tempo holds until the next point; model
a gradual change with verified steps, not an assumed continuous ramp.

Add separate dry marker attacks at bars 5 and 9 with edit_subtree and validate.
Render each marker track using render_range at start: 8.15, end: 8.65 and
start: 16.75, end: 17.25, with distinct output paths and the same sample rate.
On each WAV, call audio_analyze with transients: {start: 0, duration: 0.5,
min_gap_ms: 5}. Add render start_s to detected file_seconds and compare with the
reference anchors within the reported timing resolution. Widen a missed window.

For a meter change, confirm the new grouping and its actual bar boundary before
replacing the meter map. A 7/8 bar contains 3.5 quarter notes, not seven quarters.
Recompute segment counts. Meter edits retain arranged content's absolute musical
time; inspect whether the new bar labels still mean what you intend. Recheck the
last cue, repeats, automation and audio ends. Audition the reference against the
markers and transitions. Report measured drift separately from unverified listening.
