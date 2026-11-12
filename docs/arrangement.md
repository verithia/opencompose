# Sections and the rendered arrangement

`@section` declares a named span of the arrangement. When sections are present
they define the rendered timeline: spans are played in written order and
`repeat:` plays a span more than once. This is the arrangement model the DSL
already declares (`Section { time_range, repeat, label, flags }`); no new
syntax is introduced.

```ocp
@section verse [9:1 - 17:1]
  repeat: 2
  label: "Verse"
  flags: pizzicato
```

A loop region is playback state, not arrangement data. It wraps the written
timeline the streaming transport plays, and arming it changes neither the
expansion below nor the rendered length. See
[Markers and the loop region](manual/markers.md).

## Semantics

- `repeat: N` means the section plays `N` times (N = 1 when omitted; 0 is
  rejected by the validator).
- Spans are concatenated from the first section's written start. Rendered
  position of a play = first-section start + sum of all earlier played span
  lengths.
- A content container is assigned to the section that contains its **start**
  position. Every play of that section duplicates and shifts the container.
  Where a play's end breaks the written timeline, the container is cut there
  (see [Limits](#limits)).
- Content that starts before the first section, in a gap between sections, or
  after the last section is not rendered. Gaps are removed, because the
  arrangement is the concatenation of the played spans.
- Sections must be ordered and must not overlap; the validator rejects
  otherwise.
- `flags:` names one region technique: `pizzicato` (alias `pizz`), `arco`,
  `muted` (alias `con_sord`), `sul_ponticello` (alias `sul_pont`) or `sul_tasto`.
  It applies to notes whose **onset** falls inside the rendered section. A
  note's own `articulation=` wins. The flag reaches the instrument through its
  existing articulation profile (keyswitch, CC or parameter), so a repeated
  section carries the technique on every play. Drum hits and audio clips are
  unaffected.
- When the sections already cover the written timeline with `repeat` 1 (the
  common "one section per part" case) the expansion is an identity map and the
  authored project is used unchanged.

The rendered length is `first_section_start + Σ(span_length × repeat)` in
quarter notes. It is used as the minimum length of offline renders, so trailing
rest inside a section is preserved even when the last event is earlier. Render
duration still includes the instrument tail of the last event when that is
longer. A length reported in seconds is read on the tempo map the expansion
produces, so a repeat that holds or shifts a tempo change reports what it plays.

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

Track effects, presets and mixer state are not time-positioned and are shared by
every play as they are by every clip today.

## Limits

### Played-span cut

Content is cut per play, and only where the rendered timeline stops following
the written one.

- **Cut** at a repeat seam (the next play restarts the same written span) and at
  a removed gap (the next play starts later in written time). Phrase notes,
  clip-pattern notes and drum hits that start before that end and ring past it
  are released there with an explicit `gate_quarters`; anything that starts at
  or after the end is not played in that play.
- **No cut** where the next play continues the written timeline: sections that
  tile it, and the last play of a repeated section handing over to the next
  written section. A note written across that boundary rings in full. At the end
  of the arrangement there is no following span to bleed into, so the final play
  is not cut either; its tail decays into the render tail after the last event.
- **Audio clips** are placements, not notes. A clip whose range crosses a cut is
  truncated at the cut. The exception is a `tempo_follow` clip with a stretch
  mode on: its range defines the media time mapping, so trimming the range would
  re-time the media instead of cutting the placement; it keeps its authored
  range.

Why: a play is the unit that ordering and repetition duplicate, so the same
written content must sound the same in every play. Content that sounds past its
span makes a play depend on what follows it — a repeated verse inherits the
previous verse's tail — which the section model otherwise removes. Where written
time continues, there is nothing to fix and the authored duration stands. The cut
is a note-off, not a mute: the instrument's release, the track's sends and the
bus/master FX tails keep decaying, so a reverb tail is not truncated by it.

- Tempo and time-signature maps repeat with the section that holds their points, so
  every play returns to the tempo and meter the section was written with.
  `key_map` is a written-time map for vocal tuning and MIDI export, and it does
  not repeat.
- Sections that change the arrangement cannot be combined with `performance:`
  tracks, whose timeline is absolute samples.
- There is no skip/mute syntax for a section; only repetition.
- Audio-clip trims are relative to the clip, so a repeated clip replays the same
  media window; the played-span cut clamps the placement, never the window.
- Automation **clips** are containers, so a repeated section duplicates the clip
  rather than its points: every play gets its own span and its own shifted
  points, and a play that is cut clamps the span at the cut. Outside a span the
  lane writes nothing, so the seam is a jump back to the clip's opening value
  instead of a ramp between two plays. A **whole-timeline** lane has no span and
  keeps the older behaviour: its points are shifted and duplicated into one
  lane, so it interpolates across a seam.
- Gain-automation clips are shifted, not cut. Their points keep the values they
  were written with.

## Mapped time seen by plugins

Hosted instruments and effects receive one clock context per `process` call,
offline and in streaming playback. The renderer splits a block at the next
authored tempo or meter change (and, while a map is active, at the 128-frame
source quantum), so no call straddles a change and a split never drops or
repeats a frame. A change exactly on a block boundary starts the next call; a
change on the first frame of a block applies to that call.

The values handed to a plugin use one convention:

- **PPQ** is quarter notes since the start of the written timeline. It is
  continuous across tempo changes, so a plugin never sees it jump: the frame
  where a change lands belongs to the new tempo, and the PPQ accumulated before
  it stays as integrated.
- **Beat** is the meter's denominator note. In 4/4 one beat is a quarter note;
  in 6/8 it is an eighth note. Musical position queries derive beat boundaries
  and the measure downbeat from the meter in force.
- **Bar number** is zero-indexed on the host side (a plugin sees the first bar
  as 0) and counts continuously across meter changes. A meter change always
  starts a bar, so bars never merge or restart at the change.
- **Measure downbeat** is the quarter-note position of the current bar's first
  beat; it moves to the authored change point when a new meter starts.

The same snapshots feed CLAP transport events, VST3 process context and AUv2
host callbacks, so a tempo-synced delay, LFO or arpeggiator computes the same
time in every hosted format and in offline versus streaming playback.

## Playback and the GUI

Offline export, cached playback and streaming all render the expanded
arrangement. The editor timeline still shows the written sections and written
positions; a repeated section therefore plays longer than the written timeline
and seeks are resolved against the written clock. The playhead and seek targets
are not mapped through the expansion.
