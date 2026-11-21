---
id: shape-vocal-phonemes-and-prosody
title: Shape vocal phonemes and prosody
when: A sampled or rendered vocal has swallowed consonants, flat phrasing, awkward breaths, or a vowel that misses its intended beat.
category: production
version: 1
tools: [read_skill, audio_clips_list, audition_clip_source, audio_clip_edit_range, audio_clip_set_playback, audio_clip_set_placement, project_extract, edit_subtree, project_validate, render_range, audio_compare, audition_range]
---
Keep the authorized dry source and an unchanged phrase render. State the words,
intended stress, note targets and which breath should lead into the phrase.
A phoneme, detected transient and musical note need not share a boundary.
In the GUI, select the audio clip and inspect source offset, fades and playback
processing. With tools, use audio_clips_list and retain its current revision.

## Separate onset, vowel and exit
Hear the consonant, stable vowel and release in source and song context.
Land the vowel where the groove needs it; its consonant may start earlier.
Retain enough onset for intelligibility. A click-prevention fade can still erase
an S, T or breath, so compare the complete word rather than the seam alone.
Use audio_clip_edit_range only for supported splits/trims; re-list after edits.
Draw local gain or fades through a verified subtree instead of inventing a
phoneme-gain tool. Assign one useful breath to a stack when others obscure it.

## Change one performance dimension
Use sparse warp anchors to move a vowel while protecting surrounding events.
Their clocks are source-media seconds to processed-media seconds, not song time.
Use placement for moving the whole gesture. Read fit-loops-with-pitch-and-time
before changing trim, stretch or tempo follow; those can change source ownership.
Compare independent pitch with formant preservation before intentional formant
shift. A fixed clip shift does not supply note drift, vibrato or transition curves.
Preserve an expressive scoop when it fits the harmony; do not tune noisy
consonants by guessing from a spectral peak or forcing every segment to a scale.
For an external singer render, verify lyrics, melisma, pronunciation and actual
voice-specific controls. Keep the aligned dry bounce and editable source state.
Native clip tools do not create lyrics, voicebank phonemes or independent singers.

## Hear the phrase and its role
Render identical before/after windows containing the preceding breath and exit.
Use a matched audio_compare listening pair for local differences, then hear the
phrase with drums and harmony through audition_range. Check at final tempo.
For a choir pad, compare hummed versus open-vowel sources and choose stable
vowels without pretending a copied lead is an independently performed ensemble.
Keep only changes that improve stress and timing without a lisp, doubled inhale,
robotic transition or altered word. Reopen and check the retained source/print.
