---
id: design-wave-and-hardwave-layers
title: Design wave and hardwave layers
when: A wave or hardwave section needs a moving stereo bass, stable sub, or a transition from halftime to a driving drop.
category: production
version: 3
tools: [devices_catalog, presets_list, presets_duplicate, presets_set_params, project_extract, edit_subtree, render_range, audio_analyze, audition_range, tracks_list]
---
Assign separate jobs to the center sub, moving bass and wide upper layer. Start with the sub and kick. Add the
moving bass only after the bass rhythm and pitch hold up in a mono audition.

Keep the lowest sustained notes on a stable source. Reduce overlapping low energy in the wide layer and audition
the blend. A longer release can carry one bass note into the next harmony. Shorten it in small steps and check
for clicks at the intended tempo.

For a hardwave pitch fall or rise, check when the envelope reaches the target. Move that point relative to the
kick tail so the bass arrives where the rhythm needs it. Keep an editable baseline.

Use a voicing change or a quieter layer to protect the hook before adding detune. Compare halftime with a new
kick pattern over the same phrase before changing other parts. Close a return before a drop only if its tail is
masking the entrance, and check that the cut sounds deliberate.

## Test stereo width with the sub held constant

Make a dry two-bar probe at 140 BPM in 4/4. Play a centered C2 sine on the sub track. On a separate track, play
a C4 saw with the same rhythm and short gaps so the release is audible. For the probe, use a 10 ms attack and
release to limit clicks that could contaminate the low-band reading. Make two copies of the upper patch with the
same envelope, gain and voice settings. Use `devices_catalog` to confirm the OpenSynth parameter ranges. With
only oscillator A active, set `osc.a.unison` to 5, `osc.a.detune` to 12 cents, and change `osc.a.width` from 0
to 0.65 between the two versions. Use the same explicit `output.loudness_trim_db: 0` on both patches so
automatic preset trim cannot move the comparison. Leave headroom and use dry routing for the probe.

Use `tracks_list` for the track indices. Use `render_range` with `start_bar: 1`, `end_bar: 3`, `float_wav: true`
and explicit track subsets. Print the sub alone, each upper version alone, and the sub with the wide upper
version. Read `audio_analyze` for the 63 Hz entry in `octave.centers_hz` and its corresponding
`octave.energy_db`. Aim for the wide blend to be within 1 dB of the sub-only reading in that band. The wide
upper print should have a lower `stereo.mid_side_ratio_db` than the centered one. The ratio can reach +60 dB
when a stereo print has identical channels. Use the same file window for both checks.

Audition the blend with `audition_range` in stereo and mono. Check whether the upper rhythm remains clear when
folded down and whether its release bridges the intended gaps. Use `audio_analyze` with `mono_fold: {duration: 2}`
on the same window of the sub-only and blend WAVs. Compare the 63 Hz `loss_db` without level matching. If the
blend loses bass, reduce the wide layer's low end and repeat the print. Report which channels reached the model.

Render matching phrases and compare stereo, mono, and full-mix balance. Check the softest section as well as the
drop. Report which layers carry the sub, the rhythmic attack and the stereo motion.
