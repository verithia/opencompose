---
id: mix-dialogue-and-a-music-bed
title: Mix dialogue and a music bed
when: A narration, spoken interlude or long program needs intelligible speech, controlled background music and reliable delivery.
category: mixing
version: 1
tools: [read_skill, tracks_list, audio_clips_list, audition_clip_source, project_extract, audio_clip_edit_range, devices_catalog, effects_add, effects_set_param, buses_add, tracks_output, edit_subtree, project_validate, render_range, audio_analyze, audio_compare, audition_range]
---
Use supplied dialogue and keep its raw take. Establish the delivery brief before
choosing a loudness target, channel layout, format or chapter/version structure.
Native agent tools do not arm or capture a microphone; ask for an operator take.
Identify quiet speech, loud words, breaths, pauses, edits and musical transitions.
A gain-reduction reading is not a dynamic-range or delivery-loudness measurement.

## Keep words and room continuous
Choose a complete phrase before local correction. Lower an excessive word before
inserts only when the desired detector drive should change; use a post-processing
ride for final phrase balance. Do not peak-normalize every word independently.
Compare edit edges with surrounding room and breath; digital-zero gaps may sound
less natural than retained low room tone. A gate does not remove noise under words.
Choose restoration only for actual faults and inspect consonants for watery or
swallowed detail. Learned noise/de-reverb requires a verified external processor.
For distance perspective, coordinate direct level, spectrum and reflections;
fixed filtering or a large reverb alone does not prove a particular distance.

## Make the music yield deliberately
Route music exclusively to a named group and keep the dry dialogue out of its
main audio input. Establish balance before adding vocal-keyed gain reduction.
Use a supported AUDIO key on the music compressor if automatic ducking is useful.
Verify port/tap and compare a full phrase, gap and next entry with the key silent.
Choose broadband versus a relevant dynamic band by what actually masks speech.
Tune recovery to retain word endings and let music return without syllable pumping.
For a one-off event, a bounded manual ride may be clearer than another processor.
Restore the base state before the automation end; avoid two writers on one target.

## Verify the complete program
Keep short matched A/B excerpts for audible decisions and raw-level evidence
separate. A mono/low-rate listening copy cannot certify stereo or high-band detail.
In the GUI, export and reopen the complete program. Agent render_range windows
are limited to 600 seconds; use contiguous coverage for a longer review, but
do not present those chunks as a verified assembled delivery file.
Check actual starts, ends, tails and representative quiet/loud sections, then
inspect all remaining spans for cuts or missing words rather than one busy loop.
Read the exact reported measurement: peak, window RMS and whole-program loudness
are different. Report any required metric or metadata the available tools lack.
For dialogue-only, music-only or alternate versions, document inclusion/routing,
common origin and tail, reopen outputs and verify that variants never sum together.
Leave retained source, chosen output and unresolved delivery fields in the session.
