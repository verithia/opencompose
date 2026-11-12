---
id: map-a-multisampled-instrument
title: Map and check a multisampled instrument
when: Build or troubleshoot a melodic sampler with key zones, velocity layers, repeat variants, and sustained sample loops.
category: production
version: 1
tools: [devices_catalog, project_extract, presets_add, presets_set_params, render_compat_report, audition_range, project_validate]
---
## Map pitch and dynamics separately
Copy the samples into project media. Record each sample's real root, dynamic,
and take. For two recordings rooted at MIDI 60 and 67, try adjacent ranges
57–63 and 64–70. Keep each root fixed while moving its zone boundary.
Loop a note on either side of the split and move the split if the transition
sounds like a different instrument. Extend the map only after that check.
Within each zone, try separate soft and strong regions with adjacent velocity
ranges, for example 1–79 and 80–127. Balance their recorded gain by ear.
Step across the boundary with the same note and listen for an unintended jump.

## Load the map
Use builtin.sampler with a project-relative sampler.sfz_file or sampler.sfz_text.
An SFZ region for the first soft sample can start with:
```text
<region> sample=media/c4-soft.wav lokey=57 hikey=63 pitch_keycenter=60 lovel=1 hivel=79
```
For sustained sounds, set loop points in sample frames and listen to a note
held longer than the sample, then release it. Try nearby endpoints if the
seam clicks or pulses. A one-shot hit may need no loop.
Test seq_position with repeated notes at fixed velocity. The melodic sampler
supports one global sequence. For percussion with independent repeat banks,
use a drum kit instead. Check host compatibility before relying on additional
SFZ opcodes such as keyswitches or velocity crossfades.

## Verify the saved instrument
Render a scale across every zone boundary and a velocity staircase at a fixed
pitch. Repeat a single note to hear variation, then audition the part in its
arrangement. If it fails after reopening, check project-relative sample paths
and compatibility diagnostics before changing musical settings.
