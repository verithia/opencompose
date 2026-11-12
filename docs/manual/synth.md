# OpenSynth

OpenSynth (`builtin.open_synth`) is the built-in synthesizer. A track uses it
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

`params:` is a flat `key: value` map of patch fields. The GUI patch editor
(see [the window](gui.md)) writes the same map, so a patch made by clicking and
one written by hand are interchangeable. A switch (`voice.mono`, `lfo.sync`,
`mod.N.bipolar`, `fx.N.on`, …) accepts `true`/`false` or `1`/`0`; both are the
same value.

`crates/oc-synth-core/src/synth.rs` is the implementation reference for every
key below. The declarative list lives in
`crates/oc-device-schema/src/synth.rs`; the editor's dropdowns and labels are
generated from it (`crates/oc-gui/frontend/src/data/synthCatalog.gen.ts`), so the
GUI offers every modulation source, destination, filter model, warp id and FX
module the engine accepts. Enum ids in this chapter match that catalog.

## Factory patches

The patch field in the workbench header shows the **project preset's name**. The
smaller label after it is where the values came from — a factory patch name, a
user-library entry or an imported file name — and is stored as the preset's
`patch:` field, so reopening a project still says what the patch was.

Clicking the field (or its `▾`) opens the **preset browser**: the factory bank
plus a **User library** section, each row grouped by category, with a search box.
**LOAD** is a different action: it opens a file picker and imports one external
preset file into the current project preset (see [Preset import](#preset-import)).

The instrument ships with four originals (`Init`, `Neon Bass`, `Glass Pad`,
`Pulse Pluck`) and 62 category presets — ten bass, ten lead, twelve pad, nine
pluck, eight keys, six brass, seven fx — so the factory section lists 66 patches.
See `docs/factory-presets.md` for the list and what each is written for. A
category header labels each group, and a dot marks the row the preset was loaded
from.

### Lifecycle

A patch is a project object, not a view of the factory bank:

- **Rename…** renames the `@preset` and rewrites every track, pattern and
  `extends:` reference in the same undo entry. The name is the identity inside
  the project; the `patch:` label is provenance and is never used to resolve.
- **Duplicate** / **Save as…** copy the preset's parameters and effect chain and
  point the players at the copy. The copy and the original are independent: an
  edit to one never moves the other (the engine test renders the original track
  before and after editing the copy and requires identical samples).
- **Save to library…** writes the effective patch to the user library under the
  name you enter. File-backed OpenSynth voices are saved as native state with
  their wavetables and collected sample files. They remain available if the
  source project moves. To move a library, copy its whole directory, including
  `assets/`, and set `OCP_PRESET_LIBRARY_DIR` to the new location before launch.
  Existing projects remember
  the location they loaded, so reload the library patch in those projects.
  Save As to a different folder collects the library states that the project
  uses and stores project-relative paths in the copy. Removing a browser
  entry leaves its collected assets
  in place for projects that already use them. Hosted effects still require
  their plugins and external content.

- **Share preset…** writes one `.ocppatch` file with the effective OpenSynth
  voice, its local samples, authored parameter overrides and the insert chain.
  On the other machine, choose **Load** in the OpenSynth editor and select the
  bundle. The app adds it to the library and loads it into the selected preset.
  A name conflict gets a numbered copy. Hosted inserts still need their
  installed plugins and external content.
  A compressor or other insert keyed from a project track or bus cannot travel
  as a self-contained preset. Remove that external sidechain source or use the
  insert's internal detector before sharing. Loading a bundle with an external
  key is also refused rather than binding it to an unrelated receiving track.

The agent equivalents are `preset_bundle_export` and `preset_bundle_import`.
They exchange numbered files through the saved project's `handoff/` folder.
Export requires a committed project snapshot; import loads an existing
OpenSynth project preset as one undoable edit and keeps a library copy.
See [the agent workflow](../agent-loop.md) for the arguments.

When an OpenSynth preset inherits another preset, the editor shows the
effective parameter values. Adjusting a control writes an override to the
selected child. Inherited inserts appear ahead of the child's inserts. Select
the parent to edit its inserts.

### Whole-patch load and the reset rule

A load replaces the whole object: parameters, the preset's `effects:` chain, and
the `patch:` label, in one undo entry. It does **not** merge with what was there.
Every key the loaded patch does not declare therefore falls back to the patch
default — the same table the `Init` patch uses, covering the oscillators, sub,
filters, all four envelopes, the LFOs, modulation slots, macros and the step
sequencer. `fx.count: 0` clears the synth FX rack for the same reason. Loading A,
then B, then A reproduces A's parameter set exactly; a Rust test compares the
full preset snapshots.

The batch is canonical: each key appears once, so loading the same patch twice
leaves the same parameters. A batch naming a key the synth does not understand is
rejected as a whole with the key names in the error, and the message appears
directly in the synth workbench (`role="alert"`), not only in the global status
bar.

### Browsing and audition

`↑` / `↓` move the highlight, **Enter** loads the highlighted patch, a double
click loads the row under the pointer, and **Esc** or a click outside closes the
browser without touching the preset. While a load is in flight the header field,
**LOAD** and the row auditions are disabled until the write returns, so a double
click cannot submit two overlapping edits. (**LOAD** itself imports a file; the
field is what opens this browser.)

The `▶` on a row **auditions** that patch without loading it. The candidate is
rendered in full through the same graph as a preset preview, so its own
`effects:` chain is applied, then its integrated loudness (BS.1770-4, the same
meter the renderer and the analysis tools use) is matched to the factory bank
reference of **−17.2 LUFS**, the median of the 62 factory patches measured
through the audition figure. Gain is capped at −18/+12 dB and at 0.5 dBFS of
peak headroom, so a very quiet patch becomes audible without clipping. Matched,
the raw 14.9 dB loudness spread of the bank collapses to under 0.4 dB, so
flicking through the list compares timbres instead of levels.

Audition still uses a bare patch: the candidate's own effect chain is present,
but the track's insert effects, its bus and the master chain are not (there is no
track context in the browser). The six-note figure is the same for every row; a
second audition replaces the first, and `■`, **Esc**, or closing the browser
stops it. Nothing about the project changes: no dirty flag, no undo entry, and
the loaded preset stays as it was.

### User library

The library lives in `~/.opencompose/presets` (override with
`OCP_PRESET_LIBRARY_DIR`), one JSON file per patch (`<name>.ocp-preset`) holding
the name, category, tags, provenance, parameters and effect chain. Writes go to
a temporary file in the same directory and are published with a rename, so an
interrupted save leaves the previous file intact.

Saving an existing name replaces that entry. The API also accepts
`overwrite: false`, which refuses with a readable error instead — the tests use
it to pin the duplicate-name policy. Two names that sanitise to the same file
name get a stable hash suffix instead of overwriting each other. A file that
does not parse is skipped with a warning shown in the workbench; it never hides
the rest of the library, and two files claiming one name keep the first and warn
about the second.

## Oscillators

Three oscillators, A/B/C, plus a sub, a noise source and a ring modulator.

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

`<x>` is `a`, `b` or `c`. Warp is applied to the phase before the wavetable
readout, per voice, so it retimbres each note. `warp_mode` is a numeric id:
`0` off, `1` sync, `2`–`15` phase maps (bend, PWM, asymmetry, flip, delay line,
remap, quantize, even/odd), `17`–`18` filter warps that are selectable but not
implemented yet, `19`–`32` waveshapers, `33`–`65` the FM/PD/AM/RM families, and
`66`–`69` a flat `1 - amount` gain. The FM/PD family is **not** usable yet: it is
selectable and the readout is unchanged. The editor keeps all 70 ids visible and
marks the 35 without an audible effect, rather than hiding them.
`warp_amount` is the depth; `warp_var` is the shape's secondary control, and the
second slot (`warp2_*`) runs another map on top of the first. The filter warps
`17`/`18` are resolved as off until their coefficient law exists.

Sub-oscillator keys: `sub.level`, `sub.semitones`, `sub.fine`, `sub.pan`,
`sub.waveform` (`0` sine, `1` round rect, `2` triangle, `3` saw, `4` square,
`5` pulse), `sub.initial_phase`, `sub.contiguous_phase`. Also `noise.level` and
`ring.level`.

A custom wavetable is loaded with `wavetable.file` (set by the GUI's wavetable
browser). A relative path resolves against the project directory; see Presets in
the DSL manual.

## Unison

| Key | Applies to |
|---|---|
| `unison.voices` (1–16), `unison.detune` (0–100 ct), `unison.width` (0–1) | oscillator A |
| `osc.b.unison`, `osc.b.detune`, `osc.b.width` | oscillator B |
| `osc.c.unison`, `osc.c.detune`, `osc.c.width` | oscillator C |

Voices start at independent, deterministic phases (seeded by the oscillator
slot and voice index, not by the note or the mix) and are spread by detune and
width. A layer at zero detune keeps every voice on the shared start phase, so
it stays coherent instead of cancelling.

## Filters

Two filters, each with its own model and mode, plus a filter envelope.

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
| `filter2.*` | second filter; `filter2.enabled` (0/1) gates it |

Filter models (`filter.model`): `0` two-pole state variable, `1` four-pole
ladder, `2` dirty ladder, `3`/`4` legacy three/one-pole ladders, `5` first-order
tap, `6`/`7` three-/four-pole state-variable cascades, `8`–`11` ladder taps at
6/12/18/24 dB per octave. The editor offers all twelve models for both filters. The resonance law, passband trim and measured slope of
each family are in [Filter character](../filter-character.md).

## Envelopes

| Envelope | Keys |
|---|---|
| Amplitude | `amp.attack`, `amp.decay`, `amp.sustain`, `amp.release`, `amp.curve` |
| Filter | `filter_env.attack`, `.decay`, `.sustain`, `.release`, `.curve` (+ `filter.env.amount`) |
| Env 3 | `env3.attack`, `.decay`, `.sustain`, `.release`, `.curve` |
| Env 4 | `env4.attack`, `.decay`, `.sustain`, `.release`, `.curve` |

`curve` bends every stage of that envelope. The amplitude attack is floored at
1 ms, so a zero attack still starts from silence; the modulation envelopes keep
instant attacks. See [Modulation](../modulation.md).

## LFOs

Ten LFOs: LFO 1 uses `lfo.*`, LFOs 2–10 use `lfo2.*` … `lfo10.*` with the same
fields.

| Key | Meaning |
|---|---|
| `lfo.rate` | 0–100 Hz |
| `lfo.rate_x10` | multiply the free rate by ten |
| `lfo.shape` | `0` curve, `1` path, `2` chaos A, `3` chaos B, `4` random hold |
| `lfo.shape.points`, `.x`, `.y`, `.curve` | breakpoint shape (arrays; the GUI draws it) |
| `lfo.sync`, `lfo.sync_div` | tempo sync; `sync_div` indexes the 43-step ladder in beats (entry 22 = one beat) |
| `lfo.division` | retired six-step selector, kept for old patches |
| `lfo.retrigger`, `lfo.phase` | retrigger on note-on; start phase in degrees |
| `lfo.smooth`, `lfo.rise`, `lfo.delay` | one-pole smoothing, fade-in delay and rise |
| `lfo.bend` | retired whole-cycle phase skew |
| `lfo.pitch` | LFO 1 only: direct vibrato depth in cents |

The shape output is unipolar 0–1; a mod slot's `bipolar` flag recentres it.

## Mod matrix and macros

`mod.1` … `mod.64` are the routes. Each has:

```ocp
params: {mod.1.source: 0, mod.1.destination: 4, mod.1.amount: 0.2,
         mod.1.curve: 0, mod.1.slew_up: 0.01s, mod.1.slew_down: 0.05s}
```

| Field | Meaning |
|---|---|
| `mod.N.source` | `0` LFO 1, `1` filter envelope, `2` velocity, `3`–`6` macro 1–4, `7` keytrack, `8` aftertouch, `9` mod wheel, `10` step sequencer, `11`–`19` LFO 2–10, `20` env 3, `21` env 4, `22`–`25` macro 5–8, `26` amp envelope, `27` pitch bend, `28` timbre (CC74) |
| `mod.N.destination` | `0` filter cutoff, `1` pitch, `2` oscillator mix, `3` resonance, `4` stereo width, `5` pulse width A, `6` ring mod level, `7` wavetable position A, `8` oscillator B tuning, `9` filter drive, `10` oscillator C level, `11` wavetable position C, `12`–`14` filter 2 cutoff/resonance/drive, `15` voice amp, `16`–`21` warp and warp-2 amounts A/B/C, `22` wavetable position B, `23`–`34` source density/position/morph/bend A/B/C |
| `mod.N.amount` | −1…1 |
| `mod.N.bipolar` | recentre a unipolar source; stored by the patch, but not writable through `set_param` yet |
| `mod.N.curve` | −1…1 shaping exponent around identity |
| `mod.N.slew_up`, `mod.N.slew_down` | one-pole rise/fall in seconds |
| `mod.N.auxiliary`, `mod.N.auxiliary_inverted` | second source that multiplies the route depth; imported patches only, not writable through `set_param` yet |

Four destinations are typed in the engine but have no numeric id, so a project
routine cannot reach them yet: noise level, sub level, filter wet and filter 2
wet. The catalog lists them as unmapped; the editor's destination menu stops at
the last reachable id (`34`).

Macros are `macro.1` … `macro.8`. The step sequencer has `step.rate`,
`step.sync`, `step.division` and `step.smoothing`.

The FX rack has its own modulation slots, `fxmod.N.*`, with `source`,
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

The rack runs after the voice sum and before the output stage. It has at most
16 modules.

```ocp
params: {fx.count: 2,
         fx.1.type: delay, fx.1.mix: 0.3, fx.1.feedback: 0.35,
         fx.1.time: 0.375s,
         fx.2.type: reverb, fx.2.mix: 0.2, fx.2.decay: 2.4s}
```

The catalog knows all 19 module types. Ten have dedicated editor controls
(`drive`, `compressor`, `eq3`, `delay`, `reverb`, `chorus`, `distortion`,
`phaser`, `flanger`, `filter`); the other nine are rack-ready and keep the
parameter values stored in the patch, but have no editor controls yet.

- `fx.count` caps the chain; slots are 1-based.
- `fx.N.type` is the module label (or its numeric index): `drive`, `compressor`,
  `eq3`, `delay`, `reverb`, `chorus`, `distortion`, `phaser`, `flanger`,
  `filter`, `utils`, `split`, `split3`, `split_ms`, `bode`, `parallel`,
  `hyperd`, `conv`, `multiband`.
- `fx.N.enabled` (0/1) bypasses a module.
- Every other `fx.N.<name>` is a parameter of that module (`mix`, `drive`,
  `feedback`, `time`, `cutoff`, `width`, …).
- `split`, `split3`, `split_ms` and `parallel` own the slots that follow them as
  child chains.

Only a slot with a `type` is created, so a parameter without a type does
nothing.

## Preset import

**LOAD** (and the **IMPORT PRESET** action on the OSC page) opens a file picker
and imports one external preset file into the current project preset. The file
name becomes the `patch:` provenance label in the workbench header, and the path
is stored as `serum_file`, so reopening the project still names the file it came
from.

Formats: `.SerumPreset` (the wavetable-synth preset format) is the only external
preset format the importer reads. The preset list — factory bank and user
library — never comes from this path, and a file with any other extension is
rejected with its name and the supported format.

The import is a whole-patch load, the same rule as loading from the browser:

- The backend parses the file and returns the whole patch as project
  parameters; the frontend does not parse anything itself.
- Every value the file declares is applied, including its modulation routes and
  its internal FX rack (`fx.N.*`).
- Parameters the previous patch declared but the file does not are removed, so
  they fall back to the patch default. The project preset's `effects:` chain
  and `patch:` label are replaced in the same undo entry.
- `wavetable.file` and `open_synth_state_file` are cleared; the imported
  wavetable comes from the source file.
- The imported file is not auditioned. Only a successful load changes the
  project; a file that is not a supported format, cannot be read, or does not
  parse reports the file name and the parser's reason in the workbench, and the
  preset is left as it was.
- The picker reopens in the directory of the last import (stored locally); the
  first one opens wherever the platform dialog defaults to.

The engine maps the preset when the project loads, so `serum_file` remains the
source of truth for state the numeric parameters cannot carry — custom wavetable
tables, sample layers and the arpeggiator. Keep the file where the project can
find it: a project that references a moved file reports the load error.

Mapping behaviour:

- Every unmapped module, parameter and route class produces one deduplicated
  warning instead of being silently dropped. Unmapped classes include granular
  and spectral oscillator details, some warp modes, individual parameters, and
  presets that exceed the 16-module rack budget.
- Deliberately not mapped: the Bode **Blur** control, the Bode **Delay Balance**,
  **Retrig** (a post-voice rack has no note events) and **Beat Sync** (the rack
  is not given the transport tempo), and some multiband compressor controls.
- Per-source bus sends are collapsed to the loudest send, because the post-voice
  rack has one stereo sum; a bus effect therefore processes the whole voice sum.
- Imported mod routes get the neutral `curve` and `slew` defaults; the importer
  has no scaling law for them.
- The intermediate **wet** positions of the Utils module use the shared
  amplitude crossfade; full wet and full dry are exact.

The engine-side mapping and its limits are documented in
[utility, split and frequency-shift effects](../fx-utils-split-bode.md); the originality policy is in
[compatibility and originality](../compatibility.md).
