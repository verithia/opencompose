# oc-device-schema

The typed contract for every built-in mixer device: its parameters, their units
and ranges, and its audio, key and control ports. It is data, not DSP.

## What it owns

One catalog that the DSL validator, the renderer, the GUI controls and the agent
registry all read. Because the catalog is the only description of a device, a
parameter name cannot drift between the parser, the engine and the editor: a name
the catalog does not declare is rejected when the project is validated.

The catalog also drives `ocp`'s agent-facing device listing, so a new device
parameter becomes visible to an agent without a second edit.

## Unit metadata and compatibility

Device catalog version 2 and synth catalog version 4 share the `units::Unit` authority.
Each catalog includes `units` definitions with a display multiplier, display
unit and suffix for new physical-value writes. `fraction` stores `0.4` and
displays `40%`; `%` stores and displays `40`. Display scaling never changes
persisted numbers, control steps, automation or DSP values.

Generated frontend catalogs and unit definitions are emitted by the existing
`gen-device-catalog` and `gen-synth-catalog` generators. CI runs
`scripts/check-generated-catalogs.sh`, which fails when a checked-in catalog no
longer matches its Rust owner. The GUI accepts version
1 device metadata by adapting only canonical built-in compression/multiband
mix units to `fraction`; it does not infer units from numeric ranges or alter
external devices. Static mixer percentage literals convert by 100; automation and synth patch
literals retain their historical context-specific interpretation. New fractional
writes use bare physical numbers. See [Built-in devices](../manual/devices.md).

## Synth automation laws

Synth metadata may declare `dspNormalization` independently of its display
`curve`. Cubic envelope/glide and square LFO-rise/delay or route-slew laws use
the declared physical maximum; zero remains an instantaneous time. DSP setters,
normalization and GUI automation readouts consume this shared declaration.
A UI travel curve describes slider movement and does not redefine the host's
normalized automation domain. Catalog revisions do not change the application
or project-format version.

Catalog schema 4 adds the `identity` DSP law for oscillator/output levels,
velocity depth and envelope amplitude fractions. It preserves physical 0–1
values directly, including signed zero; inverse conversion rejects values
outside that interval. This differs from saturating affine inversion. The
schema revision distinguishes the expanded closed law union for catalog
consumers; existing law spellings and project automation versions are unchanged.

Cutoff and free-running LFO/step rates declare their logarithmic host laws as
well. `dspNormalizationFloor` distinguishes the lowest reachable automated rate
from a valid static zero-rate setting: a normalized LFO position of zero means
0.01 Hz. The editor refuses to automate a physical value outside that reachable
interval rather than silently writing a different rate. Cutoff's authored Hz
range stays independent of the device; `dspRuntimeBounds` describes its separate
sample-rate-dependent bound after modulation.

Oscillator affine laws use the same declarations for pan, pulse width, B/C
transpose and detune. `dspNormalizationSpan` records an exact affine coefficient
when endpoint subtraction would change an established floating-point law;
pulse width uses 0.9. This preserves the host conversion independently of
slider travel and the final physical setter clamp. LFO phase, bend and smoothing,
and the step values and smoothing, also obtain their affine laws and defaults
from the catalog. Zero-origin controls multiply directly to retain the DSP's
signed-zero behavior.

## Key types

| Type | Role |
|---|---|
| [`DeviceSchema`](../api/oc_device_schema/struct.DeviceSchema.html) | One device: keyword, category, parameters, ports, shorthand forms. |
| [`ParamSchema`](../api/oc_device_schema/struct.ParamSchema.html) | One parameter: key, unit, range, default, curve and kind (continuous, switch, choice). |
| [`PortSchema`](../api/oc_device_schema/struct.PortSchema.html) | An input: audio, key, or control. A control port is what a `route: control` edge can address. |
| [`DEVICES`](../api/oc_device_schema/static.DEVICES.html) | The catalog itself; 29 device schemas, covering the built-in devices and their variants. |
| `synth` module | The same contract for OpenSynth: parameter families, controls, macros and FX types. |

## Depends on

`serde` and `serde_json`. Nothing inside the workspace: the schema must be
readable by the parser and the GUI without pulling in the engine.

## Rules

- No DSP here. A schema describes a control; the behaviour is in `oc-dsp`.
- Every parameter needs a unit, a range and a default. A parameter without a
  range cannot be validated, and one without a unit cannot be rendered into a
  fader.
- Ports are part of the contract. Adding a key input to a device means adding a
  port here first, or `route:` and `sidechain=` cannot address it.
- Renaming a parameter is a project-format change: old documents name the old
  key. Add an alias or a compatibility warning in `oc-dsl` instead.

## API

[`api/oc_device_schema/`](../api/oc_device_schema/index.html).

## General MIDI identity

The `gm` module owns the 128 zero-based General MIDI program names, stable picker names, family
banks, stable picker categories and existing instrument playback mappings.
`oc-dsl::gm` adapts project `InstrumentPath` values without another table.
The interpreter and MIDI exporter use the same playback mapping, including
steel guitar program 25 and finger bass program 33. Explicit imported program
numbers remain authoritative; a broad suggested instrument path is not an
inverse mapping for every program.

The existing `gen-device-catalog` generator also emits `gmPrograms.gen.ts` for
the preset picker. Localized labels retain their `preset.program.N` keys;
translations and family icons do not define program identity.
