---
id: edit-hyperpop-vocals
title: Edit hyperpop and digicore vocals
when: Shape a tuned hyperpop or digicore lead, abrupt formant contrast, or glitchcore vocal stutters while keeping a hook readable.
category: production
version: 5
tools: [project_extract, edit_subtree, project_validate, audition_clip_source, audition_range, audio_compare, audio_clips_list, audio_clip_edit_range, audio_clip_set_playback, analyze_notes]
---
Preserve an untreated lead. Choose a clear hook fragment and decide which
syllable should stay intelligible when the supporting edits get dense.
Place the main line before adding doubles, octave shifts or stutters.

## Establish a stable pitch reference
For tuning, confirm the intended notes and place note targets only where
the performance calls for them. Use analyze_notes on an external
monophonic source as evidence and check octave ambiguities by ear.
Use audio_clip_edit_range to split around the intended syllable when needed.
Refresh audio_clips_list after splitting. Set the chop's pitch in semitones
with audio_clip_set_playback and enable preserve_formants for approximate
vocal character preservation. For a tone-only variant, hold pitch steady and
try formant: +3 or -3 semitones in musical mode. With formant preservation
enabled, keep formant minus pitch within ±12 semitones.

Duplicate the short region on a separate track for a timbre switch.
Keep its start and end aligned to the musical phrase. Use a stepped
pitch change on one isolated chop instead of shifting the shared lead.
For glitchcore stutters, compare a sparse and a dense repeat on one vowel
and leave the consonant or following word exposed. Add tiny edge fades if
the boundary clicks, then check the stutter still feels sharp.

## Compare the whole hook
Use audition_clip_source with the chop's index, source_start_seconds, and
duration_seconds to hear the original syllable and neighbouring consonants.
Use audition_range in project time to hear the edited chop. Raw and processed
levels can differ. For a comparison with the same mixer and placement, toggle
bypass_processing, audition again, then restore processing.
For larger shifts, save both audition_range passes to WAV at the same
project interval and use audio_compare with listen: true. Verify the playback
match and read pre_encode on each pass for headroom. Formant edits can
raise peaks even when the average level falls. If a pass exceeds full scale,
lower the level before listening. Check vowels and consonants in the pair.
Hear the lead and chops separately, then together with the beat.
If the stutters hide the lyric, remove a repeat before increasing the level.
