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

Both use the same backend. Offline projects render VST2 instrument groups and
effect chains in process; streaming playback builds the same instance for a
track. Instrument tracks with the same preset share one instance, like the
other formats.

## Limitations

- **No editor.** `effEditGetRect` is queried so the plugin's editor size is
  known, but opening one needs a native child window and a run loop that cannot
  be exercised headless. `open_plugin_gui` returns a clean error for VST2, and
  the UI reports that the editor is unavailable.
- **One stereo output pair.** Multi-output VST2 instruments are not wired; a
  `send` that asks for an auxiliary output pair is rejected with an error.
  Plugins with more than two output channels render their first pair only.
- **No sidechain.** Extra input channels (a 4-input compressor, for example)
  receive silence.
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
