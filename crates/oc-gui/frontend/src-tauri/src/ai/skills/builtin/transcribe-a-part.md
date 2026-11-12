---
id: transcribe-a-part
title: Transcribe external audio
when: External audio or a stem has notes you need written down, and they are not in the project DSL.
category: analysis
version: 7
tools: [audition_range, analyze_notes, render_range, edit_subtree, separate_stems]
---
If the notes are already in the project DSL in your context, read them and stop.
Use `audition_range` on an imported clip to hear the lead and its rhythm, and
`analyze_notes` when the external part is unclear or to check a pitch.
Measure once, then write: after one full reading of the take, place a first pass
before listening again. Refine window by window instead of measuring the whole
part before writing any of it.
Find the pulse and downbeat before placing notes. Verify the tempo across two
anchors with several bars between them. Fix a constant offset at the clip start
before treating drift as tempo or stretching a take.
If the part is buried, try `separate_stems` and audition the isolated voice.
Keep aligned stems at their original lengths and check for leakage and artifacts.

`analyze_notes({path: "/tmp/chorus.wav", start: 48, end: 64})` reads the rendered WAV, slices `[start, end)` and reports the detector's notes; times are relative to `start`, so 0.02 s means 48.02 s in the song. `min_confidence` (0-1) and `max_notes` keep a long part readable.
`tempo` and `time_signature` fix the beat grid; without them it comes from the render's project, then from `project`, then from the engine default (120 BPM 4/4), with a warning that names the assumption.

| field | meaning |
| --- | --- |
| start_s, end_s | note boundaries, relative to the window start; `confidence` is the detector's own score, 0-1 |
| midi, name, cents | equal-tempered pitch, A4 = 69; ±50 cents is the next semitone |
| start_beat | quarter-note beats from the window start, 0-based; `bar` is its 1-based 1/16 position |
| duration_beats | measured length in quarter-note beats |
| suggested_duration | nearest DSL token: w, h, q, 8th, 16th, 32nd, 64th, dotted (`q.`) or triplet (`qt`) |
| polyphonic, warnings | a chord's other voices are not transcribed; the grid and every guess are named here |

## Writing it down

`@<beats>` is the position from the phrase start and every member after a comma shares it:

    @phrase transcribed [1:1:0 - 5:1:0]
      @0.0 G4 q mf, G5 q mf

Derive the duration token from `suggested_duration` — do not invent a token or convert seconds yourself. `duration_beats` is the measurement; `quantize: true` also returns `start_beat_quantised` and `duration_beats_quantised` snapped to 1/16, while the default keeps the raw beats.
Keep the raw timing until you confirm whether the offset is intentional.

## What it cannot do

It is a single-voice detector: a chord reports the strongest voice, sets `polyphonic` and warns that the other voices were not transcribed. Drums, pitch bends and ornaments under about 50 ms are not notes; when `warnings` says a note is off the 1/16 grid, its bar and token are the nearest reading — check it by ear.
For harmony, check bass and top note separately and mark any inner voice you
cannot verify. Recheck an `octave_ambiguous` note where the target voice sounds alone.
Empty `notes` with a warning means silence or a window past the file end; a refusal names the problem, such as `end` not after `start`. Fix it, then audition what you wrote.
