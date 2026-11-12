---
id: design-future-bass-wobble
title: Design a future-bass wobble
when: Create or refine rhythmic filter movement in a future-bass bassline or pulsing chord layer.
category: production
version: 2
tools: [devices_catalog, presets_list, project_extract, presets_add, presets_duplicate, presets_set_params, tracks_list, render_range, audition_range, audio_analyze]
---
Choose the part that needs movement. Keep chord polyphony for stabs and use mono
or legato for bass only when the line calls for it. Treat brightness, amplitude,
pitch movement and kick-triggered ducking as separate decisions.

## Build the pulse
Inspect the source, patch and active routes. Duplicate an existing preset for a
reversible comparison and check which tracks or patterns will be reassigned.
Query `devices_catalog` for `builtin.open_synth` with the `lfo.`, `mod.`, `filter.`
and `osc.a.` prefixes. Use a rich oscillator through a low-pass filter. For saw
chords, read `design-and-verify-supersaw`. Start dry with room between stabs.

These values assume an audible source. Merge with `presets_set_params` and use
a free `mod.N` slot.
```json
{"filter.mode":0,"filter.cutoff":600,"filter.resonance":0.15,"lfo.sync":1,"lfo.sync_div":25,"lfo.retrigger":1,"lfo.pitch":0,"mod.1.source":0,"mod.1.destination":0,"mod.1.amount":0.12,"mod.1.bipolar":1,"output.gain":0.5}
```
Source `0` is LFO 1 and destination `0` is Filter Cutoff. `bipolar: 1` centers
the movement on the base cutoff. For an opening sweep from a closed base, try
`bipolar: 0` with positive amount. Tune cutoff and depth together. Amount is
unitless, and full depth spans 11.4 octaves.

## Shape and check the groove
`lfo.sync_div` values `22`, `25` and `28` give quarter, eighth and sixteenth note
cycles. Start with a steady rate and try the faster cycle at a phrase ending.
Retrigger gives stabs a repeatable accent. Free running follows its own cycle.
Inspect the breakpoints for shape `0` and use `lfo.phase` to place the opening
accent. Try a fast opening and slower close. If brightness moves but the rhythm
still feels flat, try a shallow second route to Voice Amp (destination `15`).

If detuned bass loses weight on some notes, isolate the sub and mid layer. Try
a centered single-voice sine sub and trim overlapping lows from the moving part.
Audition a held note, repeated stabs and the full phrase at matched loudness.
Listen with kick and vocal for accent timing, note gaps, sub weight and masking.
Use `audio_analyze` on matching renders for headroom and low-band balance.
Compare mono with stereo if the audio provider keeps both channels. Report
which listening checks the available audio supports.
