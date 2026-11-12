---
id: rebuild-a-reference-passage
title: Rebuild and verify a reference passage
when: Rebuild a reference passage or full song, or study a technique from a fan remake, artist session, or tracker module.
category: production
version: 11
tools: [project_get, import_aligned_stems, midi_import_handoff, project_extract, edit_subtree, audio_clips_list, audio_clip_transients, devices_catalog, render_range, audition_clip_source, audition_range, audio_analyze, audio_compare, project_validate]
---
Keep an untouched, authorized reference. Record the release or remix, artist, session author, revision and passage.
Distinguish creator documentation, file observations and inferences by ear.
Ask the operator to inspect a copy in the source DAW or tracker. Record editable, printed, frozen and missing layers,
routing, automation, media and plugins. For tracker retriggers or slides, retain a print and note order, row, pattern and channel.

Record the source start, tempo and meter changes, and two named landmarks with bar:beat and elapsed time.
Keep leading silence in WAVs. If the grid is unknown, use audio_analyze beat_grid and confirm pulse, meter and downbeat against a later passage.
Get the saved revision with project_get. Stage files in handoff/. Use import_aligned_stems for WAVs with optional muted MIDI.
Use midi_import_handoff for MIDI alone. Existing arrangements keep their clock. Compare the tempo and meter maps in project_get.
Extract the MIDI track with project_extract. Check a note released under pedal against the source key release and both CC64 points.

## Verify stem origin

Broadcast WAV timestamps declare an origin and may survive an edit. Missing timestamps leave origin unverified.
Against a trusted print or source log, match two distinctive attacks, including a later hit after any clock change.
Use audio_clips_list for indices, then audio_clip_transients with clip_index, source_start_seconds and duration_seconds.
For example, check 0.5 s windows starting at 1 and 3 s. Use audition_clip_source to identify each attack.
Compare source_seconds to the trusted times, with leading silence. For an attached print, use its conversation path,
audio_analyze in transients mode for file_seconds, and audition_clip_source. Preserve intentional timing between parts.
Treat differences within combined resolution_seconds as uncertain.

If reference attacks at 1.250 and 3.250 s appear at 1.125 and 3.125 s in a stem, delay that stem 125 ms.
Prefer a common-start re-export with leading silence. For an authorized fix in a project copy, extract and edit_subtree at path project.
Move both bounds of only the suspect clip by the delay. At 120 BPM in 4/4, a 125 ms shift takes 1:1 to 1:1:1.
Keep file and playback settings. Request a re-export if the grid cannot represent the delay or the move would lose a pickup.
If offsets disagree beyond detector resolution, check sample rate, tempo map and edits before shifting.
Run project_validate and audition_range around both landmarks. Render the windows with render_range and stems enabled.
Add each window start to the new file_seconds from audio_analyze transients. Report confirmed hits and any uncertain origin.

## Rebuild a keyed bass

For a four-bar ducking study, record the detector and compressor settings. Print kick, dry bass and keyed bass from one origin,
at fixed gain and with only the compressor switched. Import them, then use devices_catalog and edit_subtree to rebuild the key.
Render the bass stem with render_range and stems enabled, keeping the kick active. Compare dry to wet, then wet to the rebuild.
Use audio_analyze level_window for RMS before the kick, in the dip and after recovery. Use audio_compare on longer aligned passages.
Listen to one pass at a time with audition_range. Run project_validate and record timing, listening verdicts and remaining differences.
