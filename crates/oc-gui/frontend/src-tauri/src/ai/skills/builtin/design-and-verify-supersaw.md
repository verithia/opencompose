---
id: design-and-verify-supersaw
title: Design and verify a supersaw
when: A supersaw or wide saw lead/pad is requested, or a named synth part has the wrong actual sound source.
category: production
version: 3
tools: [devices_catalog, presets_list, project_extract, presets_add, presets_duplicate, presets_set_params, tracks_list, render_range, audition_range, audio_analyze]
---
Inspect the preset source and parameters. A strings SoundFont keeps playing
samples after renaming. Use source `builtin` and plugin `builtin.open_synth`
for a native stack. Preserve a baseline when revising an existing patch.

## Build the saw stack
`osc.a.waveform=1` selects Saw. Query `devices_catalog` for `builtin.open_synth`
with `osc.a.`, `amp.`, `filter.` and `voice.` prefixes to check units and values.
Unison copies use `osc.a.unison` (1–16), `osc.a.detune` (cents) and `osc.a.width`
(0–1). The `unison.*` aliases apply to A. B and C have their own controls.
`voice.polyphony` caps simultaneous notes in a polyphonic patch.

Start dry with one saw, explicit A/B/C levels, and headroom at `output.gain`.
Compare one copy with 5–7 modestly detuned copies at matched loudness. Tune
amp attack and release for the part. Check a held note for slow beating and
a repeated chord for clear attacks and gaps.
If a single stack blurs the hook, keep one tighter layer for pitch and attack.
Try a quieter, wider saw on B for motion. Use a separate track and preset if
the wide layer needs a slower attack and its own envelope.
For multiple chord tracks, try complementary registers or remove duplicated
low notes before adding effects. Keep the center clear for the vocal and kick.

## Verify the phrase
Compare the same short window solo and in the arrangement. Check pitch focus,
the chord's upper note, vocal masking and release overlap. Compare the mono
downmix with stereo at matched loudness before increasing width again. If the
stack thins in mono, reduce width or detune and retest before adding layers.
Use `audio_analyze` for headroom and stereo correlation as supporting evidence.

Request `audition_range` with `channel: stereo` and a higher `clip_sample_rate`
when the provider preserves them. A 16 kHz mono audition cannot certify
stereo width or high-frequency detail. If delivery fails, describe the patch
configuration and report listening as pending.
