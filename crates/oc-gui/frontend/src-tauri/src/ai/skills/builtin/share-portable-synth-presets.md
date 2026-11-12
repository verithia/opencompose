---
id: share-portable-synth-presets
title: Share and reuse portable synth presets
when: Deliver an OpenSynth sound with its samples and inserts, or reuse a received .ocppatch bundle in another project.
category: sound-design
version: 1
tools: [project_get, presets_list, preset_bundle_export, preset_bundle_import, render_range, audio_compare, audition_range]
---
Inspect the named preset with presets_list. Confirm the intended sound and complete
insert chain, including inherited values. A bundle captures OpenSynth and its samples;
it does not distribute a hosted plugin, its installer or a whole arrangement.
Remove external insert sidechains before sharing, or print the sound instead.
A track or bus used as a compressor key is not a portable part of a synth preset.

Save the project first. Finish editing and commit the active transaction, then export
in a new panel turn. Call preset_bundle_export with name and the current
expected_revision from project_get. It writes a new file inside handoff/ and returns
its path and size. Repeated exports do not overwrite earlier deliveries.
The GUI equivalent is Share preset in the OpenSynth patch menu.

Stage a received bundle inside the saved recipient project's handoff/ folder.
Create or select an existing OpenSynth preset as the destination. Read the current
revision, then call preset_bundle_import with path, destination name and revision.
The name and track references stay; the whole voice and insert chain are replaced.
The library keeps a reusable copy and numbers a duplicate library name. Undo restores
the previous project sound but leaves the imported library copy available.
The GUI equivalent is Load in the OpenSynth patch menu, choosing the .ocppatch file.

For a portability check, use a disposable recipient and the same short MIDI phrase,
velocity, sample rate and mixer routing as the sender. Render matched passages before
and after with render_range, then compare and listen with audio_compare and audition_range.
Account for track, bus and master effects outside the exported preset. Those are not
in the bundle. Confirm the recipient still renders after the original project samples
and exchange file are unavailable. Do not remove the operator's working source files.
Report a bundle as delivered only after the archive exists and a receiving project
has loaded it successfully. A valid archive alone does not prove the intended sound.
