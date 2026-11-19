# Troubleshooting

These are the failures users report most often. Each section starts with what
you see and then gives the fix.

## The window is blank

A development build loads its UI from Vite. Start the app with
`npm run tauri dev` from `crates/oc-gui/frontend`, so that Tauri starts Vite
and prints any startup errors in the terminal. A standalone release build
embeds its frontend. Before a manual release build with Cargo, run
`npm run build` in the same directory.

## A plugin vanished and the project says "plugin not found"

The scanner caches its results in `~/.opencompose/plugin-scan-receipts.json`.
Two things can go wrong with that cache.

1. The receipts are stale or overwritten. Sessions that run at the same time
   overwrite each other's file. A receipts file written on a host that could
   not run the scan helper can hold a `failed` receipt for every plugin. Delete
   the file:

   ```sh
   rm ~/.opencompose/plugin-scan-receipts.json
   ```

   Then press **Rescan** in the **Plugins** sidebar tab. If you run
   sessions in parallel, give each one its own file with
   `OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-<task>.json`.

2. A timeout quarantined the bundle. A scan that runs out of time records
   `timed_out`, and that hides the plugin. A stalled helper is retried once,
   and a `timed_out` receipt stops quarantining after 24 hours. `crashed` and
   `failed` receipts are permanent. To rescan a quarantined plugin by hand:

   ```sh
   OCP_PLUGIN_RESCAN_QUARANTINED=1 ocp resolve-plugin "<plugin id or name>"
   ```

   If the bundle still fails to scan, reinstall it, or check that the host
   binary and the plugin are built for the same architecture.

## A plugin loads but renders silence

Some plugins load their sample content on a background thread and give no
signal when the load is complete. A note that starts before the load finishes
stays silent for its whole length.

- Give the plugin a wall-clock warm-up:
  `ocp render song.ocp -o out.wav --pre-roll 8`.
- `--reruns N --require-identical` proves that the render is reproducible. It
  does not prove that it is correct: two runs can both start too early and
  still be byte-identical. Check that the expected sample layer is audible.
- `--pre-roll` sleeps for the given time before rendering. It renders no
  silence, so it does not advance the phase of free-running oscillators.

## Render fails on an effect parameter

```text
invalid typed effect parameter: high_gain=+2dB@8kHz (high_gain expects a gain in dB, such as high_gain=0dB)
unsupported effect parameter: high_shelf
```

The checked DSP path refuses a value that a device cannot type. The message
names the value it read and one spelling the device accepts. `ocp validate`
runs the same check as `ocp render`, so a project that validates can also build
its effect chain. Look the device up with `ocp devices list --json` or in
[Built-in devices](devices.md), and use the parameter names listed there.

Values need their unit (`%`, `dB`, `Hz`, `ms`). `mix=18%` is 18 %, and
`mix=0.18` is 0.18 %. A compound value such as `+2dB@8kHz` comes from the
retired shorthand (`high_shelf=…`) and matches no current control.

Older files may use the retired vocabulary, such as
`reverb: type=plate, size=large`, or `eq: high_shelf=-2dB@10kHz`,
`low_cut=30Hz`, `notch=…` and `presence=…`. The render path still accepts these
through a compatibility layer and prints a warning for each expansion (see
[DSL compatibility](../dsl-compat.md)). They are not device parameters, so
rewrite them with the help of `ocp devices list --json` when you can. A value
the layer cannot represent is reported and skipped; it is never applied
silently. To find the effect that fails, parse the file with `ocp parse --json`.

## Audio clips: missing, wrong length or over budget

- `audio clip 'intro': file not found: /project/media/intro.flac` means the
  `file:` path did not resolve. The path is relative to the `.ocp` file, and
  the DSL refuses absolute paths. `ocp validate` decodes every referenced clip,
  so run it before you render.
- A clip shorter than its media plays the start of the media up to the clip
  end. A clip longer than its media is silent after the media ends. A
  `stretch:` value outside `0.25`–`4.0` is refused.
- You cannot split a clip that uses `tempo_follow: true`, because each half
  would derive a different ratio from the same media. Set an explicit
  `stretch:` first.
- Stretched media that does not fit the in-memory derived-media budget
  (256 MiB by default, set with `OCP_DERIVED_CACHE_MEMORY_BYTES`) spills to
  the derived-media cache on disk. The load fails when the disk cache cannot
  hold it either:

  ```text
  audio clips need 2600.0 MiB of spooled media, above the available disk
  capacity for the 256.0 MiB derived-media budget
  ```

  Shorten the clips, or raise the disk cap with `OCP_DERIVED_CACHE_BYTES`
  (2 GiB by default).
- Renaming a track does not rewrite the `@audio_clip ... | track:` references
  to it. Validation then reports the stale reference. Change the clip's
  `track:` to the new name.

## Mixer and routing

- `Bus routing cycle`: the `routes:` entries must form a directed acyclic
  graph. Remove the route that closes the loop.
- The validator and the GUI refuse a send above +12 dB. A send above unity
  drives the bus harder; to change the level, use the bus return fader.
- A soloed bus keeps its inputs audible and its output running. If a track goes
  silent under a solo, it has no route into the part of the routing graph that
  the solo keeps. That is how solo works.

## Plugin format quirks

AU plugins render in process on macOS. OpenCompose hosts instruments (`aumu`),
effects (`aufx`) and music effects (`aumf`). A project names an AU by display
name (`Apple: DLSMusicDevice`), by four-character codes (`aumu:dls:appl`) or by
`.component` path. See [AudioUnit hosting](../au-hosting.md). Generator,
mixer, panner, output and format-converter units are not hosted: a track that
names one renders silent, and `ocp render` prints why. An AU has one stereo
output, so a send that asks for an auxiliary pair is an error. Host tempo and
meter reach a unit through `HostCallbacks`. A unit that rejects the property
keeps a constant context and logs one warning. AUv2 has no per-note expression
entry point, so MPE expression is dropped; per-channel pitch bend and pressure
still work. The live render thread builds an AU through the same instance as
the offline renderer. AU preset audition is not part of the verified surface.

VST3 plugins do not receive raw MIDI CC in every case; use the plugin's own
parameters. MIDI 2.0 is not passed through. Tempo and meter map points are
delivered at block starts, so a VST3 plugin sees the tempo change in steps.

Multi-output instruments and full state and preset round-trips are partly
implemented. The offline renderer and live streaming both capture auxiliary
outputs. When the loaded source cannot serve a requested pair, only the main
pair plays and one warning is printed.

Native plugin editors are qualified on macOS only.

VST3 state is written to both the processor and the controller, so the editor
shows the loaded preset and not the init patch. A plugin with additional state
streams may still not round-trip completely.

Plugin effect parameters are applied when the effect chain is built. During
playback, automation lanes move them once per block.

## The VS Code extension cannot find the language server

The extension starts `ocp-lsp`. Build it with `cargo build` (or
`cargo build --release`). Then either put `ocp-lsp` on your `PATH`, or set
`opencompose.lsp.path` in the extension settings to the absolute path of the
binary. With the setting empty, the extension first tries the development path
relative to the extension directory. An explicit path is the reliable choice.

## Export a diagnostic bundle

Press **Mod+Shift+D**, or click **Export diagnostics…** on the
**Help & About** page of Preferences. Choose where to save
`opencompose-diagnostics-<stamp>.tar.gz`. The app writes the archive and shows
it in its folder. Attach the file to your report.

The archive is a gzipped tar with these entries:

| Entry | Contents |
|---|---|
| `manifest.json` | Generation time, app version, build profile, and the `included` or `missing` status of every entry with a reason |
| `logs/recent.log` | The in-app log |
| `project/project.ocp` | The current project source, if available |
| `environment.json` | OS, architecture, OS version, app version, build profile, project path, log path |
| `plugins/scan-summary.json` | Plugin scan summary |
| `errors/last-error.txt` | The last error, if any |

A section that could not be collected appears in the manifest as `missing`,
with the reason.
