---
id: separated-vocal-mixing
title: Mix a separated vocal
when: A separated or already mixed vocal sounds wet, distant, masked or uneven; or you are adding vocal effects.
category: mixing
version: 9
tools: [project_extract, effects_set_bypass, tracks_list, devices_catalog, audio_clips_list, audio_clip_set_playback, audition_clip_source, render_range, audition_range, audio_analyze, audio_compare, analyze_notes]
---
Check alignment, inserts, sends and master with tracks_list and project_extract.
Keep the raw stem. Compare the same words and gap in audition_clip_source,
an isolated render with added effects bypassed, and one with effects on.
In an approved working copy, record the bypass states, use effects_set_bypass
on the inserts and returns under test, then restore their original states.
Keep clip playback fixed. A tail in the raw stem predates the mixer. It can
be room sound, a printed effect, bleed or separation residue.

Measure a steady vowel and two equal windows early and late in the gap with
audio_analyze level_window. Use each WAV's file-relative times and the raw
absolute path if known. Report each gap level.rms_dbfs relative to its vowel.
Repeat on a second gap. If the raw path is unavailable, report raw listening
only. A dry independent vocal is a control for attacks and vowel stability.

Source separation can leave reverb. Dedicated dereverb requires a restoration plug-in.
Check devices_catalog. Fade gaps gently and check breaths and word ends.
Compression can expose tails. For targeted S/SH control, read control-vocal-sibilance.

## Verify a repair or formant edit
Change one thing at a time. Get the revision with audio_clips_list and use
audio_clip_set_playback in musical mode. Hold pitch, timing and preservation
fixed. Try ±3 semitones before wider settings. With preservation enabled,
keep formant minus pitch within ±12 semitones.

Use render_range with float_wav: true for the same track, interval, rate
and mixer. Check pre_encode peak_dbfs and true_peak_dbfs. If either reaches
full scale, reduce both by the same output gain and render again. Use
analyze_notes at matching file-relative times and confidence. Match notes
by overlapping start_s and end_s. Compare midi and cents together, counting
unmatched and octave-ambiguous notes separately.

Match two or three consonant attacks with audio_analyze transients within
resolution_seconds. Report missing or doubled attacks and compare their RMS
against a vowel. Use audio_compare with listen: true, listening_only: true on a 0.4–8 s
window. Check listening.status is matched and |listening.delta_lu| < 0.1.
Listen for softened consonants, unstable vowels and watery tails in solo and
in the mix. Report pitch evidence separately. If versions are indistinguishable
or playback is unavailable, listening acceptance remains pending. Check air
and upper sibilance on the full-rate WAV when model audio is 16 kHz.
