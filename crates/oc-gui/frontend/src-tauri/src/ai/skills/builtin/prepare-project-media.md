---
id: prepare-project-media
title: Prepare portable project media
when: A project uses imported audio, sample libraries or hosted patches and needs to reopen reliably elsewhere.
category: workflow
version: 7
tools: [project_get, project_extract, project_validate, edit_subtree, render_compat_report, tracks_list, render_range, audition_range]
---
Inventory samples, SoundFonts, hosted patches and routing. Inspect the actual preset and file references
with `project_extract`. Confirm the saved project's directory using `project_get`. Keep imported audio and
required assets beside the project in `media/`, preserve originals, and give new renders distinct filenames.

For an OpenSynth wavetable or saved patch state, stage the actual file in `media/tables/` or `media/patches/`.
Extract the owning preset. Change the existing `wavetable.file` or `open_synth_state_file` to its new relative
path, preserve its other parameters and effects, then use `project_validate` and `edit_subtree`.

## Two projects with the same wavetable path

In project A, have a parent preset point to `media/tables/lead.wav`. Give its child a different gain, and
play one fixed note through that child. In project B, put a different table at the same relative path so
loading the wrong table still produces sound. Extract both presets and identify which one owns the path.

Save A's child as a library patch in the app, load it into B, and reopen B. Inspect the effective patch.
Use `render_range` for the same note and release at the same sample rate. The library snapshot should
play A's table even if A's file is no longer present. Compare float WAVs at the same frame offsets with
notes, tempo, effects and routing fixed.

For delivery, use Save As to a different folder. OpenCompose collects library-owned native states there.
For another external state, stage it and its named sibling samples beside the project and use a relative path. Validate and reopen the whole folder from a new location. In a disposable fixture,
make the original location unavailable so an absolute reference cannot make the test pass. Use
`project_get` to verify which project is open and repeat the render comparison.

To share one sound, ask the operator to use **Share preset…**, then **Load** the resulting `.ocppatch`
on the receiving machine. Test the imported sound and inserts before sharing the rest of the session.

## Check the handoff

Run `render_compat_report` on the intended machine. Render a short passage that exercises each unusual
dependency and a dense one with the complete routing. Use `audition_range` to check pickups and tails.
If a dependency is unavailable to a collaborator, include a printed track and state what remains editable.
Deliver the reference mix, project tempo or map, dependency notes and any required attribution.
