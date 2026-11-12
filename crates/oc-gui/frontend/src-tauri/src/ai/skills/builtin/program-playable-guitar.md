---
id: program-playable-guitar
title: Program playable guitar riffs and strums
when: Write a sampled acoustic, nylon or electric guitar part that needs plausible voicings, picking, muted strokes or slides.
category: instruments
version: 2
tools: [presets_list, tracks_list, project_extract, edit_subtree, project_validate, render_compat_report, audition_range, audio_analyze]
---
Pick a gesture and patch, then test one note at soft and hard velocities. Keep one sounding note per
string in a reachable shape. For fingerpicking, separate thumb bass from upper-note timing. For strums,
stagger explicit `@<beats>` onsets and reverse the order where an upstroke fits.

Verify documented keyswitches and controllers on the current host. If the patch cannot switch between
mute and sustain, use distinct presets. Shortening a note changes its gate and can leave the same attack.
If a continuous slide is unavailable, try a recorded transition or a written approach note.

## Two-bar voicing and strum study

1. At 100 BPM, use `guitar/acoustic_steel` with an available SoundFont. Build Em7 from E2, B2,
   D3, G3 and D4. Move only the bass to G2 in bar 2. The pitches can occupy the lowest five
   strings in standard tuning and the upper notes keep their positions.
2. Write a block attack at the start of each bar for a control. In the strummed pass, use
   `@0`, `@0.02`, `@0.04`, `@0.06` and `@0.08` from low to high. That is 12 ms between
   written attacks and 48 ms across the rake. Reverse the order in bar 2. Keep velocity and duration
   fixed for the comparison, then tune accents and trim releases where the next gesture needs room.
3. Use `tracks_list` for the guitar index, `project_extract` to find the phrase, then `edit_subtree`,
   `project_validate` and `render_compat_report`. Use `audition_range` from bar 1 to the exclusive
   end bar 3, first on guitar and then with the rhythm section. Check that the bass change
   reads by the end of the second rake and there are gaps before intended mutes or shifts.
4. Use the saved audition path for an `audio_analyze` transient window around the first rake
   with `min_gap_ms: 8`. The detector can merge attacks, so check written order and listen for
   the spread. If a doubled part loses definition, compare it in mono before adding room.
