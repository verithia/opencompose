# Tempo, meter and swing

The project clock converts written positions to seconds. Three kinds of
declaration feed it: the header values `tempo:` and `time_signature:`, the
change points in `tempo_map:` and `time_sig_map:`, and `swing:`. You write all
of them in the project file. The interpreter reads them and never rewrites the
notes.

## Base values

```ocp
@project "Clock"
  tempo: 120
  time_signature: 4/4
```

`tempo` is in quarter notes per minute. `time_signature` sets the meter of
bar 1 and of every later bar until a change point sets another one.

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

A `tempo_map` point is `bar:beat = bpm`, and the `bpm` suffix is optional. A
`time_sig_map` point is `bar:beat = numerator/denominator`. Each point goes on
its own line, indented deeper than its key; commas do not separate points. Both
maps use absolute positions. List the points in timeline order, with no
position used twice. A point at `1:1` replaces the header value.

The loader enforces these rules:

- A meter change must start a bar (beat 1, subdivision 0).
- Tempo changes are steps: a tempo holds until the next point. There are no
  ramps. Write a gradual change as a series of points.
- A map with an ambiguous or out-of-order position is rejected when the file is
  parsed. The loader neither repairs nor sorts it.
- When a tempo change falls between grid positions, its `tempo_map` point can
  carry a residual offset inside its subdivision (`offset_quarters` in the JSON
  export).

A meter change affects bar arithmetic only. A beat is the meter's denominator
note (a quarter in 4/4, an eighth in 6/8), and a subdivision is a quarter of
that beat. Note durations and `start_beat` stay in quarter notes, so a half
note is two quarter notes in every meter.

## What the maps drive

- Rendering converts positions to seconds through the maps, so a tempo change
  shortens or lengthens everything after it.
- Section spans in the arrangement are bar positions. A meter change therefore
  changes how many bars a section occupies and how long the render is.
- MIDI export writes `tempo_map` as tempo events and `time_sig_map` as
  time-signature events, so the exported file keeps the mapped clock.
- Hosted plugins that ask for musical time receive the mapped positions. A
  project with a tempo map plays through the AU path. The VST3 path takes its
  positions from the transport and does not read the file again.

## Swing

`swing: 55%` delays the off-beat note of the chosen grid, which is the eighth
note unless you choose another. The percentage gives the position of the
off-beat inside its pair of grid steps, as a percentage of the pair:

```text
delay_quarters = (percentage / 100 - 0.5) * grid_scale
```

`grid_scale` is 1 on the eighth grid and 0.5 on the sixteenth grid. The table
shows the default eighth grid:

| `swing:` | Delay (quarter notes) | Feel |
|----------|-----------------------|------|
| 50 % | 0 | straight (the default when no value is written) |
| 55 % | +0.05 | a slight push, 25 ms at 120 BPM |
| 66.67 % | +0.1667 | exact 2:1 triplet swing |
| 100 % | +0.5 | the off-beat lands on the next beat |

`swing_subdivision: 8th|16th` selects the grid that swing moves. Without it,
the grid is `8th`.

On the eighth grid, a note at written position `q` moves when `2q` is an odd
integer, and the delay is `(swing/100 - 0.5)` quarters. On the sixteenth grid,
the note moves when `4q` is odd, and the delay is half as long. At 120 BPM,
55 % therefore moves an eighth by 25 ms and a sixteenth by 12.5 ms. Triplets lie
on neither grid and stay where they are written.

To place a note between grid positions, use `grid_offset_beats` (up to ±0.125
quarter notes). That offset is part of the written position, and swing does not
change it. A swung note keeps its written duration: the note-off moves by the
same amount as the note-on, so the articulation stays the same.

Set the project swing for the default groove. A named pattern can override it
for all of its placements. Phrases, drum blocks and patterns without an
override inherit the project value. With no project value, playback is straight
(50%).

In the **Clips** panel, clear a pattern's **Swing** field to make it inherit
the project value again. Enter `0` to move an off-beat onto the preceding beat.
The **Swing grid** select in the same panel chooses the eighth or sixteenth
grid for the whole project; it sets `swing_subdivision`.

```ocp
@project "Swung"
  tempo: 120
  swing: 55%

@pattern hats | instrument: drums/acoustic_kit, swing: 66.67%
  hits:
    hihat 1:0 f
    hihat 1:2 f
```

Swing combines with tempo and meter changes in the quarter-note domain. It
moves notes, drum hits and written sustain marks. Track automation, audio clips
and bus or master gain automation keep their written positions.

Inside a placed pattern clip, an onset that swing moves before the clip start
lands on the start. An onset moved to the clip end or beyond is dropped. In a
looped clip, notes can still ring across the internal repeat seams, but the
final gate ends inside the placement. When a placement leaves the sustain pedal
down, playback adds a pedal release no earlier than the last press and the last
note-off.

MIDI export writes the swung positions of notes and hits at the selected PPQ.
Explicit hit timing offsets are added on top of swing.
