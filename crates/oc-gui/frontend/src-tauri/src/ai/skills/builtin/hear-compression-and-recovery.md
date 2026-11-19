---
id: hear-compression-and-recovery
title: Hear compression attack and recovery
when: A vocal, guitar, drum or bass needs dynamics control and compressor settings are being chosen from a recipe.
category: mixing
version: 1
tools: [read_skill, devices_catalog, project_extract, edit_subtree, project_validate, render_range, audition_range, audio_compare, audio_analyze]
---
Name the problem: phrase jumps, excessive attack, weak body, long tail or
intentional pumping. They need different decisions. Keep an untreated print
and a passage with loud and quiet events, sustained notes and a following gap.

## Establish what drives the action
Inspect gain before/after the insert, existing makeup, detector source and
wet mix. Query devices_catalog for actual controls, units and topology.
Lower threshold until the intended events react; copying another source's
threshold does not copy its processing. Soft knees and release can affect
material below the nominal threshold. Read the actual transfer behavior.
Compare a low threshold/gentle ratio with a higher threshold/steeper ratio.
Similar maximum reduction can conceal different time spent reducing gain.
For uneven phrases, try clip gain or automation before one compressor follows
every word and breath. That is a different task from catching brief peaks.
Makeup does not simply equal the largest reduction. Match listening loudness
separately and retain raw levels; peak-matched output can still be louder.

## Hear the time constants
Compare faster/slower attack on one source. Hear consonant, pick or snare
articulation as well as remaining peaks. A nominal attack time need not mean
the exact time to full reduction; detector and knee change that response.
Then vary release while hearing the next quiet event and the sustained body.
For overheads, judge snare onset and cymbal wash together: improving one can
harm the other. Fast recovery can chatter or distort bass cycles; slow
recovery can hold down the next word. A flatter waveform is not acceptance.
A hold or multi-stage/automatic release has its own semantics; don't invent
one-to-one equivalents from another device. Check the supported controls.

## Check the detector and the mix
A filtered detector changes what drives gain; it does not exempt those
frequencies from broadband attenuation. For external-key ducking, verify the
actual route and compare missing, quiet, irregular and dense trigger events.
Use unmask-kick-and-bass for low-end recovery and transient/role decisions.
Compression cannot remove recorded reverb or reconstruct a missing syllable.
Use audio_analyze level_window at matching file offsets as envelope evidence,
then listen at matched output with the wet return and master chain restored.
If breathing/noise is boosted or the phrase loses expression, reduce action
or edit locally. Retain only the control the performance actually needs.
