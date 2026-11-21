# Plugins

OpenCompose hosts third-party instruments and effects in five plugin formats,
and lists free plugins in a built-in catalog. This chapter covers which
formats are hosted, where the app looks for plugins, how a project names a
plugin, and what the free-plugin catalog contains.

## Formats hosted

| Format | Instruments | Effects | Notes |
|---|---|---|---|
| VST3 | yes | yes | In process. Opens the plugin's own editor window. |
| VST2 | yes | yes | In process. One stereo output and no plugin editor; the preset editor shows a generated parameter panel. See [VST2 plugins](../vst2.md). |
| CLAP | yes | yes | In process. Opens the plugin's own editor window. |
| AU (AudioUnit) | yes | yes | macOS only. One stereo output; the preset editor shows a generated parameter panel. See [AudioUnit hosting](../au-hosting.md). |
| LV2 | yes | yes | In process. One stereo output and no plugin UI; the preset editor shows a generated parameter panel. |

The free-plugin catalog can also list a standalone application as a download.
The host does not load standalone applications. They appear because the catalog
describes downloads, not only plugins.

## Where plugins are found

The app searches four kinds of folder: the standard folders of each format,
common folders that other music software uses, folders named by environment
variables or another app's settings, and folders you add. **Preferences >
Folders** lists every folder it searches, grouped this way; see
[Preferences](../preferences.md#folders).

Standard folders:

| Platform | Standard folders |
|---|---|
| macOS | `VST3`, `CLAP`, `Components` (AU), `LV2` and `VST` under both `~/Library/Audio/Plug-Ins/` and `/Library/Audio/Plug-Ins/` |
| Windows | `VST3`, `CLAP` and `LV2` under `%COMMONPROGRAMFILES%`; `VST3`, `CLAP` and `VST2` under `%LOCALAPPDATA%\Programs\Common`; `%LOCALAPPDATA%\LV2`; for VST2, `Common Files\VST2`, `VstPlugins`, `Steinberg\VstPlugins` and `Common Files\Steinberg\VST2` under both `%ProgramFiles%` and `%ProgramFiles(x86)%` |
| Linux | `~/.vst3`, `~/.clap`, `~/.lv2` and `~/.vst`; `vst3`, `clap`, `lv2`, `vst` and `lxvst` under both `/usr/lib` and `/usr/local/lib` |

The folders in `OCP_VST2_PATH` count as standard VST2 folders.

The app also searches these folders when they exist:

| Platform | Common folders |
|---|---|
| macOS | `/Network/Library/Audio/Plug-ins/VST3`; `~/.lv2`, `/usr/local/lib/lv2` and `/opt/homebrew/lib/lv2` |
| Windows | `%APPDATA%\LV2`; `%ProgramFiles%\VST3`; `%ProgramFiles%\Cakewalk\Vstplugins`; `%USERPROFILE%\Documents\Plugins\VST`; the VST2 folder that installers record as `VSTPluginsPath` under `HKEY_CURRENT_USER\Software\VST` and `HKEY_LOCAL_MACHINE\Software\VST` |
| Linux | `vst3`, `clap`, `lv2`, `vst`, `lxvst` and `linux_vst` under both `/usr/lib64` and `/usr/local/lib64`; `/usr/lib/linux_vst` and `/usr/local/lib/linux_vst` |

And these, from the environment and from other apps' settings:

| Source | Folders |
|---|---|
| Environment variables, all platforms | Every folder in `CLAP_PATH`, `VST3_PATH`, `VST_PATH`, `LXVST_PATH` and `LV2_PATH` |
| REAPER, all platforms | `UserPlugins/FX` in its resource folder, and every `vstpath…` and `clap_path…` entry in its `reaper.ini` |
| FL Studio, Windows | The plugin search paths it keeps under `HKEY_CURRENT_USER\Software\Image-Line\Shared\Plugin search paths\List`, and its 64-bit VST2 folder (`VstDir64` under `HKEY_CURRENT_USER\Software\Image-Line\Shared\Plugins\FL Studio`) |

The REAPER resource folder is `%APPDATA%\REAPER` on Windows,
`~/Library/Application Support/REAPER` on macOS, and `REAPER` under
`$XDG_CONFIG_HOME` (or `~/.config`) on Linux.

Other apps' settings are read with limits:

- A settings file larger than 1 MiB is not read.
- At most 64 absolute paths are taken from all other apps' settings together.
  Relative paths are skipped.
- From the FL Studio list, an entry with a placeholder such as `%FLPath%` is
  skipped. Such an entry names one of that app's own folders, which hold its
  native plugins and not VST or CLAP plugins.
- On Windows, a folder from another app's settings that lies under
  `%ProgramFiles(x86)%` is skipped, because it holds 32-bit plugins, which
  OpenCompose cannot load.

A folder that several sources name is searched once, and the Folders page lists
it under the first source in this order: standard, environment variables,
common, another app's settings, added by you.

A plugin may sit up to three folders below a search folder, so a
`<VST3>/<vendor>/<plugin>.vst3` layout is found. Each `.vst3`, `.clap`,
`.component` or `.lv2` bundle counts as one plugin file, and so does each
`.vst` (macOS), `.dll` (Windows) or `.so` (Linux) file. One file can hold
several plugins; the plugin browser lists each one.

### Adding a folder

To search another folder, choose **Add plugin folder…** on the Folders page of
Preferences or on the first page of the first-run guide. The app refuses a
folder that it already searches as a standard folder or as one of yours. Your
list is kept in `~/.opencompose/plugin-folders.json` and holds at most 32
folders. `OCP_PLUGIN_FOLDERS` points at another file. The plugin scan, the LV2
loader and the Store's installed check all read this list.

**Remove** on the Folders page takes a folder you added off the list. Its files
stay on disk.

### Sounds on this computer

The same page lists the sounds on the computer: SoundFont (`.sf2`, `.sf3`), SFZ
and sampler files, and audio samples (WAV, AIFF, FLAC, OGG and MP3). It
searches:

- the instrument folders: OpenCompose's own `soundfonts` folder and the sound
  pack folder; `SoundFonts`, `soundfonts`, `MuseScore4/SoundFonts`,
  `MuseScore3/Soundfonts` and `lmms/samples/soundfonts` under Documents;
  `SoundFonts` under Music; `~/Library/Audio/Sounds/Banks` and
  `/Library/Audio/Sounds/Banks` on macOS; `soundfonts` in the user data
  folder, `/usr/share/soundfonts`, `/usr/share/sounds/sf2`,
  `/usr/share/sounds/sf3` and `/usr/local/share/soundfonts` on Linux;
  `\soundfonts` on the system drive on Windows;
- the sample folders other music tools use: `Samples`, `Image-Line`,
  `Ableton/User Library`, `Ableton/Factory Packs`, `lmms/samples` and
  `Splice` under Documents; `Samples`, `Ableton/User Library` and
  `Ableton/Factory Packs` under Music; `Splice` and `lmms/samples` in your home
  folder;
- the sound folders you added.

Each folder is searched up to eight levels deep and at most 20,000 entries.
**Add sound folder…** adds a folder as a content library. It must hold at
least one SoundFont, SFZ or sampler file or one audio sample, so a folder of
WAV or AIFF samples alone can be added. The SoundFont bank that ships with OpenCompose is
not counted.

After you install a plugin, press **Rescan** in the plugin browser or
**Scan again** on the Folders page.

## Naming a plugin in a project

A preset names the plugin format in `source` and the plugin in `plugin`:

```ocp
@preset bass
  source: vst2
  plugin: "vst2:surge_xt"

@preset gain
  source: lv2
  plugin: "https://example.org/lv2/gain"
```

The plugin browser shows each plugin's id. The ids have these forms:

- VST3 and VST2: `vst3:` or `vst2:` and the plugin name in lowercase, with
  spaces as underscores (`vst3:surge_xt`, `vst2:surge_xt`);
- CLAP: the plugin's descriptor id (`com.vendor.product`);
- AU: the type, subtype and manufacturer codes (`aumu:dls:appl`); see
  [AudioUnit hosting](../au-hosting.md);
- LV2: the plugin URI (`https://…` or `urn:…`).

In a track's effect list, a format prefix (`clap.`, `vst3.`, `vst2.`, `au.` or
`lv2.`) in front of the id limits the lookup to that format, for example
`- plugin:"vst2.vst2:surge_xt"` or
`- plugin:"lv2.http://example.org/lv2/gain"`. Without a prefix, the id can
match a plugin of any format. The full grammar is in [The .ocp file](dsl.md).

## Load and save Kontakt state

Install and activate Kontakt and the sample library you want to use. In the
plugin browser, press **Rescan**, add Kontakt with **Add preset**, then open
its editor with **Open editor**. Load an instrument or a multi inside Kontakt.
Kontakt renders silence until a multi is loaded.

In the preset editor, open **Advanced instrument settings**, then
**Saved plugin settings**. Choose **Embed** to store the plugin state in the
project, or **Save** to write it to a state file. A file-based preset names
that file in `vst3_state_file`, and a relative path is resolved from the
project's folder. A Kontakt `.nkm` multi is a document you open inside the
plugin; it is not host state.

Assign the preset to a track and use the MIDI channel set in Kontakt. The saved
state brings the setup back. Every computer that opens the project needs the
plugin and its sample library installed and licensed. See
[Plugin support](../plugin-support.md) for what has been tested.

## The free-plugin catalog

The Store panel lists free plugins from a signed remote index. The app
downloads the index over HTTPS from the project's documentation site, caches it
on disk, and checks it against the Ed25519 keys built into the app. The app and
`ocp` contain no catalog of their own, so they need the network once to fetch
it. Until an index has been downloaded, the Store lists nothing. See
[The plugin index](../plugin-index.md).

The index has one entry per plugin, with a name, vendor, one-line description,
category tags, license, homepage and one or more installs. An install names
the platform and format it provides and where to download it. The app shows the
formats available for the current platform and the license. Depending on the
install, it downloads the archive, opens the installer or opens the vendor's
page.

OpenCompose does not bundle or host plugin binaries. Every install in the
catalog links to the vendor's own download or release file, and the app
downloads it from there. The `license` field records the plugin's license, as
an SPDX identifier or as `Freeware` for a free proprietary download. It does
not change the plugin's terms.

The app fetches the index when the first-run guide or the Store's plugin list
needs it, or when you press **Refresh**. A cached copy fills the list first,
and nothing fetches the index in the background. Opening the list uses the
cached copy if it was checked in the last six hours. **Refresh** always
fetches, but at most once every five minutes. If a refresh fails, the app
keeps the cached copy, and it warns when that copy is 14 days old or older.
The index format, hosting, signatures, caching and publishing commands are
documented in [The plugin index](../plugin-index.md).

## Limits

- The host never edits or repackages a plugin. When a vendor's download moves,
  the fix is an updated catalog, not an uploaded copy.
- VST2, AU and LV2 show a generated parameter panel instead of the plugin's own
  interface, and host one stereo output. See [VST2 plugins](../vst2.md) and
  [AudioUnit hosting](../au-hosting.md).
- Scanning runs each plugin in a separate helper process, so a plugin that
  crashes during a scan cannot take the app down. Rendering runs in process, so
  a plugin that crashes while rendering takes the app down with it.
