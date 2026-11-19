---
id: fit-loops-with-pitch-and-time
title: Fit sample loops without losing their groove
when: A loop drifts, has the wrong key, starts on a pickup, or loses attacks after tempo matching.
category: editing
version: 1
tools: [read_skill, audio_clips_list, audio_clip_transients, audio_clip_set_placement, audio_clip_set_playback, project_extract, audition_clip_source, audition_range, audio_analyze, project_validate]
---
Hear the untouched loop and identify its role, actual downbeat, pickup,
number of beats and tail. Filename BPM/key are clues; verify later landmarks.
Decide whether its performance should follow the project or set the groove.
Do not mistake a half-time label or leading silence for an incorrect tempo.

## Keep the clocks separate
audio_clip_transients returns original source-media seconds. Warp anchors
map source seconds to processed-media seconds. Clip placement is project
time; a source offset chooses media rather than delaying the whole track.
Read actual start, offset, loop, stretch and revision with audio_clips_list.
Use audio_clip_set_placement for a timing move; re-list after a split or edit.

Stretch is output duration divided by input duration. A 100 BPM loop at
128 BPM needs 100/128 = 0.78125 if beat counts match. This does not itself
resize the authored clip range. Use stretch_to when duration and edge must
change together. Fit a known trimmed passage before fitting a whole source.
tempo_follow=true fits the whole source and clears its offset and loop;
it is inappropriate for a trimmed excerpt that must retain those settings.

For a verified clip 0, a ratio-only edit with tempo follow disabled is:
```json
{"clip_index":0,"playback":{"stretch":0.78125,"stretch_mode":"percussive","tempo_follow":false}}
```
Supply the current revision required by the active tool schema.

## Choose the process by the source
Use percussive for a drum test and musical for sustained material; compare
both when a loop mixes attacks with pitched sustains. Use sparse warp anchors
at phrase landmarks; do not quantize every ghost hit or flam automatically.
Use independent pitch when tempo must stay fixed. For vocals, compare formant
preservation with the original identity. Varispeed couples speed and pitch;
clear incompatible pitch, formant and warp settings when selecting that mode.

Hear two consecutive loops plus the next transition at actual project tempo.
Check first and later downbeats, missing tails, doubled hits and graininess.
Check the last repeat against the section boundary: a small per-copy error accumulates.
Recheck that boundary after source replacement, reopening or selecting a recovered version.
If a warp smears the hit, try fewer anchors or slicing in gaps before more FX.
For excessive changes choose a closer source. Keep the original recoverable.
Validate and compare the same source gesture as well as the timeline window.
