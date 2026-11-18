# Plugins

OpenCompose hosts third-party instruments and effects in five plugin formats and
lists free ones in a built-in catalog. This chapter covers what is hosted, how a
plugin is addressed in a project, and what the free-plugin catalog does and does
not contain.

## Formats hosted

| Format | Instruments | Effects | Notes |
|---|---|---|---|
| **VST3** | yes | yes | Hosted in process; editor windows supported. |
| **VST2** | yes | yes | In process, one stereo output, no editor. See [VST2 plugins](../vst2.md). |
| **CLAP** | yes | yes | In process; editor windows supported. |
| **AU** (AudioUnit) | yes | yes | macOS only. One stereo output. See [AudioUnit hosting](../au-hosting.md). |
| **LV2** | yes | yes | In process, no plugin UI, one stereo output. |

A **standalone** application can appear in the free-plugin catalog as a download,
but the host does not load it; it is listed because the catalog describes
downloads, not only plugins.

## Where plugins are found

Bundles are discovered in the platform's standard folders, plus any folder you
add:

| Platform | Standard folders |
|---|---|
| macOS | `~/Library/Audio/Plug-Ins/` and `/Library/Audio/Plug-Ins/`, each with `VST3`, `CLAP`, `Components` (AU), `LV2` and `VST` |
| Windows | `%COMMONPROGRAMFILES%\VST3`, `\CLAP` and `\LV2`; `%LOCALAPPDATA%\Programs\Common\VST3` and `\CLAP`; `%LOCALAPPDATA%\LV2`; the usual `VstPlugins` and `Steinberg\VstPlugins` folders for VST2 |
| Linux | `~/.vst3`, `~/.clap`, `~/.lv2`, `~/.vst`, and the same names under `/usr/lib` and `/usr/local/lib` (plus `lxvst`) |

A bundle may sit up to three folders below a search folder, so a
`<VST3>/<vendor>/<plugin>.vst3` layout is found. A `.vst3`, `.clap`,
`.component` or `.lv2` bundle counts as one plugin file, and so does a `.vst`
(macOS), `.dll` (Windows) or `.so` (Linux) in a VST2 folder. A bundle can hold
several plugins; the plugin browser lists each one.

To search another folder, choose **Add plugin folder…** on the last page of the
first-run guide. The list is kept in `~/.opencompose/plugin-folders.json` (at
most 32 folders; `OCP_PLUGIN_FOLDERS` points at another file) and is read by the
scan, the LV2 loader and the Store's installed check alike. VST2 also honours
`OCP_VST2_PATH`, and LV2 honours `LV2_PATH`.

The same page counts what is already on the computer, without loading any
plugin: plugin files by format, and SoundFont (`.sf2`, `.sf3`), SFZ and
sampler-asset files in the instrument folders and your content libraries.
**Add sound folder…** registers a content library. The bank that ships with
OpenCompose is not counted.

After installing anything new, rescan from the plugin browser.

## Addressing a plugin in a project

A preset names its source and the scanner id:

```ocp
@preset bass
  source: vst2
  plugin: "vst2:surge_xt"

@preset gain
  source: lv2
  plugin: "https://example.org/lv2/gain"
```

The ids come from the scanner and are the same ones the plugin browser shows:

- `vst3:` and `vst2:` plus a lowercased name (`vst3:surge_xt`, `vst2:surge_xt`);
- the CLAP descriptor id for CLAP (`com.vendor.product`);
- the AudioComponent description for AU;
- the plugin URI for LV2 (`https://…` or `urn:…`).

In a track's effect list the format is prefixed to the id, for example
`- plugin:"vst2.vst2:surge_xt"`. The exact grammar is in
[The .ocp file](dsl.md).

## Load and save Kontakt state

Install and activate Kontakt and the sample library you want to use. In the
plugin browser, rescan VST3 instruments, add Kontakt as a project preset, then
open its editor. Load an instrument or a multi from inside Kontakt.

In the preset's **Rendering Profile → Plugin state**, choose **Embed** to
store the plugin state in the project or **Save** to write a state file. For a
file-based preset, `vst3_state_file` names the captured component state. A
relative file path is resolved from the project's directory. Kontakt's
`.nkm` multi is a document you open inside the plugin.

Assign the preset to a track and use the MIDI channel configured in Kontakt.
A saved state recalls the setup. Keep the plugin and its sample library
installed and licensed on each system that opens the project.

## The free-plugin catalog

The GUI's Store section lists free plugins from two sources:

1. a **bundled catalog** compiled into the app, and
2. a **signed remote index** that the app refreshes over HTTPS from the
   project's documentation site, caches on disk and verifies with the Ed25519
   keys pinned in the app. See [the plugin index design](../plugin-index.md).

Both carry the same document: an entry per plugin with a name, vendor, one-line
description, category tags, license, homepage and one or more installs. An install
says which platform and format it provides and where to download it. The app
shows the formats available for the current platform and the license, and either
downloads the archive, opens the installer, or opens the vendor's page.

**OpenCompose does not bundle or host plugin binaries.** Every catalog install is a link to
the vendor's own download or release asset; the app downloads it from there. The
catalog's `license` field records the plugin's license (an SPDX identifier, or
`Freeware` for a free proprietary download); it does not change the plugin's
terms.

The remote index format, hosting, signatures, caching and publishing commands are
documented in [the plugin index](../plugin-index.md). The index is self-hosted;
the app keeps a cached copy when a refresh fails and warns when the cache is more
than 14 days old. The app requests the index only when you open the Store's plugin
list or press Refresh: an existing cached copy paints the list first, and nothing
fetches the index in the background.

## Limits

- Hosting a plugin is not patching it: a bundle is never repackaged or edited. A vendor URL that moved is
  fixed by publishing a new catalog, not by uploading a copy.
- Format support differs: VST2 has no in-app editor, LV2 has no plugin UI, and
  VST2/LV2/AU host one stereo output. See the per-format notes in
  [VST2 plugins](../vst2.md) and [AudioUnit hosting](../au-hosting.md).
- A plugin that crashes while rendering still takes the process down; scanning is
  crash-isolated, rendering is not.
