---
id: check-stereo-balance-and-mono
title: Check stereo balance and mono compatibility
when: Panning a stereo sample, widening bass or checking a mix that loses weight when folded to mono.
category: production
version: 1
tools: [read_skill, project_extract, devices_catalog, edit_subtree, project_validate, render_range, audition_range, audio_analyze, audio_compare]
---
Begin with the actual channel count and an unprocessed source comparison.
Do not infer stereo content from two waveform lanes or a wide-looking meter.
Two identical copies panned apart remain the same central signal.
Separate note-allocation mono from stereo summation; they solve different problems.

## Decide what pan should do
Extract the track mix and inspect its post-insert signal and sends.
Use the documented pan_law behavior: balance attenuates the far stereo channel.
It does not independently move both recorded channels to a new position.
At center, balance preserves both channels at unity; auto detects post-insert width.
Compare center and either side with the same source before changing a musical part.
A pre send taps before the channel fader and pan; a post send follows both.
Check send pan and return routing when a supposedly centered source remains wide.

## Diagnose the width
Compare stereo and an explicitly identified mono sum at matched output level.
Record the fold-down gain convention; a louder sum is not inherently fuller.
For the half-sum convention, Mid=(L+R)/2 and Side=(L-R)/2.
A matching decoder uses L=Mid+Side and R=Mid-Side.
Use a supported matrix or external analysis when needed; do not invent a native device.
Removing Side cannot recover material already canceled in Mid.
Side above Mid is a diagnostic clue, not proof that every source disappears.
Listen to attacks, sustained low notes, lead identity and wet tails separately.

## Keep roles clear
For unstable bass, compare a centered sub with a separately controlled upper layer.
Test several pitches, short hits and sustained notes across phase/reset settings.
Do not assume an odd unison count or zero random phase guarantees constant weight.
Filter a wet width layer only where it masks the intended dry core.
A stereo return can add Mid as well as Side; compare its gain contribution.
Recombine any split bands before judging phase, level and crossover seams.
Check the complete phrase, not only a solo band or correlation reading.
Report when the audition provider cannot preserve stereo evidence.
