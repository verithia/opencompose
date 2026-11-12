# oc-host

The plugin bridge: discovering plugins, loading them, keeping their state, and
exposing their parameters and programs to everything above.

## What it owns

| Area | Modules | What it does |
|---|---|---|
| Discovery | `scanner`, `clap_host`, `vst2::scan`, `lv2::metadata` | Finds bundles and reports them as `PluginInfo`, with a receipt so a scan is not repeated and a broken bundle can be quarantined. |
| Instances | `clap_instance`, `vst3_instance`, `vst2`, `au_instance`, `lv2` | Loads one plugin and drives it as an `AudioEffect`. |
| Parameters | `params`, `param_edit` | Describes a plugin's parameter surface, and edits one parameter without a full reload. |
| Programs | `programs` | Enumerates and selects programs where the format supports it, and reports the per-format capability. |
| State | `plugin_state` | Encodes and decodes a plugin's state into a project, including a migration path from one format's state to another. |
| Built-ins | `builtin` | The internal instruments and utilities the host itself provides: gain, pan, tremolo, bitcrusher, test tone. |
| Editor bridge | `editor_host`, `editor_monitor`, `host_app`, `host_message` | The host objects a plugin's editor needs, and the lock-free monitor that carries an editor's own audio output. |

The plugin inventory is scanned once and cached for the life of the process
(`cached_plugins`, `clear_resolved_plugin_cache`, `warm_plugin_inventory`).

## CLAP ownership and execution

The GUI and CLI register a `clap_main` executor on their actual main thread
before loading plugins. Preparation, native editors, state access and final
destruction run there. An exclusive audio-call permit identifies the processing
thread, including offline worker migration; host callbacks only publish flags
from that thread. The GUI event loop services those flags. The CLI enables
explicit offline service boundaries and pumps main work while joining parallel
render jobs; this servicing is disabled on realtime processing paths.

Canonical module leases pair successful entry initialization with deinitialization
and release the owned library reference after the final instance, editor or
control plan retires. Native calls use typed C++ exception barriers, and a
recorded failure retains its first cause and quarantines the affected owner.
Memory faults, process aborts and hung native calls require process isolation.

GUI adapters release the document mutex before native preparation or teardown.
Export, range and bounce workers keep shutdown receipts with their actual
snapshots, sharing cancellation with the renderer. Shutdown closes admission,
cancels registered work and services main-thread retirement while cooperative
workers finish. Its wait budget does not bound a hung call inside a plugin.

## VST3 native calls

Native C++ adapters invoke processing, setup, processing/activation transitions,
processor/controller state methods, audio bus discovery and arrangement, bus
activation, latency and tail queries inside their exception barriers. The
prepared instrument API returns a bounded `Vst3ProcessError`, including its
`Vst3Operation` and any native result code. The shared `native_fault` owner
publishes the first immutable cause without allocating on the processing path;
diagnostic strings are produced on the reporting side.

Failed processing discards the touched block and returns silence. A quarantined
owner is not entered again and retains its interfaces, host callbacks and
buffers. Refused or throwing stop/deactivate transitions also retain ownership.
Bus metadata failures retain the exact failed operation and abort construction
or quarantine a loaded owner. Ordinary optional arrangement refusals retain the
plugin’s default layout. Editor, controller metadata, factory and reference-count
calls still have unguarded paths. Native memory faults and hangs require process
isolation.

## Key types

| Type | Role |
|---|---|
| [`PluginInfo`](../api/oc_host/struct.PluginInfo.html) | One discovered plugin: format, id, name, vendor, path, category. |
| [`ClapInstance`](../api/oc_host/clap_instance/struct.ClapInstance.html), [`Vst3Instance`](../api/oc_host/vst3_instance/struct.Vst3Instance.html), `Vst2Instance`, `AuInstance` (macOS), `Lv2Instance` | One loaded plugin per format, each usable as an effect or an instrument. |
| [`ParamSurface`](../api/oc_host/params/enum.ParamSurface.html) | The parameters a plugin exposes, with units, ranges and edit handling. |
| [`ProgramEnumeration`](../api/oc_host/programs/enum.ProgramEnumeration.html) | Program list and selection capability for one format. |
| `PluginStateContext`, `encode_plugin_state`, `decode_plugin_state` | Storing a plugin's state in a project, with the format and version it was written for. |

## Depends on

`oc-audio`, `oc-dsp`, `oc-dsl`, `oc-synth`, `oc-device-schema`, `oc-diag`, plus
the format bindings (`clap-sys`, `vst3`, `vst2` ABI, macOS AU, LV2).

## Rules

- The host does not mix, route or schedule. A plugin receives audio and events
  for a block; `oc-engine` decides when.
- Native lifecycle and processing affinity belong to each format's owner.
  Never share an active instance between concurrent render jobs; retain code
  and callback storage until its native users finish. Editor monitoring uses
  the lock-free ring in `editor_monitor`.
- A format quirk goes in the format's module and is reported as a warning or a
  capability flag. Do not weaken the shared interface to fit one format.
- State is versioned. A new encoding gets a version number and a decode path for
  the old one, because projects in the wild contain old state.
- Do not add a plugin's product name to this crate. The importer names a format;
  the host names a plugin id.

## API

[`api/oc_host/`](../api/oc_host/index.html).
