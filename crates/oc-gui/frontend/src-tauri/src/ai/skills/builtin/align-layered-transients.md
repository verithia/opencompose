---
id: align-layered-transients
title: Align layered transients
when: Layered kicks, parallel drums or doubled transients lose weight, smear, or produce a flam when combined.
category: mixing
version: 3
tools: [audio_clips_list, audio_clip_set_placement, tracks_list, project_extract, edit_subtree, project_validate, render_range, audition_range, audio_analyze, audio_compare]
---
Choose several hits with recoverable source, routing and mix states. Decide
which layer owns the attack or body. The GUI mixer offers polarity. For tools,
inspect tracks_list and project_extract, then use edit_subtree on the track mix.
Retain its other fields and validate. Polarity changes sign, not timing.

## Check the polarity path before treating a real layer
In a disposable project, duplicate one short dry drum lane with at least three
hits. Use identical media, ranges, offsets, processing, gains, pan and pan law.
Remove insert, bus and master processing from this control. Render A alone, B
alone, both with normal polarity, then both with B's mix: polarity=invert.
Pass track indices in render_range tracks, with fixed start/end and float_wav: true.
Keep separate paths. Aligned inverted copies should cancel to numerical silence;
if not, inspect source, gains and routing. Restore normal polarity before testing
different sounds. Exact cancellation is not the target for complementary layers.
On each real layer's saved WAV, use audio_analyze with transients: {start: 0,
duration: 2, min_gap_ms: 5}, provided the two-second window fits. Compare matching
attacks using file_seconds and resolution_seconds. A first click can align while
the following low cycles still cancel. Do not shift a whole part from one hit.

Choose equal attack and body windows relative to a verified onset. For an onset
at file time 0.10 s, compare level_window: {start: 0.10, duration: 0.02} and
{start: 0.14, duration: 0.08} with audio_analyze on A, B and both polarity sums.
Keep raw gain fixed for these readings and repeat for the other hits. The RMS
changes show summing or loss in those windows; a larger peak is not proof of
a better sound. Judge attack and body separately before matching listening gain.

Use audio_compare with listen: true on matching 0.4–8 second summed passages.
Check the delivered match and listen for one clear attack, useful low weight and
no flam. Compare the selected polarity in the full kit and with the bass using
audition_range. Keep the original if a change helps one note but weakens others.
Read audio_clips_list for actual start_seconds and revision. Move a whole clip
with audio_clip_set_placement: use signed nudge_ms or absolute start_seconds.
Retain duration and attack; source offset trims media instead. Recheck each hit.

For a stereo sum, separately call audio_analyze with mono_fold: {duration: 2}
on a fitting window and inspect octave-band loss without normalization. This
measures left/right cancellation, not cross-track cancellation already present
in a centered sum. Preserve solo and polarity comparisons for that diagnosis.
If no setting works across the phrase, narrow the layers' frequency roles or
choose another sample, then check the whole kit.
