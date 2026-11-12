---
id: layer-cinematic-percussion
title: Layer cinematic percussion and trailer impacts
when: Build orchestral drums, hybrid trailer hits, or a cinematic percussion rise with distinct attack, body and tail.
category: percussion
version: 1
tools: [tracks_list, project_extract, edit_subtree, project_validate, tracks_mix, render_range, audition_range, audio_analyze]
---
## Write the accent skeleton

- Place a few low hits on structural accents and check the pattern with bass and harmony. The
  listener should hear the pulse from this layer alone.
- Add faster answers with lighter taps, shakers or sticks. Use gaps between accents. Reserve a fill
  or cymbal for a transition so its arrival carries weight.
- Give heavy accents, light subdivisions and transitions separate tracks or named pads. Shape
  velocity by role. Verify quiet taps actually trigger the intended sample layer.

## Build an impact by function

- Start with a short transient, then audition a body or distant tail. Keep each extra layer only if
  it adds a different audible feature.
- If a whoosh precedes the hit, align its arrival with the transient by listening to the waveform
  region. Keep enough lead-in and tail in the audition window.
- When aligning related close and room samples, compare their sum and mono fold-down. A
  bigger-looking waveform can still lose body through cancellation.
- Check overlap between adjacent impacts. Shorten or fade a long tail when it masks a dialogue gap,
  bass attack or next cue hit.

## Verify against the arrangement

- Listen to the skeleton, then add the layers one at a time at sensible levels. Follow with the
  same window in the full cue.
- Render across the largest hit and its decay. Use `audio_analyze` to check peaks, then judge
  whether the attack and musical accents still read.
- Compare a quieter repeat and the climax. If both have the same perceived weight, reduce the
  smaller section's density before raising the whole percussion bus.
