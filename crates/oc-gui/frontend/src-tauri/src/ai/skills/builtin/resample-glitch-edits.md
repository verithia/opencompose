---
id: resample-glitch-edits
title: Resample and sequence glitch edits
when: Turn a print into glitch edits, IDM or deconstructed club fragments, or shape continuous noise into rhythmic pulses.
category: production
version: 2
tools: [project_extract, edit_subtree, project_validate, audition_range, render_range, tracks_list, audio_analyze, audio_compare]
---
Pick one source gesture that will remain recognizable after editing.
Keep the dry source and print a short pass with render_range. Decide
whether the print includes processing and tail before reimporting it.

## Print once and edit the result
Place the render on a separate audio track with a project-relative path.
Mute the source when auditioning the print unless deliberate doubling
is part of the arrangement. Check the printed first transient aligns
with the intended downbeat and leave the original recoverable.
When a feedback texture is involved, print a bounded window with room
for its tail and keep only the version you actually want to reuse.

Choose three contrasting fragments by ear, such as a transient, a
short held vowel and a noisy decay. Set the clip ranges and offsets
so each fragment has its own trigger. Vary one dimension at a time:
position, repetition count, pitch, or the gap between cells.
Use short fades or move the boundaries away from a discontinuity.
Reserve silence for the main edit instead of filling the whole bar.

## Turn a noise bed into percussion
At 120 BPM in 4/4, place a self-authored continuous noise WAV across one full
bar on its own track. If it needs warp, verify the anchors separately with
align-and-stretch-audio first. Keep the final warp and placement fixed for A/B.
Extract `project` and render the noise alone. Add this line to its
`@audio_clip` via edit_subtree on `project`, validate, and render the same range.
`gain_envelope: 0:-48dB, 0.005:0dB, 0.12:0dB, 0.125:-48dB, 0.375:-48dB, 0.38:0dB, 0.495:0dB, 0.5:-48dB, 1:-48dB`
These normalized points make two pulses with 10 ms ramps on the two-second clip.
Use audio_analyze level_window on each WAV with `duration: 0.1` and starts
of 0.05 s (open) and 0.30 s (closed), in file time. Require open RMS to
stay within 1 dB and closed RMS to drop at least 24 dB against the baseline.
Use audio_compare with `listen: true` and `duration: 2` for an aligned A/B.
Audition with drums to check the pulse, gaps and clicks at the ramps.

## Make comparisons reproducible
Keep the exact chosen print, slice positions and repeated pattern for
A/B tests. Randomness can be captured in one chosen print before the
comparison. Match playback level and verify the edited phrase still
returns to its anchor downbeat after the densest section.
