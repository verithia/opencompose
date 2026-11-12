# oc-synth-import

Mapping from foreign preset formats and project state onto OpenSynth patch
types.

## What it owns

The translation layer. A preset or a stored state arrives with another product's
schema; this crate decides which patch fields it fills, reports what it could
not map, and leaves the rest of the patch at a documented default. The parser
may name an external format — that is the one place where a foreign product name
is legitimate, because it identifies the file being read.

`load_project_patch` is the entry point the renderer uses: it turns a preset's
parameter list, including a state or preset file path, into a `SynthPatch`.

## Key types

| Type | Role |
|---|---|
| `load_project_patch` | Preset parameters plus optional state file to a `SynthPatch`. |
| [`ImportWarning`](../api/oc_synth_import/index.html) | One field that could not be mapped, and why. |
| `ImportDiagnostics` | The complete report for one import: warnings plus the counts behind them. |
| `ForeignPreset`, `PresetMetadata` | The parsed foreign preset document. |

## Depends on

`oc-synth-core` and `oc-synth-fx` (patch types), `oc-dsp` (filter and curve
constants a mapping needs), `oc-dsl` (parameter values), plus CBOR, zstd and MD5
for the container and its integrity hash.

## Rules

- An unmapped field produces a warning, never silence. A user has to be able to
  tell "not supported" from "supported and different".
- Do not change `oc-synth-core`'s parameter ABI to make a mapping easier. If a
  mapping needs a new patch field, add the field first, with its own meaning.
- Import is one-way. Exporting to a foreign format is not in scope and would
  need the format's own writer.
- Do not put a device parameter table here; the parameter contract lives in
  `oc-device-schema` and `oc-synth-core`.

## API

[`api/oc_synth_import/`](../api/oc_synth_import/index.html).
