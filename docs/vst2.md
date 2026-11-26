# VST2 plugins

OpenCompose hosts VST2.4 instruments and effects in process, next to CLAP,
VST3 and AudioUnit. A VST2 plugin is a first-class preset source and effect:

```ocp
@preset bass
  source: vst2
  plugin: "vst2:comp_3"

@track lead | preset: bass
  effects:
    - plugin:"vst2.vst2:comp_3"
```

The `plugin:` value is the same identifier any other format uses: the scanner
id (`vst2:<name>`), the bundle path, the bundle basename or the display name.
The effect form prefixes the id with `vst2.` so the renderer knows which host
to call, exactly like `vst3.` and `clap.` effects.

## Search paths

Bundles are discovered in the platform's standard VST2 directories:

| Platform | Directories |
|---|---|
| macOS | `~/Library/Audio/Plug-Ins/VST`, `/Library/Audio/Plug-Ins/VST` |
| Windows | `%ProgramFiles%\Common Files\VST2`, `%ProgramFiles%\VstPlugins`, `%ProgramFiles%\Steinberg\VstPlugins`, `%ProgramFiles%\Common Files\Steinberg\VST2`, `%LOCALAPPDATA%\Programs\Common\VST2` |
| Linux | `~/.vst`, `/usr/lib/vst`, `/usr/local/lib/vst`, `/usr/lib/lxvst`, `/usr/local/lib/lxvst` |

Set `OCP_VST2_PATH` to add directories (platform path separator: `:` on macOS
and Linux, `;` on Windows). This is also how tests and portable installs point
the host at a bundle that lives outside the system directories:

```sh
OCP_VST2_PATH=/path/to/plugins ocp resolve-plugin "vst2:my_plugin"
```

A macOS plugin is a `.vst` bundle; the host loads `Contents/MacOS/<name>`.
Windows loads the `.dll`, Linux the `.so`.

## What the host provides

- Loading through `VSTPluginMain` (falling back to `main`), `effOpen`,
  sample-rate/block-size setup, and `effMainsChanged` activation.
- Float32 stereo processing through `processReplacing`, falling back to the
  accumulating `process` entry point for older plugins.
- MIDI through `effProcessEvents` (notes, CC, program change, pitch bend,
  channel/poly pressure) when the plugin advertises `receiveVstEvents` or
  `receiveVstMidiEvent`.
- Parameters read through `effGetParamName`/`effGetParamLabel`/
  `effGetParamDisplay`; values are normalized 0..1 and can be addressed by
  name, case-insensitive name or `#index` in `params: { ... }` and automation.
- State through `effGetChunk`/`effSetChunk` (bank chunk). Presets can carry a
  sidecar with `vst2_state_file` or an inline blob with `vst2_state_hex`.
  These fields also accept `.fxb` parameter and opaque-state banks for the
  matching plugin. A version 2 bank restores its saved program selection
  when the plugin exposes programs. A version 1 bank supplies no selection.
  A `.fxp` restores the current program's parameters or opaque program state
  without selecting a different program.
  A restored insert receives one silent frame with the transport stopped before
  the mixer plans delay compensation. The host resets its processing history
  before it receives project audio.
- Host time through `audioMasterGetTime`: sample position, tempo at the start of
  the block, quarter-note position, bar start, meter and transport state. Tempo
  maps are honored the same way they are for VST3/CLAP.
- Host identity/version queries, process level, block size, sample rate,
  latency, `audioMasterIOChanged`, `audioMasterUpdateDisplay`, and
  begin/end-edit notifications. `audioMasterAutomate` feeds the plugin's own
  parameter changes back into the host parameter model.

## Scanning

The scanner never loads a third-party plugin in the app process. Every bundle
is opened in a disposable helper process. A crash, a hang and a rejected entry
point produce a scan receipt instead of taking the app down; crashed bundles are
quarantined. Clear `~/.opencompose/plugin-scan-receipts.json` to retry them.

Scanner metadata: display name, vendor, product, VST version, parameter count,
channel counts, category (`effGetPlugCategory`, with `inputs == 0` and the
synth flag as fallback) and a state fingerprint in `state_abi`.

## Streaming and offline rendering

Native-architecture VST2 plugins process in the host. A VST2 instrument with
several outputs exposes one port containing consecutive channel pairs. A
track's `pair: 1` selects the second pair as its main output. A mixer send
such as `send(drums.out4->room=100%)` taps the fourth pair. Both are read
from one processing call for the instrument block.

An insert with one input channel receives the average of the track's left
and right channels. A plugin with one output channel returns that signal
on both track channels.

On Apple Silicon, the translated helper discovers and processes Intel-only
VST2 instruments and mono or stereo inserts in desktop playback and offline export.
Projects can identify them by catalogue ID or absolute bundle path. The helper
restores native state and processes stereo PCM with the score's MIDI and
transport. For a four-input stereo effect, `route: kick -> fx1` sends the
key to channels 3 and 4 of effect slot 1. Missing helpers, worker exits
and failed control or audio requests surface as playback or export errors.
Playback sends bounded blocks on its background renderer. The audio device
callback consumes the same ring buffer it uses for native effects.

The Intel VST2 and VST3 workers have one 10-second deadline for initialization
and state restoration, including transferring the setup and reading its
reply. Concurrent attempts for the same unproven setup wait for the first
worker to respond. After a timeout, OpenCompose retires that worker and
skips the same setup for one minute in the current process. Changing the
plugin, worker binary or state allows a new attempt. **Rescan** clears
completed startup timeouts so a repaired installation can be retried
immediately. Working instances keep independent workers and the playback
and export request timeouts retain their respective budgets.

OpenCompose's npm/Tauri packaging command bundles the Intel worker in macOS
packages that include Apple Silicon. Install the Intel Rust target before
building one of those packages. At runtime macOS needs Rosetta to execute the
worker.

When running a standalone CLI from Cargo's build directory, build the helper
from the same checkout and put it beside `ocp`:

```sh
CXX=clang++ cargo build --locked --release -p oc-host \
  --target x86_64-apple-darwin --bin opencompose-vst2-bridge-x86_64
cp target/x86_64-apple-darwin/release/opencompose-vst2-bridge-x86_64 target/release/
```

You can also set `OPENCOMPOSE_VST2_BRIDGE_HELPER` to an absolute helper path.
The Intel worker accepts up to 64 native audio channels and sends the output
pairs requested by the project.

## Limitations

- **No editor.** `effEditGetRect` is queried so the plugin's editor size is
  known, but opening one needs a native child window and a run loop that cannot
  be exercised headless. `open_plugin_gui` returns a clean error for VST2, and
  the UI reports that the editor is unavailable.
- **One stereo sidechain.** Four-input effects can take a separate stereo
  key on channels 3 and 4. Without a route that input receives silence.
- **Float32 only.** The double-precision entry point is detected but not used.
- **Shell plugins are unsupported.** One bundle is one plugin; the host reports
  no current shell id.
- **Block-quantized automation.** VST2 has no sample-accurate parameter events;
  an automation change lands at the start of the block it falls in.
- **Playback trust boundary.** Scanning is crash-isolated, but a plugin that
  crashes while it is being rendered still takes the process down; that is
  inherent to in-process hosting.
- A plugin that reports zero audio outputs (an analyzer) loads and renders
  silence.
