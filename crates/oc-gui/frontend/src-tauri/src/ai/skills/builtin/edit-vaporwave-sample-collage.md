---
id: edit-vaporwave-sample-collage
title: Edit a vaporwave sample collage
when: A vaporwave, eccojams, future funk or signalwave piece needs a stable sampled loop with intentional pitch and space.
category: production
version: 3
tools: [project_extract, project_set_tempo, edit_subtree, project_validate, devices_catalog, render_range, audition_range, audio_clips_list, audio_clip_set_playback, audio_analyze, audio_compare]
---
Start with a source you can use and keep an untouched copy. Mark the phrase and the bass movement under it. A
repeated melody can hide a chord change that makes the chosen loop boundary feel wrong.

## Compare a tape-style slowdown with pitch-holding stretch

Use a one-second mono practice clip with 440 Hz bursts from 0 to 0.35 and 0.5 to 0.9 seconds, with short fades
to give each burst a distinct attack. At 120 BPM in 4/4, place two dry copies on separate tracks from `1:1` to
`1:3`, with unity stretch, zero source offset, and looping disabled. Keep the tracks centered and leave
headroom.

Read `audio_clips_list` before each edit and use its clip index and revision. On the first copy use
`audio_clip_set_playback` with `stretch_to: {"bar":2,"beat":1,"subdivision":0}`, `stretch_mode: "varispeed"`,
`pitch: 0`, `tempo_follow: false`, `formant: 0`, `preserve_formants: false`, `bypass_processing: false` and
`warp_markers: []`. Leave `stretch` out when supplying a new end. Repeat the same edit on the second copy
with `stretch_mode: "musical"` to hold pitch while doubling duration.

Render each track alone from bar 1 to the start of bar 2. Use `audio_analyze` for the strongest spectral peak
and a transient window around 1 second. Expect fundamentals near 220 Hz and 440 Hz respectively, within one
spectral bin on the tone fixture. The second attack should be near 1 second in both. Inspect attack shape
separately, since stretching can smear it. For a level-matched listening comparison request `audio_compare` with
`listen: true`, `listening_only: true` and `duration: 2`. Check delivery as in the audition-and-listen skill and
compare the second attack and pitch contour before adding echo.

Make one short loop with a clear phrase turn. Audition several repetitions at the final rate before applying
space. Adjust the boundary or source offset before adding a fade. Keep a little pre-roll when an intake or
pickup matters.

For eccojams, repeat one purposeful fragment and vary the echo feed across passes. For future funk, compare the
chopped phrase against a clear drum anchor. For signalwave, make the cut order and distance cues readable
instead of smoothing every discontinuity.

Render through the loop reset and the last echo tail. Check whether repeats build too much low end or hide the
next edit. Preserve the source and keep the intentional degradation separate from accidental clipping or file
damage.
