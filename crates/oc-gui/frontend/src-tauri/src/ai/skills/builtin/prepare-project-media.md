---
id: prepare-project-media
title: Prepare portable project media
when: A project uses imported audio, sample libraries or hosted patches and needs to reopen reliably elsewhere.
category: workflow
version: 8
tools: [presets_list, project_get, project_extract, project_validate, edit_subtree, render_compat_report, tracks_list, render_range, audition_range]
---
Inventory source clips, sample zones, SoundFonts, tables, hosted states and
required sibling files with project_extract. Confirm the saved directory using
project_get. Retain chosen media and provenance beside the project; scratch
renders and preview files are not permanent dependencies.

## Retain the actual sound
Use distinct render filenames and preserve originals. For a native table or
patch state, stage its real asset under media/tables/ or media/patches/.
Extract the owning preset, update wavetable.file or open_synth_state_file to
a relative path, preserve other fields and validate before edit_subtree.
A child can inherit the path from its parent. Read effective_preset with
presets_list and inspect both authored blocks rather than copying a guessed file.
An audible fallback does not prove the intended sample or articulation loaded.

For delivery, Save As collects library-owned native states. Other external
states can require named sibling samples: stage them and use relative paths.
A reference mix describes intent; include a printed track for a dependency the
receiver lacks and say what remains editable. Preserve required credits.
Do not delete a currently unused file if retained session states still use it.

## Verify the saved project
Reopen a copy from another folder through the supported project flow. Confirm
the actual project identity and run render_compat_report on the receiving
setup. Hear a short part exercising each unusual dependency and a dense part
with complete routing. Check pickups, sample starts, releases and shared tails.
A file being present or a nonzero meter cannot prove the intended sound.

For a library asset regression, use a disposable two-project control only
when ownership is uncertain. In A, let a parent own media/tables/lead.wav and
its child override gain. In B, put a different audible table at the same path.
Save A's child as a library patch, load it into B and reopen B. The effective
asset and same-note float render should retain A's table even without A's
original directory. Keep notes, tempo, gain, effects and render offsets fixed.
Do not run this fixture for every ordinary media import.

To share a sound, ask the operator to use Share preset and load the .ocppatch
on the receiving setup. Test the imported source and inserts there; plugin
licenses and unavailable features remain explicit dependencies.
Deliver the project, retained media, tempo/map and reference mix together.
Record missing resources and the exact file/coverage that was heard.
