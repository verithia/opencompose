---
id: develop-a-sampled-hook
title: Develop a hook from sampled phrases
when: Turn a vocal, instrument or texture sample into a repeating hook with clear answers, source changes and playable articulation.
category: production
version: 1
tools: [read_skill, project_extract, audio_clips_list, audition_clip_source, audio_clip_edit_range, audio_clip_set_playback, edit_subtree, project_validate, render_range, audition_range, audio_compare]
---
Choose one recognizable gesture: a vowel, consonant attack, instrumental note or noisy answer.
Read choose-and-fit-a-sample if the source itself is still undecided.
Hear the source phrase and mark the useful attack, stable body and release separately.
Keep the original phrase recoverable; make a short working copy before committing processing.

## Decide what each trigger means
For audio clips, keep a small table of source window, timeline onset, pitch and occupied length.
For a sampler, verify which region receives each written key before writing the hook.
A key can select a different slice rather than transpose the same sound; root and slice identity are separate.
Read map-a-multisampled-instrument for frame-based regions and key zones.
Do not assign concert pitches to an unknown slice bank from its note names alone.
Make an onset-led call and a later short answer from one recording; keep pitch and trigger grid fixed first.
Try a stable interior sustain loop for the call and a finite, unlooped region for the answer.
A long release cannot extend a finite region past its source end; move a disappearing answer's start earlier before raising gain.
Hold beyond the source length, release, repeat the key and alternate keys; separate seam chatter, clicks and old-pitch overlap.
For a ticking sustain, change the loop edge or a supported seam treatment before dulling the whole voice with EQ.
Test an expressive slide with the actual instrument; a slide flag or drawn overlap does not guarantee a glide.

## Develop the phrase without losing its identity
Write a short call, an answer and an intentional gap before the next call.
Keep one rhythmic landmark while changing only the answer's source window on the next pass.
Compare a stutter of one frozen fragment with successive fragments that advance through the source.
Keep their trigger grid identical so source progression, rather than added hits, explains the change.
Then try one pitch or register change with the source windows fixed.
If the syllable or attack becomes unreadable, restore its source onset or choose a closer register.
Keep the tail only as long as it supports the next answer; use a clean edge rather than an accidental hard cut.
Change which voice answers in a later section before adding permanent doubled layers.

## Compare the complete exchange
Keep printed processing on one chosen path; do not sum a finished print and its live source unintentionally.
Preserve source-offset, loop and warp settings when changing one playback property.
Refresh clip indices and expected_revision before each typed clip edit.
Validate and render the same phrase plus its return to the opening accent.
Use audio_compare with listen: true and listening_only: true for matched short alternatives.
Hear the complete call, answer and gap in context with audition_range.
If the hook disappears, distinguish a wrong receiving key, a missing source, masking and an unsuitable fragment.
Keep the simplest version that makes the intended response audible over two repeats.
