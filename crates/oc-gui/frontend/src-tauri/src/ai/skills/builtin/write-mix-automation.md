---
id: write-mix-automation
title: Write mix and effects automation
when: A word, transition or effect needs a local ride, filter sweep, or delay throw without moving the whole mix.
category: mixing
version: 3
tools: [project_extract, edit_subtree, project_validate, devices_catalog, render_range, audition_range]
---
Set a stable static balance and choose the section that needs a move.
Identify what should change before choosing the lane. Use a local clip gain
or gain automation for a balance ride and a device lane for a tonal change.

## The lane line

    automation: volume [3:1 - 5:1] {4:1=-6dB, 4:4=0dB}
    automation: compression.threshold {1:1=-24dB, 3:1=-6dB}

A target is `<effect>.<parameter>` or a bare control such as `volume`, then
attributes (`[ppq=N]`, `[start - end]`, `muted`, `smoothing=`,
`mode=envelope|lfo`, `speed=`, `mult=`, `pw=`, `skew=`, `amount=`), then points
`bar:beat=value` with optional `curve=` and `tension=`. The braces are part of
the line: `volume: 0dB` refuses.

Inspect the existing target and lanes. Use the project's supported target
name and units. Track instrument volume automation is a MIDI controller.
For audio level changes, use the clip gain envelope or a gain lane. Verify
a named instrument controller against the actual preset.

For catalog unit `fraction`, write bare physical numbers, such as `0.4` for
40% mix. Preserve existing percentage literals: fractional effect automation
historically reads `0.4%` as physical `0.4`, unlike a static mixer insert where
it means `0.004`. Do not copy a percent literal between these contexts.

Put an anchor at the existing value before the move and restore that
value when it should end. A bounded lane can leave its last written value
in place after the span, so write the reset before the exclusive end.
Avoid overlapping writers on the same target. Use a ramp for a natural ride
and check discrete parameters separately.

For an effect throw, let the chosen word feed the delay, then close the
feed if a suitable send target exists. Keep the return open for the decay.
If no send target is available, use an isolated throw track or a supported
effect parameter and verify how its output affects the tail.

Audition from the bars before the edit through the tail. Check repeated
sections and nearby cuts. Use the fewest points that preserve the gesture.
