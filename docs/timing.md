# Tempo, meter and swing

The project clock converts written positions to seconds. Three declarations feed
it: the header `tempo:` and `time_signature:`, the `tempo_map:` and
`time_sig_map:` change points, and `swing:`. All of them are authored in the
project file; the interpreter reads them and never rewrites the notes.

## Base values

```ocp
@project "Clock"
  tempo: 120
  time_signature: 4/4
```

`tempo` is quarter notes per minute. `time_signature` sets the meter of bar 1
and every bar after it until a change point says otherwise.

## Change points

```ocp
@project "Clock"
  tempo: 120
  time_signature: 4/4
  tempo_map:
    9:1 = 90 bpm
    17:1 = 132 bpm
  time_sig_map:
    25:1 = 3/4
```

A `tempo_map` point is `bar:beat = bpm`, one per line under the key, and the
`bpm` suffix is optional. A `time_sig_map` point is
`bar:beat = numerator/denominator`. Commas do not separate points; every point
starts on its own line at a deeper indentation than the key. Both maps are
absolute positions, both must be given in written order with unique positions,
and a point at `1:1` replaces the header value instead of layering on top of it.

Rules the loader enforces:

- A meter change must start a bar (`beat` 1, subdivision 0).
- Tempo changes are steps: the tempo holds until the next point. There are no
  ramps; a gradual change is a series of points.
- A map with an ambiguous or out-of-order position is rejected at parse time
  rather than repaired or silently sorted.
- A `tempo_map` point may carry a residual offset inside its subdivision
  (`offset_quarters` in the JSON export) when a change lands between grid
  positions.

Meter changes affect bar arithmetic, not note spelling. A beat is the meter's
denominator note (a quarter in 4/4, an eighth in 6/8), and a subdivision divides
that beat into four. Note durations and `start_beat` stay in quarter notes, so a
half note is two quarter notes in every meter.

## What the maps drive

- **Rendering**: positions are converted to seconds through the maps, so a tempo
  change shortens or lengthens everything after it.
- **Arrangement**: section spans are bar positions, so a meter change changes how
  many bars a section occupies and how long the render is.
- **MIDI export**: `tempo_map` is written as tempo events and `time_sig_map` as
  time-signature events, so the exported file keeps the mapped clock.
- **Plugin hosting**: hosts that ask for musical time receive the mapped
  positions. A project with a tempo map plays through the AU path; the VST3 path
  is driven by the transport, not by re-reading the file.

## Swing

`swing: 55%` delays the off-beat note of the chosen grid, an eighth by default.
The percentage is not a ratio of note lengths; it is the position of the
off-beat inside its pair, expressed as a percentage of the beat:

```
delay_quarters = (percentage / 100 - 0.5) * grid_scale
```

`grid_scale` is 1 on the eighth grid and 0.5 on the sixteenth grid, so the
percentages in the table below describe the default eighth grid:

| `swing:` | delay (quarter notes) | feel |
|----------|-----------------------|------|
| 50 % | 0 | straight (the default when no value is written) |
| 55 % | +0.05 | a slight push, 25 ms at 120 BPM |
| 66.67 % | +0.1667 | exact 2:1 triplet swing |
| 100 % | +0.5 | the off-beat lands on the next beat |

`swing_subdivision: 8th|16th` picks the grid swing moves; absent means `8th`.
On the eighth grid a written position `q` moves when `2q` is an odd integer, and
the delay is `(swing/100 - 0.5)` quarters. On the sixteenth grid the position
moves when `4q` is odd and the delay is half that, so 55 % at 120 BPM moves a
sixteenth by 12.5 ms instead of an eighth's 25 ms. Triplets are on neither grid
and stay where they are written. `grid_offset_beats` remains the way to place a
note between grids (up to ±0.125 quarter notes); that offset is part of the
written position, so swing leaves it where it is. A swung note keeps its written duration; the note-off
moves by the same offset as the note-on, so the articulation does not change.

Set project swing for the default groove. A named pattern can override it for
each of its placements. Phrases, drum blocks and patterns without an override
inherit the project value. With no project value, playback is straight (50%).
Clear the Swing field in the Pattern panel to restore inheritance. Enter
`0` to move an off-beat to the preceding beat. The Swing grid select in the
same panel chooses between the eighth and sixteenth grids for the whole
project; it is the same value as `swing_subdivision`.

```ocp
@project "Swung"
  tempo: 120
  swing: 55%

@pattern hats | instrument: drums/acoustic_kit, swing: 66.67%
  hits:
    hihat 1:0 f
    hihat 1:2 f
```

Swing composes with tempo and meter changes in the quarter-note domain. It
moves notes, drum hits and written sustain marks. Track automation, audio
clips and bus or master gain automation keep their written positions.

Within a placed pattern clip, an onset shifted before the clip starts lands at
the start. An onset shifted to the end or beyond is omitted. A loop still allows
notes to ring across internal repeat seams, but its final gate ends within the
placement. When a placement leaves the pedal down, playback adds a release no
earlier than its last press and last note-off.

MIDI export writes the swung note and hit positions at the selected PPQ. The
export includes explicit hit timing offsets on top of swing.
