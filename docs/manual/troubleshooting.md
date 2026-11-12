# Troubleshooting

The failures below are the ones users actually report. Each one names the
observable symptom first, then the fix.

## The window is blank

A development build loads its UI from Vite. Start the app with
`npm run tauri dev` from `crates/oc-gui/frontend` so Tauri starts Vite and reports
any startup errors in the terminal. A standalone release build embeds its
frontend. Run `npm run build` from the same directory before a manual release
build with Cargo.

## A plugin vanished and now the project says "plugin not found"

The scanner caches what it found in
`~/.opencompose/plugin-scan-receipts.json`. Two things go wrong:

1. **Stale or clobbered receipts.** Concurrent sessions overwrite the file, and
   a receipts file can hold a `failed` receipt for every plugin if it was
   written on a host that could not run the scan helper. Delete the file and
   rescan:

   ```sh
   rm ~/.opencompose/plugin-scan-receipts.json
   ```

   then press **Rescan** in the Plugins sidebar tab. For parallel work, point
   each session at its own file with
   `OCP_PLUGIN_SCAN_RECEIPTS=/tmp/receipts-<task>.json`.

2. **A timeout quarantined the bundle.** A scan that runs out of time records
   `timed_out`, which hides the plugin. A stalled helper is retried once, and
   `timed_out` stops quarantining after 24 hours; `crashed` and
   `failed` receipts stay permanent. To rescan a quarantined plugin by hand:

   ```sh
   OCP_PLUGIN_RESCAN_QUARANTINED=1 ocp resolve-plugin "<plugin id or name>"
   ```

   If the bundle genuinely fails to scan, reinstall it or check that the host
   binary and the plugin have matching architectures.

## A plugin loads but renders silence

Some plugins load their sample content on a background thread and expose no
completion signal. A note that starts before the load finishes is silent for its
whole length.

- Render with a wall-clock warm-up: `ocp render song.ocp -o out.wav --pre-roll 8`.
- `--reruns N --require-identical` proves reproducibility, not correctness: two
  runs can both lose the race and still be byte-identical. Also check that the
  expected sample layer is actually present.
- `--pre-roll` is a sleep, not rendered silence, so it does not advance
  free-running oscillator phase.

## Render fails on an effect parameter

```
invalid typed effect parameter: high_gain=+2dB@8kHz (high_gain expects a gain in dB, such as high_gain=0dB)
unsupported effect parameter: high_shelf
```

The checked DSP path rejects a value a device cannot type, and the message names
the value it read and one spelling the device accepts. `ocp validate` runs the
same check as `ocp render`, so a project that validates also builds its effect
chain. Look the device up in `ocp devices list --json`, or in
[Built-in devices](devices.md), and use the names it lists. Values also need
their unit (`%`, `dB`, `Hz`, `ms`): `mix=18%` is 18 %, while `mix=0.18` is 0.18 %.
A unit-shaped compound such as `+2dB@8kHz` belongs to the retired shorthand
(`high_shelf=…`), not to a current control.

Older files may use the retired vocabulary: `reverb: type=plate, size=large`, or
`eq: high_shelf=-2dB@10kHz`, `low_cut=30Hz`, `notch=…`, `presence=…`. The render
path still accepts these through a compatibility layer and prints a warning for
each expansion (see [DSL compatibility](../dsl-compat.md)); they are not current
device parameters, so rewrite them with `ocp devices list --json` when convenient.
A value the layer cannot represent is reported and skipped, not applied silently.
Parse with `ocp parse --json` to find the offending effect.

## Audio clips: missing, wrong length, or over budget

- `audio clip 'intro': file not found: /project/media/intro.flac` — the `file:`
  path is relative to the `.ocp` file. Absolute paths are rejected in the DSL.
  `ocp validate` decodes every referenced clip, so run it before rendering.
- A clip shorter than its media plays the trimmed prefix; longer clips hold
  silence after the media. `stretch:` outside `0.25`–`4.0` is refused.
- Splitting a clip that uses `tempo_follow: true` is refused: each half would
  derive a different ratio from the same media. Choose an explicit `stretch:`
  first.
- A project can exceed the derived-media budget at load:

  ```
  audio clips need 214.0 MiB of stretched media at load, above the 256.0 MiB
  derived-media budget; shorten the clips or raise OCP_DERIVED_CACHE_MEMORY_BYTES
  ```

  Shorten the clips or raise `OCP_DERIVED_CACHE_MEMORY_BYTES`.
- Renaming a track does not rewrite `@audio_clip ... | track:` references.
  Validation then reports the stale reference; update the clip's `track:` name.

## Mixer and routing

- `Bus routing cycle` — `routes:` must form a DAG; remove the edge that closes
  the loop.
- A send above +12 dB is rejected by the validator and the GUI. A send above
  unity is a drive into the bus, not a level control; compensate at the bus
  return fader.
- A soloed bus keeps its inputs audible and keeps its output running. If a
  channel goes silent under a solo, it does not route into the kept bus
  component; this is the solo rule, not a bug.

## Plugin format quirks

- **AU** plugins render in process on macOS: instruments (`aumu`), effects
  (`aufx`) and music effects (`aumf`), named by display name
  (`Apple: DLSMusicDevice`), four-character codes (`aumu:dls:appl`) or
  `.component` path. See [AudioUnit hosting](../au-hosting.md). Generator,
  mixer, panner, output and format-converter units are not hosted: a project
  that names one renders that track silent and `ocp render` prints why. AU
  exposes one stereo output, so a send that asks for an auxiliary pair is an
  error. Host tempo and meter reach a unit through `HostCallbacks`; a unit that
  rejects the property keeps a constant context and logs one warning. AUv2 has
  no per-note expression entry point, so MPE expression is dropped (per-channel
  pitch bend and pressure still work). The live render thread builds AU through
  the same instance as offline; AU preset audition is not part of the verified
  surface.
- **VST3** raw MIDI CC is not passed through universally; use the plugin's own
  parameters. MIDI 2.0 is not passed through, and authored tempo/meter map
  points are delivered at block starts rather than as a continuously ramping
  tempo.
- **Multi-output** instruments and full state/preset round-trips are only partly
  implemented. Auxiliary outputs are captured offline and by live streaming; a
  pair the loaded source cannot serve keeps the main pair only with one warning.
- **Native plugin editors** are qualified on macOS only.
- **VST3 state** is written to the processor and the controller, so the editor
  shows the loaded preset instead of the init patch. A plugin with additional
  state streams may still not round-trip fully.
- **Plugin effect parameters** are applied when the effect chain is built, and
  automation lanes move them per block during playback.

## The VS Code extension cannot find the language server

The extension starts `ocp-lsp`. Build it with `cargo build` (or
`cargo build --release`), then either put `ocp-lsp` on `PATH` or set
`opencompose.lsp.path` in the extension settings to the absolute binary path.
Leave the setting empty and the extension tries the development path relative to
the extension directory first; an explicit path is the reliable option.

## Export a diagnostic bundle

**Mod+Shift+D**, or **Diagnostics** in the preferences popover, writes
`opencompose-diagnostics-<stamp>.tar.gz` and opens its folder. Attach it to a
report.

The archive is a gzipped tar:

| Entry | Contents |
|---|---|
| `manifest.json` | Generation time, app version, build profile, and every entry's `included`/`missing` status with a reason |
| `logs/recent.log` | The in-app log |
| `project/project.ocp` | The current project source, if available |
| `environment.json` | OS, architecture, OS version, app version, build profile, project path, log path |
| `plugins/scan-summary.json` | Plugin scan summary |
| `errors/last-error.txt` | The last error, if any |

A section that could not be collected is listed as `missing` with a reason rather
than omitted silently.
