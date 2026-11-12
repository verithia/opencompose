---
id: program-independent-pitch-gestures
title: Program independent pitch gestures
when: A virtual instrument needs one note to bend without moving a sustained drone, another chord tone, or a different note's articulation.
category: orchestration
version: 2
tools: [devices_catalog, presets_list, project_extract, edit_subtree, project_validate, render_compat_report, audition_range, render_range]
---
## Establish the intended movement
Use a short reference phrase and mark stable pitches, approach gestures, and
arrival points. For a tuning tradition, use a reference for the particular
phrase or instrument. Derive the target from that evidence before assigning
cents. Identify which voice must stay fixed when another voice bends.

## Test the route in isolation
For OpenSynth, configure a project MPE zone such as
`mpe: { lower: 2-8, bend_range: 2 }` when a two-semitone range suits the test.
Set the controller's bend range to match for live input. Use two sustained
overlapping notes in the same patch. Give one note a clear positive test bend,
for example `C4 q mf bend=100ct`, and leave the other unbent. Check the
interpreter and patch on the actual output path before writing the real phrase.
With a clean sine patch, play C4 and A4 together and bend only A4 by 100
cents. Check for the fixed C4 near 262 Hz and the bent pitch near 466 Hz.
The DSL's `bend=` accepts nonnegative cents. For a downwards gesture, check
the supported input or automation path and verify it in the rendered result.
For a hosted instrument, verify its MPE support independently. If the patch
can only bend a whole channel, use separate instrument tracks for the fixed
and moving voices where that routing gives the required result.

## Compare with the reference phrase
Audition the moving line, the fixed voice, and the combined phrase. Check the
initial interval, bend direction, arrival pitch and whether the fixed note
moves. Compare the exact same notes with pitch movement removed. If only one
pitch can be measured at a time, measure the isolated parts and use listening
for their interaction. Save and reopen to verify the chosen expression path.
