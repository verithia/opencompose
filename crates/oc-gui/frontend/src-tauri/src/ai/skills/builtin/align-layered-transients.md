---
id: align-layered-transients
title: Align layered transients
when: Layered kicks, parallel drums or doubled transients lose weight, smear, or produce a flam when combined.
category: mixing
version: 4
tools: [audio_clips_list, audio_clip_set_placement, tracks_list, project_extract, edit_subtree, project_validate, render_range, audition_range, audio_analyze, audio_compare]
---
Choose several real hits and decide which layer owns attack, body or texture.
Inspect source, trim, placement, inserts, routing, gain and pan before moving it.
For tools, read tracks_list/project_extract and edit the track mix subtree to
change polarity while preserving other fields. Polarity changes sign, not time.

## Diagnose the layer in the actual phrase
Hear each layer and their sum with raw gain fixed. A first click can align
while following low cycles cancel. Do not shift an entire part from one hit.
On retained WAVs, audio_analyze transients: {start: 0, duration: 2, min_gap_ms: 5}
can locate matching attacks if that window fits. Use file_seconds and its
resolution_seconds, not a rounded bar display, to compare positions.
Compare equal attack/body windows relative to verified onsets. For one at
0.10 seconds, level_window: {start: 0.10, duration: 0.02} and
{start: 0.14, duration: 0.08} can show summing or loss across the onset and body.
Repeat for A, B and both polarity sums at fixed raw gain across several hits.
Larger peaks or RMS do not prove a better sound; compare articulation and weight.

## Choose the useful relationship
Use audio_compare with listen: true on matching 0.4–8 second summed passages.
Inspect delivered match/channel support. Hear one clear attack, useful body
and no flam, then compare the chosen version with the bass and full kit.
Read audio_clips_list for current clip index, start_seconds and revision.
Move a whole clip with audio_clip_set_placement using signed nudge_ms or
absolute start_seconds. Changing source offset trims media instead; retain
attack and duration unless trimming is intended. Recheck notes and loop return.

If gain/polarity/routing appears wrong, use a small disposable control:
duplicate one dry lane with identical media, placement, processing, gain and
pan law. Render A, B and their normal/inverted sums to distinct float WAVs
with track indices selected in render_range and all downstream processing off.
Aligned inverted copies should cancel numerically. Resolve a failed control
before diagnosing real layers; don't repeat it for every normal layering task.
Exact cancellation is not a goal for complementary samples. Restore polarity.

For left/right loss, use audio_analyze mono_fold on a fitting window without
normalization. That is different from cross-track cancellation already in a
centered sum; preserve the separate-layer/polarity evidence for that question.
If one shift helps a hit but hurts later notes, narrow frequency roles or use
another source. Validate edits, restore solos and keep only useful complexity.
