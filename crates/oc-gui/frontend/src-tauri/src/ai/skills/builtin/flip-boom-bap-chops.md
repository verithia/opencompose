---
id: flip-boom-bap-chops
title: Flip samples for boom bap
when: Chopping a licensed loop into a boom bap phrase, resetting the bass and drums, and keeping sample seams clean.
category: production
version: 2
tools: [sample_search, sample_pull, project_extract, edit_subtree, audio_clips_list, audition_clip_source, project_validate, audition_range]
---
Start from an approved source file or use sample_search with kind "loop"
and a small limit. Check each provider's availability and reason. If a
provider is unavailable, search an available one. Check the licence allows
the intended edit and release, and keep the credit and any share-alike terms.
If sample_pull returns licence_notice, show its terms. Retry with
accept_licence only if the user accepts. Choose another source when the
terms conflict with the project.

On a saved project, pass the result's provider and id to sample_pull. Keep
the returned attribution. Check quality. If preview_mp3 arrives, audition
it for selection and use an allowed original when the release requires one.
Place the returned media_path on its own audio track. Get the clip_index
with audio_clips_list. Call audition_clip_source with source_start_seconds
and duration_seconds up to 30 s. Hear the downbeat, candidate cuts and tail.

## Chop and resequence

- Mark candidate cuts at phrase starts and transients. Make a small set of
  @audio_clip regions that refer to the same file with different offsets.
  Give each slice its own timeline range and check that its end fits the new
  pattern. When stretching, offset is in the stretched file's time.
- Swap the order or repeat one fragment. Add a contrasting answer chop at
  the end of the second pass. Decide which original attacks should survive.
- If a splice ticks, adjust the boundary or add a tiny fade. Recheck the
  transient before lengthening the fade. Compare a deliberately gated
  stop with an overlap where the tail should continue.

## Fit it to the beat

Build a plain kick and snare around the chop accents. Use quieter ghost notes
and listen to the pocket with hats before adding layers. Pattern swing moves
the supported eighth offbeats. Use deliberate placement for the rest of the
groove. Keep kick and bass from crowding a low note already present in the
sample. Try a small filter or a different chop before removing all its body.

Test two consecutive phrase cycles, then the transition to a sparser B section.
Check that repeats feel intentional, the sample's pitch supports the new bass,
and any added noise stays below the rhythmic detail.
