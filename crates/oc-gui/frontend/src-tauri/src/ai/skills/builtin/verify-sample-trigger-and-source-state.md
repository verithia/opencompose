---
id: verify-sample-trigger-and-source-state
title: Verify sample trigger and source state
when: A chop bank, sampled bass or loop has unexpected tails, wrong tuning, broken retriggers or timing changes after editing.
category: production
version: 1
tools: [read_skill, devices_catalog, audio_clips_list, audition_clip_source, project_extract, edit_subtree, project_validate, render_compat_report, render_range, audition_range]
---
Keep the raw file and identify the actual sample, clip and instrument separately.
Read map-a-multisampled-instrument or assemble-a-sampled-drum-kit for the target.
Record file rate/channels, real root, source range and desired note-off behavior.
A drum trigger key, unity-pitch root and measured fundamental are different values.
Measure a stable pitched tail where possible; a swept attack can mislead a tuner.

## Make playback behavior explicit
For builtin.sampler inspect loop_mode, end, amp release and any choke groups.
One_shot ignores note-off and plays to end; no_loop uses the amp release.
Loop_sustain exits its loop at note-off and releases.
Loop_continuous and loop_pingpong can keep looping after note-off.
Do not mistake a loop point for a source playback-start offset.
If an instrument cannot represent a chosen source window, retain a derived slice file.
Native SFZ support does not include arbitrary opcodes or scripted articulations.

## Test the bank as an instrument
Intersect the written trigger keys with the instrument's actual receiving regions.
Repair an empty intersection before adding gain or effects; labels do not prove a map.
Compare zero-duration imported events with explicit short taps in the chosen mode.
Play a short tap, a held note, two repeats and two alternating pads.
Distinguish a pad cutting its own previous voice from cross-pad choking.
Check group/off_by direction with the actual mapped regions.
Test overlapping notes before deciding whether a source must be monophonic.
Keep velocity-zone thresholds adjacent and round-robin scope explicit.
Use the intended note range; a correct root does not prevent timbre changes far away.
Compare source duration with the desired note; release cannot invent missing recording.
For sustained material, hear loop seams in both channels and across multiple repeats.

## Test time and lifecycle changes
Change tempo only after specifying whether source rate, pitch or duration should follow.
Recheck slice starts and phrase length; do not accept inferred BPM as ground truth.
Keep arrangement time, pattern-local onset and audio-file position separate.
For a cropped phrase test its included attacks, held-note chase and tails.
Reuse an internal source window and verify it resumes that position.
Test transport pause, stop, seek, loop wrap and reopening the saved project.
A tail continuing after note-off differs from a transport that should be silent.
Check that the accepted slice and all dependencies survive the source editor closing.
After file replacement, recheck each region's source bounds, root, rate and note-off state.
Report unsupported states and a concrete retained-media alternative.
