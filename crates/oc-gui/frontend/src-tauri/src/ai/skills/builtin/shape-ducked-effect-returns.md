---
id: shape-ducked-effect-returns
title: Shape effect returns around a lead phrase
when: Reverb or echoes should bloom between words or lead notes while staying behind the next attack, without weakening the dry part.
category: mixing
version: 1
tools: [read_skill, tracks_list, project_extract, buses_add, buses_routes, tracks_send_add, devices_catalog, effects_add, effects_set_param, effects_set_bypass, edit_subtree, project_validate, render_range, audition_clip_source, audition_range, audio_compare]
---
Hear the source without new effects. Identify space already printed into it or produced by its instrument.
Choose whether to reduce that existing space or add a new room; adding a ducked return cannot remove printed reverb.
Keep the dry lead's balance fixed while comparing wet-path changes.

## Give the return its own envelope
Read route-groups-and-returns and inspect the current signal path before adding another send.
Keep the dry route and create a fully wet reverb or delay return.
Place the ducking compressor after the wet effect so it controls the audible tail, not only the effect input.
Compression before reverb changes its excitation; compare a window after new input stops to distinguish that from wet-tail ducking.
Select an actual audio key from the lead when words should clear their own space.
Use the kick as key only when the intended movement follows the drum pulse.
A silent MIDI trigger needs a compatible shaping receiver; naming it as an audio key is not equivalent.
Inspect devices_catalog and set the device sidechain in the effect definition through edit_subtree.
OpenCompose mixer audio keys are post-insert but pre-fader/pre-mute: a muted audio ghost can remain a key.
Verify the source and insert signal; MIDI-triggered shapers and normalized control followers have different contracts.

## Shape recovery in the gap
Start with gentle reduction and hear a phrase with both a held note and a short gap.
Adjust attack until the new wet onset stops covering the dry consonant or lead transient.
Set recovery so the tail blooms in the chosen gap and settles before the next entry.
If it breathes at every small articulation, reduce depth or ease recovery before shortening the source phrase.
Change the send to control what enters the next tail; change the return level to control tails already ringing.
For a phrase-end throw, increase only that phrase's feed and write its reset explicitly.
Keep the return open long enough to finish the intended decay.

## Observe the wet path without breaking its detector
Use render_range with bus: "plate" for a bus named plate, omitting tracks.
The full graph keeps running during that bus tap; a track-subset render can remove the detector.
Use audition_clip_source with the exact returned WAV path to hear the isolated saved return.
Compare bypassed and active ducking over the same window at matched listening level.
Then hear the dry lead plus return and the complete arrangement with audition_range.
Check a quiet phrase, a dense entry and the last tail; quieter wet audio alone is not proof of better phrasing.
Validate, retain the chosen key and chain order, and describe the audible tradeoff that remains.
