# Ornaments

An ornament mark turns one written note into the notes, or the gate, that a
performer would play. The mark stays in the score: `ocp fmt` writes it back
unchanged, and you can remove or replace it later. Playback and MIDI export
expand it into ordinary notes, so every render path hears the same result.

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

## Spellings

| Mark | Result |
|---|---|
| `ornament=trill` | Alternates the written pitch with the next scale step. |
| `ornament=mordent` | Plays the written pitch, the step above, then holds the written pitch. |
| `ornament=fermata` | Holds the written gate for 1.5 times its length. |
| `ornament=arpeggio` | Rolls every note that starts with the marked note, in ascending pitch order. |

A note carries at most one ornament. Any other value is refused with a message
that lists the four spellings, so a misspelled mark never plays as a plain
note.

## Pitch step

A trill or mordent takes its upper pitch from the project key at the note's
position. In C major, `B4` steps to `C5` and `E4` steps to `F4`. A note outside
the scale steps to the nearest scale note above it. Without a project key the
step is one semitone, so the mark always has an upper pitch and a project
without a key still plays.

The upper pitch is a scale step, so changing the project key changes the
ornament. The step comes from the key signature; an accidental on the note does
not change it.

## Timing

A trill counts the whole sixteenth notes in its gate and divides the gate into
that many equal parts. The first part sounds the written pitch, the second the
step above, and so on. An even count would end on the upper pitch, so the trill
uses one part fewer and closes on the written pitch. A quarter note therefore
plays three parts of a third of a beat each. A note shorter than three
sixteenths has no room for an alternation and stays whole.

A mordent plays the written pitch and the step above for one sixteenth each,
then holds the written pitch for the rest of the gate. On a note shorter than
three sixteenths the three parts share the gate equally, so the closing written
pitch is kept. A note shorter than two sixteenths stays whole.

Both marks keep the written dynamics and velocity. A note with a trill or
mordent cannot also carry an articulation, bend, vibrato, expression curve or
glissando, because each of those describes a second pitch or level path for the
same note. Remove the other mark, or leave the ornament off. A trill or mordent
needs a note of its own; mark a comma-list chord with `arpeggio`.

A fermata scales the gate after the articulation and instrument corrections. A
staccato note with a fermata is therefore still shorter than a sustained one.
The written duration, the following notes and the bar arithmetic stay where
they are.

## Arpeggio

An arpeggio delays the members of one onset by a thirty-second note each, in
ascending pitch order. Marking any member rolls the whole onset, so a chord
written as one comma list needs the mark only once:

```ocp
@track keys | instrument: piano/acoustic_grand
  @phrase chord [1:1 - 2:1]
    @0 C4 q mf ornament=arpeggio, E4 q mf ornament=arpeggio, G4 q mf ornament=arpeggio
```

The chord still fills the beat it is written on. The delay is a thirty-second
note in project time, so it follows a tempo map. No member moves before the
start of its clip or phrase. An arpeggio keeps every note's written duration
and every articulation.

## Playback and export

The marks are shorthand. They add no DSL primitive and no engine state. The
interpreter expands them before it emits note events, so offline render,
streaming playback, pattern preview, range render and MIDI export all receive
the same notes. A project that uses no marks plays exactly as written.

The written gate bounds the expansion. A long trill produces at most one part
per sixteenth note, and a note too short for the grid stays whole, so no part
has zero length.
