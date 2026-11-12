---
id: measure-and-compare
title: Measure and compare
when: You changed a mix or a sound and need evidence that it moved in the intended direction.
category: analysis
version: 8
tools: [render_range, audition_range, audio_analyze, audio_compare, analyze_notes, tracks_list]
---
Pick one musical decision, such as whether the bass leaves room for the kick.
Keep the render window, sample rate, track subset and tail allowance identical.
Use distinct absolute WAV paths in an existing folder for `before` and `after`.

1. render_range({path: before, start_bar: 9, end_bar: 17})
2. audio_analyze({path: before}) to retain the baseline.
3. Make one bounded edit and render the same window to `after`.
4. audio_compare({reference: before, candidate: after})

For note questions, listen first. Read authored notes directly. Use analyze_notes
for external audio when needed. It estimates pitch and timing and is not authoritative MIDI.

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
For a blind pass, have someone present neutral matched A/B clips. Record your
verdict before unmasking, repeat in reverse order, and check the edit in the mix.
For timing, hear the original timing from a common start before using aligned scores.

## Reading the numbers

Peak/true peak indicate headroom. Floating-point overs risk downstream clipping.
RMS, crest and band energy describe level and spectrum, with multiple causes.
For masking, hear the tracks together, then isolate and compare their attacks and octave bands.
Check correlation alongside an audible mono fold-down and a stereo pass.
DTW, alignment, MFCC and LSD measure specific features. Check the original timing
as well as aligned scores. Match lengths/rates before comparing.
Nearby scores do not prove inaudibility. Better scores cannot certify musical
quality, vocal dryness, or authenticity. State observations and limits separately.
Use audition-and-listen for delivery checks. Never claim to hear measured-only audio.
