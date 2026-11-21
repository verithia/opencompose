---
id: select-samples-by-role
title: Choose samples for a musical role
when: Finding, auditioning and choosing one-shots, loops or textures that fit a part instead of collecting arbitrary samples.
category: production
version: 1
tools: [read_skill, project_get, content_libraries, sample_search, sample_pull, project_extract, edit_subtree, audio_clips_list, audition_clip_source, audition_range, audio_analyze]
---
Define the missing role before searching: short kick body, dry hat texture,
breathy vocal answer, sustained string layer, or a top loop without a kick.
Record target tempo, register, density, tail length and available space.
Inspect audio_clips_list and content_libraries for authorized local candidates before another download.
sample_search lists the user's own matching files first (provider local);
prefer one, and sample_pull its local: id to copy it into the project.

## Search for properties, then listen
Use sample_search with a specific query, appropriate kind and small limit.
Check provider availability. Preserve returned provider and id for sample_pull;
do not fabricate a URL or assume a page-only provider offers an API download.
Try a small shortlist with contrasting source properties. The label and key
are hypotheses: a drum loop may contain a pitched bass or hidden reverb.
Hear attack, body, tail, background noise and variation at the intended rate.

Pull only a plausible candidate into a saved project. Inspect the returned
quality and licence. Keep its attribution sidecar. A preview is useful for
selection but does not become a full-quality original by upsampling it.
For licence_notice, obtain the user's acceptance before accept_licence=true;
otherwise choose material with suitable terms or use an authorized local file.
Place the returned media_path on a separate track, list its clip_index and
use audition_clip_source. Compare that source with its proposed arrangement.

## Choose what earns its place
Keep the rest of the phrase fixed while swapping candidates at matched level.
For kicks compare attack, low-body duration and interaction with bass.
For top loops check swing and whether claps or pitched lows duplicate our part.
For melodic samples hear the actual root, chord motion and recording space.
Prefer another candidate when heavy repair would erase its useful character.
Choose one and state the role it fills, its audible compromise and its credit.

Stop searching once a candidate works in the phrase. Retain the source and
chosen edits; use fit-loops-with-pitch-and-time or assemble-a-sampled-drum-kit
for placement. Search breadth and download count are not production quality.
