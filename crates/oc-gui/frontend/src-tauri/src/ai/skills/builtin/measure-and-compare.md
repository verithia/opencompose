---
id: measure-and-compare
title: Measure and compare
when: You changed a mix or a sound and need evidence that it moved in the intended direction.
category: analysis
version: 10
tools: [render_range, audition_clip_source, audition_range, audio_analyze, audio_compare, analyze_notes, tracks_list]
---
Pick one musical decision; keep the render window, rate, track subset and tail allowance identical.
For a keyed bus, use render_range with bus and omit tracks so its detector keeps running.
Hear that saved bus path with audition_clip_source before comparing the complete mix.
Use distinct absolute WAV paths in an existing folder for `before` and `after`.

1. render_range({path: before, start_bar: 9, end_bar: 17})
2. audio_analyze({path: before}) to retain the baseline.
3. Make one bounded edit and render the same window to `after`.
4. audio_compare({reference: before, candidate: after})

Before editing, name a specific audible fault and its time, the suspected cause and the expected change; report uncertainty if no fault is identifiable.
Read authored notes directly. analyze_notes estimates external pitch and timing; it is not authoritative MIDI and its default search starts at 65 Hz.

## Listening decisions

For preset A/B, render the same MIDI notes in the same register, with identical
velocity, timing, duration and controller data. Keep mixer and routing fixed.
Include a held note and a repeated figure. Leave enough silence after the held
note for both releases to settle before judging the next attack.
Use audio_analyze with level_window at matching file offsets for raw attack and body RMS and peak. Use
audio_compare with listen: true, listening_only: true and 0.4–8 s for timbre A/B.
Use reference_start and candidate_start in file seconds to locate corresponding passages.
Require listening.status = matched and check delta_lu before judging the pair.
For unmatched or unmeasured playback, report the limit. Raw level evidence
comes from the original renders. The listening copies apply their own gains.
For a blind pass, record a verdict on neutral matched A/B clips before revealing their identity.
Check delivery rate and channels before judging air or width; mono low-rate audio cannot establish either.
For timing, hear the original timing from a common start before using aligned scores.

## Reading the numbers

Peak/true peak indicate headroom. Floating-point overs risk downstream clipping.
RMS, crest and band energy describe level and spectrum, with multiple causes.
For masking, hear the tracks together, then isolate and compare their attacks and octave bands.
Check correlation alongside an audible mono fold-down and a stereo pass.
DTW, alignment, MFCC and LSD measure specific features. Check the original timing
as well as aligned scores. Match lengths/rates before comparing.
Nearby scores do not prove inaudibility; better scores cannot certify musical quality, vocal dryness or authenticity.
Keep or revert the edit from the matched audible result, then check the complete phrase; distinguish that verdict from measurements and delivery limits.
Use audition-and-listen for delivery checks. Never claim to hear measured-only audio.
