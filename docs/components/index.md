# Component reference

Every workspace crate has a page here: what it owns, the types you touch, what
it may depend on, and what must not be added to it. The pages describe
behaviour; [`api/`](../api/index.html) is the generated authority for every
public type and function.

The workspace has nineteen members. The pages below cover the eighteen that
ship a public surface; `oc-test-support` is test-only. Read them in dependency
order, bottom up:

| Layer | Crate | Page |
|---|---|---|
| Build and diagnostics | `oc-build-parity` | [oc-build-parity](oc-build-parity.md) |
| Build and diagnostics | `oc-diag` | [oc-diag](oc-diag.md) |
| Contracts | `oc-audio` | [oc-audio](oc-audio.md) |
| Contracts | `oc-device-schema` | [oc-device-schema](oc-device-schema.md) |
| Language | `oc-dsl` | [oc-dsl](oc-dsl.md) |
| Devices | `oc-dsp` | [oc-dsp](oc-dsp.md) |
| Synthesis | `oc-synth-core` | [oc-synth-core](oc-synth-core.md) |
| Synthesis | `oc-synth-fx` | [oc-synth-fx](oc-synth-fx.md) |
| Synthesis | `oc-synth-import` | [oc-synth-import](oc-synth-import.md) |
| Synthesis | `oc-synth` | [oc-synth](oc-synth.md) |
| Hosting | `oc-host` | [oc-host](oc-host.md) |
| Rendering | `oc-engine` | [oc-engine](oc-engine.md) |
| Applications | `oc-cli` | [oc-cli](oc-cli.md) |
| Applications | `oc-sample` | [oc-sample](oc-sample.md) |
| Applications | `oc-market` | [oc-market](oc-market.md) |
| Applications | `oc-lsp` | [oc-lsp](oc-lsp.md) |
| Applications | `oc-gui` | [oc-gui](oc-gui.md) |
| Data pipeline | `oc-data` | [oc-data](oc-data.md) |

## The layering rule

Dependencies point one way: contracts ← language ← devices ← synthesis ←
hosting ← rendering ← applications. A crate may use the crates to its left and
must not reach to its right. The `oc-data` crate consumes the language and the engine for project and MIDI conversion.

Two crates carry no runtime code at all. `oc-build-parity` exists so the CLI and
the GUI resolve one artifact set, and `oc-diag` compiles its call sites away
unless a feature is on. Both are deliberate: see their pages.

## Where to look next

- How a document becomes a WAV file: [The engine pipeline](../architecture.md).
- Which crate a change belongs in: the page for the crate that owns the
  behaviour, not the application that happens to call it.
- The module layout of a crate: its own API page, section *Modules*.
