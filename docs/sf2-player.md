# SoundFont 2 player

`oc_engine::sf2` reads `.sf2` banks and plays them: our own reader and voice.
It exists so a shipped app needs no SoundFont library on the user's machine.

It is the **only engine** for every SoundFont track.

```text
crates/oc-engine/src/soundfont/
  mod.rs        the renderer every render path builds
  shared.rs     shared-access wrapper for the audition callers
```

```text
crates/oc-engine/src/sf2/
  riff.rs       RIFF/sfbk chunk reader
  font.rs       INFO, sdta, pdta -> presets whose zones are fully resolved
  generator.rs  opcode subset and the per-zone generator set
  modulation.rs modulator records, the default modulator set, the vibrato LFO
  player.rs     channel state, program/bank selection, note handling, mixing
  sends.rs      the player's chorus and reverb buses and the send level law
  voice.rs      one note over one zone: pitch, loop, envelope, filter, pan
```

One bank is parsed once and shared (`SoundFont::load_shared`) — the waveform
lives in a single `Arc<SoundFont>`, never one copy per track.

For compressed SF3 banks, live preparation indexes the demuxer's emitted
packet durations before decoding the admitted program's samples. An Ogg end
granule alone can understate the PCM produced by a single audio page. Packet
extents preserve the complete loader's sample addresses without decoding
unrelated programs or relaxing the preparation memory bound.

## Engine adapter

`oc_engine::soundfont::SoundFontRenderer` is the one type the render paths
build: offline renders, the parallel track pool, the render cache's per-font
groups, the pattern previews and the streaming `BlockSource`. It offers
`new`, `enable_drum_channel`, `send`, `render_into`, `render_samples`,
`try_render_events`, `set_dynamic_layers`, `set_dynamic_ramps`,
`install_hairpin_layers`, `set_project_frame` and `sample_rate`, so switching a
call site is a `let mut` and one constructor
change. `render_into` writes into a caller-owned buffer; the
streaming path calls it on the reserved block and allocates nothing per block.

There is one engine, so nothing selects a backend at run time: `new` loads the
bank and the renderer always plays it through `oc_engine::sf2`. The public
render API (`render_to_buffer*`, `render_to_wav*`, `BlockSource`, the render
cache) and the CLI therefore take no engine parameter, and a project file never
records one.

## Reader

| Chunk | Handling |
|---|---|
| `ifil`, `INAM`, `isng` | version and bank name; other INFO strings are skipped |
| `smpl` | 16-bit points, or 24-bit points combined from `smpl` + `sm24`, or 24-bit points framed as three bytes inside `smpl` (detected from the header end offsets) |
| `phdr`, `pbag`, `pgen` | preset headers, zones and generators |
| `inst`, `ibag`, `igen` | instrument headers, zones and generators |
| `shdr` | sample headers: offsets, loop points, rate, root key, pitch correction, type |
| `pmod`, `imod` | per-zone modulator lists; the records that target a modelled modulation destination are applied, the rest is counted in a warning |

The reader is strict about structure and tolerant about content. A chunk whose
size overruns its parent, a record chunk whose length is not a whole multiple of
the record size (the 38-byte preset header trap), an odd chunk length, a zone
that names a missing sample, an inverted range or a sample with no audio all
either parse or are reported in `SoundFont::warnings` — never a panic. A bank
whose sample data is 3-byte framed is reported as a warning because the offsets
switch meaning. The terminal `phdr` record ends the last preset's zone list; it
is not a preset, so it never adds a phantom zone.

## Generator subset

Audibility only. Everything else is dropped at parse time and counted in a
warning (real GM banks lose a few thousand modulator-driven generator records).

| Group | Generators |
|---|---|
| Range | `keyRange`, `velRange` (intersected across preset and instrument) |
| Sample | `sampleID`, `startAddrs`/`endAddrs`/`startloopAddrs`/`endloopAddrs` offsets including the coarse variants |
| Pitch | `overridingRootKey` (falls back to the header key), `coarseTune`, `fineTune`, `scaleTuning` |
| Level | `initialAttenuation`, `pan`, `exclusiveClass` |
| Envelope | `delayVolEnv`, `attackVolEnv`, `holdVolEnv`, `decayVolEnv`, `sustainVolEnv`, `releaseVolEnv`, `keynumToVolEnvHold`, `keynumToVolEnvDecay` |
| Sample behaviour | `sampleModes` (0 off, 1 continuous, 3 until release) |
| Filter | `initialFilterFc`, `initialFilterQ` |
| Vibrato LFO | `vibLfoToPitch`, `freqVibLFO`, `delayVibLFO` |
| Modulation LFO | `modLfoToPitch`, `modLfoToFilterFc`, `modLfoToVolume`, `freqModLFO`, `delayModLFO` |
| Modulation envelope | `modEnvToPitch`, `modEnvToFilterFc`, `delayModEnv`, `attackModEnv`, `holdModEnv`, `decayModEnv`, `sustainModEnv`, `releaseModEnv`, `keynumToModEnvHold`, `keynumToModEnvDecay` |
| Effect sends | `chorusEffectsSend`, `reverbEffectsSend` (tenths of a percent) |

Preset-level numeric generators are added to the instrument's; ranges are
intersected; the first zone without a `sampleID` (instrument) or `instrument`
(preset) generator is the global zone. `sampleID` and `instrument` are the only
generators that must appear last in their zone, and real banks do that.

## Level laws

These were measured against the renderer this replaces, because the parity
target is "the same project sounds the same", not "the specification's letter":

| Law | Behaviour |
|---|---|
| Velocity | amplitude `(velocity / 127)^2` |
| CC7 / CC11 | amplitude `(value / 127)^2`; the MIDI default volume is CC7 = 100 |
| `initialAttenuation` | 0.4 centibels per unit (0.04 dB) using the player's generator scale. A modulator record's amount is already centibels and takes no factor |
| `sustainVolEnv` | 0.96 dB per unit (a centibel scale with the previous renderer's 4 % offset) |
| `pan`, CC10 | constant power, `cos/sin` of `(pan + 1) * pi / 4`, hard pan 3 dB above centre |
| Volume envelope | attack ramps the gain linearly in amplitude; decay and release ramp it exponentially at 96 dB per stage time, so a shallow sustain level is reached early and held; the envelope floor is 96 dB; a zero-length stage completes in one sample |
| Filter | two-pole low-pass from `initialFilterFc`/`initialFilterQ`; the cutoff is the base plus the modulation sums, clamped to `1500..13500` absolute cents (19.4 Hz .. 19.9 kHz); bypassed when the clamped total is the 13 500-cent default |
| Pitch modulation | vibrato + mod LFO + mod envelope sums, clamped to ±12 000 cents, applied as the frequency ratio `2^(cents / 1200)` |
| Volume modulation | `modLfoToVolume` is centibels of attenuation, bipolar around unity: `gain = 10^(-cB * LFO / 200)`, so 100 cB is ±10 dB |
| Attenuation modulation | a record's amount is centibels: `gain = 10^(-cB / 200)`. It adds to the generator's attenuation and the total clips at 144 dB; a negative record can only undo the generator down to no attenuation |
| Pan modulation | a record's amount is `pan` generator units (-500..500) and sums with the zone's own pan and CC10 before the constant-power law |
| Effect sends | linear in the send value: `clamp(zone + records + CC, 0, 1000)/1000` of the voice output goes to the bus; the wet return uses the player's chorus and reverb buses |
| Drum channel | channel 10 selects bank 128; a kit that the bank does not declare stays silent |

`SoundFont::sample_rate_of` scales the bank's rate against the output rate, so a
44.1 kHz bank plays at the same pitch at 48 kHz.

## Dynamic layers

A phrase `@0 cresc` or `@0 dim` mark sends CC11 expression to the player, and
the player also follows that ramp's position through the bank's velocity
layers. A note that starts inside the mark's span sounds the layers it would
select between the start and target dynamics, crossfaded linearly in amplitude
and moving with the ramp, so the spectrum follows the level instead of only the
level moving. A pattern mark keeps a gain clip instead, because a pattern may
name its own preset.

Two details keep the documented level law intact:

- Each layer voice normalises its velocity gain to the note's own velocity, so
  CC11 remains the only thing that raises the level. The layer mix changes the
  spectrum, not the level.
- The crossfade runs on the output frame, not on the note's progress, so a note
  that starts late in the swell enters at the layer the ramp is already at.

A bank with one velocity layer for the note renders exactly as before: the
crossfade needs at least two layer signatures, and a project without a mark
installs no ramps at all, so its bytes do not move. The layer ladder is capped
at eight layers per note.

Live monitoring renders the same blend. The streaming lane receives the track's
ramp table in project frames and the project frame it joins at, then only moves
its clock as blocks advance, so it computes the ramp position for a block from
the table and allocates nothing per block. A block that starts before the mark
or after it keeps hard layer selection, exactly as offline does; a seek moves
the clock and the next block reads the ramp where the new position falls.
Streaming and offline output agree within 1e-6 per sample over the same span of
a phrase under a mark.

The crossfade is **on by default** for a SoundFont track. Opt out with
`dynamic_layers: off` in the track's `engine_params:` or the preset's `params:`;
the track value wins. Both the offline and the streaming path then get hard layer
selection, the behaviour before this feature: the mark moves level only. The
synth voice's own velocity-to-level law is unchanged; this path is the SoundFont
player's alone.

```text
@track strings | preset: warm_strings, instrument: strings/violin
  engine_params: {dynamic_layers: off}
```

## Modulation

Three per-voice sources, one summing point per destination.

| Source | Destination | Unit | Law |
|---|---|---|---|
| vibrato LFO | `vibLfoToPitch` | cents | sine in [-1, 1], rate from `freqVibLFO` (absolute cents, origin 8.176 Hz), delay from `delayVibLFO` |
| modulation LFO | `modLfoToPitch` | cents | same oscillator shape, `freqModLFO` / `delayModLFO` |
| modulation LFO | `modLfoToFilterFc` | cents | added to the zone's `initialFilterFc` |
| modulation LFO | `modLfoToVolume` | centibels | bipolar attenuation: `gain = 10^(-cB · LFO / 200)` |
| modulation envelope | `modEnvToPitch` | cents | envelope value in [0, 1] times the depth |
| modulation envelope | `modEnvToFilterFc` | cents | envelope value times the depth |
| any record | `initialAttenuation` | centibels | added to the generator, total clipped at 144 dB |
| any record | `pan` | generator units | added to the zone's and CC10's pan, then the constant-power law |

The modulation envelope is its own six-stage generator set
(`delayModEnv`/`attackModEnv`/`holdModEnv`/`decayModEnv`/`sustainModEnv`/
`releaseModEnv`, plus `keynumToModEnvHold`/`keynumToModEnvDecay`). It runs the
same state machine as the volume envelope: the attack ramps linearly in
amplitude, decay and release ramp at 96 dB per stage time, and `sustainModEnv`
is a centibel attenuation on the same 0.96 dB-per-unit scale as
`sustainVolEnv`. A sustain of 0 is full scale, so a zone that only sets
`modEnvToFilterFc` gets a static cutoff offset after the (default 1 ms) attack.

Every destination is summed in its own unit and in a fixed order: the zone's
generator, then the format's default modulators, then the bank's own records.
Pitch is clamped to ±12 000 cents, the cutoff to the legal 1500..13500 absolute
cents, and the volume depth to ±960 centibels.

`initialAttenuation` and `pan` do not move with an LFO or an envelope, so the
voice folds their sums in when the controller state changes instead of reading
them per sample. The attenuation generator's raw units are 0.4 centibels each,
while a record's amount is already centibels; the two are converted separately
and the total clips at 144 dB, so a 200 cB record is 20 dB down and a negative
record can only undo the generator. A pan record's amount is generator units and
sums with the zone's own pan and CC10's before the constant-power law: a CC2
record of 250 units sits 7.66 dB right of centre at full travel. Both are exactly
neutral when no record targets them — the attenuation multiplier is 1.0 and the
pan sum is 0 — which keeps a record-free bank byte-identical. The measurements
are in `tests/suite/sf2_modulators.rs`: a 400 cB velocity record reads 19.84 dB
at velocity 64 and changes nothing at 127, a 200 cB CC record moves a sounding
note 20.0 dB, and the pan record lands on the predicted balance.

The format's default modulator list, and where each entry is handled:

| Source | Destination | Amount | Here |
|---|---|---|---|
| velocity | `initialAttenuation` | 960 cB, concave | the measured `(v/127)^2` level law |
| velocity | `initialFilterFc` | -2400 cents, concave | applied to a zone that engages its low-pass: full velocity is the generator's cutoff and a soft note closes by up to two octaves |
| channel pressure | `vibLfoToPitch` | 50 cents | applied |
| CC1 | `vibLfoToPitch` | 50 cents | applied |
| CC7 | `initialAttenuation` | 960 cB, concave | the CC7 `(v/127)^2` level law |
| CC10 | `pan` | 1000 | the constant-power pan law |
| CC11 | `initialAttenuation` | 960 cB, concave | the CC11 `(v/127)^2` level law |
| CC91 | `reverbEffectsSend` | 200 | applied, summed with the zone's generator |
| CC93 | `chorusEffectsSend` | 200 | applied, summed with the zone's generator |
| pitch wheel | `initialFilterFc` | 0, amount source pitch wheel | inert (amount 0) |

No default modulator routes the modulation LFO or the modulation envelope. A
bank reaches them through the generators above, its own records, or not at all;
a preset that leaves `modLfoTo*`/`modEnvTo*` at 0 gets no modulation from them.

`pmod`/`imod` records are read per zone: from the bag's modulator index to the
next bag's index, stopping early at the format's terminal record and capped at 16
records. The next bag bounds the list because not every bank writes a terminal
record; the MS Basic bank in this checkout relies on the index alone, so a list
that ignored the bound would lend the next zone's records to this one. The
records are applied when they target a modelled
destination with

- a source of no controller (a constant at full scale), a MIDI controller,
  channel pressure, note velocity, key number, the pitch wheel or the
  pitch-wheel sensitivity (the format's general-controller indices: 0x00 none,
  0x02 velocity, 0x03 key number, 0x0D channel pressure, 0x0E pitch wheel,
  0x10 pitch-wheel sensitivity),
- any of the four transforms: linear, switch, concave and convex. The concave
  curve is `-(20/96) log10(x^2)` clamped to 0..1, so the format's
  velocity-to-attenuation default modulator (960 units of concave attenuation,
  96 dB) reproduces the measured `(v/127)^2` level law exactly; convex is its
  complement. A constant source or a concave record would not be applied
  blindly: the curve is pinned by that level law and a unit test
  (`the_concave_curve_turns_velocity_into_the_squared_level_law`),
- any direction and polarity; the four source mappings are linear in the raw
  0..1 value — unipolar `x` / `1-x`, bipolar `2x-1` / `1-2x` — so a negative
  unipolar source is the complement of the positive one, which is the shape the
  format's velocity-to-attenuation default needs. The pitch wheel's raw value is
  its position (0.5 at centre) and the sensitivity source reads the player's
  fixed two-semitone bend range,
- an optional secondary amount source, which multiplies the amount by its own
  mapped value (0..1, or -1..1 when bipolar). The
  contribution is `amount * primary * secondary`. The secondary source carries
  its own direction and polarity and no transform.

Records with an unassigned source enumerator, an unsupported transform, or a
destination outside the modelled set are counted in `SoundFont::warnings`, as is
every record past the per-zone cap (the format's record list has no LFO or
envelope sources; the unassigned indices that some banks use for internal
modulators stay counted). The engine adds the zone's generator value and the
records' contributions in the destination's unit, so a bank that routes CC2 to
`modLfoToPitch` adds to the generator's depth instead of replacing it.

Each voice owns its vibrato LFO, modulation LFO, modulation envelope and filter
state, while the depths are recomputed from the channel's controller state at
note-on and on every controller change, so a controller move reaches sounding
notes without giving them a shared oscillator. With every depth at zero the
render is bit-identical to one from before the modulation work
(`tests/suite/sf2_modulation.rs` and `tests/suite/sf2_modulation_env.rs` pin the hash).
The LFOs run from note-on (after their delay) even while their depth is zero,
so automation that arrives mid-note joins a running oscillator; the filter's
biquad coefficients are redesigned only when the cutoff moved 4 cents and at
most once every 8 samples, because a design costs a sin/cos pair.

Polyphonic pressure is the note's own pressure: a `PolyPressure` message
refreshes the voices holding that note and no others, a note-on clears the
note's pressure, and channel pressure remains a separate source. The bank
decides what it drives, through its own records. Measured with two held notes
and a poly-pressure-to-cutoff record, pressure 127 on one note moves that note's
partial by +24.4 dB and the other by 0.000 dB.

## Effect sends

The format's two sends are live: `chorusEffectsSend` and `reverbEffectsSend`
(tenths of a percent, 0..1000), plus the CC93/CC91 default modulators that move
them. Each player owns **one** chorus bus and **one** reverb bus, not one
device per voice: a voice's dry contribution is also summed into the bus scaled
by its send, and the bus runs one device instance for the whole player; the wet
return is added to the output. The buffers are allocated once per player, a
render call is split into fixed 128-frame passes, and `render_into` allocates
nothing (`tests/sf2_sends_alloc.rs`).

| Law | Behaviour |
|---|---|
| send gain | `clamp(zone generator + modulator records + CC91/CC93, 0, 1000) / 1000`, linear in the value: doubling the send doubles the wet amplitude (+6.02 dB) |
| reverb | one Schroeder-Moorer reverb per player, wet only, 2.2 s T60 and no damping; return 0.328 |
| chorus | one two-voice chorus per player, wet only, 1.5 Hz and 3 ms around a 7 ms centre delay; return 1.854 |
| patching | the buses begin processing at the first nonzero send and keep running after the last voice, so the tail outlives the notes |
| controllers | CC91/CC93 add `200/127` of a full send per controller unit, per channel, reaching sounding notes on the next controller event |

The law and the returns were chosen by measurement: at a full send the reverb
tail sits 7.20 dB below the dry note and the chorus wet 5.97 dB above it. The
send table from the rendered audio (`tests/suite/sf2_sends.rs::the_send_table`):

| send | reverb tail dBFS | dry, first 3 ms | chorus wet dBFS | chorus sidebands |
|---|---|---|---|---|
| 0 | none | -21.68 | none | -48.3 dB (window floor) |
| 250 | -40.0 | -21.68 | -27.5 | -10.2 dB |
| 500 | -34.0 | -21.68 | -21.4 | -4.5 dB |
| 1000 | -28.0 | -21.68 | -15.4 | +0.9 dB |

The dry path does not move: the first three milliseconds, before any wet
return can arrive, are sample-identical at every send, and a send-free bank
keeps the pinned render hash `4_826_522_779_913_708_261`.

**Defaults.** A bank that declares no send and moves no controller gets no
reverb and no chorus. The controller state powers up with CC91 = CC93 = 0, and
the buses do not run at all until a send is nonzero — which is what keeps a
send-free bank bit-identical. The devices' own defaults are the chorus's 1.5 Hz
/ 3 ms and a reverb at 2.2 s with damping off. A bank's generator and the two controllers
sum in the format's unit, so a preset with `reverbEffectsSend` 200 that also
receives CC91 = 127 sends 40 %.

**What the sends do not do.** The wiring is complete — zone generator, bank
records, CC91/CC93, one bus pair per player, wet return, allocation-free passes —
and the remaining differences are design choices, not missing connections:

- One bus pair per player, not one project-global pair. A project with eight
  SoundFont tracks instantiates eight reverbs and eight choruses, so the tails of
  two tracks do not correlate; a global pair would put shared mutable state in
  the audio path, which this engine does not do.
- The sends follow the voice's stereo pan.
- The two devices' parameters are fixed (2.2 s T60, 1.5 Hz / 3 ms). The format
  declares no effect parameters, so a bank cannot ask for a different tail.
- The wet return is summed inside the player, before the track's FX chain. It
  cannot be replaced by, or sent to, a track effect.
- A renderer reused for a second, unrelated render (the audition path) keeps the
  previous tail for its decay time. A lane rebuilt for a seek gets a new player
  and an empty bus.

## Deliberately absent

- **The remaining record destinations.** A record is applied when its
  destination is one of the nineteen modelled ones (the seven pitch, cutoff and
  volume destinations, the two sends, `initialFilterQ`, the seven envelope-stage
  destinations and the attenuation/pan pair), its source is one of the
  enumerators the engine reads (any MIDI CC, velocity, key number, polyphonic or
  channel pressure, pitch wheel, pitch-wheel sensitivity) and its secondary
  amount source — if it carries one — is one of the same. The pitch-wheel
  sensitivity source reads the player's fixed two-semitone bend range. Records
  with an unassigned source enumerator (some banks use those for internal
  modulators), a transform outside the format's four, or a destination outside
  that set are counted, not applied. The four transforms are applied: linear,
  switch, and the format's concave/convex pair, whose concave curve is pinned by
  the format's velocity-to-attenuation default modulator (960 units of concave
  attenuation is exactly the measured `(v/127)^2` level law).

  Two groups of destinations are read once at note-on and keep that value for
  the life of the note: `initialFilterQ` and the envelope-stage destinations. A
  record that reaches either is summed into the destination like any other, but
  the voice builds the biquad's Q and the two envelope machines from the note-on
  sum. A controller move that arrives later updates the stored modulation and
  cannot re-aim the Q or re-arm a stage. MS Basic's 183 envelope records and 9 of
  its 14 Q records come from velocity or key number, so the note-on read is exact
  for them. Timbres of Heaven drives 7 Q and 14 envelope records from CC21, CC22
  and CC23, so it receives the value those controllers hold at note-on; a move
  after that is not followed.

  The limit is deliberate, and the cost of lifting it is why it stays. A mid-note
  Q change would redesign the biquad on the controller event, a sin/cos pair per
  sounding voice, and step the resonance gain; the step needs a ramp or it clicks.
  A mid-note envelope-time change would re-arm the running stage from its current
  level, so an attack or release would change slope in flight. Both need a
  controller-event path into the voice that the current note-on read does not
  have.

  The destinations a bank reaches and this engine does not, with the raw
  `pmod`/`imod` record count across the four parsable GM banks used for the
  audit (FluidR3_GM, GeneralUser_GS, MS_Basic and Timbres of Heaven; TimGM6mb.sf2
  is truncated and does not parse) and the decision for each:

  | Destination | Records | Decision |
  |---|---|---|
  | `startAddrsOffset` (0) and `startAddrsCoarseOffset` (4) | 65 | Not applied. A controller move would jump the sample position and click; only a note-on-time read is safe, and the local records are velocity-shaped, which cannot move the start without a discontinuity. |
  | `freqModLFO` (22) | 3 | Not applied. The LFO's phase increment is built at note-on; a rate record needs a phase-continuous rate change, which is a design choice rather than an offset. |
  | `fineTune` (52) | 6 | Not applied. Pitch records are rare and the pitch wheel's fixed two-semitone range is the player's bend law, so a cents record would have to be reconciled with it. |

  Those 74 of the 6,394 raw records stay counted in `SoundFont::warnings`; the
  other 6,320 are honoured, including all 25 `initialFilterQ` and all 197
  envelope-stage records. The warning reports the split per bank, for example
  `honoured 746 of 748 modulator records` for FluidR3_GM.
- **Linked stereo pairs.** A left/right pair plays as two mono zones with their
  own pans, which reproduces the image; the pair is not summed into one stereo
  voice. Pair linking is not a level error by itself: the constant-power pan
  law, the `initialAttenuation` law and the `sustainVolEnv` law each agree
  within 0.1 dB, and the strings pad that used to read hotter was the envelope
  decay law, measured in §"Strings stem" below.
- **Crossfade loops.** A loop is a plain forward loop; one that crossfades its
  end into its start is not synthesised that way.
- **24-bit dithering** and per-point rounding: 24-bit points are read at their
  own precision and mixed as `f32`.

## Verification

```sh
# parser, playback, modulation, sends and adapter tests (one binary per crate)
cargo test -p oc-engine --test all -- sf2::
cargo test -p oc-engine --test all sf2_sends_alloc::
# the measured send table (reverb tail, dry invariance, chorus depth, CC timing)
cargo test -p oc-engine --test all -- sf2_sends:: -- --ignored --nocapture
# the velocity curve, the fixed key/velocity generators and poly pressure
cargo test -p oc-engine --test all -- sf2_generators:: -- --nocapture
cargo test -p oc-engine --test all -- sf2_pressure:: -- --nocapture
# the attenuation and pan records, measured from the rendered buffers
cargo test -p oc-engine --test all -- sf2_modulators:: -- --nocapture
# a real GM bank, if one is installed
OCP_TEST_SOUNDFONT=/path/to/FluidR3_GM.sf2 \
  cargo test -p oc-engine --test all -- sf2_render:: -- --ignored

# the hairpin swell: level plus spectral centroid, and the opt-out control
cargo test -p oc-engine --test all -- sampled

# a project render through the CLI
ocp render song.ocp --soundfont bank.sf2 --output out.wav
```

The desktop bundle ships `FluidR3Mono_GM.sf3` under
`resources/soundfonts/` and uses it when neither `OCP_SOUNDFONT` nor a local
`soundfonts/MS_Basic.sf2` exists. The bank holds 197 presets (the 128 GM
programs plus GS variations), is 23,712,790 bytes, and is MIT-licensed; its
license text ships beside it. A CLI render still names the bank explicitly with
`--soundfont`.
