# oc-synth-fx

The synth-level effect rack: a small, fixed-size set of modules that runs after
the voice sum and before the output soft clip.

## What it owns

The rack that belongs to a patch rather than to a track. A patch stores its
rack tree in preorder — serial nodes, and split or parallel nodes whose child
chains follow them — and `SynthFxChain` resolves that tree into branch spans when
the patch loads, so processing allocates nothing.

The rack holds at most `MAX_SYNTH_FX` modules (16) with a fixed parameter palette
per type. Each time-based module has independent state, prepared when the rack topology changes;
buffers are sized for `MAX_FX_SAMPLE_RATE` (96 kHz), and above that rate a delay
time is clamped to the buffer instead of allocating on the audio thread.

## Key types

| Type | Role |
|---|---|
| [`SynthFxChain`](../api/oc_synth_fx/struct.SynthFxChain.html) | The resolved rack: branch spans plus per-module state. |
| [`SynthFx`](../api/oc_synth_fx/struct.SynthFx.html) | One module: type, enabled flag and its parameter slots. |
| [`SynthFxType`](../api/oc_synth_fx/enum.SynthFxType.html) | The available module kinds. |
| `fx_rack_latency` | Reported latency in samples, for delay compensation. |
| `MAX_SYNTH_FX`, `MAX_FX_SAMPLE_RATE`, `MAX_DELAY_SECONDS` | The fixed budget the buffers are built around. |

## Depends on

`oc-dsp` for every module implementation and for the kernels the rack shares
with the device chain. It does not depend on the voice engine.

## Rules

- The rack is post-voice and per-voice-count-independent. Do not add a module
  that needs per-note state; that belongs in the voice engine.
- Fixed budget: a new time-based module needs its buffer reserved in the
  constructor, or a documented clamp. Nothing allocates while processing.
- A module's parameter palette is part of the patch format. Adding a slot is
  fine; renumbering an existing one is a migration.
- Latency must be reported through `fx_rack_latency`, or a track that uses the
  module drifts against the others.
- Do not add track-level mixing or bus routing here. That is `oc-engine`.

## API

[`api/oc_synth_fx/`](../api/oc_synth_fx/index.html).
