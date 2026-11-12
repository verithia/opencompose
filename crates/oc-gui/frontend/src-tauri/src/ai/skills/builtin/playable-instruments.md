---
id: playable-instruments
title: Playable instruments
when: Choosing or checking a playable preset, articulation or sampled kit, or diagnosing a silent track or a missing SoundFont.
category: render
version: 5
tools: [tracks_list, presets_list, project_extract, render_compat_report, render_range, audio_analyze, audition_range]
---
Inspect actual source, preset and parameters to identify the sound. Native OpenSynth, SoundFont and supported hosted instruments have different capabilities.
A default is a way to get a playable starting point. For a saw-unison lead read `design-and-verify-supersaw`.
## A bare track already sounds
When a project is opened, rendered or edited, a track with no preset gets a
built-in default for its family; the project carries the `@preset oc_default_*`
block and the track's `preset:` line, so `project_dsl` shows what was chosen:
    piano, keys        oc_default_keys      decaying felted keyboard
    bass               oc_default_bass      mono low upright
    organ, strings, pad, vocal, woodwinds
                       oc_default_strings   slow sustained ensemble
    lead, synth, fx    oc_default_lead      sustained pulse lead
    guitar, percussion oc_default_guitar    short wooden pluck
    brass              oc_default_brass     mid-forward swell
    drums, kit, drumkit
                       oc_default_drums     @kit route, synthesised voices

An explicit `preset:` is never replaced.
## Choosing another sound
Declare `source: builtin` with OpenSynth params for a self-contained preset. `params: {}` is a plain saw with `builtin.open_synth`. `builtin.test_tone` provides a known test signal.

## Verify an inherited patch after reuse
Use `presets_list({name: "Child"})` to read the authored child and its `effective_preset`. The child overrides parameter keys and appends its inserts after the parent's. Use `project_extract({path: "preset:Child"})` before editing authored DSL.
For a library reuse check, use the two-project exercise in `prepare-project-media`. Read the effective asset paths before choosing which file to stage.
For a level check, render a held C4 followed by short C4 repeats at fixed velocity. Use `audio_analyze({path, level_window: {start, duration}})` at matching file offsets for the attack and held body. Hear the release too.
The editor's dry keys can play at the authored gain while calibration is pending. Judge the arrangement using its rendered track, including the preset inserts.

## Match the patch to the performance
- For short strings, choose a loaded patch with the right attack and release for each part. If the exact stroke is absent, compare the available short strokes at the intended tempo and dynamic. Give independently phrased sections separate presets where needed.
- For a hosted instrument, confirm the patch, MIDI channel and articulation mapping. Check which controllers shape sustained notes and whether the host path delivers them. Audition an isolated articulation change. When the mapping is uncertain, choose explicit short and long patches and audition each.
- In a sampled `@kit`, use separate velocity windows for soft and hard recordings. Put alternate takes in the same window to rotate repeated hits. Test the soft and hard attacks and then repeat one velocity to isolate the rotation. Group open and closed hat pads with the same nonzero `choke` if the closed hat should cut the open one.

## A SoundFont needs its file
`source: soundfont` plays from `preset.file:`. `project_validate` and
`project_edit` refuse a missing file before it reaches a render:
    preset 'grand': file 'media/grand.sf2' not found (project directory '/song')
Resolve the path against the project directory. For `Track 'x' needs a SoundFont`, assign a preset with an existing `file:` or choose a playable built-in preset.

## Check the result
`tracks_list` shows the preset and instrument. Check mute, solo and whether the window contains events. Use `render_compat_report` for hosted features, `audition_range` to check the sound, or `render_range` with `stems: true` and `audio_analyze` to diagnose silence.
