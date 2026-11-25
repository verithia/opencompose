---
id: unmask-kick-and-bass
title: Unmask kick and bass
when: A kick disappears under an 808 or bass, or sidechain pumping weakens the low-end groove.
category: mixing
version: 3
tools: [devices_catalog, tracks_list, effects_add, effects_set_param, edit_subtree, render_range, audio_analyze, audition_clip_source, audition_range, read_skill, project_validate, project_extract]
---
Choose a phrase with kick and bass together, separately and across different
bass pitches. Hear which cue is missing: kick bottom, upper knock, bass
articulation or sustained weight. Inspect their actual sources and routing.

## Choose the smallest overlap change
Try level, note placement, source envelope or complementary samples first.
Compare a shorter kick tail with longer bass recovery; shortening either
too far removes weight or continuity. A gap is useful only if it fits the groove.
A harmonic upper bass layer can read on small speakers while the sub stays
stable. Removing low Side does not reconstruct content already cancelled in Mid.
Do not equate every spectral overlap with harmful masking or cut harmonics
because they do not lie on the song's equal-tempered scale.

## Choose a trigger and a target
Query devices_catalog for actual detector ports, controls and units.
Audio-key compression follows the source envelope; a tempo curve keeps
running without kicks, and a note-triggered shape follows its note events.
Choose deliberately, then check rests, syncopations and section changes.
Keep the detector non-summing if it should not add dry kick to the bass path.
Use edit_subtree to set an effect's audio sidechain source.
The native audio key is pre-fader and pre-mute, after source inserts; a muted
key can still drive it. A CONTROL follower has different mute behavior.
Keep key level and depth fixed first; source or insert changes affect the detector even when its fader is inaudible.
If ducking lasts too long, compare a shorter dedicated audio key before repeatedly shortening release.
For a dynamic EQ, check the band's key switch and range direction separately. Detector
bandwidth, processed frequency band and stereo Mid/Side are different choices;
detector EQ changes what triggers compression but does not make a broadband gain change band-selective.
Use selective band action when only one region competes; preserve useful
upper texture. Recombine split paths with the actual crossover and latency.

## Recover into the phrase
Set depth on simultaneous hits, then let the following bass note speak.
Compare short collision control with a longer musical pump. Serial gain
curves multiply; stacking them can swallow more bass than either alone.
Fast recovery can buzz on low cycles; slow recovery can hide quiet pickups.
Decide whether reverb/delay belongs before ducking, after it or on a separately
keyed return. Recheck the final ceiling after multiband or tonal processing.

Render the same range and compare raw onset/body levels and mono weight,
then matched listening. Check several notes rather than align one waveform
peak and assume the whole bassline is fixed. Hear full drums and harmony.
To observe a processed bass bus, render_range with bus and no tracks preserves the kick detector.
Hear that saved return with audition_clip_source, then restore full-mix listening.
Validate edits and name source key, target, timing, depth/band and compromise.
