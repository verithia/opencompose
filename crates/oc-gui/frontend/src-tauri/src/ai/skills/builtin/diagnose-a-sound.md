---
id: diagnose-a-sound
title: Diagnose a sound and correct the cause
when: A lead, arp, kick, vocal or sample sounds wrong and repeated parameter changes are not improving it.
category: analysis
version: 1
tools: [read_skill, project_get, project_extract, devices_catalog, audio_clips_list, audition_clip_source, render_range, audition_range, audio_compare, audio_analyze]
---
State the intended role and one audible failure in a named passage.
Describe behavior: attack too sharp, low note disappears, pulse arrives late,
words smear, or the release masks the next note. A style adjective alone
does not identify a control. Read the current preset, notes and signal path.

## Locate the cause before treating it
Hear the full phrase, the suspect alone, and the suspect with its competitor.
For a sample, compare audition_clip_source with the arrangement audition.
Check register, note lengths, velocity and sample choice before adding effects.
Source is wrong: change the source or patch. Source is right but edits smear
it: inspect stretch, offset and envelopes. Solo is right but mix is wrong:
test overlap, levels and routing. A loudness difference can mimic all three.

## Make a falsifiable edit
Save one before render with its revision, range and routing. Predict what
one control family should change and what should remain audible. Query
devices_catalog for the actual control, unit, range and enum before editing.
If attack is missing, test a shorter source fade or less fast compression;
if the whole source lacks attack, select another source instead of boosting hiss.
For a pulse that is late, check retrigger and phase before increasing depth.
For lost bass notes, check tuning, note length and mono summing before gain.

Render the same passage after the edit. Use measure-and-compare for matched
listening; keep source gain evidence separate from the listening copies.
Listen for the predicted cue and a side effect, including the following note.
If the cue does not improve, restore the before state and test the next cause.
Do not accumulate an unrelated chain to conceal an unverified source problem.

For existing retained renders, a four-second stereo listening pair is:
```json
{"reference":"/project/before.wav","candidate":"/project/after.wav","duration":4,"listen":true,"listening_only":true,"channel":"stereo","clip_sample_rate":48000}
```
Replace both paths; inspect delivered matching and provider channel support.

## Finish the decision
Keep the simpler treatment when neither change is clearly better in context.
Record passage, symptom, hypothesis, changed controls, heard outcome and limit.
If inline audio was absent or unsupported, state listening pending; metrics
can support a diagnosis but cannot establish that the instrument sounds right.
