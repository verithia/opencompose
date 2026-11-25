---
id: write-mix-automation
title: Write mix and effects automation
when: A word, transition or effect needs a local ride, filter sweep, or delay throw without moving the whole mix.
category: mixing
version: 5
tools: [project_extract, edit_subtree, project_validate, devices_catalog, render_range, audition_range, audio_compare]
---
Name the musical change: a clearer word, rising tension or a strong arrival after a fill.
Set a stable static balance. Use clip gain or a gain lane for level, and a device lane for tone.

## The lane line

    automation: volume [3:1 - 5:1] {4:1=-6dB, 4:4=0dB}
    automation: compression.threshold {1:1=-24dB, 3:1=-6dB}

A target is `<effect>.<parameter>` or a bare control such as `volume`, followed by attributes and points.
Attributes include `[ppq=N]`, `[start - end]`, `muted`, `smoothing=`, `mode=envelope|lfo`,
`speed=`, `mult=`, `pw=`, `skew=`, `amount=`. Points are `bar:beat=value` with optional `curve=` and `tension=`.
The braces are part of the line: `volume: 0dB` refuses.

Inspect existing lanes and catalog units. Track instrument volume automation is a MIDI controller;
use clip gain or a gain lane for audio level, and verify instrument controllers against the preset.

For catalog unit `fraction`, write bare physical numbers, such as `0.4` for 40% mix.
Preserve existing percentage literals: fractional effect automation reads `0.4%` as physical `0.4`,
while a static mixer insert reads it as `0.004`. Do not transfer the literal between those contexts.

Anchor the existing value before a move and write its intended reset before the exclusive lane end.
The last value can persist after a bounded lane. Keep one writer per target and check discrete controls separately.
When moving or repeating a phrase, inspect its automation span and points separately from the audio or pattern clip.
A crop can select the returning part of an existing gesture; do not recreate it from its opening value by assumption.

## Let the next section recover
Save a render from the build through the next complete phrase, including effect tails.
Increase rhythmic density in only part of the build; retain a main accent and space for the arrival.
Treat filter opening, gain trim and echo feed as separate gestures before coordinating their handover.
Restore the intended filter and gain at the arrival. Close an echo feed while leaving its return open for decay.
If no send target exists, use an isolated throw track or supported effect parameter and hear its tail.
Compare density-only, control-only and combined versions over the same window at matched listening level.
Use audio_compare with listen: true and listening_only: true on corresponding short passages, then audition the whole phrase.
If the arrival loses its attack, thin the last fill hits or shorten their tails before boosting the next section.
If repeats wash over it, reduce the preceding feed; if the next phrase stays thin, inspect resets and competing lanes.
Validate, render and hear both the first transition and its later repeat after correcting one cause.
