# The .ocp file

An OpenCompose project is a UTF-8 text file with the extension `.ocp`. The
grammar lives in `crates/oc-dsl/src/grammar.pest`. This chapter describes what
you write; the grammar file describes how it is parsed.

## File shape

```ocp
@ocp "0.6.0"                 // optional version line

@project "Song title"        // required, properties follow
  tempo: 120
  time_signature: 4/4
  key: C major

// then @preset, @pattern, @track, @bus, @section, @master in any order
```

A comment starts with `//` and runs to the end of the line. Indentation is
cosmetic: the `@` keywords give the file its structure.

You can write the blocks in any order. A `@pattern` or a `@section` may follow
a `@track`, and a `@bus` may come before the tracks that route into it. Each
block is contiguous, though. The next project-level `@` keyword ends the block
before it, so a track's content has to stay inside its own `@track` block. If a
line that belongs to a track appears after another block, the parser refuses it
and names it.

An inline drum pattern (`@pattern name [range] xN` followed by hit lines) is
track content and goes inside a `@track`. A `@pattern` at the project level is a
named definition, which you place on a track with `clip:`.

Names are bare identifiers or quoted strings. Track, bus and preset names are
the routing identity.

The version line is optional, and new projects write `0.6.0`. Files written for
`0.2.0` load unchanged. Each later version introduces one construct: `0.3.0`
brings `@vocal_tune`, `0.4.0` its automatic `snap`, `reference`, `cap` and
`window` fields, `0.5.0` note timing, and `0.6.0` `@vocal_harmony`.

## Time and positions

A position is `bar:beat:subdivision`. Bars and beats count from 1, and
subdivision `0` is on the beat. A range is `[start - end]` with an exclusive
end, so `[1:1 - 5:1]` covers bars 1 to 4.

```
[1:1]        bar 1, beat 1
[9:4]        bar 9, beat 4 (not the end of bar 9)
[10:1]       the end of bar 9
[1:1:0]      explicit subdivision 0 (on the beat)
```

Durations are `w`, `h`, `q`, `8th`, `16th`, `32nd` and `64th`. You can dot them
(`q.`, `q..`), make them tuplets (`qt`, `q5:4`, `q7:8`) and tie them (`q~h`,
`C4 q~h~q`). A rest takes the same durations: `rest q`, `rest 2w`.

A phrase or pattern range must cover all of its content, rests included. A
four-bar phrase in 4/4 is `[1:1 - 5:1]`; `[1:1 - 4:4]` stops one beat short.

## Project properties

| Property | Example | Meaning |
|---|---|---|
| `tempo:` | `tempo: 120` | BPM |
| `time_signature:` | `time_signature: 4/4` | meter |
| `picture_clock:` | `picture_clock: {rate: "30000/1001", origin: "01:00:00;00"}` | optional picture frame rate and the label at project zero |
| `key:` | `key: Bb minor` | key signature (`major`/`minor` or `maj`/`min`) |
| `swing:` | `swing: 55%` | project swing; 50 % is straight, absent is straight |
| `swing_subdivision:` | `8th` or `16th` | grid swing moves; absent is `8th` |
| `tempo_map:` | `tempo_map: 9:1 = 90` | tempo change at a written position (also editable in the timeline tempo lane) |
| `time_sig_map:` | `time_sig_map: 17:1 = 3/4` | meter change at a bar start (also editable in the timeline tempo lane) |
| `key_map:` | `key_map: 17:1 = Db major` | key change |
| `mpe:` | `mpe: {lower: 2-7, bend_range: 48}` | MPE zone layout |
| `export:` | `export: {container: wav, wav: {depth: i24, dither: true}}` | export container, its per-container options and the sample rate for exports; the older flat `export: {format: i24, dither: true}` still loads (see [export format](../export-format.md)) |
| `solo_mode:` | `solo_mode: upstream` | how a solo propagates: `both` (the default) keeps the whole graph neighbourhood, `upstream` keeps only the sources that feed the soloed channel |
| `live_resample:` | `live_resample: 3` | resampling quality rung for the streaming preview: `0` fastest to `7` best |
| `automation_normalization:` | `automation_normalization: 2` | stored synth automation table: supported values are 1 and 2; absent means version 1, and unsupported versions are refused before rendering |

The map properties take one entry per line in the same block. `export:` also
accepts `resample: 0…7`, the rung the offline render uses, and one
per-container block: `wav: {depth, dither}`, `flac: {depth, level}` or
`mp3: {bitrate, vbr_quality}`.

## Sections

`@section` names a span of the arrangement. Once a project has sections, they
define the rendered timeline: they play in written order, and `repeat:` plays a
span again. Content belongs to the section that contains its start. Gaps
between sections are removed, and content outside every section does not
render.

```ocp
@section verse [9:1 - 17:1]
  repeat: 2
  label: "Verse"
  flags: pizzicato
```

`repeat:` is at least 1. Sections must be in order and must not overlap.

`flags:` selects one region technique for the note onsets inside the section:
`pizzicato`/`pizz`, `arco`, `muted`/`con_sord`, `sul_ponticello`/`sul_pont` or
`sul_tasto`. A note's own `articulation=` takes precedence. The flag drives the
same articulation profile as a per-note mark, so the section's keyswitch, CC or
parameter is sent before each note that starts in it. See
[Sections and the rendered arrangement](../arrangement.md).

## Markers

`@marker` names a point on the timeline. The name is a string or a bare
identifier, and the position uses the usual `bar:beat:subdivision` form.

```ocp
@marker "Verse 2" at 9:1

@marker outro at 41:1:2
```

Markers are annotations and never change what renders. They are saved with the
project in their written order, and they may sit past the last audio. A project
with no markers writes no marker lines. See
[Markers and the loop region](markers.md).

## Patterns and clips

A top-level `@pattern` is a named, reusable block of content. A `clip:` line
places it on a track at a timeline position, and you can place one pattern as
many times as you like.

```ocp
@pattern keys_verse | instrument: piano/acoustic_grand, preset: keys, color: "#4fc3f7"
  events:
    C4 q mp
    E4 q mp
    G4 h mf

@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 9:1]
```

The inline properties after `|` are `instrument:`, `preset:`, `transpose:`
(semitones), `swing:` (per-pattern swing, which overrides the project's),
`bars:` (the pattern's length in bars) and `color:`.

`bars:` bounds where the piano roll and the drum lane place new content. If you
draw past the last bar, the new content lands on the last editable step inside
the pattern. Without `bars:`, the pattern ends at the end of the last bar its
written content occupies. When you shorten a pattern you can keep or trim the
notes past the new end. Kept notes stay in the document, and the roll marks
them as out of range.

### Transposition

`transpose:` moves the pattern's pitched `events:` by whole semitones before the
instrument plays them. With `-2`, a written C4 sounds as Bb3. The field accepts
`-127` to `127`. A value outside that range is refused with a message that
names the range; it is never read as zero.

The shift applies on every path that plays the pattern: offline render,
streaming playback, MIDI export, analysis and range render. The rendered audio
and the exported MIDI therefore match the value in the panel and the pattern
preview.

The shift changes the note number, so it adds to whatever the instrument does
to every note. On the built-in OpenSynth, a preset's `tuning.transpose` still
applies on top: a pattern at `-2` on a preset at `+12` sounds ten semitones
above the written pitch. The pattern's own `preset:` and `instrument:` choose
what plays it (the track's preset when the pattern names none). They do not
replace the transposition.

`transpose:` does not move `hits:`. A hit addresses a sample or a `@kit` pad by
name, so transposing it would play a different drum. A drum pattern sounds the
same with or without the value, and the render reports that the value was
ignored for the hits.

### Pattern bodies and inline content

A pattern body holds only `events:`, `hits:` or both. `automation:`, `mix:`,
`effects:` and `engine:` are not pattern fields. A pattern that carries one of
them fails to parse with a diagnostic, so the field is never silently dropped.
A pattern is content placed on a track, and the effect chain, mixer state,
automation and engine selection belong to that track.

You can also write content directly in a track:

- `@phrase name [range]` with a note list places notes inline.
- `@pattern name [range] xN` with a hit list places a drum pattern inline.

```ocp
@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 3:1] x2
    kick  1:0 ff
    snare 2:0 f
```

### Slip and loop

Two optional clip properties choose which part of the pattern a clip plays and
whether it repeats. Neither moves the clip:

```ocp
@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 5:1] offset: 1q loop: 2q
```

`offset:` hides that many quarter notes from the head of the pattern. The clip
keeps its start, end and length, so the phrase shifts against the beat while
the block stays put. The hidden head is silent: a note that starts inside it
never plays, and pedal marks there are dropped. You cannot slip a clip that
already shows the whole pattern.

`loop:` repeats that many quarter notes of content, starting at the offset, for
the full length of the clip. The loop must be shorter than the clip and must end
inside the pattern's content. The last repeat stops at the clip end, and a note
that runs past the end is cut there. The timeline draws a dashed seam at each
repeat.

A clip that uses neither property writes neither field.

### Clip groups

`@group` binds clips together: one drag moves the whole set and one click
selects it. A group lists its members by clip identifier, and a clip takes an
identifier with `id:` on its own line:

```ocp
@track keys | instrument: piano/acoustic_grand
  clip: keys_verse [1:1 - 9:1] id: c1
  clip: keys_verse [9:1 - 17:1] id: c2

@group g1 {
  members: c1, c2
}
```

A group holds at least two clips. Every member must name a clip that carries
that identifier, and identifiers are unique across the project. An identifier
exists only while a group names its clip. Deleting or splitting a clip drops it
from every group, and a group left with fewer than two members is removed
together with the identifiers it held. A project without groups contains no
group lines.

Groups affect editing only. They never change what renders.

## Notes

A note line is an optional `@N` followed by one or more events separated by
commas:

```ocp
C4 q mp
F#5 8th f articulation=staccato v=110
Bb3 w mf expression={dynamics: mp->mf}
@grace Db5 16th f
@0 Bb3 h mf                          // absolute beat position 0
@0 C4 q mf, E4 q mf, G4 q mf         // three events at beat 0
@2 [C4 E4 G4] q mf, Bb3 q mf         // chord token as one member
```

Every member of a comma list sounds at the line's position and keeps its own
duration, dynamic and modifiers. `@0.0 G4 q mf, G5 h mp articulation=legato` is
one short note and one long note starting together. A line without `@` takes
the current sequential position. The line advances that position once, by the
length of its longest member.

A comma list stays on one physical line and cannot repeat a pitch. The parser
refuses a leading comma, a trailing comma, a member without a duration and a
comma across a line break, each with its own message.

`[C4 E4 G4] q mf` is shorthand for the comma line `C4 q mf, E4 q mf, G4 q mf`:
the bracketed pitches share one duration, dynamic and modifier set. The
serializer writes the comma form, so a chord token saves back as
`@0 C4 q mf, E4 q mf, G4 q mf`. Simultaneous notes that share every property
except pitch collapse into one comma list on save. Notes that differ in
duration, dynamics, velocity or articulation stay on separate lines.

| Part | Values |
|---|---|
| pitch | `A`–`G` with `#`, `##`, `b`, `bb`, then octave `-1` or a digit `0`–`9`: `C-1`, `C4`, `F#5`, `Bb3` |
| list | note events separated by `,` on one line: `G4 q mf, A4 q mf` |
| chord (sugar) | `[` pitches separated by whitespace `]`: `[C4 E4 G4]`, one shared duration/dynamic/modifier set |
| duration | `w h q 8th 16th 32nd 64th`, dots, tuplets, ties |
| dynamics | `ppp pp p mp mf f ff fff` (optional) |
| `v=` | numeric velocity 0–127, overrides dynamics |
| `@N` | absolute start beat inside the pattern, applied to every member on the line |
| `articulation=` | `legato`, `staccato`, `pizzicato`, `tremolo`, `palm_mute`, `ghost_note`, `sforzando`, … (29 names in the grammar) |
| `text=` | per-note annotation, e.g. lyrics |
| `bend=` | pitch bend in cents |
| `vibrato={rate: 5Hz, depth: 20ct, delay: 0.2s}` | vibrato |
| `expression={dynamics: mp->mf}` | expression curve; any control name with an optional `->` target |
| `ornament=` | `trill`, `mordent`, `fermata`, `arpeggio` (see [Ornaments](../ornaments.md)) |

## Sustain pedal

A pedal mark is a phrase or pattern event with its own position:
`@<beat> pedal down` or `@<beat> pedal up`.

```ocp
@track piano | instrument: piano/acoustic_grand
  @phrase theme [1:1 - 5:1]
    C4 q mf
    @1.0 pedal down
    @2.0 E4 q mf
    @3.0 pedal up
```

`pedal down` holds every note whose note-off falls while the pedal is down, and
`pedal up` releases them. You always write the position, because a pedal spans
notes and sits outside the sequential line model. A mark without `@` is refused
with a message that names the missing position. A pedal mark outside a phrase
or pattern is refused the same way. A `hits:` list does not accept pedal marks,
because a drum hit has no note-off to hold.

## Dynamics and hairpins

A dynamic on a note sets one level. A hairpin moves the level across the notes
it spans. `@<beat> cresc` and `@<beat> dim` are phrase or pattern events with
their own position, like pedal marks.

```ocp
@track strings | instrument: strings/violin, preset: warm_strings
  @phrase swell [1:1 - 5:1]
    @0.0 cresc
    @0 C4 w pp
    @4 C4 w
    @8 C4 w
    @12 C4 w ff
```

The mark ramps to the next dynamic written on a note in the same phrase, or to
the end of the phrase when no later note carries one. A note that restates the
same dynamic ends the span, so a `cresc` over notes that are all marked `pp` is
refused: the level would not move. With no written target, `cresc` runs from
`pp` to `f` and `dim` from `f` to `pp`.

On a built-in synth, a phrase hairpin compiles to a `gain_automation:` clip and
moves only the level. A SoundFont or hosted instrument receives CC11 expression
points across the mark instead, so the instrument's own dynamics follow the
swell. A SoundFont also crossfades its velocity layers while a note in the span
sounds. The layer amplitude is compensated, so CC11 remains the only level
change. A built-in track keeps the gain clip because its synth has no CC11
receiver.

To restore hard layer selection on a SoundFont track, write
`dynamic_layers: off` in the preset's `params:` or in the track's
`engine_params:`. The track value overrides the preset value.

On a built-in track, keep the span clear of other gain clips: a track cannot
hold two of them over the same span, and a hairpin cannot start before the
previous one ends. `ocp fmt` keeps the mark and prints the clip a built-in track
expands it to as a comment.

A mark in a pattern is relative to the pattern's content. Each placement
expands the mark on the track that hosts it, once per loop repetition, so a
pattern played twice writes two ramps. Pattern marks always use the gain-clip
form, even on a hosted track, because a pattern may name its own preset. A mark
hidden by a slip is dropped, and a ramp stops at the end of the clip.

A hairpin is refused in a `hits:` list, outside a phrase or a pattern's
`events:` block, and when no note follows it in its container.

## Glissando

`gliss` on a note slides its pitch to the next note in the phrase as a pitch
curve; it does not trigger new samples.

```ocp
@track lead | instrument: synth/lead_saw, preset: slide_lead
  @phrase slide [1:1 - 3:1]
    A4 w mf gliss
    E5 q mf
```

The modifier has four spellings:

- `gliss` slides to the next note's pitch.
- `gliss=G4` names the destination.
- `gliss=120ms` reaches the next note's pitch after 120 ms and holds it.
- `gliss=run` or `gliss=run_diatonic` writes short notes in place of a curve,
  chromatic or in the project key.

The run forms exist for sampled sources, which cannot slide. A SoundFont or
sample-bank preset is a recording, so a plain `gliss` on it is refused and the
message names the run form that plays. A plugin that accepts pitch bend slides
too. A hosted source that reports no bend support is refused the same way.

A slide follows the project clock. Under a tempo map the curve is measured in
quarter notes, so it stretches and compresses with the tempo. A track that
carries a curve treats every ordinary note at the curve's pitch as a separate
voice, so the same track can also hold ordinary notes at that pitch, each with
its own pitch and release. The engine still refuses a plain event it cannot
assign to a voice (a keyswitch, a control trigger or a drum hit) at a curve's
pitch, because the pitch alone does not say which voice the event addresses.

## Ornaments

`ornament=` turns one written note into the notes, or the gate, a performer
would play. The mark stays in the score and `ocp fmt` keeps it. Playback and
MIDI export expand it into ordinary notes.

```ocp
@project "Ornaments"
  tempo: 120
  key: C major

@track lead | instrument: synth/lead_saw
  @phrase line [1:1 - 3:1]
    C4 q mf ornament=trill
    E4 q mf ornament=mordent
    G4 h mf ornament=fermata
```

| Value | Result |
|---|---|
| `trill` | Alternate the written pitch with the next scale step at the sixteenth-note grid, closing on the written pitch. |
| `mordent` | Play the written pitch, the step above, then hold the written pitch. |
| `fermata` | Hold the written gate for 1.5 times its length. |
| `arpeggio` | Delay each member of one onset by a thirty-second note in ascending pitch order. |

Trills and mordents take their step from the project key. Without a key the
step is one semitone. They cannot share a note with an articulation, bend,
vibrato, expression curve or glissando, because each of those describes a
second pitch or level path. A trill or mordent needs a note to itself. A note
too short for the grid either stays whole or divides its gate among the three
mordent parts. `arpeggio` rolls every note that starts with the marked note and
keeps each written duration.

[Ornaments](../ornaments.md) has the full timing rules and the export
behavior.

## Drum hits

A hit is `drum beat:subdivision dynamics modifiers`. Drum names are free
identifiers (`kick`, `snare`, `hihat`, `ride`, `crash`, …), and what they
trigger depends on the kit or preset. Write an exact velocity as `v=1` to
`v=127` in the same place as a note's velocity. It overrides the written
dynamic: `kick 1:0 mp v=120` plays at 120.

```ocp
hits:
  kick  1:0 ff
  hihat 1:0 mp articulation=closed
  snare 2:0 f
  hihat 2:2 mf articulation=open
```

A hit position is `beat:subdivision` inside the pattern's own bar; it has no
bar number. The beat counts from 1, and the subdivision counts sixteenths of
that beat, `0` to `3`. `1:0` is the downbeat, `1:2` the second sixteenth after
it, and `2:3` the last sixteenth of beat 2.

To place a hit between sixteenths, use `grid_offset_beats`. For example,
`kick 1:1 mf grid_offset_beats=-0.125` puts a hit one 32nd note after the
downbeat. The offset is signed, in quarter notes, and at most 0.125 in either
direction. The offset position decides clip and loop eligibility and is the
input to swing. For an expressive adjustment applied after swing, use
`timing_offset_beats`.

A pattern holds one bar of hits. A `clip:` plays a named `@pattern` once. For a
groove of several bars, write an inline pattern in the track, where `xN`
repeats the written bar: `@pattern groove [1:1 - 9:1] x8` in a track plays that
bar eight times.

The hit articulations are `closed`, `open`, `half_open`, `cross_stick`,
`ghost_note`, `flam`, `roll` and `rim_shot`. `rest 1:2` leaves that position
silent. `|` separates written bars and only aids reading.

## Automation

A track carries one lane per control:

```ocp
automation: expression {1:1=20%, 4:1=50%, 8:1=30%, 12:1=60%}
automation: volume [ppq=480] {1:1=0dB, 3:1=-6dB}
automation: pitch_bend {1:1=0ct, 2:1=50ct}
```

A value is a percentage, dB, number, string or boolean. Points are written at
`bar:beat` positions. `[ppq=N]` switches the lane to tick positions at N ticks
per quarter note.

Give a lane a time span and it becomes a clip:

```ocp
automation: filter.cutoff [3:1 - 5:1] {4:1=200Hz, 4:4=8000Hz}
automation: filter.resonance [3:1 - 5:1] muted {3:1=20%, 5:1=60%}
```

The span is `[start - end]` with an exclusive end, like every other positioned
block. `muted` is a flag without a value, and a muted lane writes nothing.

A clip writes only inside its span. Before the head, the parameter keeps the
value its `effects:` entry declares. The first point holds from the head, the
last value holds to the exclusive end, and nothing is written after the end, so
the parameter stays where the clip left it. A lane without a span drives the
whole timeline from its first point.

Two lanes with the same owner and target cannot write at the same time.
Overlapping clips are refused, and so is a clip together with a whole-timeline
lane. Clips that meet at an edge, such as `[1:1 - 3:1]` and `[3:1 - 5:1]`, are
allowed.

The lane name selects a control the track's instrument understands. `volume`
and `pan` always map to MIDI CC. The other General MIDI controls
(`modulation`, `expression`, `sustain`, `resonance`, `brightness`, `pressure`)
depend on the instrument's control profile. `pitch_bend` is in cents. On a
built-in synth track, validation refuses a name outside that vocabulary and
lists the names that work. On other instruments, a name the profile does not
cover produces no events.

A built-in synth patch parameter is a control lane only in the
`pluginparam_<hex>` form, which spells the parameter name's bytes in hex:
`pluginparam_66696c7465722e6375746f6666` is `filter.cutoff`. Declare
`control_cutoff_param: "filter.cutoff"` in the preset to give that parameter
the readable lane name `cutoff`.

A patch-parameter point is read in the parameter's own unit, the one it uses in
`params:`: hertz for a cutoff, seconds for an envelope time, cents for a
detune. A bare number from 0 to 1 is a normalized position (the value the patch
editor writes), and so is a percentage. A continuous parameter ramps from one
point to the next, so a lane from `200Hz` to `8000Hz` sweeps the filter. A
discrete parameter such as `filter.model` steps at each point.

To automate an effect parameter, name the effect and the parameter. Tracks,
buses and the master all accept these lanes:

```ocp
automation: compression.threshold {1:1=-24dB, 3:1=-6dB}
automation: fx2.Mix [ppq=480] {1:1=50%, 5:1=80%}
```

Address the effect by its type name (`compression`, `eq`, `plugin`, …) when the
chain has exactly one effect of that type, or by its one-based chain position
(`fx2`). Values use the same unit as the parameter in the `effects:` entry.
Built-in effects take a parameter name from the device schema; plugin
parameters use the plugin's own names.

Each point can carry `curve=` (`linear`, `stairs`, `half_sine` or `smooth`) for
the segment to the next point, plus `tension=` for `smooth`. `linear` is the
default, and `stairs` holds a step. A lane can add `smoothing=<time>` (default
0 ms) to run its value through a one-pole low-pass.

A plugin receives each changed value as a parameter change at its sample within
the render block. A built-in device is split at the same positions. Effect
lanes accept the same `[start - end]` span and `muted` flag as control lanes. A
span makes the lane a clip, and an LFO lane then starts its cycle at the clip
head. See [`docs/automation.md`](../automation.md).

Tracks, buses and the master also take gain envelopes:

```ocp
gain_automation: linear [1:1 - 9:1] {1:1=1, 8:1=0.9}
gain_automation: step ppq=480 [10:1 - 12:3] {10:1=0, 11:2=1}
```

Write `linear` or `step`, an optional `ppq=`, a range and then the points.
Values are linear gain, with 1.0 as unity. An owner can hold several
envelopes, but their ranges must be sorted and must not overlap. A hairpin on a
track is the notation form of one of these clips; see
[Dynamics and hairpins](#dynamics-and-hairpins).

## Control sources

You draw an automation lane by hand. A control source derives a control value
from one track's audio, so a fader can follow the music:

```ocp
@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms
```

`from:` names the track whose post-insert, pre-fader audio feeds the detector.
`sidechain(kick)` is the same tap. `attack:` and `release:` are one-pole time
constants: a step input reaches `1 - 1/e` after one constant. The optional
properties are:

| Key | Values | Default |
|---|---|---|
| `lfo:` | `none` or `sine\|triangle\|square\|saw@<rate>Hz`, e.g. `sine@2Hz` | `none` |
| `lfo_depth:` | `0`… any number | 1 |
| `formula:` | small expression over `x`, `none` disables | `none` |
| `scale:` | number | 1 |
| `offset:` | number | 0 |
| `curve:` | `linear`, `squared`, `sqrt`, `log` | `linear` |

The value is computed in this order:

1. The attack/release follower produces an envelope.
2. `scale * envelope + offset` is clamped to `0`…`1`.
3. `curve` is applied.
4. `formula` is applied.
5. `lfo * lfo_depth` is added, and the result is clamped to `0`…`1` again.

The detector takes the larger absolute value of the channels, so the control
follows sample level and does not measure loudness. Use `scale` to bring a hot
source into range.

The formula is a small expression and has no scripting features. It reads one
variable, `x`, and supports `+ - * / ^`, parentheses, unary minus, numbers, and
`sqrt(x)`, `abs(x)` and `clamp01(x)`. The result is clamped to `0`…`1`, and a
non-finite result reads as zero, so `1-x` and `x*x` are safe on every sample.

A track uses the value to ride its fader:

```ocp
@track bass | instrument: synth/bass
  mix: control(kick_env=-12dB)
```

The value in `control(...)` is the depth: the gain change at full control.
Negative values duck and positive values boost. The ride is applied after the
fader, per sample, so the gain can move on the same sample as the kick's
transient. Pre-fader sends tap the signal before the ride.

A device control input can also consume the value. A device that declares a
control port takes a second, silent input: the control signal shapes the device
and is never summed into its output. Write the connection as a `route:` line on
the channel that owns the effect chain:

```ocp
@track kick | instrument: drums/kick

@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-24dB, ratio=4:1, attack=1ms, release=100ms
  route: control kick_env -> compression
```

The target is an effect address, the same form automation lanes use: a type
name when the chain has exactly one entry of that type, or `fx2` for a one-based
chain position. The kind here is `control`. Without a kind the route is
`audio`, which feeds the device's audio key input instead (the route form of
`sidechain=`).

One control source can feed several channels. The graph evaluates it after the
chain of the track it listens to and before each consumer, so a duck starts on
the onset sample and does not lag by a block. An unknown source, a target the
chain does not have and a device without a control input are all refused with a
message.

A channel cannot feed its own control input. Control is evaluated before its
consumer, so that route would form a loop. Bus and master chains take the same
`route:` line.

## Control links

A control source feeds one consumer. A control link fans one control out to
several parameters, each with its own mapping and smoothing:

```ocp
@control_link swell
  from: cc(1, 11)
  -> track: pad param: filter.cutoff formula: exponential smoothing: 20ms
  -> track: lead param: filter.cutoff formula: linear
```

`from:` takes one of three sources:

- `cc(<channel>, <controller>)`, with a 1-based channel.
- `macro(<n>)`, one of the eight OpenSynth macros.
- `lane(<track>, <param>)`, an automation lane whose normalized value becomes
  the control.

Each `->` row names a `track:`, a `param:` and a `formula:`, and may add
`smoothing:`. The formulas are `linear`, `inverted`, `exponential`,
`logarithmic`, `midi_unit` and `midi_centered`. Zero smoothing jumps straight to
the new value, so a row without `smoothing:` behaves like step automation.

The control is normalized once per block. Each target then applies its own
formula and advances its own one-pole smoother. `midi_unit` and
`midi_centered` read the raw 0…127 value, so a CC gesture and a macro gesture
can drive the same parameter over the same range.

## Presets

A preset describes an instrument and its effects. Tracks and patterns refer to
it by name.

When a pattern names a preset, every clip of that pattern plays the pattern's
preset in place of the hosting track's, so one track can host two patterns on
two instruments. A pattern without a preset uses its track's preset. The
track's effects, sends and fader apply once, to the summed instrument output.
If a pattern names a preset the project does not declare, the render refuses it
by name and does not fall back to the track's instrument.

```ocp
@preset keys
  source: soundfont
  program: 4

@preset lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, filter.cutoff: 1400Hz}

@preset wide_lead
  extends: lead
  effects:
    - chorus: rate=0.35, depth=3.5, mix=28%

@preset lv2_gain
  source: lv2
  plugin: "http://example.org/lv2/gain"
  params: {gain: 0.5}
```

In an effect chain, address an LV2 effect plugin with the `lv2.` prefix:
`- plugin:"lv2.http://example.org/lv2/gain": gain=0.5`. LV2 control inputs
become preset parameters named by their port symbol. Bundles are found through
`LV2_PATH` and the platform LV2 directories, and a plugin's binary loads only
when a project uses it. The host supports stereo audio, control ports, MIDI
atom input and `state:interface`. It rejects plugins that require the worker,
options, log or UI extensions.

On macOS, an AudioUnit preset uses `source: au` with a display name
(`Apple: DLSMusicDevice`), a four-character code (`aumu:dls:appl`) or a
`.component` path.

| Field | Meaning |
|---|---|
| `source:` | `soundfont`, `clap`, `vst3`, `vst2`, `au`, `lv2`, `builtin` or `sample` |
| `program:` | General MIDI program number for `soundfont` |
| `plugin:` | plugin id for `clap`/`vst3`/`vst2`/`au`/`lv2`/`builtin` (e.g. `"builtin.open_synth"`) |
| `patch:` | preset/program name inside the plugin |
| `file:` | sample file for `source: sample` |
| `root_note:` | root note for sample playback |
| `params: { ... }` | plugin or built-in patch parameters |
| `effects:` | an effect chain applied to this preset |
| `extends:` | inherit another preset, then override |

Plugin state goes in `params:`; there is no preset field for it. Write
`<format>_state_file` or `<format>_state_hex` with the format name, for example
`vst3_state_file`, `au_state_file` or `clap_state_hex`. The generic
`plugin_state_file` and `plugin_state_hex` use the preset's own format. State
bytes are applied after the parameters, so a state file overrides any
individual parameter it also contains.

Preset assets resolve against the directory of the `.ocp` file. The rule covers
`file:`, `wavetable.file`, `sampler.sample_file`, `sampler.sfz_file`,
`<format>_state_file`, a kit's layer samples, and `serum_file` with its
`serum_root`. It also holds on the first load after the project opens, because
the load request carries the project directory itself and does not read it
back from the GUI.

An absolute path is used as written and needs no directory. A relative path
with no project directory is refused, and a file missing from disk is refused
with the resolved path that was tried. The OpenSynth importer reports the
missing directory as
`Relative synth asset requires a saved project directory`.

## Drum kits

`@kit` declares a drum instrument with one pad per note. It lowers to a builtin
preset, so `preset: basic` on a track is all you need.

```ocp
@kit basic
  max_voices: 32
  pad kick       | note: C2, gain: -2dB, decay: 0.35s, one_shot: true, choke: 1
  pad hat_closed | note: F#2, gain: -3dB, choke: 2
  layer kick     | low: 1, high: 127, sample: "samples/kick.wav"
  layer kick     | low: 1, high: 127, sample: "samples/kick_alt.wav"
```

Pad fields are `note`, `gain`, `pan`, `tune`, `cents`, `decay`, `release`,
`reverse`, `one_shot` and `choke`. Layer fields are `low`, `high`, `sample` and
`gain`. Layers whose velocity windows overlap form a round-robin bank. See
[Drum kit](../drum-kit.md).

## Tracks

```ocp
@track lead | instrument: synth/lead_square, preset: lead, channel: 1
  mix: vol=-6dB, pan=0.15, send(hall=12%), polarity=normal
  effects:
    - compression: ratio=3:1, threshold=-12dB
  automation: brightness {5:1=40%, 7:1=70%}
  clip: lead_hook [5:1 - 9:1]
```

`instrument:` is `category/name` (`piano/acoustic_grand`, `drums/acoustic_kit`,
`synth/lead_square`, `audio/clip`, …). A bare name is a drum-kit instrument.

`channel:` (0–15) sets the MIDI channel for events derived from notes. `port:`
(0-based) selects one of a plugin's declared MIDI ports, and `pair:` (0-based
within that port) selects the stereo output pair the track reads; see
[multi-output routing](../multi-output-routing.md).

The track header does not accept `engine:`. The instrument and preset select
the engine, so the header rejects the field with a parse error and never stores
an unused override. `engine_params: {key: value}` passes per-track overrides to
the instrument. The SoundFont player reads `dynamic_layers: off` from it (see
[Dynamics and hairpins](#dynamics-and-hairpins)).

`mix:` properties:

| Key | Values | Default |
|---|---|---|
| `vol=` | dB, `-120`…`+24` | 0 dB |
| `pan=` | `-1.0`…`1.0` | 0 |
| `send(bus=level)` | `%`, number or dB, up to +12 dB | none |
| `send(bus=level, pre)` | `pre` or `post` | post-fader |
| `send(bus=level, pan=x)` | `-1.0`…`1.0` | 0 |
| `send(owner.outN->bus=level)` | plugin auxiliary output, N ≥ 2 | n/a |
| `mute=` / `solo=` | boolean | false |
| `output=` | bus name (exclusive main route) | master |
| `polarity=` | `invert`, `normal`, `true`, `false` | normal |
| `solo_safe=` | boolean | false |
| `display_name=` | quoted string, 1–64 chars | track name |
| `color=` | quoted CSS colour | palette |

`display_name` and `color` only change how the track looks. The track name
remains the routing identity. See [Mixer routing](../mixer-routing.md).

`pan` is a fraction of the stereo field. `-0.1` is 10 % left, `-1` is hard
left, `0` is centre and `1` is hard right. It is neither a percentage nor an
angle. A value outside `-1.0`…`1.0` fails validation and is not clamped.

## Buses and master

```ocp
@bus hall
  mix: vol=-6dB, mute=false, solo=false
  routes: [master]
  effects:
    - reverb: decay=2.4s, mix=100%

@bus drum_verb
  routes: [hall]            // bus to bus

@master
  volume: -1dB
  loudness: -14dB
  effects:
    - limiter: ceiling=-0.8dB
```

A bus takes `mix:` (`vol`, `mute`, `solo`), `routes:` and `effects:`.
`routes:` names other buses or `master`, and the validator rejects cycles.

A track's sends run in parallel with its main route. Sending to a bus does not
take the track out of the master.

`@master` takes `volume:`, `loudness:` (a LUFS target from −80 to 0),
`effects:` and `gain_automation:`.

## Effects

An effect chain is a list of `- device: key=value, …` entries. The device name
is its DSL keyword; [Built-in devices](devices.md) lists every device and its
controls.

```ocp
effects:
  - eq: low_freq=150Hz, low_gain=-2dB
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
  - reverb: decay=1.6s, mix=18%
  - limiter: ceiling=-0.8dB
```

Values take the unit suffixes `dB`, `%`, `Hz`, `ct`, `ms` and `s`. Ratios are
written `3:1`, and booleans are `true` or `false`. A parameter name the device
does not define is a render error and is never silently ignored, so copy the
names from [Built-in devices](devices.md). The retired forms `reverb: type=hall`
and `eq: high_shelf=…` still load through the compatibility layer with a
warning (see [DSL compatibility](../dsl-compat.md)). They are not current
controls. External effects use `- plugin:"org.example.fx": mix=50%`.

`bypass=true` on any entry takes that device out of the chain. A bypassed
device processes nothing, reports no latency and adds no tail, so the render is
bit-identical to the same project with the entry deleted. `bypass` is a field of
the effect entry, outside the device's own controls. It therefore works on every
built-in device and on hosted plugins, and no device may define a parameter
with that name. The serializer writes it after the device's parameters.
`bypass=false` is the default and is never written, so a project with nothing
bypassed serializes without the field. The legacy `dynamic_eq: bypass=…`
parameter is read as the same field.

## Audio clips

`@audio_clip` places a decoded media file on a track. The track's mix, sends
and effects apply to it exactly as they do to notes.

```ocp
@audio_clip intro [1:1 - 5:1] | track: tape
  file: "media/intro.flac"       // relative to the .ocp file
  offset: 250ms                  // trim from the start
  gain: -3dB
  gain_envelope: 0:0dB, 0.5:-6dB, 1:0dB  // relative to gain, position 0..1
  fade_in: 5ms
  fade_out: 1.5s
  crossfade: 10ms
  stretch: 1.25                  // output length = media length x 1.25
  mode: musical                  // off | percussive | musical
  tempo_follow: false            // derive the ratio from the range instead
  pitch: -2                      // semitones, fractional allowed, -24..=+24

@audio_clip take2 [1:1 - 3:1] | track: tape
  file: "media/take-vox-2.wav"
  take_group: "vox take 1720000000000"  // shared by every pass of one session
  take: 2                              // one-based pass number
  muted: true                          // keep it, but play nothing

@track tape | instrument: audio/clip
  mix: vol=-6dB
```

| Field | Meaning | Default |
|---|---|---|
| `file:` | media path relative to the project; absolute paths are rejected | required |
| `offset:` | seconds trimmed from the start of the (stretched) media | 0 |
| `gain:` | clip gain in dB | 0 |
| `gain_envelope:` | optional `position:gain` points, normalised to the clip and relative to `gain:`; first position 0, last 1 | absent |
| `fade_in:` / `fade_out:` | linear fades in seconds | 0 |
| `crossfade:` | equal-power fade at the head and tail, in seconds | 0 |
| `stretch:` | time-stretch factor, `1.0` leaves unprocessed audio unchanged; allowed range `0.25`–`4.0` | 1.0 |
| `mode:` | `off` disables processing, `percussive` is WSOLA, `musical` is a phase vocoder | musical |
| `tempo_follow:` | ignore `stretch:` and fit the media to the clip range; wins over `stretch:` | false |
| `pitch:` | transposition in fractional semitones without changing duration | 0 |
| `formant:` | independent spectral-envelope displacement in fractional semitones | 0 |
| `preserve_formants:` | approximately compensate for the formant displacement introduced by pitch | false |
| `warp:` | repeated source-to-target anchors, for example `warp: 0.5s -> 0.65s` | none |
| `take_group:` | identifier shared by the passes of one loop-record session | absent |
| `take:` | one-based pass number inside `take_group` | absent |
| `muted:` | keeps the clip and its media but contributes no audio; the engine skips it like an absent clip | false |

While stretch is active, `offset` is measured in output time. With
`tempo_follow`, a tempo change changes the ratio.

Warp times are measured from the media origin, before the offset. The anchors
follow an implicit `0s -> 0s`, must increase on both axes, and the explicit
stretch ratio applies after the last anchor. Warp requires `tempo_follow` off.

The effective formant correction is limited to ±12 semitones. Pitch, warp and a
nonzero formant shift all require processing on (`mode:` other than `off`).
`pitch:` is refused when `stretch × pitch` leaves the stretch range.

Media longer than the clip is trimmed, and shorter media is followed by
silence. A loop-record session writes one clip per pass, all with the same
`take_group:`. The newest pass is unmuted and the others are muted until a comp
chooses another. Session solo is not a DSL attribute; it only changes the live
audition. See [Audio clips](../audio-clip.md), [Comping](../comping.md) and
[Time-stretch](../audio-clip-stretch.md).

## Vocal tuning

`@vocal_tune` stores your per-note tuning decisions for one audio clip. Only the
decisions live in the project. The detected notes and f0 come from the derived
`.ocn` analysis.

```ocp
@vocal_tune lead_tune | clip: lead
  scale: A major
  amount: 60%
  vibrato: flatten
  notes:
    - at=0.480s tgt=A3 amount=100% vibrato=preserve
    - at=2.013s tgt=+2st
    - at=5.470s tgt=5@4 bypass=true
```

The block also sets the automatic policy for notes without a stored target:

| Key | Default | Meaning |
|---|---|---|
| `snap:` | `auto` | `auto` (the reference when one is set, otherwise the scale), `scale`, `reference` or `off`. |
| `reference:` | none | A clip whose detected pitch becomes the target under `snap: reference`. |
| `cap:` | `100ct` | Largest scale-snap correction; a wider move is not proposed. |
| `window:` | `35ct` | The search window around a note's pitch. |
| `max_move:` | none | Upper bound on one note's correction. |
| `vibrato:` | `preserve` | `preserve` keeps the performer's vibrato, `flatten` holds a note at its measured median, `scale` keeps half its measured depth. A note can override it with `vibrato=preserve\|flatten\|scale`. |

`at` is the detected onset the decision was made against. `tgt` accepts a pitch
name (`A3`), absolute cents (`5700ct`), a semitone or cent offset (`+2st`,
`-25ct`), or a scale degree and octave (`5@4`). `ocp vocal apply` resolves the
decisions and bakes the clip. With every amount at `0%`, the bake is
sample-exact. See [Vocal tuning](../vocal-tune.md).

## Vocal harmony

`@vocal_harmony` derives a second voice from a tuned audio clip. The voice is a
pitch-shifted copy of the clip at a diatonic interval, added to the clip's own
track. The lead stays as it is.

```ocp
@vocal_harmony third | clip: lead
  interval: 2         // diatonic steps above the lead note
  level: -6dB         // voice level relative to the lead
  pan: -0.3           // -1 left to 1 right
```

| Key | Default | Meaning |
|---|---|---|
| `clip:` / `track:` | none | The source clip, or a track that owns exactly one clip. Exactly one is required. |
| `interval:` | `2` | Diatonic steps above the lead note inside the clip's scale; negative puts the voice below. Without a scale it counts chromatic semitones. |
| `level:` | `-6dB` | Voice level in dB relative to the lead's RMS over the same notes. |
| `pan:` | `0` | Stereo balance of the voice, `-1` to `1`. |

`interval` counts scale steps. A third above C is E, and a third above D is F.
The scale comes from the clip's own `@vocal_tune` block, then from the key map
at the note, then from the project key. A project with no scale anywhere uses
chromatic semitones and the render warns about it. An interval that would move
a note by more than ±12 semitones is refused, and the message names the note.
See [Vocal tuning](../vocal-tune.md).

## Raw channel events

A track can carry a raw, MIDI-shaped performance in place of notes. Positions
are absolute quarter notes, channels count from 0, and every event ends with
`;`:

```ocp
@track out | preset: meridian_keys, instrument: keys/kontakt
  performance: {
    @0 ch2 program 65;
    @0.125 ch2 note_on 60 90;
    @0.25 ch2 bend -8192;
    @1.25 ch2 note_off 60 37;
  }
```

The commands are `note_on <note> <velocity>`, `note_off <note> <velocity>`,
`cc <controller> <value>`, `program <program>`, `bend <value>`,
`pressure <value>` and `poly_pressure <note> <pressure>`. Events must be in
non-decreasing position order. A track with this block cannot also carry
phrases, patterns or clips; the validator refuses the combination.

## A complete example

```ocp
@ocp "0.4.0"

@project "Manual Example"
  tempo: 120
  time_signature: 4/4
  key: C major
  swing: 55%

@preset keys
  source: soundfont
  program: 4
  effects:
    - eq: low_freq=150Hz, low_gain=-2dB
    - reverb: decay=1.6s, mix=18%

@preset lead
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.7, filter.cutoff: 1400Hz, filter.resonance: 0.35, amp.attack: 0.005s, amp.decay: 0.3s, amp.sustain: 0.55, amp.release: 0.35s, output.gain: 0.3, control_cutoff_param: "filter.cutoff"}

@preset tight_drums
  source: soundfont
  program: 0
  effects:
    - compression: ratio=3:1, threshold=-8dB, attack=5ms, release=60ms

@pattern keys_verse | instrument: piano/acoustic_grand, preset: keys, color: "#4fc3f7"
  events:
    C4 q mp
    E4 q mp
    G4 h mf expression={dynamics: mp->mf}
    |
    F4 q mf
    A4 q mf
    C5 h f

@pattern lead_hook | instrument: synth/lead_square, preset: lead
  events:
    @0 G4 8th f articulation=accent
    @1 C5 8th f
    @2 Eb5 q ff vibrato={rate: 5Hz, depth: 20ct, delay: 0.2s}
    rest q
    @3 G5 h. ff articulation=legato

@pattern groove | instrument: drums/acoustic_kit, preset: tight_drums, swing: 60%
  hits:
    kick  1:0 ff
    hihat 1:0 mp articulation=closed
    snare 2:0 f
    hihat 2:2 mf articulation=open

@track keys | instrument: piano/acoustic_grand, preset: keys
  mix: vol=-3dB, pan=-0.1, send(hall=20%)
  clip: keys_verse [1:1 - 9:1]

@track lead | instrument: synth/lead_square, preset: lead
  mix: vol=-6dB, pan=0.15, send(hall=12%)
  clip: lead_hook [5:1 - 9:1]
  automation: cutoff {5:1=40%, 7:1=70%, 8:1=50%}

@track drums | instrument: drums/acoustic_kit, preset: tight_drums
  mix: vol=-2dB
  clip: groove [1:1 - 9:1]

@bus hall
  mix: vol=-6dB
  routes: [master]
  effects:
    - reverb: decay=2.4s, mix=100%

@section verse [1:1 - 9:1]
  repeat: 2
  label: "Verse"

@master
  volume: -1dB
  loudness: -14dB
  gain_automation: linear [1:1 - 9:1] {1:1=1, 8:1=0.9}
  effects:
    - eq: low_freq=40Hz, low_gain=-1dB
    - limiter: ceiling=-0.8dB
```

Check the file with `ocp validate` and listen to it with `ocp render`; both
commands are described in [Getting started](getting-started.md). Two of the
presets are SoundFont programs, so render with
`--soundfont soundfonts/MS_Basic.sf2`.
