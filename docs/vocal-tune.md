# Vocal tuning: the note model, automatic targets and the bake

`@vocal_tune` tunes one audio clip. It stores a human's per-note decisions and
an automatic policy for the notes that have none: snap to the scale, follow a
reference take, or both with the reference preferred. The engine matches the
decisions to the derived `.ocn` analysis, resolves a target for every detected
note, and applies the shifts to the clip's playback window with the offline
pitch shifter (`oc-dsp/src/vocal_shift.rs`). The same decision can reshape the
note's vibrato: `flatten` holds it at the measured median and `scale` keeps half
its measured depth. A project that carries the block renders with the tuning
applied, and `ocp vocal apply` freezes the same bake into a WAV that the project
can read instead.

`@vocal_harmony` adds a second voice to a tuned clip: the same bake, shifted by
a diatonic interval inside the clip's scale. See **Harmony voices** below.

The block requires DSL `0.3.0`, the automatic fields `0.4.0`, the timing
fields `0.5.0` and `@vocal_harmony` `0.6.0`. The bake applies every vibrato
mode. Projects written for `0.2.0` parse and render unchanged, a
block that declares `snap: off` keeps the pre-1.5 behaviour, and a block with
no timing field takes the pre-1.6 bake path bit for bit.

- `crates/oc-dsl/src/ast.rs` — `VocalTune`, `VocalTuneNote`, `VocalTarget`,
  `VocalSnapPolicy`, `VocalHarmony`.
- `crates/oc-dsl/src/{grammar.pest,parser.rs,serializer.rs,validator.rs}` — the
  block syntax, `fmt` and validation.
- `crates/oc-engine/src/vocal_snap.rs` — the automatic choosers: scale
  candidates, the contour tie-break, octave folding and reference matching.
- `crates/oc-engine/src/vocal_tune.rs` — anchor matching, resolution for both
  stored and automatic targets, the shift curve, the bake, the render-time bake
  and the cache key.
- `crates/oc-engine/src/vocal_harmony.rs` — the harmony voice: interval
  resolution, the same shift curve, the voice bake, its cache key and the mix.
- `crates/oc-dsp/src/vocal_shift.rs` — the offline TD-PSOLA shifter and its
  report.
- `crates/oc-cli/src/main.rs` — `ocp vocal apply`.

## The block

```ocp
@vocal_tune lead_tune | clip: lead
  scale: A major
  snap: auto
  reference: guide_take
  cap: 100ct
  window: 35ct
  max_move: 120ms
  amount: 60%
  retune: 25ms
  formant: 0ct
  vibrato: flatten
  notes:
    - at=0.480s tgt=A3 amount=100% retune=10ms vibrato=preserve
    - at=2.013s tgt=+2st amount=60% start=-30ms
    - at=5.470s tgt=5@4 start=+15ms length=1.08
    - at=6.100s bypass=true
```

The header selects the tuned clip with `clip: <audio_clip name>` or, when the
track owns exactly one clip, `track: <track name>`. Exactly one selector is
required and one clip can have one block.

| Field | Default | Meaning |
|---|---|---|
| `scale` | `inherit` | Scale used by degree targets and scale snap. `inherit` reads `@project key` / `key_map` at the note's position. |
| `snap` | `auto` | Where notes without a stored target get one: `auto` (reference when a reference is set, else the scale), `scale`, `reference` or `off`. |
| `reference` | none | Audio clip whose pitch contour automatic targets follow. |
| `cap` | `100ct` | Largest scale-snap correction in cents. A wider move is not proposed. `0..=1200ct`. |
| `window` | `35ct` | Ambiguity window for the contour tie-break. `0..=200ct`. |
| `max_move` | `120ms` | Largest note move and largest length change the block accepts. `0..=500ms`. |
| `amount` | `100%` | Default correction for notes that do not carry their own. `0%` leaves the note's pitch alone and disables the automatic pass. |
| `retune` | `25ms` | Glide into and out of a note's shift. |
| `formant` | `0ct` | Formant shift in cents, independent of the pitch. Positive raises the formants. Clamped to ±300; `0ct` bypasses the envelope pass. |
| `vibrato` | `preserve` | `preserve` keeps the performer's vibrato, `flatten` holds the note at its measured median, `scale` keeps half its measured depth. The depth comes from the analysis frames; see **Vibrato editing**. |
| `notes` | empty | Sparse overrides, one line each, ordered by `at`. |

`snap: reference` requires `reference:`; `snap: scale` with `reference:` is an
error rather than a silently ignored line; `snap: off` may keep a `reference:`
for the next edit, because `off` means "no automatic target at all". The
reference must be another audio clip of the same project.

Per-note items:

| Item | Meaning |
|---|---|
| `at=2.013s` | **Required.** The detected onset this decision was authored against. Accepts `123ms` or `1.5s`. |
| `tgt=...` | Target pitch (below). Without it the note follows the automatic policy. |
| `amount=60%` | Overrides the block default for this note. |
| `retune=25ms` | Overrides the block default glide. |
| `formant=-150ct` | Overrides the block default formant shift (same ±300 clamp). |
| `start=+30ms` | Move this note later by 30 ms (negative moves earlier). Relative to the detected onset, so the note's pitch decisions and the take stay the reference. |
| `length=1.08` | Length multiplier on the detected note span; `1.0` keeps it. `0.25..=4.0` in the file, and the resulting change is capped by `max_move`. |
| `vibrato=flatten` | Overrides the block default for this note. `preserve` restores the performer's vibrato on a note the block flattens or scales. |
| `bypass=true` | Leave this note alone, including the block defaults, the automatic policy and its timing fields. |

Targets are written in the note's own terms:

| Form | Example | Meaning |
|---|---|---|
| Pitch name | `tgt=A3`, `tgt=F#4`, `tgt=Bb2` | Absolute pitch. Octave numbers are MIDI's: C4 = middle C = MIDI 60. |
| Absolute cents | `tgt=5700ct` | Pitch in cents from MIDI 0: C-1 = 0, A4 = 6900. Unsigned on purpose. |
| Semitone offset | `tgt=+2st`, `tgt=-12st` | Transpose the detected pitch by semitones. |
| Cent offset | `tgt=+25ct`, `tgt=-25ct` | Transpose the detected pitch by cents. The sign distinguishes it from absolute cents. |
| Scale degree | `tgt=5@4` | One-based degree `5` in the tonic's octave `4`. `1@4` in A major is A4; `3@4` is C#5. |

Scale degrees use the key's mode: major is `0 2 4 5 7 9 11` semitones above
the tonic, minor is natural minor `0 2 3 5 7 8 10`. The octave belongs to the
tonic, so a degree can cross an octave boundary the way a pitch name does.
Degrees are diatonic; there is no chromatic degree spelling.

Validation bounds: `amount` is `0%..=100%`, `retune` is `0..=1s`, `formant` is
`-1200..=1200ct` in the file (the bake clamps above ±300, see **Formant
control**), `cap` is `0..=1200ct`, `window` is `0..=200ct`, `max_move` is
`0..=500ms`, `start` is `±max_move`, `length` is `0.25..=4.0`, absolute cents
are `0..=12700`, cent offsets are `-4800..=4800`, semitone offsets are
`-48..=48st`, degrees are `1..=7` and octaves `-1..=9`. Anchors must be
non-negative and strictly ascending. Unknown fields are rejected with the
accepted set, for example `unknown @vocal_tune field 'timing'; expected scale,
snap, reference, cap, window, max_move, amount, retune, formant, vibrato or
notes`.

## Only decisions are stored

The project file holds what the human decided and nothing the analyzer
produced: no f0 frames, no note boundaries, no detected frequencies, no chosen
targets. The one analysis-derived value is the `at` anchor, and it is stored
because it is the only stable way to say "this decision belongs to that
detected note" after the project is closed and reopened. On a re-analysis the
anchor is matched with a 30 ms tolerance; when nothing matches, the decision is
reported as stale and left untouched rather than silently re-aimed at a
different note.

Automatic targets are not stored either. The policy (`snap`, `reference`, `cap`,
`window`, `scale`, `vibrato`) is, because it is what the human asked for; the
resulting targets and the vibrato contour are recomputed from the analysis and
the policy every time, so an analyzer update re-derives them instead of reading
stale values.

Timing is stored the same way: `at` is the absolute anchor, `start=` is the
human's move relative to the detected onset, and `length=` is a ratio of the
note's reported span. The absolute destination is recomputed from the analysis every
time, so a re-analysis (or a re-recorded take) moves the note relative to what
the singer actually did instead of pinning it to a stale absolute time.

The anchor comes from the analyzer's *detected onset*, not from the reported
frame span. A note that opens out of silence flips the voicing decision 0.45 of
an analysis window before the event, so the reported start sits up to 21 ms
early; anchoring a decision there would place every edit most of a window away
from the attack. `at` is matched against `onset_s`, and `start=` is measured
from it. The span an edit moves stays the reported frame span, so the bake of a
block with no timing fields is byte-identical to before and the note grid keeps
reading the grid. [`vocal-analysis.md`](vocal-analysis.md) has the two spans and
the measured cost of the window smear.

## Resolution rules

Stored overrides are matched and resolved first; the automatic pass then fills
every detected note the block did not target itself.

1. **Match.** For each override, the nearest detected note onset within 30 ms of
   `at` is the target. A stored anchor usually matches its own note; a
   re-analysis that moves a boundary further than that reports
   `stale-anchor`.
2. **One override per note.** Two anchors that match the same detected note are
   an error, not an implicit merge. Validation also requires anchors to be
   strictly ascending, so the file order is the note order.
3. **Confidence.** A note whose analysis confidence is below `0.5` is left
   untouched and reported as `low-confidence`, whether it has a stored target
   or would have received an automatic one. This is the analyzer's documented
   consumer policy: a low-confidence note has no reliable pitch to tune.
4. **Target.** `tgt` is resolved to absolute MIDI cents. Degree targets use the
   block `scale`, or the key active at the note's timeline position from
   `key_map` (falling back to `@project key` when no map point is active).
5. **Shift.** `shift_cents = amount / 100 × (target_cents − detected_cents)`, so
   `amount=50%` moves half way to the target. The resulting frequency is
   `440 × 2^((detected_cents + shift_cents − 6900) / 1200)`.
6. **Octave crossings.** A target more than an octave from the detected pitch
   is resolved normally and reported with a warning; the resolution neither
   wraps nor clamps it. That catches a wrong octave in a degree target without
   refusing a deliberate octave jump. The shifter itself stops at ±1200 cents,
   so a larger resolved shift is clamped there and resolution adds a second
   warning when it would be.
7. **Bypass.** `bypass=true` skips the note before any target work, including
   the automatic pass and its timing fields.
8. **Timing.** After pitch resolution, every stored `start=`/`length=` becomes a
   destination span (see **Note timing**). Timing is independent of the pitch
   outcome: a note can be retuned and moved, and a low-confidence note can still
   be moved. It is not independent of matching: a timing field on an anchor that
   matched no detected note is an error, not a warning.

### Precedence

Every detected note gets exactly one of these, top to bottom:

| Order | Rule | Outcome |
|---|---|---|
| 1 | `bypass=true` on the matching override | `bypassed` |
| 2 | Analysis confidence below `0.5` | `low-confidence` |
| 3 | Stored `tgt=` | `shift` / `pass-through` |
| 4 | Reference note covering the take note (only under `snap: reference`) | `snapped` / `in-tune` |
| 5 | Scale snap, when a key is active | `snapped` / `in-tune` / `capped` |
| 6 | No key at all | `no-key` |
| — | `snap: off`, or block `amount: 0%`: only rules 1–3 run | `no-target` |

An explicit target therefore beats the reference, which beats the scale; a
bypass beats all of them; and a low-confidence note is never moved even if it
carries a target. A stale anchor never re-aims that decision, but the detected
note it would have claimed is still available to the automatic policy — the
stale warning says the decision was dropped, not that the take renders dry.
A project with no `@project key:` and no block `scale:` has nothing to snap to:
each note is reported as `no-key` and the block warns once.

Vibrato editing sits outside this order. It runs after target resolution for
every matched note the block covers, so a `capped`, `no-key` or `in-tune` note
can still have its vibrato flattened or scaled; only `bypass=true`, a stale
anchor and a low-confidence note skip it. A note that only the vibrato edit
moves reports `vibrato`.

Reported outcomes are `shift`, `pass-through`, `no-target`, `snapped`,
`in-tune`, `capped`, `no-key`, `low-confidence`, `stale-anchor`, `bypassed` and
`vibrato`. `ocp vocal apply --dry-run` prints them without writing anything.

## Automatic targets

A block out of `snap: off` proposes a target for every detected note that has
no stored `tgt=`. `snap: auto` (the default) means "reference if `reference:` is
set, else the scale"; `snap: scale` and `snap: reference` name one source
explicitly. The pass runs only when the block amount is above zero, so an
`amount: 0%` block proposes no pitch target. A vibrato mode is independent of
this pass and still edits the notes it covers.

### Scale snap

The scale is the block `scale` if set, else the `key_map` point active at the
note's timeline position, else `@project key`. Only the seven diatonic pitch
classes of the key are candidates: there is no chromatic spelling and no
"snap to the nearest black key". Every candidate is an equal-tempered MIDI
pitch, so the result is a pitch class plus an octave.

The chooser is one forward pass over the detected notes. Its tie-break order is
fixed and each rule has its own test:

1. **Tolerance.** If the nearest scale pitch is within `10ct`, the note is
   `in-tune`: its pitch is not corrected at all, not even by the residual, and
   with `vibrato: preserve` its samples are bit-identical to the input. A
   `flatten` or `scale` mode still edits its vibrato. (`vocal_snap::TOLERANCE_CENTS`.)
2. **Cap.** Candidates farther than `cap` (default `100ct`) are dropped. If none
   is left, the note is `capped`: it is left at its detected pitch and reported.
   The cap is what stops a note in a wide pentatonic gap from being dragged a
   long way. A note 90 cents from the nearest scale pitch with `cap: 50ct` is
   left alone.
3. **Window.** Of the two candidates either side of the note, if the nearer one
   wins by more than `window` (default `35ct`), it always wins: outside the
   window the nearest pitch is the answer.
4. **Contour.** Inside the window the choice is ambiguous, so the chooser
   compares the *performed* intervals with the *chosen* ones and takes the
   candidate with fewer disagreements. With `prev_detected → detected` and
   `detected → next_detected` as the performed moves, a candidate that flattens
   or reverses a performed move costs one; the candidate that keeps the line
   moving the way the singer moved wins. A take that rises into an ambiguous
   note and falls away from it snaps to the peak; a take that is falling
   through the window keeps falling.
5. **Distance.** Ties go to the candidate nearer the detected pitch.
6. **Pitch.** Remaining ties go to the lower pitch, which makes the pass
   deterministic.

The *next* note's contribution is its provisional target: a stored `tgt=` when
it has one, else its own nearest scale pitch. That keeps the pass a single
deterministic sweep — there is no global optimisation, no lookahead over later
decisions and no dependence on iteration order.

A note that is `capped`, `no-key` or `low-confidence` contributes no chosen
target, so the following note's contour starts from the last note that was
actually snapped. `in-tune` notes do participate: they are already the scale's
pitch.

### Reference snap

`reference: <audio_clip>` follows another take's pitch contour.

- **Alignment is by timeline time, not by note order.** Each take note's
  timeline start (`clip_start + detected.start_s`) is mapped into the reference
  clip's local time; the reference note whose span covers that instant, or
  whose onset is within 30 ms, is its counterpart. Ordinal alignment (the *n*th
  note of the take to the *n*th note of the reference) was rejected: it is only
  honest when both takes have identical note counts, and a dropped or split
  note would silently shift every later pairing.
- **Target.** The counterpart's `f0_median_hz`, in cents. No scale quantisation
  is applied on top: the reference is authoritative for the notes it covers.
- **Octave folding.** The target is transposed by whole octaves to within ±600
  cents of the take note. A reference an octave above or below the take
  therefore tunes the take in its own register instead of dragging it an
  octave; a reference interval within a fifth is untouched.
- **Tolerance.** A folded target within `10ct` of the take is `in-tune` and the
  note is not moved.
- **Cap.** The cap bounds scale snap only. A reference target is exact, because
  "tune to that take" is the point of the field; a reference correction wider
  than `cap` is reported on the note rather than refused.
- **Fallback.** Where the reference has no note (a rest, a different phrase
  length, a low-confidence note) the note falls back to scale snap. The count is
  reported as `reference_fallbacks` and warned once per block, so a partially
  covered reference is visible in `apply --json` rather than silent.
- **No overlap.** A reference clip that does not overlap the take on the
  timeline warns (`does not overlap the take`) and the whole block falls back to
  the scale. It does not silently do nothing.

`reference_notes` counts the notes that took their target from the reference.

### Chord context

There is no chord track in the DSL: `@project` carries `key` and `key_map`
(keys at positions) but no chord symbols, and the only "chord" in the format is
several notes sharing one position in a pattern. Chord-tone preference would
have to invent a chord analysis from unrelated material, so this analysis does
not do it: scale snap is key-aware only. `key_map` is still honoured per note,
so a key change inside the clip is followed.

### Determinism

The same project and the same analysis produce the same proposals, in the same
order, every time: candidates are enumerated from the MIDI grid, the pass is
forward-only, and the final tie-break is the pitch. A change to any of these
rules is a change to what a bake contains, so `BAKE_VERSION` moved to 3 in this
analysis and the automatic fields joined the decision stamp.

### Measured on synthetic takes

`crates/oc-engine/tests/vocal_snap.rs`, 48 kHz, synthetic tones, measured with
an autocorrelation oracle that shares no code with the detector:

| Take | Gate | Measured |
|---|---|---|
| C4 E4 D4 F4 E4, each 20–40 ct sharp, `scale: C major` | chosen degrees C4 E4 D4 F4 E4, no zigzag | **exactly that sequence**, worst **0.052 ct** after the bake |
| One A4 note already in tune, `vibrato: preserve` | untouched bit for bit | **bake == playback window, f32 bits** |
| C4 + 90 ct with `cap: 50ct` | left alone and reported | **`capped`, no shift, bake == window** |
| Reference an octave up (take 30 ct sharp) | folded, not dragged an octave | **target A3 (5700 ct), +0.048 ct after the bake** |
| `ocp vocal apply` with only `scale: C major` | ≤ 5 ct | **+1.443 ct** (oracle resolution at 220 Hz) |
| Render of the same scale with `snap: off` | byte-identical to no block | **identical bytes** |

## Vibrato editing

`vibrato` is independent of the pitch target. The resolver measures the
detected note's intra-note pitch movement from the analyzer frames and adds a
per-frame correction on top of the note's pitch shift:

- `preserve` adds nothing; the samples are the ones the pitch path would make.
- `flatten` targets the note's own `f0_median_hz`: each contour point shifts by
  `median - f0(t)`, so the note holds one pitch (the resolved target when there
  is one, the median otherwise).
- `scale` targets `median + 0.5 * (f0(t) - median)`: each point shifts by
  `-0.5 * (f0(t) - median)`, so the note keeps half its measured depth. The
  file grammar carries no factor, so 0.5 is fixed; a factor would need a new
  value form.

The measurement uses only the analysis the bake already reads. Frames inside
the detected span that the detector did not voice, or whose frame confidence is
below the 0.5 floor, are filled by linear interpolation from their neighbours.
The note's depth is half the 5th..95th percentile spread of the voiced frame
deviations; a depth below 1 cent is not resynthesised, so the vibrato part of a
steady note stays a pass-through (its pitch target still applies). The
per-frame track is then interpolated onto a 1 ms contour: the
shifter reads the curve at each grain (about one period), and a 5 ms step would
add a step of its own to the output.

The contour is added to the note's pitch shift and tiled over the note's
destination span, so a timing edit stretches the vibrato shape with the note.
`amount` does not scale it: `flatten` removes the measured swing even at
`amount: 0%`, because the vibrato is the take's own movement, not a target.
`bypass=true`, a stale anchor and a low-confidence note skip the vibrato edit
for the same reasons they skip a pitch target. A note moved only by its vibrato
reports `vibrato`; a note that also has a target reports `shift`. `--json`
carries the mode, the measured input depth and the contour point count.

Two limits are deliberate. The mode edits **depth against the median**, not the
vibrato **rate**: a 6 Hz swing and a 4 Hz swing flatten to the same steady
pitch, and a slower pitch drift inside the note is flattened with the vibrato.
And the measurement is the analyzer's, not the source's: its 40 ms
autocorrelation window averages a fast swing, so a 40 cent 6 Hz tone measures
about 35 cents of frame deviation. `flatten` removes that measured swing; the
few cents the window cannot see remain as residual depth.

## What the bake does

The bake runs the time pass (`oc_dsp::vocal_shift::time_warp`) first, only when
a note carries a timing edit, and then one call to
`oc_dsp::vocal_shift::shift` per clip on the resulting window:

1. **One segment per shifting note, over its detected span.** Each resolved
   note — stored decision or automatic proposal — that lands on a non-zero shift
   contributes a segment `[detected.start_s, detected.end_s)` in frames; a note
   with a timing edit contributes its destination span instead, so the pitch
   follows the note. A vibrato-edited note subdivides that span at its 1 ms
   contour points, so the shift varies inside the note. Nothing else in the
   window is in the curve, so a note with no shift, formant or vibrato edit is
   left bit-identical, and so is every unvoiced span. Segments are built in
   detected-note order, whichever list the decision came from.
2. **The glide is per note.** `retune` becomes that segment's slew in frames
   (the note's override, else the block default). The shifter therefore needs
   one call per clip, not one per retune group: the curve carries the values
   `oc_dsp::vocal_shift::ShiftCurve::from_ramped_segments` and
   `ShiftSegment` — the alternate design (grouping segments by retune and
   bridging the groups with extra ramp segments) would pay a level trim per
   group and shift the wet/dry crossfade to the group edges, for no gain.
   A change *into* a note's shift ramps over its `retune` after the note start;
   a change back to dry ramps over the same span before the note end. Zero
   spans stay exactly zero, which is what makes them bit-identical. A vibrato
   note turns `retune` into an envelope over its two edges; its interior
   contour points use a one-point slew, so the glide does not round the
   measured shape.
3. **The shifter owns the boundary.** Segments are not crossfaded by the
   engine: the shifter resynthesises each visible (shifted and voiced) run,
   crossfades wet to dry over 2 ms at the run edge, and reports its worst
   boundary transient as `boundary_step_ratio` (the worst sample step within
   20 ms of a boundary over the input's own worst step in the same window).
   Two adjacent notes with different shifts ramp down to dry and back up,
   which is why they do not click. The measured budget on synthetic takes is
   `≤ 1.5`; the two-note case measures 1.10.
4. **The block `retune` also configures the call.** It sets
   `ShiftConfig::ramp_frames`, which the shifter uses for the level-trim ramp,
   the wet/dry fade clamp and the formant filter's edge blend; per-note
   overrides only affect that note's own segment.
5. **Formant comes from the same segment.** A note's effective formant is its
   own `formant`, else the block default. A note with no pitch decision but a
   non-zero formant still contributes a segment (its `cents` is zero), so the
   envelope moves while the pitch stays. The engine clamps a request above
   ±300 cents to ±300 and reports a render warning; `bypass`, stale anchors and
   low-confidence notes do not get the formant treatment either. A vibrato note
   carries the formant on every contour segment under the same edge envelope,
   so the envelope and the pitch move together.

Consequences worth knowing before you listen:

- **Pitch moves, formants stay unless asked.** TD-PSOLA keeps the period; a
  shift beyond about ±3 semitones starts to sound like a chipmunk/formant
  mismatch because the spectral envelope travels with the source. Set
  `formant` when the note should change vocal-tract size independently; the
  safe range is ±3 semitones (see below).
- **Unvoiced spans pass through dry.** Consonants, breaths and silence inside
  a shifted note are not resynthesised. On breathy material that is audible as
  "half shifted" (the voiced part moves, the noise floor does not); it is
  deliberate, because shifting noise only makes it worse.
- **±1200 cents is the quality limit.** The grain policy is designed for ±12
  semitones; beyond that the shift is clamped. Large downward shifts also lift
  the peak (measured up to +2.2 dB at −700 cents) because the overlap-add
  becomes more constructive; the RMS is trimmed back, the crest is not.
- **22.05 kHz is not supported.** 44.1, 48 and 96 kHz are.

Measured through the whole pipeline (analysis → resolve → bake) on synthetic
48 kHz takes, with an autocorrelation oracle that shares no code with the
detector (`crates/oc-engine/tests/vocal_tune_shift.rs` for the explicit-target
rows, `tests/vocal_snap.rs` for the automatic ones):

| Take | Gate | Measured |
|---|---|---|
| One note, `tgt=+100ct` | f0 error p95 ≤ 2 ct | **p95 0.004 ct** |
| C major, eight notes each −40 ct | each ≤ 5 ct | **worst 0.047 ct** |
| ±25 ct / 6 Hz vibrato through a +100 ct shift | depth within ±10 % | **24.53 → 24.56 ct (+0.1 %)** |
| Same tone, grain-rate sidebands | ≤ −55 dBc | **−89.1 dBc** (window floor) |
| Two adjacent notes, +100 and −100 ct | click budget ≤ 1.5 | **1.100** |
| Five-note melody 20–40 ct sharp, scale snap | chosen degrees, ≤ 5 ct | **C4 E4 D4 F4 E4, worst 0.052 ct** |
| Reference an octave up | no octave drag | **folded target, +0.048 ct** |
| 90 ct away with `cap: 50ct` | untouched, reported | **`capped`, bit-identical** |
| 6 Hz ±40 ct vibrato, `vibrato: preserve` | depth within 5 ct of 40 | **39.10 ct** |
| Same tone, `vibrato: flatten` | residual depth below 8 ct | **5.63 ct** |
| Same tone, `vibrato: scale` | 20 ± 5 ct | **22.14 ct** |
| Two notes, one `vibrato=flatten` / one `vibrato=preserve` | only that note changes | **5.14 / 39.44 ct** |

The vibrato rows measure depth as half the 5th..95th percentile spread over
the note interior. The same tone's analyzer frame track reads 34.59 ct, not
40, because the 40 ms autocorrelation window averages a 6 Hz swing; the bake
follows that measured track, which is why `flatten` lands at 5.63 ct rather
than zero and `scale` at 22.14 ct rather than 19.55.

## Formant control

`formant` moves the spectral envelope by `2^(cents/1200)` while the pitch moves
by the tuning decision. Positive cents raise F1/F2/F3 (a smaller vocal tract),
negative cents lower them. It is a note-level decision: one envelope per
note-segment, estimated from the take and applied before the pitch resynthesis,
so PSOLA only has to preserve an envelope that is already correct.

**Method (ours).**

1. For each note segment, estimate the log magnitude envelope of every
   42.7 ms sqrt-Hann frame (75 % overlap) with a 4 ms cepstral lifter. The
   lifter resolves formants about 250 Hz apart and stays below the ~7 ms pitch
   comb of a 140 Hz voice.
2. Average the per-frame log envelopes over the note. A frame-by-frame gain
   would be a linear periodically time-varying system whose hop-rate modulation
   adds sidebands (measured at −3 dBc on a periodic vowel); one filter per note
   leaves a periodic excitation periodic.
3. Read the average envelope at `f / ratio`, clamp the log ratio to ±24 dB per
   bin, and inverse-transform it into a zero-phase FIR. The FIR is applied with
   block overlap-add convolution.
4. Run the PSOLA pitch path on the shaped take. When every formant is zero the
   step is skipped and the output is the pitch-only shifter's, bit for bit.

The envelope estimate is taken from the unshifted take, not from the PSOLA
output: at +1 semitone the shifted comb splits F2 into grain sidebands in a
42.7 ms estimate, which made a frame-based envelope move an octave-less but
wrong peak.

**Cap: ±300 cents (3 semitones).** The DSP accepts ±1200, but the measured
F1/F2 error through the LPC oracle that the tuner's acceptance table uses
starts at 5.6 % at −150 cents and reaches 8.9 % at −700, and the artefact
budget fails at +1200 (worst non-harmonic peak −52.8 dBc, above the −55 dBc
budget). At ±300 the LPC error is within 5 %, the clean-source sidebands are
−73/−68 dBc and the log-spectral distance matches the request. A request beyond
±300 is clamped to ±300 and reported as a render warning; it is not refused, so
an existing project keeps rendering with a documented, audible cap.

Measured on a 48 kHz source-filter vowel (pulse train + jitter/shimmer +
breath, F0 140 Hz, formants 700/1220/2600 Hz). F1/F2 are read from an
independent 170 ms cepstral envelope: the LPC root finder is biased by the
140 Hz comb once a formant lands between harmonics, and at ±200 it measures
−2.3…+3.8 % where the envelope measures −0.2…+1.1 %. LSD is the log-spectral
distance between the take and the shifted envelope over 0.1–5 kHz; "request" is
the distance between the take's envelope and its own warp.

| Request | F1 error | F2 error | LSD measured | LSD request | clean-source spur |
|---|---|---|---|---|---|
| −1200 ct | +2.5 % | +0.5 % | 12.40 dB | 12.49 dB | −79.8 dBc |
| −700 ct | +2.8 % | −0.8 % | 9.65 dB | 9.67 dB | −72.0 dBc |
| −300 ct | +0.3 % | −0.6 % | 6.39 dB | 6.47 dB | −73.2 dBc |
| −200 ct | +1.1 % | −0.2 % | 4.90 dB | 5.12 dB | — |
| −100 ct | −0.2 % | +0.3 % | 2.87 dB | 3.07 dB | −79.2 dBc |
| +100 ct | −0.2 % | −0.1 % | 2.93 dB | 3.16 dB | −78.7 dBc |
| +200 ct | +0.1 % | +0.7 % | 5.20 dB | 5.41 dB | — |
| +300 ct | −0.6 % | −0.1 % | 6.95 dB | 6.99 dB | −67.6 dBc |
| +700 ct | −0.2 % | −1.0 % | 11.79 dB | 11.73 dB | −66.1 dBc |
| +1200 ct | −0.4 % | +1.9 % | 15.88 dB | 15.88 dB | −52.8 dBc |

The spur column is measured on a strictly periodic harmonic vowel at 200 Hz
(no jitter, shimmer or breath), so it isolates the processing. On the jittery
vowel the source's own sidebands sit at −4…−15 dBc and the formant pass does
not raise them. Pitch measured on every output above stays within 0.001 cents
of 140/200 Hz; the pitch shift is unchanged by the formant pass.

Bit identity: with `formant = 0` the shifter takes the pitch-only path
untouched. A fixed 24,000-frame buffer hashes to `55de4f5b93edc088`
(mono, −300 ct), `8beb264a2cf930bd` (mono, 0 ct), `bef4abadecedac75`
(mono, +100 ct) and `35fc28c2c953b894` (mono, +700 ct), the same values the
1.3 shifter produced; the stereo counterparts are
`c82b625bdefe4711`, `efb2460379ceb6a6`, `f872722eb2fa4b43` and
`d80e0a1e8314a63e`. The bake-level guard renders a block with `formant: 0ct`
byte-identically to the same block without the field.

CPU (release, 48 kHz stereo, one second of audio, best of three): pitch only
0.0127 s, formant only 0.0186 s, both 0.0194 s. The pass is offline only.

**Remaining gap.** There is no realtime formant path, and no intra-note
envelope motion: the decision is one envelope per detected note. If it ever ran
realtime the analysis window alone would add 42.7 ms of latency (21.3 ms if
centred) plus the block convolution; it does not, and the audio callback never
calls it.

## Note timing

`start=` moves a note and `length=` resizes it, both without changing its pitch.
Times are stored relative to the detected onset (`at` is the only absolute
anchor), in the DSL's usual time tokens: `start=+30ms`, `start=-0.030s`,
`length=1.08`.

| Field | Unit | Meaning |
|---|---|---|
| `start=` | ms (or s) | Offset from the detected onset. Positive is later. The block `max_move` caps it. |
| `length=` | ratio | Multiplier on the note's reported frame span. `1.0` keeps the length, `1.08` adds 8 %. |
| `max_move` | ms | Block field, default `120ms`: the largest move **and** the largest resulting length change. |

The move is relative, not absolute, because the performance is the reference:
a re-recorded or re-analysed take moves the note by the same 30 ms instead of
pinning it to a time the singer no longer occupies. It also makes the stored
value itself the thing the cap applies to.

### Validation

A timing edit is a decision, so a bad one is refused rather than fudged:

- `|start| ≤ max_move` at parse/validate time (the field is bounded statically).
- The resulting length change `|target_len − detected_len| ≤ max_move` at
  resolve time, because it needs the detected span. A `length=` on a 1 s note
  that asks for +300 ms is rejected even though `1.3` is inside the ratio range.
- The destination has to stay inside the take: a move that would start before
  the take or run past its end is rejected with the overshoot in ms. The take
  never grows; a timing edit cannot change the clip length.
- Destinations may not overlap. Every detected note owns a destination (its own
  span unless it was edited), and a move that lands on a neighbour is an error
  with both anchors and the overlap in ms. There is no automatic insertion,
  ducking or overlap fudge.
- A `start=`/`length=` on an anchor that matches no detected note is an error
  (`timing fields need a detected note`), not the warning a stale pitch decision
  gets: there is nothing to apply it to.
- `bypass=true` ignores the timing fields with the rest of the note.

### The time pass

The shifter is pitch-only, so time is a second pass over the same window:

1. The note's **detected span** is the source. Its linear map onto the
   destination span is the edit: a pure move has slope 1, a stretch has slope
   `target_len / source_len`.
2. The destination is resynthesised with grains one source period apart, each
   read from the analysis mark nearest the mapped source position. The output
   epoch grid therefore keeps the source period (**pitch unchanged**) and only
   the *number of periods* changes: a stretch repeats marks, a shrink skips
   them. The last grain is the note's own last period; the rest of the
   destination span is room, so a stretched note does not sustain past its end.
3. The pitch shifter then runs on the warped window, over the notes' destination
   spans, so a note can be retuned and moved at the same time.

The grain window, the fractional read and the window-sum normalisation are the
same primitives the pitch path uses (`oc-dsp/src/vocal_shift.rs`); the level is
unity because period add/drop does not make grains cancel the way a large pitch
lift does. The edge fade is 2 ms and the fade back to dry inside the destination
uses the same window-sum blend as the pitch path.

### What fills the gap

Moving a note leaves the take's room behind. The pass never synthesises room:
it lifts the note's whole source span and fills it with **the take's own room**,
copied from the closest free window of the same take (a span covered by no note
and no destination) that is long enough, crossfaded over 10 ms at the edges.
When the take has no unvoiced window at least 10 ms long, that much is copied
and the rest is faded to silence; the take has no more room to give.

### Measured on synthetic takes

`crates/oc-dsp/src/vocal_shift.rs` (time pass alone) and
`crates/oc-engine/tests/vocal_timing.rs` (analysis → resolve → time pass → pitch
shifter → WAV), 48 kHz, with an energy/autocorrelation oracle that shares no code
with the detector or with the time pass:

| Case | Gate | Measured |
|---|---|---|
| Time pass alone: one note moved +30 ms / −30 ms | onset ≤ 5 ms, pitch ≤ 2 ct | **residual +0.23 / +0.21 ms**, pitch **−0.003 ct**, seam **0.999×** |
| Time pass alone: stretch +40 ms / shrink −40 ms | length ≤ 3 ms, pitch ≤ 2 ct | **−0.77 / −1.35 ms** audible, pitch **≤ 0.004 ct** |
| Pipeline: one note moved +30 ms / −30 ms with `tgt=+100ct` | onset ≤ 5 ms, retuned pitch kept | **residual +0.00 / +0.00 ms**, pitch **−0.002 ct** |
| The same through `ocp vocal apply` (detector span, `start=30ms length=1.05`) | onset ≤ 5 ms | **+2.77 ms** residual (the note sits at the edge of the first grain), length span error **0.00 ms** |
| Five notes 10–40 ms off a 0.5 s grid, quantised by `start=` | every onset ≤ 5 ms, no note doubled or dropped | **+0.02 / +0.00 / +0.02 / +1.00 / +0.02 ms**; five audible regions; pitch ≤ **0.002 ct** |
| Pipeline: stretch +40 ms and shrink −40 ms | length ≤ 3 ms, pitch ≤ 2 ct, take length unchanged | span error **0.00 ms**, audible content-scaling error **−0.51 / −0.06 ms**, take length identical |
| A zero request on every note | bit-identical bake | **identical f32 samples** |
| A move onto the neighbour | rejected at validation | **`notes at 0.283s and 0.883s would overlap by 55 ms`** |
| Move into a room-only gap | the gap keeps the take's room | **−0.23 dB** of the room level, not digital silence |
| Pre-1.6 bake hash (pitch + formant, no timing) | unchanged | `3e34b19f91c12669` |

The seam budget is the same metric the pitch shifter reports
(`boundary_step_ratio`, worst sample step within 20 ms of a seam over the
largest input step in that window or in the edited note's own span) with a
budget of **≤ 1.5**; measured 0.999–1.002 on the moves and stretches above.

### What this cannot do

The pass repeats or drops pitch periods; it does not resynthesise the voice.

- **It cannot invent a longer syllable's formant transitions.** A stretched note
  holds the last period it has; the extra time is the same spectral envelope,
  not a new gesture. A shrunk note loses the periods between the marks it keeps.
- **It can only edit what the detector marked.** The detected note span is the
  source. If the analysis under-covers the note (a soft tail, a breath), the
  part outside the span does not move. A note that is legato with its neighbour
  cannot be moved into it; that is a validation error, not a hidden resize.
- **It cannot grow the clip.** A destination outside the take is refused. A
  bigger musical move needs a clip-level edit (stretch/offset), not a note edit.
- **It does not follow tempo changes.** `start=` is time, not a grid position;
  a tempo change moves the grid under the note, and the stored move stays a
  time offset. This is the honest v1 choice: the schema has no musical-time
  editing yet, and pretending to a grid would silently re-quantise on a tempo
  change.
- **Unvoiced content has no pitch marks**, so a span the detector found no marks
  in is moved by linear resampling. For a breath that is fine (its pitch is not
  a thing to preserve), but it is not a PSOLA move.

## The `amount: 0%` guarantee

With every effective amount at zero, every formant at zero and every vibrato
at `preserve`, no note contributes a segment, the curve is silent, and the bake
is a pass-through: the baked file contains exactly the clip's analysed playback
window, sample for sample, in 32-bit float stereo. A `flatten` or `scale` mode
is the deliberate exception: it moves samples even when the pitch amount is
zero, because it edits the take's own vibrato rather than a target.
The clip's audible window is the same buffer the analyser saw as a mono sum, so
this single equality proves the whole chain (media decode, resample, stretch,
offset, window slice, shifter, WAV round trip) is wired correctly. The bake
still calls the shifter; its zero-cents path returns the input unchanged, bit
for bit.

Two visible consequences:

- **The bake is sample-exact.** The baked WAV decodes bit-for-bit to the
  playback window, and rendering the baked project produces the same samples
  as rendering the source project.
- **The render is byte-identical.** A project that carries a zero-amount
  `@vocal_tune` block with `vibrato: preserve` renders to the same bytes as the
  same project without the block.

The applied project drops the block and reads the baked media, because the
decisions are baked into audio. Keep the input project if you want to keep
editing the decisions; `--in-place` is the deliberate "freeze this take" action.

## Harmony voices

`@vocal_harmony` derives a second voice from a tuned clip. The voice is the
clip's baked playback window shifted by a diatonic interval inside the clip's
scale, rendered as a second part on the clip's own track. The lead keeps its
samples: the block adds audio, it never moves the lead.

```ocp
@vocal_harmony third | clip: lead
  interval: 2
  level: -6dB
  pan: -0.3
```

The selector is the same one `@vocal_tune` uses: `clip: <audio_clip>` or, when
the track owns exactly one clip, `track: <track name>`. One clip can carry one
harmony block.

| Field | Default | Meaning |
|---|---|---|
| `interval` | `2` | Diatonic steps above the lead note. Negative puts the voice below. |
| `level` | `-6dB` | Voice RMS relative to the lead's RMS over the same notes. |
| `pan` | `0` | Stereo balance of the voice, `-1` left to `1` right. |

### The interval rule

The interval counts steps of the clip's scale, so `interval: 2` above C is E and
above D is F: it is a third, not two semitones. Resolution runs per detected
note:

1. **Key.** The clip's own `@vocal_tune` `scale` when it has one; otherwise the
   `key_map` point active at the note's timeline position; otherwise
   `@project key`.
2. **Degree.** The detected pitch matches the nearest scale pitch, the same grid
   scale snap uses and without the snap cap, so a note 30 cents sharp of C still
   counts as C.
3. **Step.** The voice takes the scale pitch `interval` positions further up (or
   down) that grid. The result is an equal-tempered scale pitch by
   construction, so the voice is in the key even when the lead is not.
4. **Range.** A move wider than ±1200 cents is refused with the note and the
   amount, for example `@vocal_harmony 'wide': C4 at 0.183s would move +1400
   cents; a harmony stays within ±1200 cents of the lead`. The shifter's ±12
   semitone limit is the reason; nothing is clamped or dropped silently.

A detected note below `0.5` confidence is left out of the voice, exactly as it
is left out of the tuning: there is no reliable pitch to transpose from. The
lead's samples around it are not touched.

### Without a scale

A clip with no `@vocal_tune` scale, no active `key_map` point and no
`@project key` has no key to be diatonic in. The interval then counts
**chromatic semitones**: `interval: 2` is a whole tone, not a third. Every
render reports it once per block (`no scale is active for clip 'lead'; interval
2 counts chromatic semitones`), so the fallback is never silent. A project that
wants a diatonic voice declares a key, on the tune block or on the project.

### The bake

The voice comes from the same call the tune pass makes, not from a second
shifter: one `oc_dsp::vocal_shift::shift` over the clip's window with one
segment per detected note span, each carrying its own glide, which is the curve
shape the tuning uses.

- **Formant handling follows the tune default.** The voice takes the tune
  block's `retune` for its glide and leaves the formant at `0ct`, so the
  spectral envelope travels with the source. A harmony is a second singer, not a
  second vocal-tract size; a harmony block cannot ask for a different envelope.
- **The voice follows the tuned lead.** The bake reads the lead's baked window,
  so a note the tuning moved is transposed from where the lead actually sounds.
- **Level.** One gain for the whole voice, computed so its RMS over the detected
  note spans sits `level` below the lead's RMS over the same spans. A per-note
  trim would step the level at every boundary; one gain keeps the voice steady.
- **Pan** is applied at mix time. A mono source pans with the constant-power law
  normalised to unity at the centre; a source that already carries a stereo
  image is balanced with the engine's send law. A mono source stays mono, a
  stereo one keeps its image.

### What the voice is not

- **The source is monophonic.** One detected note at a time is transposed, so a
  chord, a double or a polyphonic take gets one interval per detected note, not
  one interval per voice. Two singers in the source produce one harmony line.
- **There is no chord awareness.** The DSL has no chord track, so the interval
  is a scale degree, not a chord tone: a third above the lead is a third above
  the scale degree even when the harmony section sits on the fifth. Scale snap
  documents the same limit.
- **One block per clip.** A second `@vocal_harmony` on the same clip is a
  validation error, not a stack of voices; two voices need two clips.
- **The lead is not moved.** The mix adds the voice. It does not duck, pan or
  rebalance the lead, and the lead's bake is byte-identical to the same project
  without the block.

### Measured on a synthetic take

`crates/oc-engine/tests/vocal_harmony.rs`, 48 kHz, a five-note C-major melody
C4 D4 E4 F4 G4 with a three-harmonic tone, `interval: 2` and `level: -6dB`. The
pitch column is an autocorrelation oracle and the onset column an energy
oracle; neither shares code with the detector or the resolver.

| Lead | Voice | Measured f0 | Error | Onset | Level |
|---|---|---|---|---|---|
| C4 | E4 | 329.6 Hz | −0.015 ct | +0.00 ms | −6.00 dB |
| D4 | F4 | 349.2 Hz | −0.006 ct | +0.00 ms | −6.00 dB |
| E4 | G4 | 392.0 Hz | +0.013 ct | +0.00 ms | −6.00 dB |
| F4 | A4 | 440.0 Hz | +0.019 ct | +0.00 ms | −6.00 dB |
| G4 | B4 | 493.9 Hz | +0.048 ct | +0.00 ms | −6.00 dB |

Worst case: **0.048 cents**, **0.00 ms**, **0.00 dB** against gates of 10 cents,
2 ms and 0.2 dB. The same test checks that the lead's bake is bit-identical with
and without the block and that the mixed track is the lead plus the voice sample
for sample, and that two bakes of the same project produce the same bytes. An
`interval: 8` on the same take is refused with the note and the amount.

## What render does with an un-applied block

`ocp render` does not refuse a tuned project and does not ignore the block: it
bakes the clip on the fly through the same path `ocp vocal apply` uses, under
the same bake key. So a project renders tuned without running `vocal apply`
first, and a block whose anchors went stale renders dry (with the warning on
stderr) exactly as `apply` would report it.

The render-time bake is cached at
`<derived cache>/bake/v<BAKE_VERSION>.<ALGO_VERSION>/<clip>-<hash>.wav`
(the derived cache is the `.ocn` directory, `OCP_DERIVED_CACHE_DIR` or the
media cache directory). A second render reads that file and skips both the
analysis and the shift. The first render after a decision edit pays the
analysis and one PSOLA pass: about 0.013 s of CPU per second of stereo audio,
so a 3 minute take is roughly 2.4 s plus analysis.
Cache writes there are best effort; a read-only cache directory makes the
render slower, never wrong.

A block with no `notes:` at all is a valid, fully automatic block: the scale
or the reference supplies every target, and the bake moves the whole take. A
vibrato mode edits every detected note the block covers, including a block with
`snap: off` and no stored targets.

## CLI

```
ocp vocal apply <project.ocp> <clip-or-track> --output out.ocp
ocp vocal apply <project.ocp> <clip-or-track> --in-place
ocp vocal apply <project.ocp> <clip-or-track> --dry-run --json
```

The selector is an `@audio_clip` name first, then a track that owns exactly one
audio clip. The analysis is read from the `.ocn` cache and recomputed on a miss,
so `apply` works on a project that was never analysed by hand.

- `--output <FILE>` writes the baked project. It must live in the source
  project's directory: every media path in an `.ocp` is relative, and moving the
  project would silently break the other clips.
- `--in-place` writes the baked project back to the source path.
- `--dry-run` analyses and resolves, then reports; no audio and no project is
  written.
- `--sample-rate` (default 48000) is the rate of the baked buffer and the
  analysis. The baked media is fixed at that rate; render the applied project at
  the same rate for sample-exact output. Other rates resample the bake once.
- `--json` prints the per-note resolution, the automatic proposals, cache
  results and warnings. `snap_policy` is the policy that ran (`auto` is
  resolved to `scale` or `reference`), `reference` names the reference clip,
  `reference_notes` / `reference_fallbacks` count the automatic notes, `notes`
  are the stored overrides and `proposals` are the automatic targets. Each note
  carries a `timing` object when it was moved or resized:
  `requested_move_ms`, `applied_move_ms`, `onset_residual_ms`,
  `requested_length_ms`, `applied_length_ms`, `length_error_ms` (baked span
  minus requested span), `audible_length_ms`, `audible_length_error_ms` (audible
  content minus the input's audible length scaled by the requested ratio) and
  `audible_onset_seconds`. A note a vibrato edit applies to also carries a
  `vibrato` object with `mode`, `detected_depth_cents` and `points`. The
  top-level `time` object reports the timing pass: `notes`, `edited_frames`,
  `room_frames`, `boundary_step_ratio`, `peak`. Table mode prints the same rows
  under `timing edits:`.

Baked media goes to
`<output-stem>.vocal/v<BAKE_VERSION>.<ALGO_VERSION>/<clip>-<hash>.wav` next to
the output project; for `--in-place` that is `<project-stem>.vocal/...`. The
directory is disposable: delete it and the next apply recomputes. On a bake
miss `--json` additionally carries the shifter's report (`shift.wet_frames`,
`shift.boundary_step_ratio`, `shift.peak`), which is how a bake can be checked
without listening to it; a cache hit reports `shift: null` because the shift
was not run again.

## Cache keys

The analysis key is unchanged:

```
StretchKey(MediaKey, target_rate, ratio_bits, stretch_mode, stretch_algo_version)
  | offset_frames | length_frames | ANALYZER_VERSION | detector_params_hash
```

The bake key extends it with the decisions, the reference and both algorithm
versions:

```
bake stamp = analysis stamp | FNV-1a(decision stamp) | reference | BAKE_VERSION.ALGO_VERSION
file name  = <clip>-<FNV-1a(bake stamp)>.wav
directory  = <output stem>.vocal/v<BAKE_VERSION>.<ALGO_VERSION>
```

`reference` is the hash of the reference clip's analysis key when the block
names one, and the literal `none` otherwise: a re-recorded or re-stretched
reference take has to invalidate the bake of the take that follows it.
Computing it reads the reference's playback window, so a reference block pays
one extra decode per render; the reference's own analysis is still cached.

`ALGO_VERSION` is `oc_dsp::vocal_shift::ALGO_VERSION`. It is composed into the
same version field rather than added as a parallel key, so a change to the
shifter alone renames both the directory and the file; `BAKE_VERSION` is bumped
when the bake itself changes. It is 5 for vibrato editing (4 added note timing,
3 added the automatic targets and the formant-aware bake), and `ALGO_VERSION`
is 3 because the time pass lives in the same module and can move samples.

The decision stamp covers every field that can move samples: the block amount,
retune, formant, vibrato policy, scale, `snap`, `reference`, `cap`, `window`,
`max_move`, and each note's anchor, target, amount, retune, formant, `start`,
`length`, `vibrato` and bypass. Editing any
of them renames the bake, which is a miss by construction; an unchanged block
hits the existing file after a header check (stereo, 32-bit float, expected
rate and frame count). `BAKE_VERSION` is also in the directory name, so a
version bump never even looks at older files. A stale `.ocn` (different analyzer
version, detector parameters, media or stretch identity) is a transparent
re-analysis, exactly as before.

### The harmony key

A harmony voice reuses the lead's bake key and adds the voice fields that can
move its samples:

```
harmony stamp = bake stamp | FNV-1a(voice stamp) | HARMONY_VERSION
file name     = <clip>-harmony-<FNV-1a(harmony stamp)>.wav
directory     = <derived cache>/bake/v<BAKE_VERSION>.<ALGO_VERSION>
```

The voice stamp covers the block name, `interval` and `level`. `pan` is not in
it: the balance is applied when the voice is mixed, so moving the voice in the
image does not rename a finished bake. `HARMONY_VERSION` is 1 and lives beside
`BAKE_VERSION`, which stays 4 because the tune bake's samples did not move: a
harmony edit renames only the harmony file, never the lead's bake, and a shifter
change invalidates both through `ALGO_VERSION`. The range render's per-track
clip fingerprint also carries the voice stamp, so a cached range window is
re-rendered when the interval changes.

## What later versions add

- Vibrato editing changes depth against the median, not rate or shape. A rate
  change or a depth envelope across the note would need a new parameterisation
  and a different measurement than a single depth.
- A timing edit is stored as time, not as a grid position, so tempo-following
  quantise needs a musical-time field first (see **Note timing**).
- Moving a note onto a neighbour stays an error; insertion/ripple edits are an
  editor-level feature, not a resolver rule.

Not yet: an editor, polyphony, a merge rule for overlapping overrides, note
split/merge commands, real-time correction, chord-aware targets (no chord track
exists), formant motion inside a note, vibrato rate or shape editing,
legato-aware timing that would split a shared boundary, more than one harmony
voice per clip, and a harmony over a polyphonic source. The bake shape stays as
it is: analysis stamp, decision stamp, reference identity, both versions, one
bake per clip, plus the harmony key for a voice.
