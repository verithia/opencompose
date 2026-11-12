---
id: writing-notes
title: Writing notes and phrases
when: Adding or editing notes, chords, rests, dynamics, or drum hits, shaping a groove, arranging phrases, checking an interrupted MIDI take, or fixing a note-line parse error.
category: editing
version: 7
tools: [project_extract, edit_subtree, project_validate, tracks_list, audition_range]
---
`Parse error`, `Syntax error at line`, `trailing comma`, `comma list`, `chord`,
`rest is not a chord member`, and `Unknown pattern reference` are common note-edit failures.

    @phrase intro [1:1:0 - 5:1:0]
      G4 q mf
      @3 C4 q mf, E4 q mf, G4 q mf
      rest q
      |
      C2 h f articulation=accent

Pitch is a letter with optional accidentals and an octave (`F#4`). Durations include `w h q 8th 16th 32nd 64th`, dots and tuplets (`q.t`). A `rest <duration>` advances time and a bare `|` is a bar line that consumes none.
MIDI keys span `C-1` (0) through `G9` (127). Preserve the `-1` octave when importing or editing low keys. Higher written notes such as `A9` remain valid notation, but cannot be exported as MIDI keys; move them into range before MIDI export. Pitches below `C-1` are refused in projects.
`@<beats>` (`@1.5`) sets the line's start in quarter notes from the phrase start. Without it the cursor advances by the longest note on the line. Place a positioned note after a gap to leave silence. Rests take no `@`.
`[C4 E4 G4] q mf` shares a duration, dynamic and modifiers. The canonical comma list lets members carry separate values. Use `v=` for explicit velocity, which overrides the written dynamic.
`@2.0 pedal down` and `@4.0 pedal up` sustain intervening note-offs. `@2.0 cresc` or `@2.0 dim` ramps gain toward the next dynamic or phrase end. `gliss=G4` names a target and `gliss=120ms` controls arrival. Use `gliss=run` or `gliss=run_diatonic` where a continuous slide is unavailable.
Phrase ranges are one-indexed with an exclusive end. `[1:1:0 - 5:1:0]` covers bars 1–4. A track's `clip:` places a named `@pattern`. Put a `@phrase` directly in its track.

## Shape the part
- Draw the phrase's velocity contour first. In a fast repeated string figure, try quieter notes between accents, then listen at the softest dynamic to make sure they still speak. Move only the onsets that need space, using decimal `@` positions within the phrase. Check for notes pushed past clip boundaries.
- Under a lead, start with wider spacing in the lower register. Carry useful common tones across chord changes and move the remaining voices a small distance when their ranges allow it. Audition with the lead for space.
- Build related sections from one recognisable motif. Place a simpler version earlier, leave a deliberate gap before the main entrance, and keep the core rhythm when changing the later repeat. Compare the bars on both sides of the entrance.

## Program a groove
In a named drum `@pattern`, put hits under `hits:`. Write the pad name and `beat:subdivision`, where beat starts at 1 and the sixteenth subdivision is 0–3. An inline drum pattern in a track writes the hits directly and can use `xN` to repeat its bar.

    kick  1:0 f  v=106
    snare 1:3 p  v=25
    snare 2:0 f  v=100

Set the kick and backbeat first. Put a few softer hits in gaps and leave room around doubled kicks. The velocities above are starting points. Move them to suit the kit and listen for a quiet pickup into the backbeat.
Use project or per-pattern `swing:` to move eighth-note offbeats. Add `swing_subdivision: 16th` to the project to move every second sixteenth instead, which is how hats at `1:3` get a groove; triplets stay where written. When nudging pitched-note triggers for a groove, move deliberately coincident attacks together if separating them makes an unwanted flam.
Validate the changed block and use `audition_range` on the same short window before and after. Listen with the bass or lead once the solo part works.

## Check an interrupted MIDI take

Use `project_extract` at `pattern:<name>` and `audition_range` on the affected track across the last captured notes and their release.
Live MIDI recording folds sustain into saved note lengths; check note ends and separately authored `pedal up` marks.
Use an authorized reference or confirmed release point for the smallest duration/pedal edit with `edit_subtree`; keep unverified notes as captured and report where the take stopped.
Validate with `project_validate` and audition the same window after applying it.
