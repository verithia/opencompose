# OpenSynth

OpenSynth (`builtin.open_synth`) is the built-in synthesizer. A track plays it
through a preset:

```ocp
@preset bass
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.74,
           sub.level: 0.42, filter.cutoff: 740Hz, filter.resonance: 0.32,
           filter.env.amount: 3.8, amp.attack: 0.004s, amp.decay: 0.18s,
           amp.sustain: 0.62, amp.release: 0.14s,
           voice.mono: true, voice.legato: true, voice.glide: 0.07s,
           output.gain: 0.26}
```

`params:` is a flat `key: value` map of patch fields. The patch editor in the
window (see [the window](gui.md)) writes the same map, so a patch you build with
the mouse and one you type are interchangeable. A switch such as `voice.mono`,
`lfo.sync`, `mod.N.bipolar` or `fx.N.enabled` accepts `true`/`false` or
`1`/`0`, and the two spellings mean the same value.

The implementation of every key below is in `crates/oc-synth-core/src/synth.rs`.
The declarative list is in `crates/oc-device-schema/src/synth.rs`. The editor's
dropdowns and labels are generated from that list
(`crates/oc-gui/frontend/src/data/synthCatalog.gen.ts`), so the editor offers
every modulation source, destination, filter model, warp id and FX module that
the engine accepts. The enum ids in this chapter match that catalog.

## Factory patches

The patch field in the workbench header shows the name of the project preset.
The smaller label after it records where the values came from: a factory patch
name, a user-library entry or an imported file name. That label is stored as
the preset's `patch:` field, so a reopened project still shows the origin.

Click the field (or its `▾`) to open the preset browser. It lists the factory
bank and a **User library** section, grouped by category, with a search box.
**LOAD** does something else: it opens a file picker and imports one external
preset file into the current project preset (see [Preset import](#preset-import)).

The instrument ships with four original patches (`Init`, `Neon Bass`,
`Glass Pad`, `Pulse Pluck`) and 62 category presets: ten bass, ten lead, twelve
pad, nine pluck, eight keys, six brass and seven fx. The factory section
therefore lists 66 patches. [OpenSynth factory presets](../factory-presets.md)
lists them with the use each one is written for. A header labels each category,
and a dot marks the row the current preset was loaded from.

### Lifecycle

A patch belongs to the project. It does not point back into the factory bank.

- **Rename…** renames the `@preset` and rewrites every track, pattern and
  `extends:` reference in the same undo entry. The name identifies the preset
  inside the project. The `patch:` label only records provenance and is never
  used to resolve a reference.
- **Duplicate** and **Save as…** copy the preset's parameters and effect chain
  and point the players at the copy. The copy and the original are independent,
  and an edit to one never changes the other. An engine test renders the
  original track before and after editing the copy and requires identical
  samples.
- **Save to library…** writes the effective patch to the user library under the
  name you enter. A file-backed OpenSynth voice is saved as native state, together
  with its wavetables and collected sample files, so it stays available when the
  source project moves.
  - To move a library, copy its whole directory, including `assets/`, and set
    `OCP_PRESET_LIBRARY_DIR` to the new location before you launch the app.
    Existing projects remember the location they loaded from, so reload the
    library patch in those projects.
  - Saving the project with **Save As…** to a different folder collects
    the library states the project uses and stores project-relative paths in
    the copy.
  - Removing a browser entry leaves its collected assets in place for projects
    that already use them.
  - Hosted effects still need their plugins and external content.
- **Share preset…** writes one `.ocppatch` file with the effective OpenSynth
  voice, its local samples, the authored parameter overrides and the insert
  chain. On the other machine, choose **Load** in the OpenSynth editor and select
  the file. The app adds it to the library and loads it into the selected preset.
  If the name is taken, the app adds a numbered copy. Hosted inserts still need
  their installed plugins and external content.

  A compressor or another insert keyed from a project track or bus cannot travel
  in a self-contained preset. Before you share, remove the external sidechain
  source or switch the insert to its internal detector. The app also refuses to
  load a bundle that has an external key, because it would otherwise bind the key
  to an unrelated track on the receiving side.

For an agent, the same operations are `preset_bundle_export` and
`preset_bundle_import`. They exchange numbered files through the `handoff/`
folder of the saved project. Export needs a committed project snapshot. Import
loads an existing OpenSynth project preset as one undoable edit and keeps a copy
in the library. [The agent workflow](../agent-loop.md) lists the arguments.

When an OpenSynth preset inherits from another preset, the editor shows the
effective parameter values. Moving a control writes an override to the selected
child. Inherited inserts appear ahead of the child's own inserts. To edit those
inserts, select the parent.

### Whole-patch load and the reset rule

A load replaces the whole object in one undo entry: the parameters, the preset's
`effects:` chain and the `patch:` label. It does not merge with the previous
values. Every key the loaded patch does not declare falls back to the patch
default. The defaults are the table the `Init` patch uses, and they cover the
oscillators, sub, filters, all four envelopes, the LFOs, the modulation slots,
the macros and the step sequencer. For the same reason, `fx.count: 0` clears the
synth FX rack. Loading A, then B, then A gives back A's exact parameter set; a
Rust test compares the full preset snapshots.

The batch is canonical. Each key appears once, so loading the same patch twice
leaves the same parameters. If a batch names a key the synth does not know, the
whole batch is rejected and the error lists the key names. The message appears
in the synth workbench itself (`role="alert"`) as well as in the global status
bar.

### Browsing and audition

In the browser:

- `↑` and `↓` move the highlight.
- **Enter** loads the highlighted patch, and a double click loads the row under
  the pointer.
- **Esc** or a click outside closes the browser and leaves the preset as it was.

While a load is running, the header field, **LOAD** and the row auditions are
disabled until the write returns, so a double click cannot submit two
overlapping edits.

The `▶` on a row auditions that patch without loading it. The candidate is
rendered in full through the same graph as a preset preview, so its own
`effects:` chain is applied. Its integrated loudness is then matched to the
factory bank reference of −17.2 LUFS. The meter is BS.1770-4, the same one the
renderer and the analysis tools use, and −17.2 LUFS is the median of the 62
category patches measured on the audition figure. The gain is capped at
−18/+12 dB and at 0.5 dBFS of peak headroom, so a very quiet patch becomes
audible without clipping. Unmatched, the bank spreads over 14.9 dB of loudness;
matched, the spread is under 0.4 dB, so when you step through the list you hear
differences in timbre and not in level.

An audition plays a bare patch. The candidate's own effect chain is present, but
the track's insert effects, its bus and the master chain are not, because the
browser has no track context. Every row plays the same six-note figure. A second
audition replaces the first, and `■`, **Esc** or closing the browser stops it.
The project does not change: there is no dirty flag and no undo entry, and the
loaded preset stays as it was.

### User library

The library lives in `~/.opencompose/presets`, or in the directory named by
`OCP_PRESET_LIBRARY_DIR`. Each patch is one JSON file, `<name>.ocp-preset`, that
holds the name, category, tags, provenance, parameters and effect chain. A save
writes a temporary file in the same directory and publishes it with a rename, so
an interrupted save leaves the previous file intact.

Saving under an existing name replaces that entry. The API also accepts
`overwrite: false`, which refuses the save with a readable error; the tests use
it to pin the duplicate-name policy. Two names that sanitize to the same file
name get a stable hash suffix, so neither overwrites the other. A file that does
not parse is skipped with a warning in the workbench, and the rest of the
library still loads. When two files claim the same name, the first one is kept
and the second produces a warning.

## Oscillators

OpenSynth has three oscillators (A, B and C), a sub oscillator, a noise source
and a ring modulator.

| Key | Meaning |
|---|---|
| `osc.<x>.waveform` | `0` sine, `1` saw, `2` square, `3` triangle, `4` wavetable, `5` granular, `6` spectral |
| `osc.<x>.level` | 0–1 |
| `osc.<x>.semitones` | coarse tuning (A: ±48, B/C: −24…+24) |
| `osc.<x>.pan` | −1…1 |
| `osc.<x>.pulse_width` | 0.05–0.95 |
| `osc.<x>.wavetable_position` | 0–1 frame position |
| `osc.<x>.warp_mode`, `.warp_amount`, `.warp_var` | first warp slot |
| `osc.<x>.warp2_mode`, `.warp2_amount`, `.warp2_var` | second warp slot |

`<x>` is `a`, `b` or `c`. Warp acts on the phase before the wavetable readout,
per voice, so it changes the timbre of each note. `warp_amount` sets the depth
and `warp_var` is the secondary control of the shape. The second slot
(`warp2_*`) applies another map on top of the first.

`warp_mode` is a numeric id:

| Ids | Family |
|---|---|
| `0` | off |
| `1` | sync |
| `2`–`15` | phase maps: bend, PWM, asymmetry, flip, delay line, remap, quantize, even/odd |
| `17`–`18` | filter warps; selectable, not implemented |
| `19`–`32` | waveshapers |
| `33`–`65` | FM, PD, AM and RM families; selectable, the readout is unchanged |
| `66`–`69` | a flat `1 - amount` gain |

The filter warps `17` and `18` behave as off until their coefficient law exists.
The editor shows all 70 ids and marks the 35 that have no audible effect yet.

Sub-oscillator keys are `sub.level`, `sub.semitones`, `sub.fine`, `sub.pan`,
`sub.waveform` (`0` sine, `1` round rect, `2` triangle, `3` saw, `4` square,
`5` pulse), `sub.initial_phase` and `sub.contiguous_phase`. The noise source and
the ring modulator have `noise.level` and `ring.level`.

`wavetable.file` loads a custom wavetable; the wavetable browser in the editor
sets it. A relative path resolves against the project directory, as described
under Presets in the [DSL manual](dsl.md).

## Unison

| Key | Applies to |
|---|---|
| `unison.voices` (1–16), `unison.detune` (0–100 ct), `unison.width` (0–1) | oscillator A |
| `osc.b.unison`, `osc.b.detune`, `osc.b.width` | oscillator B |
| `osc.c.unison`, `osc.c.detune`, `osc.c.width` | oscillator C |

Each unison voice starts at its own deterministic phase. The phase is seeded by
the oscillator slot and the voice index; the note and the mix do not affect it.
Detune and width then spread the voices. A layer at zero detune keeps every voice
on the shared start phase, so the voices stay coherent and do not cancel.

## Filters

There are two filters, each with its own model and mode, plus a filter envelope.

| Key | Meaning |
|---|---|
| `filter.cutoff` | 20 Hz–20 kHz |
| `filter.resonance` | 0–1 |
| `filter.drive`, `filter.wet` | pre-filter drive, dry/wet |
| `filter.mode` | `0` low-pass, `1` band-pass, `2` high-pass, `3` notch |
| `filter.model` | see below |
| `filter.keytrack`, `filter.keytrack.origin` | cutoff follows pitch; origin in MIDI notes |
| `filter.env.amount` | filter envelope depth, −8…+8 |
| `filter.routing` | `0` serial (A→F1→F2), `1` parallel |
| `filter2.*` | second filter; `filter2.enabled` (0/1) switches it on |

Filter models (`filter.model`):

- `0` two-pole state variable
- `1` four-pole ladder
- `2` dirty ladder
- `3`/`4` legacy three-pole and one-pole ladders
- `5` first-order tap
- `6`/`7` three-pole and four-pole state-variable cascades
- `8`–`11` ladder taps at 6, 12, 18 and 24 dB per octave

The editor offers all twelve models on both filters. The resonance law, passband
trim and measured slope of each family are in
[Filter character](../filter-character.md).

## Envelopes

| Envelope | Keys |
|---|---|
| Amplitude | `amp.attack`, `amp.decay`, `amp.sustain`, `amp.release`, `amp.curve` |
| Filter | `filter_env.attack`, `.decay`, `.sustain`, `.release`, `.curve` (+ `filter.env.amount`) |
| Env 3 | `env3.attack`, `.decay`, `.sustain`, `.release`, `.curve` |
| Env 4 | `env4.attack`, `.decay`, `.sustain`, `.release`, `.curve` |

`curve` bends every stage of its envelope. The amplitude attack has a 1 ms floor,
so a zero attack still starts from silence. The modulation envelopes keep
instant attacks. See [Modulation](../modulation.md).

## LFOs

There are ten LFOs. LFO 1 uses `lfo.*`, and LFOs 2–10 use `lfo2.*` … `lfo10.*`
with the same fields.

| Key | Meaning |
|---|---|
| `lfo.rate` | 0–100 Hz |
| `lfo.rate_x10` | multiply the free rate by ten |
| `lfo.shape` | `0` curve, `1` path, `2` chaos A, `3` chaos B, `4` random hold |
| `lfo.shape.points`, `.x`, `.y`, `.curve` | breakpoint shape (arrays; the editor draws it) |
| `lfo.sync`, `lfo.sync_div` | tempo sync; `sync_div` indexes the 43-step ladder in beats (entry 22 = one beat) |
| `lfo.division` | retired six-step selector, kept for old patches |
| `lfo.retrigger`, `lfo.phase` | retrigger on note-on; start phase in degrees |
| `lfo.smooth`, `lfo.rise`, `lfo.delay` | one-pole smoothing, fade-in delay and rise |
| `lfo.bend` | retired whole-cycle phase skew |
| `lfo.pitch` | LFO 1 only: direct vibrato depth in cents |

The shape output is unipolar, 0–1. The `bipolar` flag of a mod slot recentres it.

## Mod matrix and macros

The routes are `mod.1` … `mod.64`. Each one has these fields:

```ocp
params: {mod.1.source: 0, mod.1.destination: 4, mod.1.amount: 0.2,
         mod.1.curve: 0, mod.1.slew_up: 0.01s, mod.1.slew_down: 0.05s}
```

| Field | Meaning |
|---|---|
| `mod.N.source` | `0` LFO 1, `1` filter envelope, `2` velocity, `3`–`6` macro 1–4, `7` keytrack, `8` aftertouch, `9` mod wheel, `10` step sequencer, `11`–`19` LFO 2–10, `20` env 3, `21` env 4, `22`–`25` macro 5–8, `26` amp envelope, `27` pitch bend, `28` timbre (CC74) |
| `mod.N.destination` | `0` filter cutoff, `1` pitch, `2` oscillator mix, `3` resonance, `4` stereo width, `5` pulse width A, `6` ring mod level, `7` wavetable position A, `8` oscillator B tuning, `9` filter drive, `10` oscillator C level, `11` wavetable position C, `12`–`14` filter 2 cutoff/resonance/drive, `15` voice amp, `16`–`21` warp and warp-2 amounts A/B/C, `22` wavetable position B, `23`–`34` source density/position/morph/bend A/B/C |
| `mod.N.amount` | −1…1 |
| `mod.N.bipolar` | recentre a unipolar source on zero |
| `mod.N.curve` | −1…1 shaping exponent around identity |
| `mod.N.slew_up`, `mod.N.slew_down` | one-pole rise/fall in seconds |
| `mod.N.aux`, `mod.N.aux_invert` | second source that multiplies the route depth: `0` none, `1`–`29` the source ids above plus one; `aux_invert` negates it |

The engine has four more destinations without a numeric id, so a project route
cannot reach them yet: noise level, sub level, filter wet and filter 2 wet. The
catalog lists them as unmapped, and the editor's destination menu ends at the
last reachable id, `34`.

The macros are `macro.1` … `macro.8`. The step sequencer has `step.rate`,
`step.sync`, `step.division` and `step.smoothing`.

The FX rack has its own modulation slots, `fxmod.N.*`, with the fields `source`,
`parameter`, `fx_index` and `amount`.

## Voice and quality

| Key | Meaning |
|---|---|
| `voice.mono` | monophonic |
| `voice.polyphony` | max voices |
| `voice.legato` | legato note changes |
| `voice.glide` | glide time in seconds |
| `quality.oversampling` | `0`…`2` |
| `osc.start_phase`, `osc.random_phase` | oscillator start phase and its random spread |
| `tuning.reference_hz`, `tuning.transpose`, `tuning.fine` | tuning |
| `mpe.enabled`, `mpe.bend_range`, `mpe.lower_first`/`lower_last`, `mpe.upper_first`/`upper_last` | MPE zones |
| `output.gain` | patch output level |

## The synth FX rack

The rack runs after the voice sum and before the output stage, and holds up to
16 modules.

```ocp
params: {fx.count: 2,
         fx.1.type: delay, fx.1.mix: 0.3, fx.1.feedback: 0.35,
         fx.1.time: 0.375s,
         fx.2.type: reverb, fx.2.mix: 0.2, fx.2.decay: 2.4s}
```

- `fx.count` sets the length of the chain. Slots count from 1.
- `fx.N.type` is the module label or its numeric index: `drive`, `compressor`,
  `eq3`, `delay`, `reverb`, `chorus`, `distortion`, `phaser`, `flanger`,
  `filter`, `utils`, `split`, `split3`, `split_ms`, `bode`, `parallel`,
  `hyperd`, `conv`, `multiband`.
- `fx.N.enabled` (0/1) bypasses a module.
- Every other `fx.N.<name>` is a parameter of that module (`mix`, `drive`,
  `feedback`, `time`, `cutoff`, `width`, …).
- `split`, `split3`, `split_ms` and `parallel` take the slots that follow them as
  child chains.

The app creates a module only for a slot that has a `type`. A parameter on a slot
without a type has no effect.

The catalog knows all 19 module types. Ten of them have their own controls in the
editor: `drive`, `compressor`, `eq3`, `delay`, `reverb`, `chorus`, `distortion`,
`phaser`, `flanger` and `filter`. The other nine run in the rack and keep the
parameter values stored in the patch, but the editor has no controls for them
yet.

## Preset import

**LOAD**, and the **IMPORT PRESET** action on the OSC page, open a file picker
and import one external preset file into the current project preset. The file
name becomes the `patch:` provenance label in the workbench header. The path is
stored as `serum_file`, so a reopened project still names its source file.

The importer reads one external preset format: `.SerumPreset`, the preset format
of a wavetable synthesizer. The factory bank and the user library never load
through this path. A file with any other extension is rejected, and the message
names the file and the supported format.

An import is a whole-patch load and follows the same rule as a load from the
browser:

- The backend parses the file and returns the whole patch as project parameters.
  The frontend does no parsing.
- Every value the file declares is applied, including its modulation routes and
  its internal FX rack (`fx.N.*`).
- Parameters that the previous patch declared and the file does not are removed,
  so they fall back to the patch default. The project preset's `effects:` chain
  and `patch:` label are replaced in the same undo entry.
- `wavetable.file` and `open_synth_state_file` are cleared, because the imported
  wavetable comes from the source file.
- The imported file is not auditioned. Only a successful load changes the
  project. If the file has an unsupported format, cannot be read or does not
  parse, the workbench shows the file name and the parser's reason, and the
  preset stays as it was.
- The picker reopens in the folder of the last import, which is stored locally.
  The first import opens wherever the platform dialog defaults to.

The engine maps the preset when the project loads. `serum_file` therefore stays
the source of truth for state the numeric parameters cannot hold: custom
wavetables, sample layers and the arpeggiator. Keep the file where the project
can find it. If the file has moved, the project reports a load error.

How the mapping behaves:

- Every unmapped module, parameter and route class produces one deduplicated
  warning; nothing is dropped silently. Unmapped classes include granular and
  spectral oscillator details, some warp modes, individual parameters, and
  presets that need more than the 16 modules of the rack.
- Some controls are deliberately not mapped: the Bode **Blur** and
  **Delay Balance** controls, **Retrig** (a rack after the voices receives no note
  events), **Beat Sync** (the rack does not receive the transport tempo) and some
  multiband compressor controls.
- Per-source bus sends collapse to the loudest send, because the rack after the
  voices has a single stereo sum. A bus effect therefore processes the whole voice
  sum.
- Imported mod routes get the neutral `curve` and `slew` defaults, because the
  importer has no scaling law for them.
- The intermediate **wet** positions of the Utils module use the shared amplitude
  crossfade. Fully wet and fully dry are exact.

The engine-side mapping and its limits are documented in
[utility, split and frequency-shift effects](../fx-utils-split-bode.md). The
originality policy is in [compatibility and originality](../compatibility.md).
