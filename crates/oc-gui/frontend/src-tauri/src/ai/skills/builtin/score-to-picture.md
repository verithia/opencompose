---
id: score-to-picture
title: Build and time a cinematic cue
when: Turn a sketch into a score cue, hit picture or scene anchors, or map a build, climax and release to specific moments.
category: scoring
version: 4
tools: [picture_timecode, picture_clock_set, project_get, project_extract, sections_list, tempo_map_set, edit_subtree, project_validate, render_range, audition_range, audio_analyze]
---
Agree on the picture frame rate, timecode convention and picture frame at project zero. Record both
the original picture label and convert it with `picture_timecode`. Save the confirmed settings with
`picture_clock_set` and the revision from `project_get` when asked to persist the clock. If the convention is
unknown, get frame indices or converted seconds. Identify fixed hits, movable anchors and tail allowance.

For a constant-tempo segment, multiply its written quarter notes by 60 and divide by duration in
seconds. Inspect existing tempo points before calling `tempo_map_set`, which replaces the complete map.
Points hold their tempo until the next point. Preserve unrelated points and keep the order ascending.
After an early tempo edit, recheck later anchors and any affected audio clip timing or stretch.

## Two locked hits

Use a disposable 4/4 project at 120 BPM, with project zero at picture frame 0 and picture at 24 fps.
Put dry isolated attacks at bars 5 and 8. The target frames are 192 and 312, or 8 and 13 seconds.
The first four bars occupy eight seconds. The next three bars occupy five seconds at 144 BPM.
Set `points: [{bar: 5, bpm: 144}]` and validate. Convert both bar positions again against the new map.

Use `render_range` on the attack track over [7.75, 8.50) and [12.75, 13.50) seconds. Run
`audio_analyze` with `transients: {start: 0, duration: 0.75, min_gap_ms: 5}` on both WAVs. Add
each render's `start_s` to its detected attack's `file_seconds`. Aim for both positions within 10 ms.
Record sample rate and time resolution. If the strongest attack is a different event, listen to the
window and identify the intended hit before changing tempo.

## Hear the whole cue

Map a recognizable motif to the anchor with one voice before adding layers. Give one voice the
foreground and build toward the largest doubling or impact. Audition into and out of transitions
with tails intact. A written note and its audible attack can differ with a slow swell or long reverb.
Check the key hits in the full mix. Keep delivered layers at a common origin and end, and state
whether timing was checked against actual picture or supplied cue points.
