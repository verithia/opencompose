---
id: design-dnb-and-halftime-bass
title: Design D&B and halftime bass layers
when: Build or troubleshoot a Reese, jump-up or growl bass, or a repeated halftime or riddim dubstep bass cell.
category: production
version: 2
tools: [read_skill, project_extract, devices_catalog, presets_duplicate, presets_set_params, edit_subtree, project_validate, render_range, audition_clip_source, audition_range, audio_analyze, audio_compare, tracks_list]
---
Write a short phrase with a clear stop and space around the backbeat; keep its drum reference fixed.
Mark notes that carry low weight separately from upper reattacks or a brighter answer.

## Keep the foundation stable while colour changes
Give the foundation its own centered patch. It may hold through several upper reattacks rather than copy them all.
Put the moving colour on another track, especially when unison, distortion or delay changes its low end.
Duplicate a starting preset and query devices_catalog for actual waveform, filter, envelope and route controls.
For a fresh calibration patch, silence unused oscillators, sub and noise before adding one source at a time.
Inspect option status: an accepted noOp warp is not working FM. Choose an implemented route or a suitable authorized plugin.
For a Reese, start with modest saw detune on the colour layer and compare several notes for uneven beating.
For a growl, move one supported filter, wavetable or warp parameter with the amplitude rhythm fixed.
Compare two contrasting colour patches on selected notes before adding more permanent layers.
Keep each colour's release intentional; a patch change, note-off, mute and choke do not have the same tail behavior.

## Separate the front of a note from its body
Try a prompt articulation beside a quieter, slower-opening body, using independent tracks and envelopes.
With amp sustain equal to its peak, shortening amp decay does not make a pluck.
For amplitude decay lower sustain; for a held body with a darkening spectrum, use the filter envelope instead.
Compare filter attack with the shortest gate. If the note ends before it opens, test a faster filter separately from longer amp release.
When the front masks the body, let an explicit articulation track key gentle compression on the body.
Keep that internal handoff separate from kick ducking on the bass group; verify the actual detector tap.
Read route-groups-and-returns rather than assuming a whole-patch send equals a selected-layer tap.

## Keep texture, width and pitch under control
For sampled colour, read map-a-multisampled-instrument: source pitch, assigned root and preset transpose work together.
Check whether the source channels actually differ before attributing width to a stereo file.
Put widening or delayed motion on the colour path; inspect the foundation and mono sum independently.
If resonance changes the perceived pitch, reduce its depth or keep it off the foundation.
Match listening output after distortion; changing its input drive to match level changes the sound being compared.

## Hear the handoff in context
Compare foundation, colour and both under the same kick and snare, including the shortest gate and longest held note.
Use a bounded render_range bus tap without tracks to inspect a keyed bass bus while its detector keeps running.
Hear the returned saved bus path with audition_clip_source, then audition the complete phrase and stop.
Use matched short audio_compare excerpts for one envelope, width or drive change at a time.
Keep the version with the intended attack, stable low weight and clear gap; measurements alone do not approve the timbre.
