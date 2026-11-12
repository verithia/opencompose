---
id: tune-and-align-vocal-doubles
title: Tune and align vocal doubles
when: Correct sung pitch, preserve slides and vibrato, align vocal doubles, or give a double a distinct vowel colour.
category: editing
version: 4
tools: [project_extract, edit_subtree, project_validate, analyze_notes, audition_clip_source, audition_range, audio_compare, audio_clips_list, audio_clip_set_playback]
---
Start with an approved lead comp and a separate double. Keep an untreated
copy. Listen dry at the same bars, then align obvious offsets at a shared
word onset. Let vowel ends differ slightly when the blend benefits.

Confirm the key and intended notes. Use analyze_notes on isolated audio
when pitch is uncertain. A project-level @vocal_tune targets one clip.
Use snap: off for selected note corrections. Use the analyzed clip onset
for at=, and keep start= and length= edits within max_move. Lengthen
retune or bypass a note to retain its scoop or attack. Preserve vibrato
and leave the note tuner's formant neutral on the first pass.

For a different vowel colour, use audio_clips_list for the revision.
Call audio_clip_set_playback in musical mode and try formant: -1 or +1
semitone with pitch and timing unchanged. If transposing deliberately, use
preserve_formants before deciding on additional colour.

Choose a short phrase with a stressed consonant and sustained vowel. Use
audition_clip_source in source seconds for a dry control. For separated
stems, compare an untreated vocal recording when available to account for
attacks softened by separation.

Save two audition_range WAVs using the same project interval, isolated
track, pitch and mixer. Change only formant or preserve_formants and
refresh the revision. Compare saved paths using audio_compare with listen:
true and a shared 0.4–8 s duration. Verify the playback match. Listen for
consonant attack, sibilance and vowel colour. Check sustained notes still
land at their intended pitch. Confirm ambiguous note readings by ear.

Read pre_encode peak and true peak on each audition_range result.
Set float_wav: true to keep an overrange full-rate WAV for inspection.
The inline listening copy can still clamp. Formant correction can raise
peaks even when average level falls. If a pass exceeds full scale, lower
the level and render both sides again before judging the edit.

Compare the lead alone, the double and both in the chorus. Listen for
flams, wavering vowels or lost lyric clarity. Validate the chosen edit.
