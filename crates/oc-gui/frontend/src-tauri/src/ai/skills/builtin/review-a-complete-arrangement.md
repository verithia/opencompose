---
id: review-a-complete-arrangement
title: Review a complete arrangement by listening
when: Finishing a whole song after isolated sound-design edits, or checking that the final render was fully delivered and heard.
category: analysis
version: 1
tools: [read_skill, project_get, project_extract, render_range, audition_range, audition_clip_source, audio_analyze, audio_compare]
---
Read the current project and section map. Mark the intended opening,
entrances, energy changes, climax and ending. Review the actual saved revision,
not a collection of successful auditions from older revisions.
Choose the end from musical intent, contributing sources and tails; a distant control clip alone is not a song ending.

## Cover the whole musical arc
Use audition_range in sequential bounded windows through the end. Confirm
each delivered start/end, duration and truncated flag; request any missing
remainder. Join the ledger by actual delivered ranges, not requested ranges.
Use overlap at section boundaries to hear pickups, releases and entrances.
When a section is copied, verify its automation, resets and source windows as well as its notes.
If the audio limit shortens a window, shorten the next request rather than
assuming the model received the complete final render.

Do one pass for phrase development and one for recurring technical faults
where needed. Ask whether the lead develops, the bass supports the harmony,
the drum energy changes and the drop earns its entrance. More layers and
greater loudness do not establish a stronger chorus.

## Correct what the pass actually reveals
Keep a short issue ledger with passage, audible symptom and next action.
Use diagnose-a-sound for the highest-impact issue, then compare that passage
before and after and hear its neighboring section. Avoid rewriting the whole
arrangement to fix a local transient or a single mismatched sample.
After a substantial global mix change, repeat coverage of affected sections.

Check the final file or retained render path as well as the editable project.
Include leading silence, intended ending and effect tails. Match the delivered
revision and path to the one reported. A render finishing is not a listening
pass. If audio reception fails, report which ranges remain unreviewed.

Review stereo/high-frequency detail only at a supported rate and channel mode;
16 kHz mono cannot certify it. Measure clipping/headroom in the full-rate file.
Report what changed, which final ranges were heard and what remains uncertain.
