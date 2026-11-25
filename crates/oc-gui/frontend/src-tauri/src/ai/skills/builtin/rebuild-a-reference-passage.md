---
id: rebuild-a-reference-passage
title: Rebuild and verify a reference passage
when: Rebuild a reference passage or full song, or learn an arrangement, sample-editing or mixing technique from an existing piece.
category: production
version: 13
tools: [read_skill, project_get, import_aligned_stems, midi_import_handoff, project_extract, edit_subtree, audio_clips_list, audio_clip_transients, devices_catalog, render_range, audition_clip_source, audition_range, audio_analyze, audio_compare, project_validate]
---
Keep an untouched, authorized reference and choose a short passage with an entrance and a phrase ending. Record the
release or remix, artist, session author, revision and passage. Listen first and name the target: groove, chord motion,
bass articulation, a sample gesture or vocal depth. Describe what changes at the entrance and which part carries the
hook; keep uncertain explanations as hypotheses. Separate comparison-only audio from kept source material, and keep it
out of candidate playback, export, sends and returns. Prove exclusion with a reference-path ablation; a name or mute
button alone is not proof. Distinguish creator documentation, file observations and inferences by ear. Ask the operator
to inspect a copy in the source DAW or tracker. Record editable, printed, frozen and missing layers, routing,
automation, media and plugins. For tracker retriggers or slides, retain a print and note order, row, pattern and
channel.

## Rebuild the musical roles
Sketch the main accents, bass rhythm and chord movement with simple sounds before refining the texture. Separate a held
foundation, rhythmic middle and short answer when those jobs are audible. Match attacks, occupied note lengths and
useful silences before adding more layers. For each sound, identify the required attack, stable body and decay, then
select a suitable source or supported synth. Change source choice, pitch, note length and processing separately so a
timing improvement is not mistaken for a better patch. Read choose-and-fit-a-sample for source selection and
write-mix-automation for a coordinated section handover.

## Give each sound one clear owner
Choose the live sound or its processed print for playback; keep the other recoverable and inactive unless layering is
intentional. Trace direct output, sends, parents and returns with project_extract when a part is too loud, hollow or
washed out. Restore one clear path first, then test a deliberate parallel blend at compensated level. If a print is late
or cuts off, check its first transient, source offset and retained decay before changing the master mix.

## Preserve the phrase when transferring material
When editable parts are available, use them to test a note, articulation or routing hypothesis. For synchronized stems,
keep leading silence and the common origin. Confirm an early and later landmark by ear. Use import_aligned_stems for
aligned audio or midi_import_handoff for MIDI, with the saved project_get revision. Preserve the existing project's
clock and check tempo changes, pickups, note releases and pedal state after transfer. Read align-and-stretch-audio if
the second landmark drifts; a metadata label alone does not verify alignment.

## Choose the next correction by listening
Render the same passage and tail before and after one change, with the reference excluded. Use audio_compare with
listen: true and listening_only: true on corresponding short passages for matched listening. Audition the complete
phrase and its repeat, with the target role alone and in the full mix. If the attack is hidden, shorten a competing tail
or thin the fill; if harmony is unclear, simplify the voicing before widening it. For keyed bass, hear kick plus dry
bass, then the keyed version, keeping the kick active while rendering bass stems. Check the dip and recovery by ear; use
audio_analyze level_window as support rather than a quality score. Validate the chosen edit, hear its neighboring
transition, and state the audible improvement and remaining mismatch. For a full song, carry the accepted roles through
the form and audition the complete final render, including transitions and ending.
