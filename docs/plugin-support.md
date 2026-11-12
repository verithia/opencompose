# External plugin support

OpenCompose hosts third-party instruments and effects in five formats (VST3,
VST2, CLAP, AU, LV2). A hosted plugin is one device in a project: its
parameters, preset format and DSP are the plugin's own, and the host does not
reimplement them.

This chapter is the one place in the manual where third-party products are named
by name. It is about supporting them, the way a DAW manual lists the plug-ins it
has been tested with. Design provenance is not recorded in this chapter; see
[Compatibility and originality](compatibility.md).

## How a hosted plugin becomes a device

1. **Scan.** The scanner enumerates the platform's plugin directories and writes
   a stable id per bundle: `vst3:`/`vst2:` plus a lowercased name, the CLAP
   descriptor id, the AudioComponent description, or the LV2 URI. A project
   addresses the plugin by that id.
2. **Inventory.** On load the host calls the plugin's parameter API
   (`getParameterInfo` for VST3) once per parameter and records the id, the name,
   the step count and the default. Units are not a separate field in VST3; the
   host formats the normalised minimum, maximum and default through the plugin's
   own `getParamStringByValue`, so a unit such as `dB`, `Hz` or `%` comes from
   the plugin's string, not from a host table.
3. **Model.** Every parameter becomes a normalised `0.0..=1.0` automation lane.
   That is the only surface the host guarantees for an arbitrary plugin. Plain
   values (dB, Hz, enum index) are available only when the host can invert the
   plugin's own formatted string, which is what the `.ffp` importer uses.
4. **Render.** The effect is called with the block and the transport; parameter
   changes are queued and delivered in the parameter-change queue of the block,
   not written into the processor behind its back. On the streaming path a
   plugin that has been silent and inputless for four seconds stops being
   called; its reported tail lengths that decision, and a track or effect can
   opt out with `smart_disable=false`. See
   [Playback latency](playback-latency.md#smart-disable-for-silent-plugins).

`ocp plugin-params <id> --json` prints the inventory (id, name, step count,
formatted minimum/maximum/default, flags) without opening an editor. It is the
same data the device panel shows.

### Inventory cache

Building the inventory is expensive: a cold scan walks every installed bundle
and inspects each one in a helper process (2.5–4 s for 64 entries). The app
therefore stores the result in
`~/.opencompose/plugin-inventory.json` and treats a warm start as a cache read.

The file holds the host inventory, the Store's installed list, and the key both
were taken from:

- one stamp per plugin directory (the platform directories plus
  `OCP_VST2_PATH`) and one for `~/.opencompose/runtimes`, each a path and an
  mtime;
- the scan-receipt file (`~/.opencompose/plugin-scan-receipts.json`), by mtime
  and length, because a rewrite changes both;
- the app version and the built-in plugin count, so a new build never serves the
  previous build's built-ins;
- a schema tag, so an older file is ignored rather than misread.

Validating the key costs one `stat` per directory and no directory walk (about
70 µs). When the key matches, startup publishes the stored list before the first
panel asks. When it does not, startup still serves the stored list and rescans
in a background thread; the new scan is published, written back, and announced
with `plugin_inventory_updated` (payload: entry count), which the plugin browser
uses to repaint its list and compatibility report. The window, the project-open
path and the first transport press never wait for a scan.

The explicit **Rescan** button and `plugins_scan` with `rescan: true` always walk
the bundles, publish the result, and rewrite the cache. A launch with no cache
starts the scan in the background and writes the file, so the next launch is
warm; an inventory read that arrives first waits for that scan. A corrupt or
oversized cache file is ignored and rebuilt. The
file is written 0600 inside a 0700 directory by the same private-file writer as
the rest of the app state; `OCP_PLUGIN_INVENTORY` overrides its path for
harnesses.

### Scan quarantine

A bundle that crashes the helper or fails to load produces a receipt in
`~/.opencompose/plugin-scan-receipts.json`. The receipt keys the bundle's
loadable binary by path, mtime and size, so replacing the binary in place
invalidates it and the next scan of any kind retries the bundle. A bundle whose
binary has not changed stays quarantined, so opening a project does not pay for
it again.

The explicit **Rescan** button, `ocp scan --rescan`, and `plugins_scan` with
`rescan: true` retry every bundle, including the quarantined ones. Each bundle
still gets one bounded helper run per rescan, so one that keeps crashing is
quarantined again instead of looping; the rescan logs how many bundles it
retried and how many still failed. A `timed_out` receipt also stops
quarantining after 24 hours, and a `HelperMissing` receipt is never written.

The host drains the helper's stdout and stderr while it runs and bounds both
streams, so a helper that prints without limit cannot grow the scan's memory.
The deadline covers the helper's descendants too: a child process that keeps an
inherited pipe open is killed with the helper and the bundle is recorded as
`timed_out`, rather than leaving the scanner blocked after the helper exits.

Adding or removing a bundle or a managed runtime moves a stamped directory;
replacing a binary in place moves both its receipt and the inventory key.

### Unverified plugins

Most hosted plugins are unverified. They still load and render, and their
parameters are still discoverable and automatable, but the host knows nothing
about their meaning:

- parameters are addressed by the plugin's own names and indices, with no
  musical mapping and no rename;
- no preset parser: the plugin's state is saved and restored as an opaque blob,
  so a preset written by the plugin's own UI round-trips but a factory preset
  file does not import;
- an unverified plugin may fail to instantiate, may render silence, or may crash
  the process, because rendering is not sandboxed; scanning is.

A plugin graduates to *verified* when the host has been measured against it.

## What "verified" means

"Verified" is a claim about the host, not about the sound. A plugin is verified
when all of the following were measured with the installed
build:

- it instantiates at a chosen sample rate and block size;
- the parameter inventory is stable across loads, and parameter edits reach the
  audio of the next block;
- it renders offline, repeatably where the plugin itself is deterministic;
- its state saves and restores through the host;
- a preset format is implemented if the plugin ships one.

The measurement lives in the test suite (`crates/oc-host/tests/suite/` and the
gated probes) and in the `.ffp` fixtures; it is not a compatibility guarantee
for other builds, other versions or other platforms.

Plugins that are not hosted still have special handling. Serum preset files are
imported by their own parser ([the synth import component](components/oc-synth-import.md)),
and a `.ffp` file can be imported into a loaded VST3
instance by `ocp plugin-preset import`. Both are format importers, not hosting.

## Verified plugins

All rows were measured on macOS arm64, 48 kHz, against the builds installed in
the standard plugin directories.

| Plugin | Formats | What was verified | Not supported |
|---|---|---|---|
| Pro-Q 2 | VST3, AU, CLAP | Inventory (328 parameters, 191 real), offline render, `.ffp` factory presets, deterministic render | Dynamic bands (the plugin has none) |
| Pro-Q 3 | — (not installed) | `.ffp` signature dispatch from a third-party file | Everything else: the plugin is not installed |
| Pro-Q 4 | VST3, AU, CLAP | Inventory (737 parameters), editor attach, output-level render, `.ffp` text presets | Spectral and surround band controls are exposed but unmeasured |
| Pro-C 3 | VST3, AU, CLAP | Inventory (237 parameters, 100 real), `.ffp` ASCII presets, parameter round trip | — |
| Pro-R | VST3, AU, CLAP | Inventory (222 parameters, 85 real), `.ffp` binary presets | The render is not bit-repeatable while Character is above 0, because the plugin modulates randomly |
| Pro-MB | VST3, AU, CLAP | Inventory (288 parameters, 151 real), `.ffp` binary presets | — |
| Pro-DS | VST3, AU, CLAP | Inventory (157 parameters, 20 real), `.ffp` binary presets | — |
| Pro-L 2 | VST3, AU, CLAP | Inventory (169 parameters, 32 real), `.ffp` binary presets | Loading requires the plugin's `*.limiter` resources next to the host binary |
| Kontakt 8 | VST3, AU | Instantiates (4145 parameters), declares 32 stereo output pairs, renders offline, accepts a factory multi through state | Renders silence until a multi is loaded; a `.nki` is not host state; a multi cannot be built without the plugin's UI |
| ChowMultiTool | VST3 | Editor opens, attaches, resizes and closes | — |
| Airwindows Consolidated | VST3 | Editor opens, attaches, resizes and closes | — |

"Not supported" is specific to the host path that was measured; a plugin's own
features remain available through its editor.

### Kontakt 8

The VST3 build (8.14.0) declares 32 stereo output pairs and 4145 parameters. An
empty rack renders silence: a 4 s phrase renders 32 s at peak 0.0, because the
plugin reports an infinite tail and the renderer bounds it. A factory multi
loads through the state port as a native `.nkm`, and the state read back differs
by a few bytes between loads, so compare audio rather than bytes. A `.nki`
instrument is not host state: feeding one to `vst3_state_file` aborts the
process inside the plugin. Building a multi needs the plugin's own editor, which
a headless render does not provide.

The AU build reports one stereo output, so a multi-output project cannot route
it, and it accepts a `.aupreset` rather than a native multi. Loading the VST3
build in-process has crashed in the plugin's own Qt teardown at process exit, so
the gated probe measures in a child process and asserts on its report. The
scanner's cached metadata for the bundle still reports two inputs and two
outputs; the instance's bus inventory is authoritative.

## `.ffp` preset import

Factory presets in the `.ffp` container are read directly and applied to the
plugin:

- two containers: a binary one (4-byte signature, version, parameter count,
  then one little-endian `f32` per parameter in id order) and an ASCII one
  (`[Preset]`/`[Parameters]` with `name=value` lines);
- the signature selects the product and the expected parameter count;
- the payload holds **plain** values (frequencies are `log2(Hz)`, enums are
  indices, gains are dB); the importer converts them through the plugin's own
  normalisation, so the numbers land where the plugin puts them;
- a parameter the running plugin does not expose is reported as unmapped rather
  than guessed.

The import was verified against the factory presets shipped with the installed
plugins. Custom presets written by another version of a plugin
are not covered: an unknown signature is refused, and a known signature with a
different count is refused.

## Limits

- CLAP and VST3 instruments expose their declared stereo output pairs; VST2,
  LV2 and AudioUnit hosts are single-pair, and an effect that needs a second
  pair is refused. See [Multi-output routing](multi-output-routing.md),
  [VST 2 hosting](vst2.md) and [AudioUnit hosting](au-hosting.md).
- VST2 and LV2 have no in-app editor.
- Scanning is crash-isolated; rendering is not. A plugin that crashes while
  rendering takes the process down.
- Parameter automation is normalised. A plain-value automation curve is not
  guaranteed for an unverified plugin.
