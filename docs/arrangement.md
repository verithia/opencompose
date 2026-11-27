# Sections and the rendered arrangement

`@section` declares a named span of the arrangement. When a project has
sections, they define the rendered timeline: the spans play in written order,
and `repeat:` plays a span more than once. The model is the DSL's existing
`Section { time_range, repeat, label, flags }`, with no extra syntax.

```ocp
@section verse [9:1 - 17:1]
  repeat: 2
  label: "Verse"
  flags: pizzicato
```

A loop region belongs to playback state and is not part of the arrangement. It
wraps the written timeline that the streaming transport plays. Arming it changes
neither the expansion described below nor the rendered length. See
[Markers and the loop region](manual/markers.md).

## Semantics

- `repeat: N` plays the section `N` times. Without `repeat:` N is 1. The
  validator rejects 0.
- Spans are joined end to end from the first section's written start. A play
  starts at the first section's start plus the lengths of all earlier plays.
- A content container belongs to the section that contains its start position.
  Each play of that section copies the container and shifts it. Where a play's
  end breaks the written timeline, the container is cut there (see
  [Limits](#limits)).
- Content that starts before the first section, in a gap between sections or
  after the last section is not rendered. The arrangement is the sequence of
  played spans, so gaps disappear.
- Sections must be in order and must not overlap. The validator rejects any
  other layout.
- `flags:` names one region technique: `pizzicato` (alias `pizz`), `arco`,
  `muted` (alias `con_sord`), `sul_ponticello` (alias `sul_pont`) or
  `sul_tasto`. It applies to notes whose onset falls inside the rendered
  section, and a note's own `articulation=` takes precedence. The flag reaches
  the instrument through the instrument's articulation profile (keyswitch, CC or
  parameter), so every play of a repeated section carries the technique. Drum
  hits and audio clips ignore it.
- When the sections already cover the written timeline with `repeat` 1, the
  usual one-section-per-part layout, the expansion maps every position to
  itself and the project renders as written.

The rendered length, in quarter notes, is
`first_section_start + Σ(span_length × repeat)`. Offline renders use it as their
minimum length, so a rest at the end of a section survives even when the last
event comes earlier. If the last event's instrument tail is longer, the render
includes the tail. A length reported in seconds is read on the tempo map that
the expansion produces, so a repeat that holds or shifts a tempo change reports
the duration it plays.

## What moves

The expansion covers every timed container:

| Container | Assignment | Shift |
|-----------|------------|-------|
| phrase | `time_range.start`, or 0 when absent | `time_range` (or `start_beat` when absent) |
| clip | `time_range.start` | `time_range` |
| inline track pattern | written 0 | first play stays inline; later plays become clips on generated top-level patterns |
| drum `@pattern` | `time_range.start`, or 0 | `time_range.start` |
| track automation, whole timeline | each point's position | each point, sorted after expansion |
| bus/master effect automation, whole timeline | each point's position | each point, sorted after expansion |
| automation clip (lane with a span) | `time_range.start` | span and points per play (end clamped at a cut) |
| audio clip | `time_range.start` | `time_range` (`offset_seconds` unchanged; end clamped at a cut) |
| bus/master gain automation | clip start | range and points, sorted after expansion |
| track gain automation | clip start | range and points (a hairpin expands to one of these before a repeat) |
| tempo map point | written position | each play moves the point, so the rendered tempo restarts with the section |
| meter map point | written position | each play moves the point, and a play must start on a bar |

Track effects, presets and mixer state have no timeline position. Every play
shares them, in the same way every clip does.

## Limits

### Played-span cut

Content is cut per play, and only where the rendered timeline stops following
the written one.

- A cut happens at a repeat seam, where the next play restarts the same written
  span, and at a removed gap, where the next play starts later in written time.
  Phrase notes, clip-pattern notes and drum hits that start before the cut and
  would ring past it are released at the cut with an explicit `gate_quarters`.
  Anything that starts at or after the cut is not played in that play.
- No cut happens where the next play continues the written timeline: between
  sections that tile it, and where the last play of a repeated section hands
  over to the next written section. A note written across that boundary rings
  in full. The final play of the arrangement is not cut either, because no span
  follows it; its tail decays into the render tail after the last event.
- An audio clip is a placement. A clip whose range crosses a cut is truncated at
  the cut. The exception is a `tempo_follow` clip with a stretch mode on: its
  range defines how media time maps to timeline time, so trimming the range
  would re-time the media. That clip keeps its authored range.

The reason for the cut: a play is the unit that ordering and repetition copy, so
the same written content must sound the same in every play. If content sounded
past its span, a play would depend on what follows it, and a repeated verse
would inherit the previous verse's tail. Where written time continues there is
nothing to correct, and the authored duration stands. The cut is a note-off and
does not mute anything. The instrument's release, the track's sends and the
bus and master effect tails keep decaying, so the cut does not truncate a reverb
tail.

Other limits:

- Tempo and time-signature maps repeat with the section that holds their
  points, so every play returns to the tempo and meter the section was written
  with. `key_map` is a written-time map for vocal tuning and MIDI export, and it
  does not repeat.
- Sections that change the arrangement cannot be combined with `performance:`
  tracks, whose timeline is in absolute samples.
- A section can be repeated but has no skip or mute syntax.
- Audio-clip trims are relative to the clip, so a repeated clip replays the same
  media window. The played-span cut clamps the placement and leaves the window
  alone.
- An automation clip is a container, so a repeated section copies the clip:
  every play gets its own span and its own shifted points, and a play that is
  cut clamps the span at the cut. Outside its span the lane writes nothing, so
  at a seam the value jumps back to the clip's opening value instead of ramping
  between two plays. A whole-timeline lane has no span. Its points are shifted
  and copied into one lane, so it interpolates across a seam.
- Gain-automation clips are shifted and never cut. Their points keep their
  written values.

## Mapped time seen by plugins

Hosted instruments and effects receive one clock context per `process` call,
offline and in streaming playback. The renderer splits a block at the next
authored tempo or meter change. While a map is active it also splits
instruments at the 128-frame source quantum, and effects at every 1024th frame
of the timeline, so an effect sees the same calls in an offline pass as in
streaming chunks. No call spans a change, and a split never drops or repeats a
frame. A change exactly on a block boundary starts the next call. A change on
the first frame of a block applies to that call.

The values passed to a plugin follow one convention:

- PPQ is quarter notes since the start of the written timeline. It is
  continuous across tempo changes, so a plugin never sees it jump: the frame
  where a change lands belongs to the new tempo, and the PPQ accumulated before
  it stays as integrated.
- A beat is the meter's denominator note. In 4/4 one beat is a quarter note; in
  6/8 it is an eighth note. Musical position queries derive beat boundaries and
  the measure downbeat from the meter in force.
- The bar number is zero-based on the host side (a plugin sees the first bar as
  0) and counts on across meter changes. A meter change always starts a bar, so
  bars never merge or restart at the change.
- The measure downbeat is the quarter-note position of the current bar's first
  beat. When a new meter starts, it moves to the authored change point.

The same snapshots feed CLAP transport events, the VST3 process context and
AUv2 host callbacks. A tempo-synced delay, LFO or arpeggiator therefore computes
the same time in every hosted format, offline and in streaming playback.

## Playback and the GUI

Offline export, cached playback and streaming all render the expanded
arrangement. The editor timeline shows the written sections at their written
positions. A repeated section therefore plays longer than the written timeline,
and seeks resolve against the written clock. The playhead and seek targets are
not mapped through the expansion.
