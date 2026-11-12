# AGENTS.md

**OpenCompose** is a music description language (`.ocp`) with a desktop DAW
(Tauri + SolidJS), an offline and streaming audio engine, a synth and DSP
suite, a plugin host, a CLI (`ocp`) and a language server (`ocp-lsp`).

This file holds the rules that apply to every change. Read the topic file for
the area you touch:

| File | Read it when you… |
|---|---|
| [AGENTS-rust.md](AGENTS-rust.md) | change any Rust crate, the audio path, plugin hosting or the Tauri backend |
| [AGENTS-frontend.md](AGENTS-frontend.md) | change `crates/oc-gui/frontend/` or `editors/vscode-ocp/` |
| [AGENTS-testing.md](AGENTS-testing.md) | add tests, run checks, measure performance or touch CI |
| [AGENTS-repo.md](AGENTS-repo.md) | add files, docs, assets, dependencies or third-party names; commit |

[docs/contributing.md](docs/contributing.md) explains the reasoning behind
these rules for human contributors.

## Hard rules

1. **Never launch the desktop GUI, a Vite dev server that opens a window, or
   anything that plays audio through speakers.** Verify with headless Rust tests
   and Vitest.
2. **Nothing private goes in the repository.** Notes, review reports, task
   logs, renders, screenshots, corpora, keys and machine-specific paths or
   measurements belong in `./private/` (ignored) or outside the tree.
3. **English only** in code, comments, docs, commits and logs. Exceptions: UI
   locale dictionaries and tests of localized output or Unicode handling.
4. **Licensing.** The application is AGPL-3.0-or-later. `oc-dsl`,
   `oc-device-schema`, `oc-lsp`, `oc-test-support` and `editors/vscode-ocp` are
   MIT OR Apache-2.0 and must never depend on an AGPL crate. Never copy code
   whose license is incompatible with the destination.
5. **Originality.** Describe our design in our own terms. No reverse-engineering
   notes, decompiled names, offsets, reference-product constants, A/B harnesses
   against commercial products or competitor comparisons. A third-party name
   appears only to select a file format or identify a tested plugin host. See
   [docs/compatibility.md](docs/compatibility.md).
6. **One owner per rule.** Parameter ranges, defaults, units and enum labels
   live in `oc-device-schema`. GUI, CLI, agent tools, playback and export call
   the same domain operations through thin adapters.
7. **The real-time audio path** does not allocate, lock, log, spawn threads or
   touch the filesystem or network, including during seek, reset and parameter
   changes. Playback streams from its first correct block.
8. **Typed boundaries.** Use enums and structs inside; keep JSON and raw strings
   at adapters. Never infer state or errors from English text. Never rename a
   persisted key or wire identifier without a migration.
9. **Untrusted input is bounded, and failure is atomic.** A failed edit, import
   or install leaves the document, undo history and published files unchanged.
10. **Every change ships with a test** at the layer that owns the behavior, and
    every `unsafe` block has a `// SAFETY:` comment stating its invariant.
11. **Comments explain why, in the present tense.** No history ("used to",
    "before this change", "fixed bug where").

## Workspace

Rust workspace (edition 2024, resolver 2) under `crates/`:

| Crate | Role |
|---|---|
| `oc-dsl` | `.ocp` grammar (`src/grammar.pest`), AST, validator, formatter, serializer |
| `oc-device-schema` | Device and synth parameter schemas, ranges and units: the single source of truth |
| `oc-audio-types` | Effect and transport contracts shared by the DSP graph and plugin hosts |
| `oc-dsp` | Built-in filters, dynamics, EQ, reverb, delay, pitch/time and vocal processors |
| `oc-synth-core`, `oc-synth`, `oc-synth-fx`, `oc-synth-import` | OpenSynth voices, wavetables, modulation, rack FX, sampler, SF2/SF3, preset import |
| `oc-host` | VST3, CLAP, AU, LV2 and VST2 hosting; out-of-process scan and editor helpers |
| `oc-audio` | Device I/O (`cpal`) and deterministic test backends |
| `oc-engine` | Offline renderer, streaming block graph, render cache, analysis, MIDI I/O |
| `oc-market` / `oc-sample` | Signed plugin catalog; online sample providers with license tracking |
| `oc-data`, `oc-diag`, `oc-test-support`, `oc-build-parity` | Dataset conversion, JSONL diagnostics, test instrumentation, shared feature set |
| `oc-cli` (`ocp`), `oc-lsp` (`ocp-lsp`) | Command-line tool and language server |
| `oc-gui` | Tauri backend in `crates/oc-gui/frontend/src-tauri/`, SolidJS UI in `crates/oc-gui/frontend/` |

Also: `docs/` (mdBook), `examples/`, `benchmarks/`, `editors/vscode-ocp/`,
`scripts/`.

## Quick checks

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p <crate> --test all -- <filter>      # focused integration tests
scripts/check-docs.sh
(cd crates/oc-gui/frontend && npx tsc -b --noEmit && npx vitest run <files>)
```

The full list and the CI contract are in [AGENTS-testing.md](AGENTS-testing.md).
