---
id: warp-drum-loops-without-flattening-groove
title: Warp drum loops without flattening the groove
when: A live or sampled drum loop misses the bar line, but swing, ghost hits, and fills should retain their feel.
category: editing
version: 8
tools: [audio_clips_list, audio_clip_transients, audio_clip_set_playback, project_validate, audition_clip_source, audition_range, audio_analyze, audio_compare, transport_state, tracks_list]
---
Audition the entrance against the beat. Choose which downbeats and backbeats
must align. Keep pickups, ghost notes and swing unless they clash. Preserve
the source and playback settings. For editable drums, print unquantized from
a known origin with pickups intact. Measure sounded hits after groove, row
delay and retriggers. Attribute a remake's timing to its editor.

Get the clip index and revision with audio_clips_list. Find candidate hits
with audio_clip_transients and confirm uncertain hits with audition_clip_source.
Both use raw source seconds. Sort candidates by time. Check fast retriggers
by ear because the source detector separates candidates by about 35 ms.

Use tracks_list for the track index and audition_range for its before WAV.
Predict an unanchored ghost or swung hit from its fractional position. At
120 BPM, map 2.08 s to 2.00 s and 4.16 s to 4.00 s. The 0.312 s offbeat is
15% of the first span, so expect it near 0.300 s.

Use percussive mode with tempo follow off. Keep the range if it fits the
intended bars. Send the full sparse, increasing warp_markers list in one
audio_clip_set_playback call with expected_revision. Set the tail ratio
with explicit stretch. Add the output-time offset to targets measured from
the visible clip start. Source time uses the raw media. The origin is implicit.

Save the after WAV to a distinct path with the same window and track subset.
Check the anchors and the unanchored hit with audio_analyze transients
with min_gap_ms: 5. Keep the analysis within the returned duration_s. Measured
project time is start_s + file_seconds. To predict project time, add the
processed time to clip start and subtract clip offset. Check the timing error
against detector resolution. Narrow the window around masked ghost notes.
If hits overlap or remain unmeasurable, report that and check by ear.

Check tails with audio_analyze level_window {duration: 0.04}, starting 0.04 s
before each hit in file time. A sharp level.rms_dbfs drop can flag a dropout.
Use audio_compare with listen: true and listening_only: true to hear 0.4–8 s.
Use reference_start and candidate_start to align a landmark the edit moved.
For repeated loops, audition the last tail into the next downbeat and check
that it sounds once. Hear the join with backing and validate the project.

During approved playback, check transport_state for playing, starting
clearing and position_seconds advancing across anchors. Have the operator
verify live sound. Report it separately from render results.
