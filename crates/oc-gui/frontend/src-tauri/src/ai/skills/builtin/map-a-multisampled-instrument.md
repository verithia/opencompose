---
id: map-a-multisampled-instrument
title: Map and check a multisampled instrument
when: Build or troubleshoot a melodic sampler with key zones, velocity layers, repeat variants, and sustained sample loops.
category: production
version: 3
tools: [devices_catalog, project_extract, presets_add, presets_set_params, render_compat_report, analyze_notes, audio_analyze, audio_compare, audition_range, project_validate]
---
For printing a source read resample-a-playable-instrument; for drum pads read assemble-a-sampled-drum-kit.

## Establish what each key receives
Copy samples into project media and record each sample's root, dynamic and take.
Compare several stable-body windows, excluding a falling attack. analyze_notes starts at 65 Hz; confirm a deeper sub's octave against a tuned reference, using spectral peaks only as clues.
An assigned root of 60 does not prove the file sounds C4. Inspect other pitch offsets before calibrating a new map for concert-pitch notes.
For an existing phrase, preserve the total note-minus-root-plus-transpose relationship; correcting the root alone can break an already compensated patch.
Separate a pitched instrument from a bank whose keys select different syllables or slices.
Check the lowest, highest and repeated keys used by the actual phrase against the receiving zones.
For recordings rooted at MIDI 60 and 67, try adjacent ranges 57–63 and 64–70.
Keep each root fixed while moving its zone boundary. Loop a note on either side of the split and move the split if the transition sounds like a different instrument.
Within each zone, try soft and strong regions at 1–79 and 80–127 velocity.
Balance their recorded gain and step across the boundary on one note before extending the map.

## Load the map and preserve its source regions
Use builtin.sampler with a project-relative sampler.sfz_file or sampler.sfz_text.
For example, start a soft region with:
```text
<region> sample=media/c4-soft.wav lokey=57 hikey=63 pitch_keycenter=60 lovel=1 hivel=79
```
Use offset and end to select a sample region; these and loop points use sample frames.
Do not substitute an audio clip's processed-time offset for a raw sample-frame boundary.
For a slice bank, keep receiving keys and source regions stable while editing the melody.
Test note-off: one_shot ignores it, whereas loop_sustain leaves its loop on release.
For a bidirectional texture, try loop_pingpong and check both turnarounds and the release envelope.
Hold a note longer than the source, then repeat it with a short gap to expose seams or stale voices.
Test two different keys close together: a one-shot tail can keep sounding across the next bass pitch.
For a single-note role, compare a gated source with short ampeg_release against the retained one-shot.
SFZ group/off_by chokes immediately; check clicks and jointly triggered layers before using it as a mono solution.

## Check dynamics and saved behavior
Use seq_position and seq_length for repeat variants, then interleave another mapped key.
The melodic sampler has one global sequence; do not promise independent repeat banks per pad.
For independent percussion repeat banks, use a drum kit. Read compatibility diagnostics for unsupported opcodes.
Keyswitches, release-trigger samples and velocity-layer crossfades are not established by ordinary key/velocity zones.
Compare the recorded root with its untransposed source and verify note-off behavior.
Audition a scale across every zone boundary and a velocity staircase at fixed pitch.
Then hear the actual hook with its neighboring parts; audible pitch and articulation matter more than a map label.
If it fails after reopening, check project-relative assets, receiving regions and compatibility diagnostics before changing musical settings.
