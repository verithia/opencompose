---
id: compare-multiband-and-parallel-dynamics
title: Compare multiband and parallel dynamics
when: One register blooms, quiet detail disappears, or a parallel body layer needs to retain a useful dry attack.
category: mixing
version: 1
tools: [read_skill, devices_catalog, project_extract, effects_add, effects_set_param, effects_set_bypass, buses_add, buses_routes, buses_mix, tracks_send_add, edit_subtree, project_validate, render_range, audio_analyze, audio_compare, audition_range]
---
Choose a phrase with loud and quiet events, sustain, gaps and full-mix context.
Keep the dry source and name the fault before selecting another dynamics stage.
Compare balance or a static EQ first when the excess is present consistently.
A moving EQ band, crossover band and stereo Mid/Side component are different.

## Start from the correct topology
Use the mixer Multiband Compander for independently timed crossover bands.
Inspect devices_catalog and project_extract for current target, index and units.
Begin neutral and hear recombination before adding gain action; minimum-phase
splits can change phase even when their magnitude sum is flat.
Choose the fewest crossovers that isolate the heard behavior. Inspect quiet and
loud notes, not just a spectrum valley; restore every band solo/mute afterward.
Mixer bandN_range zero is neutral. Negative range allows attenuation only;
positive range permits bounded quiet boost and loud reduction around threshold.
Makeup is static gain after the clamp. Range is a limit, not fixed reduction.
Start attenuation-only when the aim is controlling a bloom; test quiet boost
separately for raised breaths, hiss or cymbal bleed. Call it below-threshold boost.
Choose attack/recovery by the following syllable or bass cycle, not a fixed recipe.
External key selection changes detection, not the frequency band that is audible.
The synth-rack compressor instead shares threshold, ratios and timing across its
three bands; do not promise the mixer's controls or phase modes in that rack.

## Compare a parallel alternative
Keep the main dry route and add one fully processed return with a named role:
punch, sustain, density or grit. Verify routing and latency before blending.
With a fixed processor, upstream drive sets response and return gain sets blend.
A return fed by an existing dry bus must not recreate an accidental extra dry path.
Bring the return from barely audible to excessive, then remove it from the full
blend and name what disappears. More level alone does not establish more punch.
Only add another path when its distinct contribution survives this subtraction.

## Accept the recombined result
Render identical dry, selected-band and parallel candidates with the master fixed.
Use audio_compare matched listening copies and audio_analyze level_window for
specific attack/recovery evidence. Keep raw peak measurements separately.
Hear quiet events, low-cycle distortion and sustained noise in the complete mix.
Check section changes, crossover/phase latency, and restored solo/bypass states.
Choose the smallest treatment that solves the named fault and preserves identity.
