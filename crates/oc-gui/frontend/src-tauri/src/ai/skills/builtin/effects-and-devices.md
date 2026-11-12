---
id: effects-and-devices
title: Effects and devices
when: Adding an EQ, compressor or any other device to a track, bus or master; or when an effects block fails to parse.
category: mixing
version: 6
tools: [project_extract, edit_subtree, project_validate, devices_catalog, effects_add, effects_set_param]
---
`expected effect_entry`, `expected effect_type`, `or effect_entry`, or `unknown effect type:` — check device syntax and catalog IDs.
`effects:` holds one device per line on a track, bus or master. Entries run in
signal order from the first insert to the last, before the channel fader.

    effects:
      - eq: low_gain=6dB, mid_freq=800Hz, mid_gain=-3dB, high_gain=-6dB

Start with `devices_catalog`. Use the returned `dsl_id` in DSL, such as
`de_esser` for UI id `de-esser`. Read parameter names, ranges and units there.
Catalog unit `fraction` means a physical number: compression/multiband `mix=0.4` is 40% wet.
Native `%` controls such as reverb mix use `mix=40%`. Static compression/multiband
`mix=40%` also means 0.4; existing `mix=0.4%` means 0.004. Effect automation and
synth patch percentages have context-specific legacy behavior: preserve existing
literals and use bare physical numbers for new fractional writes.
Each entry begins with `- `. Keep all comma-separated `name=value` pairs on
that line. A continued or nested parameter list may fail with
`expected EOI, top_level_block, track_content, or effect_entry`.
`invalid typed effect parameter` points to the value. `has no parameter` points
to the key. `effects_add` and `effects_set_param` offer structured edits.
`eq` is a low shelf, mid bell and high shelf. Use the dedicated cuts on
`dynamic_eq` to audition a frequency region before making a corrective EQ move.
```ocp
@track reference | instrument: audio/clip
  effects:
    - dynamic_eq: high_cut_freq=250Hz, high_cut_enabled=1

@track reference_top | instrument: audio/clip
  effects:
    - dynamic_eq: low_cut_freq=300Hz, low_cut_enabled=1
```

Choose the smallest edit that addresses the heard problem. If a part disappears
in mono, check for a competing frequency range or cancellation, then return to
stereo to confirm balance. A useful solo EQ setting can differ from the mix setting.
For parallel space, send to a fully wet effect on a bus routed to master. The send
controls new signal entering the effect; the return fader controls the whole tail.
Key a compressor after reverb from the vocal if words need to stay forward; compare
at similar playback level and check recovery between phrases. For a baked-in tail,
read `separated-vocal-mixing`.
