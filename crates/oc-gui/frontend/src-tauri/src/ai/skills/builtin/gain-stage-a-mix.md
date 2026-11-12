---
id: gain-stage-a-mix
title: Gain stage a mix
when: A track, bus, or master clips, a processor changes character with level, or the mix needs headroom.
category: mixing
version: 1
tools: [tracks_list, project_extract, tracks_mix, devices_catalog, render_range, audio_analyze, audition_range]
---
Render a loud section and write down its peak, true peak, RMS and LUFS.
Identify whether overload begins in the media, instrument, insert, bus or
final output. A final meter alone cannot identify where it began.

For recorded audio, clipped samples already on disk need a replacement or
restoration. Turning the clip down only reduces the level of that damage.
For an insert that saturates, lower clip or preset output before the insert.
A post-insert fader can reduce bus level without changing that insert's drive.

Use matching windows and include the loudest combined hits. Render a suspect
track alone, then the group, then the full mix. Follow the route and sends.
Check parallel returns too. Lower the stage that sums too hot before adding a
master limiter. A floating-point meter above zero flags downstream risk,
while an overloaded non-linear processor may already have changed the sound.

Compare the corrected window at similar playback loudness. If the fix made
the part sound thinner, restore the intended drive locally and reduce the
post-device level. Keep gain, pan and effects separate when testing.

Leave margin appropriate to the next stage. Avoid one peak or LUFS target
for every individual track. Report final headroom and any distortion that
remains audible in the source.
