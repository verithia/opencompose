---
id: align-and-stretch-audio
title: Align imported audio and stems
when: Imported stems drift, long recordings have mixed sample rates, or a clip needs time stretch or transposition.
category: editing
version: 9
tools: [audio_clip_set_placement, import_aligned_stems, audio_clips_list, audio_clip_transients, audio_clip_set_playback, audio_clip_edit_range, project_get, project_extract, project_set_tempo, tempo_map_set, edit_subtree, project_validate, audition_range]
---
Find the file origin and tempo. For synchronized stems, keep their lengths
and leading silence. Audition them against the original before editing.
For an editable project or tracker loop, note meter, tempo changes and
downbeats. Export same-origin WAVs with pickups and optional clock MIDI.
Stage files in handoff/ beside a saved empty project, then use
import_aligned_stems with those relative paths and the project_get revision.
Use the prints if MIDI omits tracker timing.

Use audio_clip_transients in early, middle and late source windows of up to
30 seconds. Check the same event across stems by ear. Measure source seconds
including leading silence. Sample rates do not change the unit. A constant
error suggests placement or trim. Use audio_clip_set_placement with signed
nudge_ms to move whole clips without trimming attacks. Growing error suggests rate/tempo
mismatch. Variable drift may need a tempo map or separately aligned regions.

Read audio_clips_list for the clip index and revision. Call
audio_clip_set_playback with that index, expected_revision and a playback
object containing the changed fields. Pitch and formant are in semitones.
Set stretch_to to stretch and resize together. Warp markers replace the
full list with increasing source_seconds and target_seconds pairs. Both
times precede clip offset. Disable tempo_follow to warp. Explicit stretch
sets the tail ratio. An empty marker list clears the anchors.

Use percussive mode for attacks and musical mode for sustained sounds.
Explicit stretch is output length divided by input length. Explicit tempo_follow
fits the whole file to the range and clears trim/loop; do not also supply stretch.
For an equal-length phrase, source BPM / target BPM gives 130 / 150 ≈ 0.867.

Offset is in output time after stretch. Compare bypassed and processed
versions in the same placement, then restore processing. Compare equivalent
events when a timing edit changes their positions. Use the same mapping for
stems with a shared origin. Otherwise map matching events to the same project
positions. Check measured offsets within detector resolution, then listen at
the start, end and joins for flams and stereo changes. Report measurements
and listening separately. If disk space is tight, use shorter musical
regions and audition the seams. Trimming or splitting a tempo-follow clip
freezes its current ratio.
