# Ornaments

An ornament mark turns one written note into the notes or the gate a performer
would play. The mark stays in the score, so `ocp fmt` writes it unchanged and a
later edit can remove or replace it. Playback and MIDI export expand it into
ordinary notes, which means every render path hears the same result.

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

One note carries at most one ornament. A value outside the table is refused
with a message that names the four spellings, so a misspelled mark does not
silently become a plain note.

## Pitch step

A trill and a mordent use the project key at the note position. In C major,
`B4` steps to `C5` and `E4` to `F4`. A note outside the scale takes the nearest
step above it. Without a project key the step is one semitone, so the mark still
has a defined upper pitch and a project without a key never fails to play.

The upper pitch is a written pitch, not a fixed interval: changing the project
key changes the ornament. The step is read from the key signature, not from a
per-note accidental.

## Timing

A trill divides its gate into equal parts at the sixteenth-note grid. The first
part sounds the written pitch, the next the step above, and the alternation
continues. An even number of parts would end on the upper pitch, so the last
part is dropped to close on the written pitch. A note shorter than two
sixteenths stays whole: there is no room for an alternation.

A mordent uses two sixteenth-note parts for the written pitch and the step
above, then holds the written pitch for the rest of the gate. On a note shorter
than three sixteenths the three parts share the gate equally, so the closing
written pitch is not lost. A note shorter than two sixteenths stays whole.

Both marks keep the written dynamics and velocity. They do not accept an
articulation, bend, vibrato, expression curve or glissando on the same note:
each of those would describe a second pitch or level path for the same note.
Remove the other mark, or leave the ornament off. A trill or mordent needs a
note of its own; a comma-list chord carries `arpeggio` instead.

A fermata scales the gate after the articulation and instrument corrections, so
a staccato note marked with a fermata is still shorter than a sustained one. The
written duration, the following notes and the bar arithmetic do not move.

## Arpeggio

An arpeggio delays each member of one onset by a thirty-second note in ascending
pitch order. Marking any member rolls the whole onset, so a chord written as one
comma list needs the mark once:

```ocp
@track keys | instrument: piano/acoustic_grand
  @phrase chord [1:1 - 2:1]
    @0 C4 q mf ornament=arpeggio, E4 q mf ornament=arpeggio, G4 q mf ornament=arpeggio
```

The written onset is still the beat the chord fills. The delay is a
thirty-second note in project time, so it follows a tempo map, but a member is
not moved before the clip or phrase start. Arpeggio keeps every note's written
duration and every articulation.

## Playback and export

The marks are sugar: they add no DSL primitive and no engine state. The
interpreter expands them before it emits note events, so offline render,
streaming playback, pattern preview, range render and MIDI export all see the
same notes. A project that uses no mark writes the same notes and the same audio
as before the marks existed.

The expansion is bounded by the written gate. A long trill produces at most one
part per sixteenth note, and a note too short for the grid stays whole instead
of producing a zero-length part.
