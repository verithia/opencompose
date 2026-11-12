---
id: prepare-loopable-score-stems
title: Prepare loopable score stems
when: An interactive cue needs synchronized layers, a seamless repeat, or a transition stinger that plays over the loop.
category: workflow
version: 4
tools: [import_aligned_stems, edit_subtree, project_extract, project_validate, render_range, audition_range, audio_analyze, render_compat_report]
---
## Define the loop and the intro
Choose one origin, end and sample rate for all loop layers. Keep count-in, pickups and intentional silence
explicit. For a cue with a separate first entry, prepare the intro independently and mark its handoff to the
loop. Choose a transition point where the harmony and rhythmic pulse can repeat.

## Capture the tail through the boundary
Copy the intended loop for at least two cycles. Keep the last events of the first cycle and all returns active
when rendering the next cycle. Use render_range with the second cycle's exact bounds and stems enabled. Print a
dry reference and a separate wet pass when the routing allows it. If the texture continues changing between
later cycles, compare a later window or shorten the tail. Pick an intentional entry rather than assuming that
one cycle represents the sustained mix.

## Test the handoff
Import the chosen files at the same origin and repeat their shared range at least three times. Compare the joint
with the original cue around the same point. Listen for a doubled onset, an abrupt loss of ambience, or a level
step. Check the frame count and start alignment for every layer. Toggle each additive layer across the boundary
and test a stinger over the densest combination. If the game runtime crossfades or schedules transitions, verify
those settings with the exported files in the runtime. Record the loop bounds, sample rate, intro handoff, and
checked layer sets.

## Exercise with a synchronized layer set

At 120 BPM in 4/4, write three explicit copies of a four-bar loop. Use a dry rhythmic layer, a sustained layer
with a tail, and a dry one-shot layer with an intentional one-bar rest at the start and no event in the last
bar. At a configured output rate of 48 kHz, call `render_range` for [8, 16) seconds with `stems: true` and
`float_wav: true`. The mix and each loop layer should contain exactly 384,000 frames. The first 96,000 frames of
the delayed layer should retain their intended silence if that layer has a dry route. Check the returned WAV
paths and check each stem's reported `frames` and `sample_rate`, including the delayed layer. Keep the printed
mix separate from the additive layers.

In a clean receiving project, use aligned-stem import for stereo balance at unity, or set `pan_law=balance`
on each receiving track through `edit_subtree`. Place three copies at 0, 8 and 16 seconds, then render
[15.75, 16.25) to inspect the second seam. For a seam that should sustain, use `audio_analyze` with separate 40 ms
`level_window` reads at file seconds 0.21 and 0.25. Compare those readings with the same window of the
uninterrupted cue. For an intentional downbeat attack, use `transients` and check for one onset at the expected
position. Audition the sustained layer alone, then the full layer set. Compare any shared return separately
because a sum of track stems can differ from the uninterrupted bus or master.

Deliver a loop only after the frame count and the joint agree in the receiving project. Keep the separate intro
or stinger outside that file. Mark transition behavior as pending until the target runtime has been checked.
