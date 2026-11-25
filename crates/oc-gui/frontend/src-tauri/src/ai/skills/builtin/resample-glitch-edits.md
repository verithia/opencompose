---
id: resample-glitch-edits
title: Resample and sequence glitch edits
when: Turn a print into glitch edits, IDM or deconstructed club fragments, or shape continuous noise into rhythmic pulses.
category: production
version: 4
tools: [project_extract, edit_subtree, project_validate, audition_range, render_range, tracks_list, audio_analyze, audio_compare]
---
Pick a gesture that should remain recognizable: an attack, pitched answer, vowel or noisy decay. Keep the dry source and
use render_range for a short processed pass with its chosen tail.

## Print once and edit the result
Place the print on a separate track and keep its source recoverable. Choose which path supplies the finished attack,
body and space; mute the other unless a blend is intentional. Hear the print before reapplying distortion, filtering or
reverb that it may already contain. Check its first transient against the downbeat and retain the decay needed by the
next gesture. For feedback or random textures, capture one chosen pass so later comparisons reuse the same sound.

## Separate source choice from the edit
Choose three fragments by ear, then set ranges and offsets so each has its own trigger. Compare a repeated frozen
fragment with successive source windows on the same trigger grid. Source progression changes the phrase without
requiring more onsets or random pitch. When replacing a fragment, keep the onset, gain and processing fixed first. Find
the intended attack inside the new source before moving the whole rhythmic gesture. For a repeating percussion texture,
compare three fixed source-loop windows at one key, matching output after the instrument. Shortening a loop changes
repetition rate, seam shape and average level; then fix one window and change register separately. Use supported static
regions or prints; granular density is not a held sampler's continuously changing loop length. If retuning removes the
body, try a closer source register; if a long decay fills a useful gap, shorten it with a clean fade. If the front
ticks, move the cut or adjust its fade before dulling the entire source.

## Give the edits a larger rhythm
Keep one downbeat or backbeat recognizable while a short burst changes subdivision or pitch. Answer a sustained texture
with a clipped gesture, then leave a gap that makes the next attack matter. For a continuous noise source, shape
gain_envelope pulses around the groove instead of adding more hits. Use ramps that remove clicks without softening the
intended accent; keep warp and pitch fixed during this comparison. Compare the burst version with dense edits across the
whole phrase and keep the clearer intended contrast.

## Hear the phrase, not only the slice
Validate and render the same phrase with identical routing and tail allowance. Use audio_compare with listen: true and
listening_only: true for matched listening of corresponding short passages. Hear the source alone, with its nearest
competing part, and in the full groove over two repeats. If the edit loses its anchor, reduce one burst or restore a gap
before adding another layer.

For note-owned stepped tuning on `builtin.open_synth`, write `pitch_curve="0:0,0.125:700,0.25:-500,1:0"
pitch_curve_mode="step"`. Points use quarters:cents, hold their value to the next point, and target one voice without
changing its original key. Use at most 1024 ordered points, -9600..9600 cents, and an optional `pitch_curve_horizon` up
to 256 quarters when a release tail needs later updates. Do not add `pitch_curve_tension` in step mode. For a granular
source, active grains follow these updates only when its prepared patch selects `pitch_mode: "follow"`; the default
samples pitch at spawn. Render and listen to the transition, including its tail, rather than assuming a score mutation
proves the sound.
