---
id: edit-vaporwave-sample-collage
title: Edit a vaporwave sample collage
when: A vaporwave, eccojams, future funk or signalwave piece needs a stable sampled loop with intentional pitch and space.
category: production
version: 4
tools: [project_extract, project_set_tempo, edit_subtree, project_validate, devices_catalog, render_range, audition_range, audio_clips_list, audio_clip_set_playback, audio_analyze, audio_compare]
---
Keep an untouched authorized source. Mark its phrase, bass movement and
recorded space; repeating melody can conceal a chord change at a bad seam.
Choose a purposeful fragment and hear it repeated before adding degradation.

## Choose a pitch/time relationship
Use fit-loops-with-pitch-and-time to distinguish source length, placement and
source tempo. Varispeed changes pitch with speed; pitch-holding stretch keeps
pitch while changing duration and can smear attacks. Neither is universally
better for a collage. Compare the actual phrase at the intended final rate.

Read audio_clips_list before editing and retain its clip index and revision.
When supplying stretch_to, omit stretch. Choose the actual mode and inspect
pitch, tempo_follow, formant, preserve_formants, bypass_processing and existing
warp_markers together; an old transform can hide the new decision.
A formant shift changes vowel/body character independently where supported;
slowing a mix does not imply independent control of every voice in that mix.

If the mode or timing path is uncertain, use a short known calibration clip:
at 120 BPM in 4/4, a one-second 440 Hz source with attacks at 0 and 0.5 s spans
1:1 to 1:3 at unity. Doubling it with stretch_to: {bar: 2, beat: 1, subdivision: 0}
gives roughly 220 Hz in varispeed and 440 Hz in musical mode; the second attack
moves near 1 second in both. Use short fades, zero offset, no loop and no other
processing. Check actual frames, spectrum resolution and onset shape.
This is a conditional diagnostic, not a prerequisite for every creative edit.

## Give repetition a direction
For eccojams, repeat one recognizable cell and vary echo feed across passes.
For future funk, hear chopped accents against a clear drum anchor. For
signalwave, make cut order, silence and distance cues intentional rather than
smoothing every discontinuity. Use a small contrasting candidate bank if the
source is wrong; accumulating effects cannot invent its missing gesture.
Keep a pickup before the grid when it carries speech or musical articulation.
Adjust boundary/source placement before reaching for a long crossfade.

Render repeated loops through reset and the last echo tail. Use audio_compare
for matched listening and inspect delivery support before judging high detail.
Hear pitch, groove, bass continuity and whether echoes hide the next edit.
Keep intentional noise/aliasing distinct from accidental clipping, missing
media or a damaged source. Validate edits and retain the chosen transformation.
