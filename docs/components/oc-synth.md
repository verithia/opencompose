# oc-synth

The stable surface of the native instruments: OpenSynth, the preset importer,
the drum kit, the sampler and the performance scheduler. The implementations
live in `oc-synth-core`, `oc-synth-fx` and `oc-synth-import`.

## What it owns

The module paths that the rest of the workspace imports. `oc_synth::synth`
re-exports the OpenSynth surface and the preset importer, and this crate adds
the four pieces that sit around a voice engine:

| Module | Owns |
|---|---|
| `drumkit` | `@kit`: pads, velocity layers, choke groups, round-robin, and the lowering to a builtin preset. |
| `audio_region` | Sample-file regions read from a document, and the `AudioRegion` playback path. |
| `performance` | Ordered channel commands (raw MIDI-shaped events) and their scheduling. |
| `native_performance` | Binding a patch to a performance program, and rendering it without the project. |

## Key types

| Type | Role |
|---|---|
| [`DrumKit`](../api/oc_synth/drumkit/struct.DrumKit.html) | The pad grid: `Kit`, `Pad`, `Layer`, choke groups and round-robin banks. |
| [`AudioRegion`](../api/oc_synth/audio_region/struct.AudioRegion.html) | A decoded sample-file region with its root note and loop settings. |
| `ScheduledCommand` | One time-ordered channel command handed to a voice engine. |
| `PatchBinding` | A patch plus the program it is bound to, for a standalone render. |

## Depends on

`oc-dsl`, `oc-synth-core`, `oc-synth-import` and `hound` for sample files. It
does not depend on the engine, the host or a device crate.

## Rules

- This crate is a facade: no new DSP. A change to voice behaviour goes in
  `oc-synth-core`; a new rack module in `oc-synth-fx`; a new mapping in
  `oc-synth-import`.
- Keep the re-exported paths stable. `oc_synth::synth::OpenSynth`,
  `oc_synth::drumkit::*` and `oc_synth::sampler` are imported across the
  workspace, and a move here breaks three crates at once.
- Do not add plugin hosting or a project-level mixer.

## API

[`api/oc_synth/`](../api/oc_synth/index.html).
