---
id: prepare-a-portable-production-template
title: Prepare a lightweight production template
when: Reusing project setup, section routing, returns or orchestral resources while keeping dependencies portable and the next song editable.
category: workflow
version: 1
tools: [read_skill, project_get, project_extract, tracks_list, content_libraries, edit_subtree, project_validate, render_compat_report, audition_range, audio_compare]
---
Start from the next task's needs: sketching, orchestration, vocals or a live
handoff. Preserve a source project. A template removes repeated setup; it need
not decide the next song's key, tempo, melody, arrangement or mastering chain.

## Keep useful structure
Name tracks and buses by musical role. Inspect output paths, shared returns,
key/control routes and processing ownership with project_extract.
Use route-groups-and-returns for actual native graph rules. Do not invent
bus-send levels or hardware-output routes from another host's menu labels.
Keep optional material identifiable and muted or absent as appropriate.
Muted, bypassed or minimized instruments need not release memory or latency;
choose a lighter source or an explicitly supported unload/freeze operation.
For sampled libraries retain provenance, required articulation and media files.
A smaller patch can be useful when a passage needs one vowel or articulation.

## Separate audition from production
Keep an authorized reference's playback gain/processing separate from the song.
Use file-based audio_compare if a monitor-only reference route is unavailable;
do not silently mix the reference into export or claim an invented bypass path.
A sample shortlist is a choice aid, not a reason to download every pack.
Leave optional starter rhythms easy to replace. For a ghost key, inspect the
actual audio-key versus control-follower behavior rather than assuming mute
has the same effect on both. Keep disabled writers from latching old values.

## Check the saved starting state
Validate the project and inspect render_compat_report for actual support gaps.
Read prepare-project-media for retained sources and portable relative paths.
Check the saved state has the intended tempo/grid, no surprise solo, active
reference, stuck notes or unwanted noise generator. Test loaded notes and
articulation only over a short relevant phrase; repeated developer fixtures
are not required on every new creative use.
Reopen through the supported project flow when verifying portability. Missing
licensed instruments or restoration features remain explicit dependencies.
Keep the reusable setup distinct from the accepted song and retained session
history; deleting a currently unused file can still break an earlier state.
