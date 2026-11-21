---
id: resample-a-playable-instrument
title: Resample a playable sound without double processing
when: Printing a synth or processed sample into reusable one-shots or a melodic sampler while preserving root, tails and portability.
category: production
version: 1
tools: [read_skill, project_get, project_extract, render_range, audio_analyze, audition_range, devices_catalog, edit_subtree, project_validate]
---
Decide what the print captures: one source, its inserts, a return, or the
complete mix. Keep an editable source and its authored notes. A mix render
is not an isolated instrument merely because its filename says one-shot.

## Capture the intended signal
Use render_range on a bounded passage with the required tracks or bus.
Inspect whether master/return processing contributes; track isolation may
retain shared processing. Name a distinct file for the chosen print.
Leave attack pre-space and enough release. Inspect actual frames, rate and
channels; avoid trimming by the requested range if delivery was truncated.
For rhythmic motion, capture a chosen cycle with known phase and trigger.
For melodic playing, print a documented root and use several roots where
large transpositions change duration or formants too much.

## Map the print
Place retained media beside the project before final library or handoff use.
For builtin.sampler, read map-a-multisampled-instrument for SFZ key and velocity
zones. A fixed one-shot trigger and a pitch-tracking sampler are different.
Use the recorded root, not the key pressed when the synth had extra transpose.
Compare the root at the source duration before extending the key range.
For drums, use assemble-a-sampled-drum-kit and confirm one-shot/gate behavior.
Loop sustained material only when a clean loop is needed; do not loop a noisy
decay merely to fill every note. Retain the release when the part needs it.

## Avoid doubling and recirculation
Mute the original in the print's comparison unless deliberate layering is
intended. Remove duplicate inserts/returns that are already baked into it.
Never route the reimported print back into its own capture path.
Compare source and print at their root, then a low note, a high note and a roll.
Listen for missing attack, repeated tail, wrong root and pitch-dependent gaps.

Validate file references and reopen the saved project. Do not rely on a
reclaimable scratch path as permanent sampler media; retain the chosen print.
