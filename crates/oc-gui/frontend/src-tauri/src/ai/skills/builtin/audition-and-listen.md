---
id: audition-and-listen
title: Audition and listen
when: You need to know how a section sounds, not only how loud it measures.
category: render
version: 7
tools: [render_range, audition_range, audio_clips_list, audition_clip_source, audio_analyze, audio_compare, plugin_search, plugin_pull, separate_stems]
---
Start with the full mix. Name one audible issue and the section where it occurs.

## Source and arrangement

Use audition_clip_source with an attached path or a clip_index from
audio_clips_list to hear the source before clip edits or mixer effects.
Match an attack or lyric across source and project time.

Use `audition_range({start: 12, end: 28})` to hear the processed arrangement.
Use `start_bar`/`end_bar` for bars and `tracks` for a track subset. A bus return
has no track index: ask `render_range` for it with `bus` and audition that file.
Hear the suspect track alone, then with its nearest competitor and in the mix.
Include the event and its tail within 30 s. Check `truncated` on the excerpt.

Omit `path` for reclaimable scratch. For repeatable A/B, name distinct WAVs.
After reopening, pass the exact saved render path from this conversation to
`audition_clip_source`. Use `source_start_seconds` relative to that file and
`duration_seconds` up to 30 s. For a render starting at project 52 s, project
64 s is file 12 s. If the archive expired, attach a retained copy or render the current project and label it new.

## Splitting a part out of the mix

Find the separator with plugin_search. Install once with plugin_pull and dry_run: false.
Pass its catalog runtime_id to separate_stems. Check method and vocals/accompaniment outputs.
Band filtering is not source separation and neither promises a dry vocal. Place
imported stems at their shared start, keeping their original lengths.
Compare against the original before judging bleed, missing parts, or timing.
For wet vocals, read separated-vocal-mixing before adding FX.

After eight windows shorter than one bar (and under 4 s) on one project revision,
`render_range` refuses with `narrow-window loop: ...`. Audition the section.

## Evidence and delivery

Match playback levels where possible. Metric normalization alone does not
level-match the delivered audio. 16 kHz mono cannot certify stereo width or
full-band detail. Check stereo if the model keeps both channels, then mono.
If delivery fails, use render_range and audio_analyze with a named WAV.
Rendering, delivery, and listening are distinct. Report a failed stage explicitly.
Never claim to have listened to audio you only measured or did not receive.
