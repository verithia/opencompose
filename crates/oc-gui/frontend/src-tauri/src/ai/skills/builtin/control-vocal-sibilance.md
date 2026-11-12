---
id: control-vocal-sibilance
title: Control vocal sibilance without dulling the voice
when: A lead or vocal stack has sharp S or SH sounds, a de-esser causes a lisp, or compression and brightening expose harsh syllables.
category: mixing
version: 1
tools: [tracks_list, project_extract, devices_catalog, effects_add, effects_set_param, effects_set_bypass, render_range, audio_analyze, audio_compare, audition_range]
---
Keep the original vocal. Mark several offending words and a clean vowel; check
whether sharpness is in the source, a later brightening insert or a return.
Use the same phrase and routing for every comparison. De-essing does not remove
printed reverb or repair a watery separation artifact; read separated-vocal-mixing.
In the GUI, add De-esser in the vocal channel's mixer inserts and open its controls.
For tools, find the target and zero-based insert index using tracks_list and
project_extract. Use devices_catalog for current parameters, ranges and enum choices.
The UI device is de-esser; effects_add uses its de_esser DSL id. Preserve bypass
states and inspect the chain again after adding an insert.

Start with one detector band that catches the heard S or SH, not a fixed frequency
for every singer. With Auto, high_frequency is the sidechain high-pass corner and
low_frequency is its low-pass corner. Do not confuse these with an audible EQ.
A fixed sibilance_band or band_override supersedes those corners. For a first
controlled pass keep band_estimation off, band_override at zero and settings fixed.

Use effects_set_param with the discovered target and index. Compare band_processing
values split_band and wide_band separately. Split reduces the upper band; Wide
reduces the whole signal when triggered. Set threshold and range conservatively,
then tune attack and release while checking the start and end of several words.
Range changes both the gain-law slope and ceiling, not a guaranteed reduction.
If phrase levels vary, compare threshold_mode relative with absolute in a separate
pass; do not change threshold policy, band and timing simultaneously.
Save bypassed, Split and Wide render_range WAVs for the same phrase, track subset
and mixer gain. Keep pre_encode peaks below full scale. Check whether lookahead
or split processing shifts the audio. Mark matching S/SH and clean-vowel windows
in file seconds; use audio_analyze level_window with equal durations at those
events in each version. Record each window's raw RMS change.
Full-band RMS can flag collateral vowel loss but cannot isolate sibilance.

Use audio_compare with listen: true on matching 0.4–8 second passages; account
for latency with reference_start and candidate_start and verify the listening match.
Compare consonants, vowel body and recovery into the
next word, first solo and then against the backing with audition_range. Check a
quiet line as well as the chorus. A softer S is not an improvement if it becomes
a lisp, swallows a breath or pumps the vowel. Raise the threshold, reduce range,
retune the band or keep the bypassed version when those tradeoffs remain.
Restore the chosen bypass state. Report listening and measurements separately.
If listening or full-rate upper-band checking is unavailable, leave that acceptance
pending rather than inferring it from RMS alone.
