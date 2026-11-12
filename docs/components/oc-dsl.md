# oc-dsl

The language: the PEG grammar, the AST, validation, serialisation, the project
diff and the arrangement expansion every consumer shares.

## What it owns

- **Parsing and serialising.** `grammar.pest` defines the syntax; `parse` and
  `serialize` are the round trip. The serialiser's output is canonical, so a
  formatted project and a parsed one compare equal.
- **The AST** (`ast`), which is the only description of a project that the rest
  of the workspace reads. Every consumer — renderer, GUI, agent tools, training
  pipeline — works from this type, never from the text.
- **Validation** (`validate`): the grammar accepts the shape, the validator
  rejects what the engine cannot honour, with a line and a message.
- **Editing operations** (`ops`, `diff`): extract or replace one subtree by
  path, and compute or invert a patch, which is what the GUI's undo and the
  agent's transactions are built on.
- **Arrangement expansion** (`arrangement`): sections, `repeat:` and the
  rendered length, shared by the renderer and the timeline.
- **Timing and automation** (`timing`, `automation`, `formula`, `performance`):
  the tempo map as a clock, lane validation, the control formula, and raw
  channel events.
- **Instrument and media resolution** (`instruments`, `preset`, `media`): which
  preset a track ends up with, and which files a project needs.

## Key types

| Type | Role |
|---|---|
| [`Project`](../api/oc_dsl/ast/struct.Project.html) | Name, properties, presets, tracks, patterns, buses, master, sections. |
| [`ProjectProperties`](../api/oc_dsl/ast/struct.ProjectProperties.html) | Tempo, meter, key, swing, solo mode, MPE, export settings and the tempo, meter and key maps. |
| [`ProjectClock`](../api/oc_dsl/timing/struct.ProjectClock.html) | Converts between bars, quarter notes and samples through the tempo and meter maps. |
| [`Arrangement`](../api/oc_dsl/arrangement/struct.Arrangement.html) | The rendered timeline after sections and repeats are applied. |
| `ProjectPatch`, `ProjectHash` | The reversible edit unit used for undo and for agent transactions. |
| [`ParseError`](../api/oc_dsl/error/enum.ParseError.html), `ValidationError` | Line, column and message for the two failure kinds. |

## Depends on

`oc-device-schema` only, plus `pest`, `serde` and `serde_json`. It does not
depend on audio, the engine or a plugin host.

## Rules

- This crate is the language, so it must not learn to render. No `oc-audio`,
  `oc-dsp`, `oc-engine` or `oc-host` dependency: the moment the AST knows about
  a device, the format stops being engine-agnostic.
- Adding syntax means touching five places in this crate: the grammar rule, the
  AST type, the parser, the serialiser and the validator. A construct that parses
  but does not serialise loses data on save.
- The serialiser's canonical form is part of the format. Changing it changes
  every stored project and every GUI key binding for whole-project operations.
- A construct that the engine ignores is a bug, not a convenience. The parser
  refuses unknown fields on a track and pattern rather than storing them.
- Refusals carry a message that names the fix, because the message is what a user
  and an agent both read.

## Examples

The crate-level example parses a project and reads the AST. Every
hand-written `.ocp` block on this site is parsed by
`scripts/check-docs.sh`, so the documented syntax and the grammar cannot drift
apart silently.

## API

[`api/oc_dsl/`](../api/oc_dsl/index.html).
