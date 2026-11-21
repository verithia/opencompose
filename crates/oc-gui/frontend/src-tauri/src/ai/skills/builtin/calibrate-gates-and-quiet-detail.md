---
id: calibrate-gates-and-quiet-detail
title: Gate noise while preserving quiet detail
when: Reducing bleed, noisy gaps or excess drum tails without cutting wanted ghost hits, breaths or releases.
category: mixing
version: 1
tools: [read_skill, project_extract, devices_catalog, edit_subtree, project_validate, audition_range, audio_compare, audio_analyze]
---
Hear a wanted quiet event, a loud event and an unwanted gap in the same source.
A gate separates levels over time; it cannot separate two overlapping sounds
merely because they have different identities. Keep an untreated comparison.

## Establish what can be separated
Inspect the actual insert chain and gate catalog. The native gate exposes
threshold, attack, hold, release and range. Its linked stereo detector takes
the larger absolute channel sample. It has no separate external detector key,
filtered detector or exposed hysteresis.
Use a verified external processor when that behavior is essential, or make
local clip-gain edits. Do not invent unsupported detector controls.
The synth compander can boost below-threshold bands; that is not this gate.

## Set attenuation before chasing silence
Use modest negative range for partial attenuation. At 0 dB range the native
gate is transparent; deeper negative values close further. Raise threshold
until unwanted gaps reduce, then check the softest wanted events again.
If their levels overlap the bleed, accept some bleed or edit locally instead
of raising the threshold until the musical detail disappears.
Set attack from the wanted onset: a slower opening can remove consonants or
hit definition. Hold keeps the gate open after crossings; release shapes its
return to the floor. Tune both against the tail and next event at this tempo.
Threshold chatter is not repaired by a new tone label; compare a longer hold,
a gentler range or a different detector-capable device when needed.

## Judge the source in context
Compare the same passage at matched playback level, including the last tail
and next pickup. Hear for missing ghosts, clipped breaths, pumping noise and
an artificial stop. A cleaner gap is not useful if the phrase loses articulation.
A gate can shorten gaps in a separated vocal, but cannot remove baked-in
reverb during a sung vowel; use triage-stem-artifacts for that decision.
Use audio_analyze only to locate envelope changes, not to certify the repair.
Validate the edit and retain the gentler result when isolation costs too much.
