---
id: assemble-a-sampled-drum-kit
title: Assemble and tune a sampled drum kit
when: Building a drum kit from one-shots or replacing weak drum sounds while retaining timing, dynamics and choke behavior.
category: production
version: 1
tools: [read_skill, devices_catalog, project_extract, edit_subtree, project_validate, audio_clips_list, audition_clip_source, render_range, audition_range, audio_analyze]
---
Choose roles for kick, snare, hats and auxiliary hits before layering.
Hear a source's complete body and tail. A sample may already be compressed,
stereo, reverberant or clipped; another bus chain can exaggerate that state.
Use select-samples-by-role for discovery, not a large unfiltered kit dump.

## Map sources with explicit behavior
Use builtin.drumkit and inspect its pad/layer schema in devices_catalog.
Declare actual sample layers; an audible synthesized fallback is not evidence
that the chosen sample loaded. Inspect compatibility and render warnings.
Preserve project-relative file paths. Keep original samples and license data.
Give hats a shared nonzero choke group when a closed hit should stop an open
tail. Keep kick and snare out of that group unless intentional mutual cutting.
Use one_shot for full hits or gate behavior when note-off should end a sound.

For recorded dynamics, non-overlapping velocity windows select soft and hard
hits. Overlapping windows cycle matching layers on that pad, not a global
random kit. Do not call gain-scaled copies new recorded velocity layers.
Tune a tonal kick body only after hearing it with the bass. A short noisy
snare need not be forced to the key. Trim decay to leave space for the next hit.

## Prove the mapping before mixing
Play each pad alone, a soft-to-hard velocity staircase, repeated equal hits,
and an open-hat hit followed by a closed hat. Check intended samples, missing
zones, repeat variants, note-off and choke. Test rapid rolls at actual tempo.
Then hear the complete kit; set source/pad gain before bus compression.

For layers, assign attack, body or texture roles and read align-layered-transients.
Judge several hits and bass notes; one aligned click can hide body cancellation.
Compare with a single-source version. Remove a layer that adds only loudness.
Render a phrase with a fill and the loop return. Listen for flams, truncated
tails, velocity jumps and disappearing lows; retain only useful complexity.
