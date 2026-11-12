# AGENTS-rust.md

Rules for Rust crates, the audio path, plugin hosting and the Tauri backend.
The hard rules in [AGENTS.md](AGENTS.md) still apply.

## Ownership and structure

- **One owner per rule.** Ranges, defaults, units and enum labels come from
  `oc-device-schema`. Dynamics and velocity curves, dB and pitch conversion,
  clock math, catalog validation and media I/O each have one owning module.
  Reuse it. When you consolidate copies, test parity across every caller first.
- **One behavior for every surface.** GUI, CLI, agent tools, live playback and
  export share domain operations and validation. A second implementation needs
  an explicit semantic difference and a test for it.
- **Dependency direction.** Pure DSP and domain crates (`oc-dsp`,
  `oc-synth-core`, `oc-dsl`, `oc-device-schema`, `oc-audio-types`) never depend
  on device I/O, GUI, networking, `oc-host` or command dispatch. Shared types
  move down to their owner or a dependency-light crate.
- **Module size.** Split by responsibility. When you add a responsibility to a
  large file, extract a cohesive module with its tests instead of growing a
  dispatcher. Keep mechanical moves and behavior changes in separate commits.
- **APIs.** Use named option and result structs instead of long positional
  argument lists, tuples or boolean flags.
- **Manifests.** Inherit `[workspace.package]` metadata. Declare shared external
  dependencies once in `[workspace.dependencies]`. Remove unused dependencies.

## Real-time audio

- The audio callback and everything it calls do not allocate, lock, log, spawn
  threads, or touch the filesystem or network. That includes plugin
  processing, seek, reset and parameter updates.
- Prepare scratch buffers, assets and event capacity outside the callback.
  Reuse them through explicit prepare and `process_into`-style APIs.
- Prove hot-path changes with allocation-counting and seek/reset regression
  tests, not with comments.

## Streaming playback

- Play, seek and previews start from the first correct block. They never wait
  for a whole-song or range render, unrelated library decoding or preparation
  of future sources. A background thread alone does not make blocking work
  streaming.
- Prepare what the requested cursor needs first, off the callback. Preserve
  authored program changes, sample positions and effect state. A silent
  placeholder is not a correct first block.
- Measure cold start through the production render worker, ring buffer and
  device callback, separately from command dispatch and compilation. Use
  audible fixtures to prove that resume delivers new PCM. Follow the budgets in
  `docs/performance-targets.md` and report misses.

## Typed boundaries and errors

- Deserialize `serde_json::Value` and raw strings at the adapter, then validate
  into domain types.
- Message kinds, session states and error kinds are tags or enums, never
  inferred from English text. Errors carry a stable code and structured cause.
  The UI localizes them.
- Derive schemas and frontend bindings from the owning types where you can.
  Otherwise add field, enum, unit and default parity tests. A matching count
  is not a matching contract.
- Persisted keys, wire tags (`opencompose_plugin_scan_receipt_v1`), the bundle
  identifier and the `OCPC` header are stable. A format change needs a
  migration and round-trip tests.

## Untrusted input and atomic failure

- Projects, presets, media, catalogs and archives are untrusted. Bound
  compressed and expanded bytes, counts, nesting, loop work and allocation.
  Use checked arithmetic and encoding-aware offsets (an LSP UTF-16 column is
  not a byte offset).
- Carry a checked limit through the actual read. Allocate from the captured
  capacity and reject growth or truncation.
- GUI, CLI and agent installs share one verification path: signatures, trust
  roots, checksums, version and rollback policy, path containment and atomic
  publication. Test keys never become production defaults.
- Stage and validate before publishing. A failure leaves the document, undo
  and redo stacks and published files unchanged.
- Asynchronous work captures document, target and gesture identity. Check it
  when the mutation is admitted and again before publishing the result. A
  stale reply or release must not overwrite newer work.
- Unsupported devices, block sizes and parameters return explicit errors or a
  documented, visible fallback. Never truncate silently or substitute silence.

## FFI and plugin hosting

- At every native boundary, define ownership, initialization, teardown, thread
  affinity and buffer lifetime. Clean up deterministically on failure paths too.
- Contain panics and C++ exceptions for each ABI. Never unwind across one that
  forbids it.
- Every `unsafe` block has a `// SAFETY:` comment with the exact invariant.
  `unsafe impl Send`/`Sync` states the real reason, not "tests run serially".
- UI-thread authority comes from the real executable or event-loop bootstrap.
  Keep editor and controller work apart from processing, and never release
  UI-owned native objects on another thread.

## Diagnostics and comments

- Library crates do not leave `eprintln!` or `dbg!` behind. Use `log` or
  `oc-diag`.
- Comments explain intent, invariants and DSP math, in the present tense.
  History belongs in git.

## Reference

- Grammar: `crates/oc-dsl/src/grammar.pest` (pest). Positions are
  `[bar:beat:subdivision]`, with bar and beat 1-indexed. Clip ends are
  exclusive: `[1:1:0 - 5:1:0]` spans bars 1–4.
- Binary project format: `[4B "OCPC"][1B ver=1][4B uncompressed_size_LE][DEFLATE(JSON)]`.
- Undo and redo coalescing and structural diffs live in
  `crates/oc-gui/frontend/src-tauri/src/state.rs`; see `docs/patch-system.md`.
