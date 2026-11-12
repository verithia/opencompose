# VST3 conformance harness

The conformance harness answers one question for every installed VST3 bundle:
does this plugin meet the host contract this DAW relies on? It runs a fixed
battery of steps in a separate process per bundle and records pass, fail or a
termination for each step. The result is a matrix, not a pass/fail verdict for
the host as a whole.

Use it to turn "plugin X does not work here" into a row with a step, an elapsed
time and the error text the host received.

## The process model

`ocp plugin conformance` discovers VST3 bundles in the system and user plugin
folders, then starts one child process per bundle name:

```text
ocp plugin conformance
  ├── child: bundle A → load, buses, state, presets, params, editor, process, teardown
  ├── child: bundle B → ...
  └── child: bundle C → ...
```

The child hosts the plugin and streams one JSON line per finished step. The
parent collects those lines, adds a hard deadline of its own and prints the
matrix. A module that aborts, hangs the main thread or calls `exit` takes down
one child, not the matrix.

Two safety layers cover a plugin that does not return:

- **Child watchdog.** Each step arms a deadline, `--timeout-ms` and 5000 ms by
  default. When it expires, the child prints `{"fatal":"hang","step":"<name>"}`
  and leaves with status 70.
- **Parent hard deadline.** The parent kills a child that outlives twelve
  per-step deadlines or thirty seconds, whichever is larger, and records the
  first unreported step as `timeout`.

`SIGABRT`, `SIGBUS` and `SIGSEGV` handlers in the child print the step name and
leave with status 70. The child exits through `oc_host::exit_process`, so a
hosted module's static destructors never run.

The editor step needs AppKit on the main thread. The child is a command-line
process, so the editor is attached to a hidden, offscreen `NSWindow`; no window
is shown and no crash-report dialog is expected. Verify the diagnostic report
directory before and after a full run if that matters for the measurement.

## The battery

| Step | What it measures | Passes when |
|---|---|---|
| `load` | Component and edit controller, class name, factory version. | Both interfaces initialize and the factory names the class. |
| `buses` | Audio input and output bus counts, channel count and arrangement per bus, whether any output bus beyond the main pair exists. | At least one audio output channel is declared. |
| `state` | Processor `getState`, a second instance, `setState`, then the full normalized parameter vector and the same test render on both instances. | The parameter inventory is unchanged, values differ by at most 1e-4, and peak/RMS differ by at most 0.1 dB. Byte identity is informational. |
| `presets` | `IUnitInfo` presence, program count and `IProgramListData` presence. | The query returns; a plugin without programs reports zero. |
| `params` | Parameter count and one `setParamNormalized` round trip on the first parameter. | The setter returns `kResultOk` and the value reads back inside the parameter's step size. |
| `editor` | `createView`, `attached` to a hidden offscreen window, `getSize` before and after attach, one `resizeView` through the host frame, `canResize`, then `removed` and close. | The view attaches, reports a size after attach, accepts the host resize and detaches. |
| `process` | Two seconds of silence, then a note for an instrument at 48 kHz with 512-frame blocks, peak and RMS on the main output. | The block calls return; an instrument with a zero peak is reported `silent`, which is informational. |
| `teardown` | Destroy the state instance, then the main instance, in that order. | Both destructors return. An abort is recorded as `crash`. |

`--no-editor` records `editor` as `skipped` with that reason. A skipped step is
not a failure, but it is also not evidence of conformance.

## Statuses

| Status | Meaning |
|---|---|
| `pass` | The step met its condition. |
| `fail` | The step returned an error; the error text is in the report. |
| `skipped` | The step was disabled or could not run, for example `--no-editor` or a failed load. |
| `silent` | An instrument produced a zero peak after a note; informational for an empty sampler. |
| `hang` | The child watchdog expired during this step. |
| `crash` | The child died in this step, through a signal or a non-zero exit. |
| `timeout` | The parent killed the child after its hard deadline. |

`fail`, `hang`, `crash` and `timeout` block "passes every step". `silent` and
`skipped` do not.

## Command

```sh
# Build once, then run over every VST3 bundle the scanner finds.
cargo build -p oc-cli
./target/debug/ocp plugin conformance

# Machine-readable matrix for a notes file.
./target/debug/ocp plugin conformance --format json > matrix.json

# A subset, with a longer per-step deadline.
./target/debug/ocp plugin conformance \
  --plugin "/Library/Audio/Plug-Ins/VST3/Example.vst3" \
  --timeout-ms 15000 --format table
```

Flags:

| Flag | Meaning |
|---|---|
| `--format table\|json\|markdown` | Output format; `table` is the default. |
| `--plugin <path>` | Test only this bundle. Repeatable. Without it, every VST3 bundle in the system and user plugin folders is tested. |
| `--no-editor` | Skip the editor step. |
| `--timeout-ms N` | Per-step child deadline; default 5000. |

The command exits with status 1 when at least one plugin fails a step, after it
has printed the matrix. The JSON document is still complete.

## The JSON document

The document is one object with `schema`, `host`, `host_version`,
`generated_at_unix`, `timeout_ms`, `editor`, `plugins` and `summary`.

Each entry of `plugins` has `path`, `name`, `vendor`, `version`, `category`,
`steps`, `exit_code`, an optional `child_stderr`, and `duplicates` when the same
bundle name is installed in more than one folder. One row stands for one bundle
name; the other installation paths are listed in `duplicates`.

Each entry of `steps` has `step`, `status`, `ms`, a step-specific `detail`
object and an optional `error`. The detail carries the measurements the step
made: bus channel counts and arrangements, state byte sizes and whether they
are identical, parameter-vector differences, rendered peak and RMS, editor
sizes and resize results.

`summary` carries `plugins`, `passed_all`, `status_counts`, `step_failures`
(count per step), `failures` (one group per distinct step and error text) and
`top_failure_kinds`, the first three of those groups. Grouping is exact: two
plugins share a kind only when their error text is identical.

## Contract decisions

The harness records what the plugin returned; these decisions affect how a
return is classified.

- **State is behavioural, not byte-identical.** VST3 does not promise that
  `getState` returns the same bytes after a reload: a length prefix or a
  timestamp may change. The step therefore compares the normalized parameter
  vector and the rendered test tone. `state_bytes_identical`, the size delta and
  the first differing offset stay in the detail as information. Two outputs at
  or below -120 dBFS are treated as the same silence, not as a level difference.
- **`setComponentState` returning `kNotImplemented` passes with a note.**
  VST3 marks this interface optional. The step records
  `controller_set_component_state: "not implemented"` and still checks the
  parameter vector and the render. A controller that implements the call and
  rejects the state fails the step.
- **A silent instrument is reported `silent`, not `fail`.** An empty sampler
  that has no library loaded is an instrument that correctly renders silence.
  The status is informational and does not block "passes every step"; the row
  still shows the cell so the measurement stays visible. The instrument flag
  comes from the factory subcategories, so a synth that declares an audio input
  for sidechain or audio-in use is still tested with a note.
- **An architecture mismatch fails `load` before `dlopen`.** On macOS the
  harness reads the bundle's Mach-O slices and reports
  `bundle has no <arch> slice` instead of the host's generic load error. The
  host currently discards `dlerror()`, so this check is what makes the failure
  precise.
- **One row per bundle name, not per installation.** When the same bundle is
  installed in both the user and system folder, the first path in discovery
  order runs the battery and the other paths are listed in `duplicates`.

## Limits

- VST3 only. CLAP is the next format; the discovery and report shape leave room
  for it, but the battery currently calls VST3 interfaces.
- The harness measures the host contract, not sound quality, latency or CPU
  cost. It does not compare renders against a reference.
- The process step uses one note for an instrument and one second of audio after
  two seconds of silence. The `state` step reuses that render. A plugin with a
  longer attack or a multi-second load is measured as it behaves under that
  budget, not as it behaves in a full arrangement.
- The level comparison uses a 0.1 dB tolerance and a -120 dBFS silence floor.
  It detects a state that changes the sound, not a plugin whose oscillator
  phase or noise floor is not sample-exact.
- The editor step proves that the view attaches, resizes and detaches. It does
  not drive the plugin's own controls.
