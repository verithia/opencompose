---
id: prepare-a-stem-handoff
title: Prepare a stem handoff
when: A collaborator needs aligned tracks, buses, dry and wet choices, editable MIDI, or a mix reference for import.
category: workflow
version: 3
tools: [tracks_list, project_get, project_extract, render_compat_report, render_range, audio_analyze, audition_range, midi_export_handoff, midi_import_handoff, project_validate]
---
Agree on the stem list, dry versus printed processing, format and sample
rate before rendering. Preserve the mix state in a separate project copy
when disabling effects for handoff. Keep a rough mix with processing as a
reference, labeled so it will not be mistaken for a stem.

Choose one origin and end for every file. Preserve pickup notes and leading
silence. Allow enough room for the longest intentional tail. Label files by
role and name, and record tempo, meter, and any tempo map or count-in offset.

Use render_range with stems for a bounded QA window. For whole-song delivery,
use the app's Export Stems command. A range render is before loudness
normalization. The exported whole-song mix may have a loudness target that
does not apply to individual stems.

Avoid handing off both a group sum and all of its child tracks as though
they should be summed together. Label alternate dry/wet versions and note
which returns or shared processing are printed. Nonlinear bus processing
can keep a stem sum from matching the processed master exactly.

## Verify an editable MIDI companion
Print a reference stem. Export in a fresh panel turn after edits commit.
Read the committed revision with project_get, then call midi_export_handoff
for a new path in handoff/ and a report of anything MIDI could not carry.

For a round trip, stage the MIDI in the receiving project's handoff folder.
Call midi_import_handoff with the relative path and its project_get revision.
A clean receiving project takes the MIDI clock. An existing arrangement
keeps its clock. Note lengths and CC64 are imported separately. Run
project_validate and use project_extract to check note ends and pedal events.
In the File menu, turn off Expand sustain pedal on manual import.
At 480 PPQ with 60% swing, the eighth-note offbeat at offset 0.5 quarters
exports at tick 288, while offset 0.75 stays at tick 360. Keep the reimport
straight so you can check the timing already in the file.
Audition through the receiver's instrument when available. If pedal, note
length or groove differs, compare same-origin prints and include the original
stem beside the MIDI.

Reimport a selection at the common origin. Check length, sample rate,
silence, clipping, channel count and start offsets. Listen through a busy
section and the end before describing the set as ready.
