# oc-synth-core

The OpenSynth voice engine: oscillators, warps, filters, envelopes, the mod
matrix, the arpeggiator, and the sampler that shares the same voice path.

## What it owns

The synthesis itself. A track with a `builtin` preset renders through
[`OpenSynth`](../api/oc_synth_core/synth/struct.OpenSynth.html), which owns the
patch, the voice pool and the block-level processing. The sampler, the granular
and spectral sources and the arpeggiator are modules of this crate because they
feed the same voice path and must obey the same realtime rules.

`SynthPatch` parameter names are a versioned project-file ABI: documents in the
wild name them, so they stay stable.

## Key types

| Type | Role |
|---|---|
| [`OpenSynth`](../api/oc_synth_core/synth/struct.OpenSynth.html) | The polyphonic instrument: note events in, stereo audio out. Single-threaded by contract. |
| [`SynthPatch`](../api/oc_synth_core/synth/struct.SynthPatch.html) | The full parameter set, including the FX chain tree and the mod matrix. |
| [`PatchDocument`](../api/oc_synth_core/synth/document/struct.PatchDocument.html) | A self-contained patch file: version, metadata and an embedded full state, never overrides against changing defaults. |
| [`SamplerPatch`](../api/oc_synth_core/sampler/struct.SamplerPatch.html) | Sample regions, loop modes, velocity curves and the amp envelope for `source: sample`. |
| `WaveformPreview`, `waveform_preview` | One-screen preview data for the patch editor. |

## Depends on

`oc-dsl` (parameter values), `oc-dsp` (filter kernels and the shared biquad
code), `oc-synth-fx` (the rack after the voice sum) and `oc-device-schema`
(parameter contracts).

## Rules

- Realtime: no allocation, no locks and no threads after construction. The
  renderer parallelises across tracks; it never parallelises a voice.
- The FX rack runs after the voice sum and before the output soft clip. A new
  effect belongs in `oc-synth-fx`, not here.
- Do not add hosting. Loading a plugin, scanning a bundle or drawing a plugin
  editor belongs in `oc-host`.
- Patch parameter names are an ABI. A rename needs a migration in
  `oc-synth-import` and a note in the DSL chapter.
- A patch never silently substitutes a factory wavetable for a missing custom
  one; the packaging step must say so.

## API

[`api/oc_synth_core/`](../api/oc_synth_core/index.html).
