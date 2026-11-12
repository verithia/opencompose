---
id: capture-algorithmic-patterns
title: Capture algorithmic patterns in the DAW
when: A source session, external sequencer, or live-coded performance needs editable notes, isolated prints, or a repeatable arrangement with unequal pattern lengths.
category: workflow
version: 3
tools: [project_get, import_aligned_stems, project_extract, sections_list, edit_subtree, project_validate, render_range, audio_analyze, audition_range, midi_import_handoff]
---
Capture editable MIDI for note timing and isolated audio for synthesis and effects. Preserve the original,
leading silence, tails and two landmarks. Stop and restart once to learn whether the source cycle resets or
carries phase. Stage same-origin WAV and optional MIDI in handoff/ beside a saved empty project. Call
`import_aligned_stems` with the `project_get` revision. For MIDI alone, use `midi_import_handoff` with the
staged project-relative path and current revision. The latter preserves the clock of an existing arrangement.
Inspect the imported tracks and the muted MIDI with `project_extract` before moving any events.

## Check whether a restart shifts the clock or just the pattern

Make two four-bar captures at 120 BPM in 4/4, restarting the source in between. Include a separate dry marker
track with identical short clicks at 0.5 and 6.5 seconds in each capture, independent of the generator. Import
and render each marker track alone with `render_range` from bar 1 to the start of bar 5. Run `audio_analyze`
with `transients` on both the source and render. Use a 0.25-second window starting at 0.4 seconds, then another
starting at 6.4 seconds. Record both attack times and their separation for each take. A common displacement with
unchanged separation points to a start offset. A change in separation greater than the larger of 4 ms and twice
the reported resolution_seconds calls for checking tempo or sample-rate conversion. Widen the windows if a
marker falls outside them.
Once the markers align, compare the first generated accent against the early click in each take, preferably
using an isolated percussion voice. If the phase changes between takes while both clock markers agree, keep each
take's original generator phase until you decide which phase the arrangement needs. A pair of matching takes is
evidence of consistent phase for that restart procedure.

## Compare a six-beat cycle with a bar reset

For an original exercise, set a straight 4/4 grid at 120 BPM. Keep the kick on every quarter note for six bars.
On a separate tom track, place a named low tom `@pattern` with `bars: 1.5` in its header and hits at `1:2`,
`4:2`, and `6:2`. These are quarter-note offsets 0.5, 3.5, and 5.5 from the pattern start. Place it once at each
range: `[1:1 - 2:3]`, `[2:3 - 4:1]`, `[4:1 - 5:3]` and `[5:3 - 7:1]`.

For the reset version, make a one-bar pattern with hits at `1:2` and `4:2` and place it once per bar. Both
versions have twelve tom hits and use the same sounds, gains, and clock. Use explicit clip placements to
preserve the final half beat of space in each six-beat cycle.

Validate and use render_range with stems enabled from bar 1 to the start of bar 7 in each version. Check the
isolated tom stem with audio_analyze transients and allow for sample attack and detector resolution. At 120 BPM
the continuous cycle has a hit at beat 6.5 (3.25 s from the start) where the reset version has a gap. The
combined cycle restarts after three bars, with a tom at beat 12.5 (6.25 s). Keep the kick steady in both. Listen
with audition_range for the changing tom accents against the kick.
Reopen the saved project and audition the same window. Verify that the printed passage or written notes repeat
as intended and record which parts still depend on the external generator.
