# oc-data

The MIDI-to-project-to-audio pipeline that produces `(DSL text, WAV audio)`
training pairs.

## What it owns

```text
MIDI files → midi2dsl → AST → serializer → .ocp files
                                 │
                           engine render → .wav files
                                 │
                      (DSL text, WAV audio) pairs
```

| Module | Owns |
|---|---|
| `midi2dsl` | Bounded file input and strict dataset acceptance over the engine-owned MIDI importer and summary. |
| `pipeline` | Walking a directory, writing the `.ocp` and the rendered `.wav`, and reporting per-file results. |
| `generate_midi` | Producing synthetic MIDI input for datasets with no source material. |

## Key types

| Type | Role |
|---|---|
| `process_single`, `process_directory` | One file, or a tree, to `(dsl, wav)` pairs. |
| [`ProcessResult`](../api/oc_data/pipeline/struct.ProcessResult.html) | Per-file outcome: written paths, counts, or the failure reason. |
| `midi_to_project` | Strict compatibility entrypoint; rejects missing releases, zero-length repairs and clock-loss warnings. |
| `midi_to_import`, `midi_bytes_to_import` | Explicit shared import result with source-PPQ timing and repair/loss summary. |

## Depends on

`oc-dsl` and `oc-engine`. It is not a dependency of any application crate: the
CLI calls it for the `batch-*` commands, and nothing else links it.

## Rules

- Output is text plus audio, and both are checked in by nothing. Do not write
  generated data into the repository.
- A conversion problem is reported per file and does not abort the run; the
  summary counts what failed.
- Determinism matters for a training set: the same input file must produce the
  same `.ocp` and the same WAV bytes.
- Do not grow this crate into the app's import path. Interactive MIDI import is
  `oc-engine::midi`; this crate is the batch tool.

## API

[`api/oc_data/`](../api/oc_data/index.html).

## Shared dataset import

SMF parsing, clocks, notes, percussion and program mapping live in
`oc-engine::midi`. The dataset profile retains source PPQ, raw timing annotations,
exact gates and velocities, standard notation durations, humanization offsets,
and drum grids with exact offset annotations. Simultaneous same-key onsets share
their next explicit release. Source tracks remain separate even when they share
a MIDI channel, avoiding the former converter's channel merge loss.

The application profile keeps its 480 PPQ and retrigger/sustain policy. Both
profiles expose the same summary type. Strict dataset conversion refuses repaired
missing/zero-length releases and clock warnings; callers can inspect the explicit
summary API instead. Pipeline results include `import_summary` when import
succeeded, even if strict acceptance then rejects the file. Dataset file reads use the shared engine regular-file reader and import budgets,
including a 64 MiB byte cap. Asynchronous type-2 SMF files are refused by this profile.
