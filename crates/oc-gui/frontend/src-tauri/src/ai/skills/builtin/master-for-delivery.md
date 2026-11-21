---
id: master-for-delivery
title: Master for delivery
when: A finished mix needs a clean master, true-peak margin, or an export suited to a delivery target.
category: mastering
version: 2
tools: [project_extract, devices_catalog, effects_add, effects_set_param, render_range, audio_analyze, audio_compare, audition_range, read_skill, edit_subtree, project_validate]
---
Preserve the mix and define the delivery format, peak/loudness constraints
and creative target. References guide expectations; no single LUFS value or
fixed headroom makes every style correct. Keep monitoring gain separate.

## Test what the mix needs
Hear the complete song without editing and note source defects, section
balance, starts and tails. Fix local problems in the mix when practical.
A clipped word cannot generally be restored by cutting one frequency band.
Use a loud passage to stress the chain, then a quiet passage and the next drop.
A short integrated-loudness reading describes that window, not the whole song.
Try a broad tonal correction only for a heard imbalance. A good mix can need
little processing. Keep creative master effects identifiable when bypassing
optional loudness stages; deleting the whole chain may change the composition.

## Hear the cost of peak control
Read devices_catalog for the actual limiter/clipper, ceiling, detector and
release controls. A high-ratio compressor is not automatically a brick wall.
Compare limiting with modest upstream peak control when a few hits dominate.
Clipping changes harmonics; limiting changes the envelope and can distort too.
Hear kick bottom, snare attack, vocal sibilance and cymbal roughness as drive
rises. Fast recovery can distort low cycles; slow recovery can hold down the
following note. Test release on the source rather than copy a tempo formula.
Compare at matched playback loudness. Keep raw-level prints for peak and gain
evidence. An unchanged sample ceiling does not prove unchanged punch or tone.
Preserve the quiet break if below-threshold boost would erase its contrast.

## Verify the actual deliverable
Inspect sample peak and true peak separately; processing after a limiter can
create new peaks. Recheck the final chain, not only an intermediate meter.
The bounded render tool is before project loudness targeting and does not
certify final export loudness. Verify the actual delivery file separately.
Measure whole-program integrated loudness over that file and inspect its
loudest short passages. Check a decoded lossy copy when that is the target.
Choose bit depth for delivery. Dither belongs at final integer reduction,
not on float intermediates or repeatedly between processing stages.
Reopen the delivered file and hear its start, full sequence and final decay
with no duplicate playback. Use adjacent bounded auditions if required;
check returned coverage and joins rather than claiming unheard portions.
Check mono for lost roles, and retain dynamic contrast unless the target
intentionally removes it. Report the file, measurements and listening limits.
