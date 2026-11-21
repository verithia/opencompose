---
id: design-delay-feedback-and-ducking
title: Design delay feedback and ducking
when: A vocal throw, dub echo or rhythmic wet layer needs controlled repeats, phrase-gap reveal, or a deliberate tail ending.
category: mixing
version: 1
tools: [read_skill, tracks_list, project_extract, devices_catalog, effects_add, effects_set_param, effects_set_bypass, tracks_send_add, buses_add, buses_routes, edit_subtree, project_validate, render_range, audio_compare, audition_range]
---
Name the wet role: body slap, rhythmic answer, selected-word throw or texture.
Retain a dry reference and draw source, feed, delay, return and output paths.
Read route-groups-and-returns; keep one intended dry path and a fully wet return.
A send selects new excitation. Return gain changes the already accumulated tail.
Feedback changes future recirculation; clearing delay memory is a separate action.

## Set an actual delay
In the mixer, add Delay on the chosen return and inspect its real controls.
For tools, find the target/index with project_extract and devices_catalog.
Mixer time_l/time_r are milliseconds; feedback and mix are percentages.
The synth-rack delay has different units, range and mix law; do not copy values
between these devices or use that rack's minimum time for a short slap.
A quarter-note candidate is 60000/BPM ms; dotted is 1.5 times and triplet is
2/3 times that duration. Choose against the phrase rather than a mandatory grid.
Unequal tap times with equal feedback need not decay for the same duration.
Compare first repeat, later repeats and last useful tail at restrained output.

## Select, reveal or stop
Use a supported send lane to capture a chosen word, or isolate a throw clip
when no appropriate send-automation target exists. Preserve its onset and fades.
Closing the feed should leave existing repeats audible. Automate return output
only when those repeats must also end, writing a bounded reset before the end.
For phrase-gap reveal, put compression after the delay and verify a real dry
vocal AUDIO key route. The source should trigger without being summed into wet.
If compact discovery omits ports, inspect supported routing instead of guessing.
Compare detector release with the next word; makeup gain does not undo the duck.
render_range with bus set isolates the return while its key graph keeps running.

## Keep memory and level bounded
An internal feedback filter shapes later circulation; a post-delay EQ shapes
every repeat at output. Do not claim native feedback-filter controls not exposed.
Use supported internal feedback or a retained print, never an assumed bus cycle.
Output ducking hides repeats; it does not clear them or prevent internal buildup.
Compare stops, seeks, missing-key sections and re-enable behavior before relying
on a hard cut. Bypass, feedback zero and state reset are not interchangeable.
Hear the full mix through the tail, then retain only the useful bounded version.
For fine A/B, use matched copies; keep raw peak and full-tail evidence separately.
