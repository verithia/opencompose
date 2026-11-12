# oc-engine

Rendering: interpretation, sources, the mix graph, the render caches, media
handling and the WAV writer. This is the crate that turns a `Project` into
audio.

## What it owns

The pipeline described in [The engine pipeline](../architecture.md):
`interpret` builds the timeline, `block_source` and the soundfont, sampler and
plugin paths produce audio, `graph` sums it, `render` applies loudness and the
ceiling, `wav` writes the file. Around that spine sit the caches and the
analysis tools the GUI and the agent surface call.

| Group | Modules |
|---|---|
| Timeline | `interpret`, `performance_send`, `mpe`, `playability`, `note_analysis`, `align` |
| Sources | `block_source`, `soundfont`, `sf2`, `audio_clip`, `stretch`, `phase_vocoder` |
| Mixing | `graph`, `render_parallel`, `stems`, `touch` |
| Output | `render`, `wav`, `resample`, `analysis`, `spectrum` |
| Caches | `render_cache`, `media_cache`, `derived_cache` |
| Voice work | `vocal_analysis`, `vocal_tune`, `vocal_snap` |
| MIDI | `midi`, `midi_io` |

## Key types

| Type | Role |
|---|---|
| [`RenderCache`](../api/oc_engine/render_cache/struct.RenderCache.html) | The authoritative render path: shared instruments, output fan-out, effects, buses and delay compensation, with per-track products the GUI can reuse. |
| [`AudioGraph`](../api/oc_engine/graph/struct.AudioGraph.html) | Track chains, sends, the bus tree and the master; the only place that sums audio. |
| [`BlockSource`](../api/oc_engine/block_source/enum.BlockSource.html) | A stateful instrument that produces audio in blocks for the length of one render. |
| [`MediaCache`](../api/oc_engine/media_cache/struct.MediaCache.html) | Decoded and resampled media, keyed by path, modification time and resample rung. |
| [`RenderPool`](../api/oc_engine/render_parallel/struct.RenderPool.html) | The offline worker pool: tracks render in parallel, the graph sums sequentially. |
| `PresetRegistry`, `Sf2Renderer`, `VocalAnalysis` | Preset resolution, the in-crate SoundFont player and the cached vocal analysis. |

## Depends on

`oc-audio`, `oc-diag`, `oc-dsl`, `oc-dsp`, `oc-synth` and optionally `oc-host`.
It does not depend on the GUI, the CLI or the LSP.

## Rules

- One graph, one truth. Do not add a second mixing path or a second WAV writer;
  stems and the full mix come from the same graph so they line up.
- The block loop allocates nothing and takes no locks. Anything that needs
  either belongs in setup or in the offline worker pool.
- Plugin instances come from `oc-host` and are used here; scanning policy is not
  this crate's business.
- Determinism is a feature: the same project, sample rate and options produce
  the same bytes. Anything random (dither) is seeded from the options.
- A new render-time decision that changes output needs a gate render to stay
  byte-identical, or a note that says why it cannot.
- Do not add DSL grammar here. If the engine needs a new field, add it to
  `oc-dsl` and read it from the AST.

## API

[`api/oc_engine/`](../api/oc_engine/index.html).
