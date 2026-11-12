# The .ocp file

An OpenCompose project is a UTF-8 text file with the extension `.ocp`. The
grammar is defined in `crates/oc-dsl/src/grammar.pest`; this chapter covers what
you write, not how it is parsed.

## File shape

```ocp
@ocp "0.6.0"                 // optional version line

@project "Song title"        // required, properties follow
  tempo: 120
  time_signature: 4/4
  key: C major

// then @preset, @pattern, @track, @bus, @section, @master in any order
```

- `//` starts a comment that runs to the end of the line.
- Indentation is cosmetic; structure comes from `@` keywords.
- The order of the blocks is free: a `@pattern` or a `@section` may follow a
  `@track`, and a `@bus` may precede the tracks that route into it.
- A block is contiguous. The next `@` keyword at the project level ends the
  block before it, so a track's content stays inside its own `@track` block; a
  line that belongs to a track and appears after another block is refused by
  name.
- An inline drum pattern (`@pattern name [range] xN` with hit lines) is track
  content and belongs inside a `@track`. A `@pattern` at the project level is a
  named definition, placed on a track with `clip:`.
- Names may be bare identifiers or quoted strings. Track, bus and preset names
  are the routing identity.
- The version line is optional. New projects use `0.6.0`. Files written
  for `0.2.0` load unchanged. `0.3.0` added `@vocal_tune`; `0.4.0` added its
  automatic `snap`, `reference`, `cap` and `window` fields; `0.5.0` added note
  timing and `0.6.0` added `@vocal_harmony`.

## Time and positions

Positions are `bar:beat:subdivision`: bar and beat are 1-indexed, and
subdivision `0` is on the beat. A range is `[start - end]` and the **end is
exclusive**: `[1:1 - 5:1]` covers bars 1–4.

```
[1:1]        bar 1, beat 1
[9:4]        bar 9, beat 4 (not the end of bar 9)
[10:1]       the end of bar 9
[1:1:0]      explicit subdivision 0 (on the beat)
```

Durations are written `w`, `h`, `q`, `8th`, `16th`, `32nd`, `64th`, optionally
dotted (`q.`, `q..`), optionally tupleted (`qt`, `q5:4`, `q7:8`) and tied
(`q~h`, `C4 q~h~q`). A rest takes the same durations: `rest q`, `rest 2w`.

A phrase or pattern range must cover all of its content, rests included: a
four-bar phrase in 4/4 is `[1:1 - 5:1]`, not `[1:1 - 4:4]`.

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

Map properties are followed by one entry per line in the same block. `export:`
also takes `resample: 0…7`, the rung the offline render uses, and a
per-container block: `wav: {depth, dither}`, `flac: {depth, level}` or
`mp3: {bitrate, vbr_quality}`.

## Sections

`@section` names a span of the arrangement. When sections exist they define the
rendered timeline: they play in written order and `repeat:` plays a span again.
Content is assigned to the section that contains its **start**; gaps between
sections are removed and content outside every section is not rendered.

```ocp
@section verse [9:1 - 17:1]
  repeat: 2
  label: "Verse"
  flags: pizzicato
```

`repeat:` must be at least 1. Sections must be ordered and must not overlap.
`flags:` selects one region technique (`pizzicato`/`pizz`, `arco`, `muted`/
`con_sord`, `sul_ponticello`/`sul_pont`, or `sul_tasto`) for note onsets inside
the section; a note's own `articulation=` wins. It drives the same articulation
profile as a per-note mark, so the section's keyswitch, CC or parameter is sent
before each note that starts in it.
See [Sections and the rendered arrangement](../arrangement.md).

## Markers

`@marker` names a point on the timeline. The name is a string or a bare
identifier, and the position uses the usual `bar:beat:subdivision` form.

```ocp
@marker "Verse 2" at 9:1

@marker outro at 41:1:2
```

Markers are annotations. They are saved with the project and keep their written
order, they may sit past the last audio, and they never change what renders. A
project without markers writes the same bytes as before the construct existed.
See [Markers and the loop region](markers.md).

## Patterns and clips

A top-level `@pattern` is a named, reusable block of content. A `clip:` places it
on a track at a timeline position. One pattern can be placed many times.

```ocp
@pattern keys_verse | instrument: piano/acoustic_grand, preset: keys, color: "#4fc3f7"
  events:
    C4 q mp
    E4 q mp
    G4 h mf

@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 9:1]
```

Inline properties after `|`: `instrument:`, `preset:`, `transpose:` (semitones),
`swing:` (per-pattern swing, overrides the project), `bars:` (the pattern's
length in bars), `color:`.

`bars:` bounds where the piano roll and the drum lane place new content: a draw
past the last bar lands on the last editable step inside it instead of outside
the pattern. Without `bars:`, the length is the end of the last bar the written
content occupies. Shortening a pattern keeps or trims notes past the new end;
kept notes stay in the document and the roll marks them out of range.

`transpose:` moves the pattern's pitched `events:` by whole semitones before the
instrument plays them: with `-2`, a written C4 sounds as Bb3. The field accepts
`-127` to `127` semitones; a value outside that range is refused with a
message naming the range instead of being read as no transposition. Every path that
plays the pattern carries the shift — offline render, streaming playback, MIDI
export, analysis and range render — so the rendered audio and the exported MIDI
agree with the panel's value and with the pattern preview.

### Slip and loop

Two optional properties choose which part of the pattern a clip plays and
whether it repeats. Both leave the block where it is:

```ocp
@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: keys_verse [1:1 - 5:1] offset: 1q loop: 2q
```

`offset:` hides that many quarter notes of the pattern's head. The clip keeps its
start, end and length, so the phrase moves against the beat without the block
moving. The hidden head is silent: a note that starts inside it never plays, and
the pedal marks there are dropped too. Slip is refused when the clip already
shows the whole pattern.

`loop:` repeats that many quarter notes of content, starting at the offset, for
the clip's full length. It must be shorter than the clip and must end inside the
pattern's content. The last repeat stops at the clip end, and a note that runs
past the end is cut there. The timeline draws one dashed seam per repeat.

A project that uses neither property writes the same bytes as before the fields
existed.

### Clip groups

`@group` binds clips so one drag moves the set and one click selects all of it.
A group names its members by clip identifier, and a clip takes that identifier
with `id:` on its own line:

```ocp
@track keys | instrument: piano/acoustic_grand
  clip: keys_verse [1:1 - 9:1] id: c1
  clip: keys_verse [9:1 - 17:1] id: c2

@group g1 {
  members: c1, c2
}
```

A group holds at least two clips, every member must name a clip that carries its
identifier, and identifiers are unique across the project. An identifier exists
while a group names the clip: deleting the clip drops it from every group,
splitting it drops it the same way, and a group left with fewer than two members
is removed along with the identifiers it held. A project without groups writes
the same bytes as before the construct existed.

Groups are editing-only. They never change what renders.

The shift is applied to the note number, so it adds to whatever the hosting
instrument does to every note it plays. On the built-in OpenSynth, a preset's
`tuning.transpose` still applies on top: a pattern at `-2` on a preset at `+12`
sounds ten semitones above the written pitch. The pattern's own `preset:` and
`instrument:` choose the instrument that plays it (the track's preset when it
names none) and never replace the transposition.

`transpose:` does not move `hits:`. A hit addresses a sample or a `@kit` pad by
name, so a transposed hit would play a different drum; a drum pattern sounds the
same with or without the value, and a render reports that the value was ignored
for the hits.

A pattern body holds only `events:` and/or `hits:`. `automation:`, `mix:`,
`effects:` and `engine:` are not pattern-level fields: a pattern carrying any of
them fails to parse with a diagnostic instead of being ignored. A pattern is
content placed on a track, so the effect chain, mixer state, automation and
engine selection belong to that track.

Content can also sit directly in a track instead of in a named pattern:

- `@phrase name [range]` with a note list — the same notes, placed inline.
- `@pattern name [range] xN` with a hit list — a drum pattern placed inline.

```ocp
@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 3:1] x2
    kick  1:0 ff
    snare 2:0 f
```

## Notes

A note line is `@N` followed by one or more events separated by commas:

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
duration, dynamic and modifiers: `@0.0 G4 q mf, G5 h mp articulation=legato` is
one short and one long note starting together. Without `@` the line shares the
sequential position. The line advances the position once, by its longest member.
A comma list has to stay on one physical line and cannot repeat a pitch. A
leading comma, a trailing comma, a member without a duration, and a comma across
a line break are refused with their own messages.

`[C4 E4 G4] q mf` is sugar for the comma line `C4 q mf, E4 q mf, G4 q mf`: the
bracketed token shares one duration, dynamic and modifier set across its
pitches. The serializer's canonical form is the comma form, so a chord token
saves back as `@0 C4 q mf, E4 q mf, G4 q mf`, and rows of simultaneous notes
that share every property except pitch collapse into one comma list. Rows that
differ in duration, dynamics, velocity or articulation stay on separate lines.

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

A pedal mark is a phrase or pattern event with its own position, written
`@<beat> pedal down` or `@<beat> pedal up`:

```ocp
@track piano | instrument: piano/acoustic_grand
  @phrase theme [1:1 - 5:1]
    C4 q mf
    @1.0 pedal down
    @2.0 E4 q mf
    @3.0 pedal up
```

`pedal down` holds every note whose note-off lands while the pedal is down, and
`pedal up` releases those notes. The position is always written, because a pedal
spans notes and does not take part in the sequential line model: a mark without
`@` is refused with a message that names the missing position. A pedal mark
outside a phrase or pattern is refused the same way, and a `hits:` list does not
accept one, because a drum hit has no note-off to hold.

## Dynamics and hairpins

A dynamic on a note sets one level. A hairpin moves the level across the notes it
spans: `@<beat> cresc` and `@<beat> dim` are phrase or pattern events with their
own position, like a pedal mark.

```ocp
@track strings | instrument: strings/violin, preset: warm_strings
  @phrase swell [1:1 - 5:1]
    @0.0 cresc
    @0 C4 w pp
    @4 C4 w
    @8 C4 w
    @12 C4 w ff
```

The mark swells to the next dynamic written on a note in the same phrase, or to
the end of the phrase when no note carries one. A note that restates a dynamic
ends the span, so a `cresc` over notes all marked `pp` is refused: the level
would not move. Without a written target, `cresc` runs `pp` to `f` and `dim` runs
`f` to `pp`.

A phrase hairpin on a built-in synth compiles to a `gain_automation:` clip, so
it moves level alone. A SoundFont or hosted instrument instead receives CC11
expression points across the mark: the instrument's own dynamics law follows
the swell, and a SoundFont also crossfades its velocity layers while a note in
the span sounds. The layer amplitude is compensated, so CC11 remains the only
level move. A built-in track keeps the gain clip because its synth has no CC11
receiver.

Write `dynamic_layers: off` in the preset's `params:` or in the track's
`engine_params:` to restore hard layer selection on a SoundFont track. The
track value wins over the preset value.

Keep the span clear of other gain clips on a built-in track, because a track
cannot hold two of them over the same span, and one hairpin cannot start before
the previous one ends. `ocp fmt` keeps the mark and prints the clip a built-in
track expands to as a comment.

A pattern mark is relative to the pattern's content: each placement expands the
mark on the track that hosts it, once per loop repetition, so one pattern played
twice writes two ramps. Pattern marks keep the gain-clip form even on a hosted
track, because a pattern may name its own preset. A mark a slip hides is
dropped, and a ramp stops at the clip's end.

A hairpin is refused in a `hits:` list, outside a phrase or a pattern's
`events:` block, and when no note follows it in its container.

## Glissando

`gliss` on a note slides its pitch to the next note in the phrase, as a curve
rather than as new samples:

```ocp
@track lead | instrument: synth/lead_saw, preset: slide_lead
  @phrase slide [1:1 - 3:1]
    A4 w mf gliss
    E5 q mf
```

Four spellings share one modifier: `gliss` slides to the next note's pitch,
`gliss=G4` names the destination, `gliss=120ms` arrives that early, and
`gliss=run` or `gliss=run_diatonic` writes short notes instead of a curve —
chromatic, or in the project key. The run forms exist because a sampled source
cannot slide: a SoundFont or sample-bank preset is a recording, so a plain
`gliss` on it is refused and the message names the run form that plays. A plugin
that takes pitch bend slides too; a hosted source that reports no bend support
is refused the same way.

A slide follows the project clock: under a tempo map the curve is measured in
quarter notes, so it stretches and compresses with the tempo. A track that
carries a curve identifies every ordinary note at a curve's pitch as its own
voice, so the same track may also hold ordinary notes at that pitch; each keeps
its own pitch and release. The engine still refuses a plain note it did not
identify (a keyswitch, a control trigger, or a drum hit) at a curve's pitch,
because pitch alone cannot say which voice that event addresses.

## Ornaments

`ornament=` turns one written note into the notes or the gate a performer would
play. The mark stays in the score, so `ocp fmt` keeps it; playback and MIDI
export expand it into ordinary notes.

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

A trill and a mordent use the project key; without a key the step is one
semitone. They cannot share a note with an articulation, bend, vibrato,
expression curve or glissando, because each would describe a second pitch or
level path. A trill or mordent needs a note of its own; a note too short for its
grid stays whole or shares its gate between the three mordent parts. `arpeggio`
rolls every note that starts with the marked note and keeps each written
duration.

See [Ornaments](../ornaments.md) for the full timing rules and export behavior.

## Drum hits

A hit is `drum beat:subdivision dynamics modifiers`. Drum names are free
identifiers (`kick`, `snare`, `hihat`, `ride`, `crash`, …); what they trigger
depends on the kit or preset. An exact velocity is written `v=1` to `v=127` in
the same position as a note's velocity, and it overrides the written dynamic:
`kick 1:0 mp v=120` sounds at 120.

```ocp
hits:
  kick  1:0 ff
  hihat 1:0 mp articulation=closed
  snare 2:0 f
  hihat 2:2 mf articulation=open
```

A hit position is `beat:subdivision` inside the pattern's own bar, not
`bar:beat`: the beat is 1-indexed and the subdivision counts sixteenths of that
beat, `0` to `3`. `1:0` is the downbeat, `1:2` the second sixteenth after it and
`2:3` the last sixteenth of beat 2.

Use `grid_offset_beats` to write a position between sixteenths. For example,
`kick 1:1 mf grid_offset_beats=-0.125` writes a hit one 32nd note after the
downbeat. The signed offset is in quarter notes and can be at most 0.125 in
either direction. This position determines clip and loop eligibility and is
the input to swing. Use `timing_offset_beats` for a separate expressive
adjustment after swing.

A pattern holds one bar of hits. A named `@pattern` is placed by a `clip:`,
which plays it once; a groove of several bars is an inline pattern in the track,
where `xN` repeats the written bar. `@pattern groove [1:1 - 9:1] x8` in a track
plays that bar eight times.

Hit articulations: `closed`, `open`, `half_open`, `cross_stick`, `ghost_note`,
`flam`, `roll`, `rim_shot`. `rest 1:2` leaves that position silent, and `|`
separates written bars for reading only.

## Automation

A track carries one lane per control:

```ocp
automation: expression {1:1=20%, 4:1=50%, 8:1=30%, 12:1=60%}
automation: volume [ppq=480] {1:1=0dB, 3:1=-6dB}
automation: pitch_bend {1:1=0ct, 2:1=50ct}
```

The value is a percentage, dB, number, string or boolean. `[ppq=N]` switches the
lane to tick positions at N ticks per quarter note; without it, points are
bar:beat.

A lane can carry a time span, which turns it into a clip:

```ocp
automation: filter.cutoff [3:1 - 5:1] {4:1=200Hz, 4:4=8000Hz}
automation: filter.resonance [3:1 - 5:1] muted {3:1=20%, 5:1=60%}
```

The span is `[start - end]` with the end exclusive, the same range the other
positioned blocks use. `muted` is a flag with no value and silences the lane.
A clip writes only inside its span: before the head the parameter keeps the
value its `effects:` entry declares, the first point is held from the head, the
last value holds to the exclusive end, and nothing is written after the end, so
the value stays where the clip left it. A lane without a span keeps its old
meaning and drives the whole timeline from its first point. A muted lane writes
nothing at all. Two lanes on one owner and one target may not write at the same
time: overlapping clips, or a clip together with a whole-timeline lane, are
refused; adjacent edges such as `[1:1 - 3:1]` and `[3:1 - 5:1]` are allowed.

The lane name selects what the track's instrument understands. `volume` and
`pan` always map to MIDI CC, and the other General MIDI controls (`modulation`,
`expression`, `sustain`, `resonance`, `brightness`, `pressure`) depend on the
instrument's control profile; `pitch_bend` is in cents. On a built-in synth track
validation refuses a name outside that vocabulary and lists the names that work;
on another instrument a name it does not profile produces no events.

A patch parameter of the built-in synth is a control lane only in the
`pluginparam_<hex>` form, which holds the parameter name bytes in hex:
`pluginparam_66696c7465722e6375746f6666` is `filter.cutoff`. Declaring
`control_cutoff_param: "filter.cutoff"` in the preset gives that parameter the
readable lane name `cutoff`.

A patch-parameter point is read in the parameter's own unit, the unit the same
parameter uses in `params:`: hertz for a cutoff, seconds for an envelope time,
cents for a detune. A bare number inside 0..1 is a normalized position, the
value the patch editor writes, and a percentage is a position too. A continuous
parameter ramps from one point to the next, so a lane from `200Hz` to `8000Hz`
sweeps the filter; a discrete parameter such as `filter.model` steps at each
point.

An effect parameter is automated by naming the effect and the parameter. Tracks,
buses and the master all accept these lanes:

```ocp
automation: compression.threshold {1:1=-24dB, 3:1=-6dB}
automation: fx2.Mix [ppq=480] {1:1=50%, 5:1=80%}
```

The effect is addressed by its type name (`compression`, `eq`, `plugin`, …) when
the chain has exactly one of that type, or by its one-based chain position
(`fx2`). Values use the same unit as the parameter in the `effects:` entry.
Built-in effects must name a parameter from the device schema; plugin parameters
are the plugin's own names. A point carries an optional `curve=` (`linear`,
`stairs`, `half_sine`, `smooth`) for the segment to the next point, plus
`tension=` for `smooth`; `linear` is the default and `stairs` is a held step. A
lane can add `smoothing=<time>` (0 ms default) for a one-pole low-pass on its
value. A plugin receives each changed value as a parameter change at the
sample inside the render block; a built-in device is split at the same
positions. An effect lane takes the same `[start - end]` span and `muted` flag
as a control lane; a span makes it a clip, and an LFO lane's cycle then starts
at the clip head. See [`docs/automation.md`](../automation.md).

A track, a bus and the master also use a gain envelope:

```ocp
gain_automation: linear [1:1 - 9:1] {1:1=1, 8:1=0.9}
gain_automation: step ppq=480 [10:1 - 12:3] {10:1=0, 11:2=1}
```

`linear` or `step`, an optional `ppq=`, a range, then points. Values are linear
gain (1.0 = unity). An owner can hold several envelopes, but their ranges must be
sorted and must not overlap. A hairpin is the notation form of one of these clips
on a track; see [Dynamics and hairpins](#dynamics-and-hairpins).

## Control sources

An automation lane is authored by hand. A control source instead derives a
control value from one track's audio, so a fader can follow the music:

```ocp
@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms
```

`from:` names the track whose post-insert, pre-fader audio feeds the detector
(`sidechain(kick)` is the same tap). `attack:` and `release:` are one-pole time
constants; a step reaches `1 - 1/e` in one constant. Optional properties:

| Key | Values | Default |
|---|---|---|
| `lfo:` | `none` or `sine\|triangle\|square\|saw@<rate>Hz`, e.g. `sine@2Hz` | `none` |
| `lfo_depth:` | `0`… any number | 1 |
| `formula:` | small expression over `x`, `none` disables | `none` |
| `scale:` | number | 1 |
| `offset:` | number | 0 |
| `curve:` | `linear`, `squared`, `sqrt`, `log` | `linear` |

The value is built in this order: envelope from the attack/release follower,
then `scale * envelope + offset` clamped to `0`…`1`, then `curve`, then
`formula`, then `lfo * lfo_depth` added, clamped again to `0`…`1`. The detector
is the larger absolute channel value, so the control tracks level, not loudness:
`scale` is how a hot source is brought into range.

The formula is not a scripting language. It reads one variable `x` and supports
`+ - * / ^`, parentheses, unary minus, numbers, and `sqrt(x)`, `abs(x)`,
`clamp01(x)`. The result is clamped to `0`…`1`; a non-finite result reads as
zero, so `1-x` and `x*x` are safe per sample.

A track spends the value on its fader ride:

```ocp
@track bass | instrument: synth/bass
  mix: control(kick_env=-12dB)
```

A device control input is the other consumer. A device that declares a control
port takes a second, silent input: the control signal shapes it and is never
summed into the output. Write the edge with a `route:` line on the channel that
owns the effect chain:

```ocp
@track kick | instrument: drums/kick

@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-24dB, ratio=4:1, attack=1ms, release=100ms
  route: control kick_env -> compression
```

The target is an effect address, the same one automation lanes use: a type name
when the chain has exactly one entry of that type, or `fx2` by one-based chain
position. The kind is `control` here; omitting it means `audio`, which feeds the
device's audio key input instead (the edge form of `sidechain=`). One control
source may feed several channels; the graph evaluates it after its feed track's
chain and before each consumer, so a duck starts on the onset sample, not one
block later. An unknown source, a target the chain does not have, or a device
without a control input is refused with a message instead of being ignored.

A channel's own track cannot feed its own control input: control is evaluated
before its consumer, so that edge would be a loop. Bus and master chains take
the same `route:` line.

`depth` is the gain change at full control: negative ducks, positive boosts. The
ride is applied post-fader and per sample, so the gain can move on the same
sample as the kick's transient; pre-fader sends tap before it.

## Control links

A control source feeds one consumer. A control link fans one control out to
several parameters, each with its own mapping and smoothing:

```ocp
@control_link swell
  from: cc(1, 11)
  -> track: pad param: filter.cutoff formula: exponential smoothing: 20ms
  -> track: lead param: filter.cutoff formula: linear
```

- `from:` is `cc(<channel>, <controller>)` with a 1-based channel, `macro(<n>)`
  for one of the eight OpenSynth macros, or `lane(<track>, <param>)` for an
  automation lane whose normalized value becomes the control.
- Each `->` row names a `track:`, a `param:` and a `formula:`; `smoothing:` is
  optional. The formulas are `linear`, `inverted`, `exponential`, `logarithmic`,
  `midi_unit` and `midi_centered`.
- Zero smoothing is a hard over, so a link without `smoothing:` behaves like
  step automation.

The control reading is normalized once per block, every target applies its own
formula, and every target advances its own one-pole smoother. A formula that
reads the raw 0…127 value is what `midi_unit` and `midi_centered` are for, so a
CC gesture and a macro gesture can reach the same parameter over the same range.

## Presets

A preset describes an instrument and its effects. Tracks and patterns refer to
it by name.

A pattern's preset plays that pattern's clips. When a pattern names a preset,
every clip that references it plays that preset instead of the hosting track's
preset, so one track can host two patterns on two instruments. A pattern without
a preset follows its track's preset. The track's effects, sends and fader apply
once to the summed instrument output, not once per pattern. A pattern preset
that the project does not declare is refused by name at render time; the
renderer never falls back to the track's instrument.

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

LV2 effect plugins are addressed by the `lv2.` prefix in an effect chain:
`- plugin:"lv2.http://example.org/lv2/gain": gain=0.5`. LV2 control inputs
become preset parameters under their port symbol. Bundles are discovered from
`LV2_PATH` and the platform LV2 directories; a plugin's binary is loaded only
when a project uses it. The host supports stereo audio, control ports, MIDI
atom input and `state:interface`, and rejects plugins that require worker,
options, log or UI extensions.

AudioUnit presets use `source: au` on macOS with a display name
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

Plugin state is passed as a parameter, not as a preset field: write
`<format>_state_file` or `<format>_state_hex` with the format name, for example
`vst3_state_file`, `au_state_file` or `clap_state_hex`, or the generic
`plugin_state_file` / `plugin_state_hex` when the preset's format decides. State
bytes are applied after the parameters, so a state file wins over an individual
parameter that it also carries.

A preset asset is resolved against the directory of the `.ocp` file. That rule
covers `file:`, `wavetable.file`, `sampler.sample_file`, `sampler.sfz_file`,
`<format>_state_file`, a kit's layer samples, and `serum_file` with its
`serum_root`. It holds on the first load after the project opens, because the
load request carries the project directory instead of reading it back from the
GUI. An absolute path is used as written and needs no directory. A relative one
with no directory is refused, and a file that is not on disk is refused with the
resolved path it tried; the OpenSynth importer names the missing directory as
`Relative synth asset requires a saved project directory`.

## Drum kits

`@kit` declares a pad-per-note drum instrument. It lowers to a builtin preset, so
`preset: basic` on a track is all you need.

```ocp
@kit basic
  max_voices: 32
  pad kick       | note: C2, gain: -2dB, decay: 0.35s, one_shot: true, choke: 1
  pad hat_closed | note: F#2, gain: -3dB, choke: 2
  layer kick     | low: 1, high: 127, sample: "samples/kick.wav"
  layer kick     | low: 1, high: 127, sample: "samples/kick_alt.wav"
```

Pad fields: `note`, `gain`, `pan`, `tune`, `cents`, `decay`, `release`,
`reverse`, `one_shot`, `choke`. Layer fields: `low`, `high`, `sample`, `gain`.
Layers with overlapping velocity windows form a round-robin bank. See
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
`channel:` (0–15) sets the MIDI channel for note-derived events. `port:` (0-based)
selects a plugin's declared MIDI port, and `pair:` (0-based inside that port)
selects the stereo output pair a track reads; see
[multi-output routing](../multi-output-routing.md). `engine:` is not
accepted: the instrument and preset select the engine, so the track header
rejects it with a parse error instead of storing an unused override.
`engine_params: {key: value}` passes per-track overrides to the instrument;
`dynamic_layers: off` is the one the SoundFont player reads (see
[Dynamics and hairpins](#dynamics-and-hairpins)).

`mix:` properties:

| Key | Values | Default |
|---|---|---|
| `vol=` | dB, `-120`…`+24` | 0 dB |
| `pan=` | `-1.0`…`1.0` | 0 |
| `send(bus=level)` | `%`, number or dB, up to +12 dB | none |
| `send(bus=level, pre)` | `pre` or `post` | post-fader |
| `send(bus=level, pan=x)` | `-1.0`…`1.0` | 0 |
| `send(owner.outN->bus=level)` | plugin auxiliary output, N ≥ 2 | — |
| `mute=` / `solo=` | boolean | false |
| `output=` | bus name (exclusive main route) | master |
| `polarity=` | `invert`, `normal`, `true`, `false` | normal |
| `solo_safe=` | boolean | false |
| `display_name=` | quoted string, 1–64 chars | track name |
| `color=` | quoted CSS colour | palette |

`display_name` and `color` are presentation only; the track name stays the
routing identity. See [Mixer routing](../mixer-routing.md).

`pan` is a fraction of the stereo field, not a percentage or an angle: `-0.1`
is 10 % left, `-1` is hard left, `0` is centre and `1` is hard right. Values
outside `-1.0`…`1.0` fail validation; they are not clamped.

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

- A bus has `mix:` (`vol`, `mute`, `solo`), `routes:` and `effects:`.
- `routes:` names other buses or `master`. Cycles are rejected by the validator.
- A track's send and its main route are parallel: sending does not remove the
  channel from the master.
- `@master` takes `volume:`, `loudness:` (LUFS target, −80…0), `effects:` and
  `gain_automation:`.

## Effects

Effects are a list of `- device: key=value, …` entries. The device name is the
DSL keyword; see [Built-in devices](devices.md) for the full list and controls.

```ocp
effects:
  - eq: low_freq=150Hz, low_gain=-2dB
  - compression: ratio=3:1, threshold=-12dB, attack=5ms, release=80ms
  - reverb: decay=1.6s, mix=18%
  - limiter: ceiling=-0.8dB
```

Values use the unit suffixes `dB`, `%`, `Hz`, `ct`, `ms`, `s`; ratios are written
`3:1`; booleans are `true`/`false`. A name the device does not define is a render
error, not silently ignored, so write the parameter names from
[Built-in devices](devices.md). The retired `reverb: type=hall` and
`eq: high_shelf=…` forms are accepted by the compatibility layer with a warning
(see [DSL compatibility](../dsl-compat.md)); they are not current controls.
External effects use `- plugin:"org.example.fx": mix=50%`.

`bypass=true` on any entry takes that device out of the chain: it processes
nothing, reports no latency and occupies no tail, so the render is bit-identical
to the same project with the entry deleted. `bypass` is a field of the effect
rather than a device control, so it works on every built-in device and on a
hosted plugin, and no device parameter list may repeat the name. The serializer
writes it after the device's own parameters, and `bypass=false` is the default
and is never written; a project that does not bypass anything serialises
unchanged. The legacy `dynamic_eq: bypass=…` parameter means the same field and
is read that way.

## Audio clips

`@audio_clip` places a decoded media file on a track. The track's mix, sends and
effects apply exactly as for notes.

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

`offset` is output-time when stretch is active. With `tempo_follow`, changing the
tempo changes the ratio. Warp times are measured from the media origin before
the offset. Anchors follow an implicit `0s -> 0s`, increase on both axes, and
use the explicit stretch ratio after the last anchor. Warp requires tempo
follow off. The effective formant correction is limited to ±12 semitones.
Pitch, warp, and nonzero formant shift require processing on. `pitch:` is
refused when
`stretch × pitch` leaves the stretch range. Media that is longer than the clip is
trimmed; shorter media holds silence. A loop session writes one clip per pass
with the same `take_group:`; the newest pass is unmuted and the others are muted
until a comp chooses another. Session solo is not a DSL attribute: it only
changes the live audition. See [Audio clips](../audio-clip.md),
[Comping](../comping.md) and [Time-stretch](../audio-clip-stretch.md).

## Vocal tuning

`@vocal_tune` stores a human's per-note tuning decisions for one audio clip.
Only decisions live in the project; the detected notes and f0 come from the
derived `.ocn` analysis.

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

The block also carries the automatic policy for notes with no stored target:

| Key | Default | Meaning |
|---|---|---|
| `snap:` | `auto` | `auto` (the reference when one is set, otherwise the scale), `scale`, `reference` or `off`. |
| `reference:` | none | A clip whose detected pitch becomes the target under `snap: reference`. |
| `cap:` | `100ct` | Largest scale-snap correction; a wider move is not proposed. |
| `window:` | `35ct` | The search window around a note's pitch. |
| `max_move:` | — | Upper bound on one note's correction. |
| `vibrato:` | `preserve` | `preserve` keeps the performer's vibrato, `flatten` holds a note at its measured median, `scale` keeps half its measured depth. A note can override it with `vibrato=preserve\|flatten\|scale`. |

`at` is the detected onset a decision was authored against. `tgt` accepts a
pitch name (`A3`), absolute cents (`5700ct`), a semitone or cent offset
(`+2st`, `-25ct`) or a scale degree and octave (`5@4`). `ocp vocal apply`
resolves the decisions and bakes the clip; with every amount at `0%` the bake
is sample-exact. See [Vocal tuning](../vocal-tune.md).

## Vocal harmony

`@vocal_harmony` derives a second voice from a tuned audio clip. The voice is a
pitch-shifted copy of the clip at a diatonic interval, added to the clip's own
track; the lead is unchanged.

```ocp
@vocal_harmony third | clip: lead
  interval: 2         // diatonic steps above the lead note
  level: -6dB         // voice level relative to the lead
  pan: -0.3           // -1 left to 1 right
```

| Key | Default | Meaning |
|---|---|---|
| `clip:` / `track:` | — | The source clip, or a track that owns exactly one clip. Exactly one is required. |
| `interval:` | `2` | Diatonic steps above the lead note inside the clip's scale; negative puts the voice below. Without a scale it counts chromatic semitones. |
| `level:` | `-6dB` | Voice level in dB relative to the lead's RMS over the same notes. |
| `pan:` | `0` | Stereo balance of the voice, `-1` to `1`. |

A third above C is E and a third above D is F: `interval` counts steps of the
scale, not semitones. The scale comes from the clip's own `@vocal_tune` block,
then the key map at the note, then the project key. A project with no scale
anywhere gets chromatic semitones and a render warning. An interval that would
move a note more than ±12 semitones is refused, naming the note. See
[Vocal tuning](../vocal-tune.md).

## Raw channel events

A track can carry a raw MIDI-shaped performance instead of notes. Positions are
absolute quarter notes, the channel is 0-based, and every event ends with `;`:

```ocp
@track out | preset: meridian_keys, instrument: keys/kontakt
  performance: {
    @0 ch2 program 65;
    @0.125 ch2 note_on 60 90;
    @0.25 ch2 bend -8192;
    @1.25 ch2 note_off 60 37;
  }
```

Commands are `note_on <note> <velocity>`, `note_off <note> <velocity>`,
`cc <controller> <value>`, `program <program>`, `bend <value>`,
`pressure <value>` and `poly_pressure <note> <pressure>`. Events must be in
non-decreasing position order, and a track that carries this block cannot also
carry phrases, patterns or clips: the validator refuses the mix instead of
playing both.

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

Check it with `ocp validate` and listen with `ocp render`; both are in
[Getting started](getting-started.md). Two of its presets are SoundFont
programs, so render it with `--soundfont soundfonts/MS_Basic.sf2`.
