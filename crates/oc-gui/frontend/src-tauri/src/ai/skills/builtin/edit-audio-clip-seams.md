---
id: edit-audio-clip-seams
title: Edit clean clip joins and loops
when: An audio cut, repeated loop or comp clicks, doubles an attack, or leaves a gap at a join.
category: editing
version: 3
tools: [audio_clips_list, audio_clip_edit_range, audio_clip_set_playback, project_extract, edit_subtree, project_validate, render_range, audition_range, audition_clip_source]
---
Find the join in context. Listen for a sharp discontinuity, double attack,
missing word ending, reverb jump or level change. Preserve the underlying
media so the edit remains reversible.

Use clip placement to move an event in time. Use source offset to choose
where inside the file it begins. Check the exclusive clip end and the
available media, especially for stretched audio. A tail past the end may
be trimmed, while short media can leave silence.

For a hard cut, try a short fade away from the main attack. At a soft seam,
try a slightly longer fade so room tone and breath connect. Use the clip's
fade or crossfade controls only where they solve the join. Listen for a dip
where correlated takes overlap. Keep room tone when natural continuity matters.

For loops, test the end into the start and a few successive repetitions.
Check that the first hit is present once, that the tail fits the loop's
role, and that intentional spaces still feel like part of the groove.

## Verify the seam against the source

Save an audition before changing the seam. If the clip is warped or pitched,
use bypass_processing on a separate pass to hear the source without those
transforms. Keep placement, fades and mixer settings fixed. Compare the same
source gesture as well as the same timeline window if the warp moves a hit.
If the cut itself is suspect, compare against an uninterrupted section of
the original file. Bypass retains the cut.

Restore processing for the edited pass. Audition several repeats at normal
speed, then the join with nearby parts at comparable level. Listen for a
click, doubled attack, or a dip between takes. Use stereo to check a click
on one side and mono to check a seam that thins when summed. If a separate
source reference is unavailable, distinguish what bypass let you check.

List clips and their revision with audio_clips_list. Use audio_clip_edit_range
with split_at or trim_to. Pass the revision and re-list after splitting.

Use audition_clip_source at source time for an uninterrupted reference before trim, processing, or routing. Compare it with audition_range on the arrangement.
