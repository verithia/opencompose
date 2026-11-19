# Preferences

Preferences is a dialog inside the main window, with one page per subject.
Open it with the **Preferences** button at the right end of the toolbar. Press
Escape, click the close button or click outside the dialog to close it.

The sidebar lists the pages in this order:

| Page | What it holds |
|---|---|
| Display | Language, theme and variant, interface scale. |
| Audio | Output device and format, input device and channels, input monitoring, MIDI input. |
| Keymap | The shortcut editor: search a command, rebind it, reset it. |
| Performance | Track rendering threads, parallel track rendering, engine diagnostics. |
| Folders | The folders searched for plugins and sounds. You can add and remove your own and scan again. |
| Storage | What the app keeps on disk, with sizes and clean-up actions, and the settings file. |
| Network | The proxy for outbound requests, a connection test and a proxy finder. |
| External Sources | Credentials for sample providers and links to their API documentation. |
| AI | Provider, key, model and defaults, permission, edit target, project instructions, toolsets. |
| Help & About | Getting started, the offline manual, version, licence, build, diagnostics archive. |

A page reads what it shows each time you open it, and stops any polling when
you leave it.

## Display

- **Language** offers **Follow system** and each interface language.
- **Theme** offers **Follow system** and each theme family. **Variant** picks a
  variant of that family, and each variant has a preview card you can click.
- **Interface scale** runs from 70% to 150% in 10% steps. Drag the slider, or
  type a value in the number box and press **Apply** or Enter. **Reset**
  returns to the starting scale: 100%, or less if the window first opened
  smaller than 1024 × 640 pixels.

The interface scale zooms the whole webview. It does not change the timeline's
own zoom.

## Audio

The Audio page holds the output device, sample rate and buffer size, the input
device, **Input channels**, **Monitor input** with **Monitor gain**, the input
level, and the MIDI input with its live input monitor.

Opening the page never asks for the microphone. The page reads the device list
and the last measured state, and does not open a capture stream. Capture opens
only when one of these happens:

- you choose an input device or a channel layout;
- you turn **Monitor input** on;
- a take is armed.

Until then the input level reads **Not capturing**.

Input is stereo by default. A mono device is copied to both channels, and a
stereo interface keeps its stereo image. Choose **Mono** to mix the input down
to one channel instead. **Monitor input** is off by default, because a
microphone played through speakers feeds back. **Monitor gain** runs from
−24 dB to +12 dB and works only while monitoring is on.

## Performance

**Track rendering threads** sets how many workers an offline render uses.

| Value | Meaning |
|---|---|
| Auto | One worker per reported core. This is the default. |
| 1 to the core count | Exactly that many workers. The list stops at the core count, or at 64. |

**Parallel track rendering** turns parallel rendering on or off. When it is
off, every track renders on one worker, whatever the thread count says. The
line under the menu shows how many workers the next render uses.

The app saves the setting and passes it to the engine as `OCP_RENDER_THREADS`.
The engine reads it when an offline render starts, so a change applies to the
next render and not to one that is already running.

**Engine diagnostics** expands to read-only facts: the core count, the
lookahead, the block size, the first chunk, the sample rate of the output
device, the audio startup latency of the last start and the start path it took,
and the first-block budget.

## Folders

The Folders page lists every folder OpenCompose searches, in two blocks:
**Plugin folders**, and **Sound folders** for SoundFont, SFZ and sampler files
and samples. Each block groups its folders by where they come from:

| Group | Block | Where the folders come from | Can you change them here? |
|---|---|---|---|
| Added by you | Both | Folders you chose with **Add plugin folder…** or **Add sound folder…**. | Yes. **Remove** stops searching the folder and leaves its files on disk. |
| Installed by OpenCompose | Sounds | The folder that sound packs install into. | No |
| Standard locations | Both | The usual folders for each plugin format or sound format. | No |
| Common locations | Both | Folders other music software often uses, searched when they exist. | No |
| From environment variables | Plugins | `CLAP_PATH`, `VST3_PATH`, `VST_PATH`, `LXVST_PATH` and `LV2_PATH`. | No. Change the variable. |
| From another app's settings | Plugins | Plugin folders that another music app is set to search. | No. Change them in that app. |

Each row shows how many plugins, or how many instruments and samples, the
folder holds. A standard or common location that does not exist on this
computer is not listed; the line under the block counts them. A folder you
added that no longer exists stays in the list and reads **Not found**.
[Plugins](manual/plugins.md) lists every location.

When you add a plugin folder, the page saves it, recounts the folders and
starts a plugin scan in the background, so the folder's plugins appear in the
plugin browser. A sound folder must hold at least one SoundFont or SFZ file.

When the app refuses a change, it writes nothing and the page says why:

- the folder does not exist;
- the folder is already searched;
- the folder is no longer in your list;
- your list of plugin folders is full (it holds 32);
- the saved folder list cannot be read, so it is left unchanged;
- the folder has no SoundFont, SFZ or sampler files;
- the change could not be saved.

**Scan again** reads every plugin again, including ones that could not be read
before, and then counts the sound folders. The page shows the progress of both
steps, as the first-run guide does, and the totals for plugins, instruments and
samples update when the scan ends.

## Storage

The Storage page reads every size and count from the disk, so it shows what is
there and not what a project expects. Most blocks have their own **Scan**
button, so you can recount one block without waiting for the others.

| Block | What it shows | Actions |
|---|---|---|
| Archived render format | The format of full audition renders: WAV, or MP3 at 128 to 320 kbps. | Choose the format. |
| Edit history | The size of the open project's history. | Set the size at which the app shows a notice. |
| Project data | Edit histories, AI notes, recovery copies and conversations of each project and workspace folder, with sizes and a gone or due state. | **Remove**, offered only for data whose file or folder is gone. |
| Conversations | The saved conversations of each workspace, and the audition clips and renders they share. | **Delete** one conversation, **Clear old conversation history**, **Clear audition audio**, **Open folder**. |
| SoundFonts | The installed packs and the files in their folder. | **Open folder**. |
| Plugins | Plugin files in the search folders, counted per format, and the size of each folder. | **Open folder** for each folder, and **Manage folders…**, which opens the Folders page. |
| Clip audio cache | Decoded audio, processed audio, and vocal analyses and bakes against their 1 GiB budget. | **Clear unused clip audio** clears all three. |
| Sample cache | Files and bytes in the open project's `media/samples` folder. | **Open folder**, **Clear**. |
| Other app data | Helper runtimes, the plugin catalog, the preset library, sample providers, audition scratch, loudness trims and diagnostic logs. | **Open folder**. |
| Settings file | The path of the settings file. | **Open folder**, **Export preferences…**, **Import preferences…**. |

Project data whose file or folder is gone is removed by the startup sweep after
30 days. You can remove it earlier with **Remove**. See
[On-disk storage](storage-layout.md).

**Clear** in the Sample cache block deletes every file in the open project's
`media/samples` folder and keeps the folder. It does not touch the project
file, audio clips stored outside that folder, or other projects. Samples that
presets refer to go missing from the project, so the app asks first. A project
that has never been saved has no media folder; the block says to save the
project first.

The settings file is `~/.opencompose/gui-settings.yaml`. If the YAML file does
not exist yet, the app reads an older `gui-settings.json` and writes YAML at
the next save. **Export preferences…** and **Import preferences…** write and
read a separate file that you choose. See
[Preferences bundle](preferences-bundle.md).

### Separate configuration profiles

Set `OPENCOMPOSE_CONFIG_DIR` before you start the app or `ocp` to use a
separate configuration folder. Without it, the folder is `~/.opencompose`. GUI
and audio preferences, saved AI conversations, presets and the other files
under that folder go into the folder you name. The app does not copy existing
settings into it. Diagnostic logs from `--debug` or `--perf` go to `logs/`
inside the profile. Without a profile, they go to the platform's log folder.

The value must be a non-empty absolute path whose parent folder exists. Let
OpenCompose create the profile folder itself. In a macOS or Linux shell:

```sh
mkdir -p "$HOME/.opencompose-profiles"
OPENCOMPOSE_CONFIG_DIR="$HOME/.opencompose-profiles/review" ocp validate song.ocp
```

On Windows, choose a folder under `%LOCALAPPDATA%`, which must itself resolve
to a location under `%USERPROFILE%`. OpenCompose resolves existing parent
folders before it checks this and refuses other locations. A Windows profile
uses the permissions it inherits from the user profile.

OpenCompose marks a profile folder it creates so that it can reuse it later.
It refuses an existing folder it did not create, a damaged marker, a profile
folder that is a symbolic link, and a Unix profile with unsafe permissions; it
never changes that folder's permissions. On Unix, a profile folder it creates gets
mode `0700` and its marker `0600`. A profile error stops startup; the app does
not fall back to the default folder. A process keeps its profile until it
exits, so restart to switch.

Quit any running instance before you switch profiles. A profile does not
change the app's single-instance identity, its WebView storage, the operating
system's credential store, plugin install folders or project locations. It does
not change `HOME` or the settings of other applications.

## Network

**Proxy** chooses how outbound requests to sample markets, the plugin index and
AI providers connect:

| Choice | Setting value | Meaning |
|---|---|---|
| Follow the system | `system` | Use the operating system's proxy. This is the default. |
| Use this proxy | `manual` | Use the address in **Proxy URL**. |
| Connect directly | `none` | Use no proxy. |

The choice is saved as `network.proxyMode` in the settings file, and the
address as `network.proxyUrl`. The app refuses **Use this proxy** without a
usable address.

**Test connection** requests `https://api.openverse.org/v1/` through the
chosen route and shows the time and HTTP status, or the error.

**Find a proxy** checks the saved route, a direct connection and the usual
local proxy ports against the plugin index. It lists each route with its result
and time, and **Use** saves a route that works. A local port counts as a proxy
only if it accepts a CONNECT tunnel to the index host, so a web server on that
port, or a proxy that cannot reach the index, is not offered. The same check
runs when the first-run guide or the Store cannot fetch the index, and you can
apply a working route there before the fetch is retried.

## External Sources

This page stores sample provider credentials in the operating system's
credential store:

- Freesound: an **API key**, plus **Freesound original quality**, which
  connects your Freesound account so you can download original files;
- Jamendo: a **Client ID**.

Each provider has a link to its API documentation. **Public search APIs** links
to the documentation of Openverse, Wikimedia Commons and Internet Archive,
which need no credential. See [Samples](samples.md).

## AI

The AI panel's **Provider settings** button opens Preferences on this page.

Provider, model and default edits stay drafts until you press
**Save settings**; the line above the form says so. Permission, edit target and
toolsets apply at once. **Test Connection** tests the saved provider and model,
so save your edits first. The app asks before it removes a provider or a saved
key.

Drafts survive a switch to another page. When you close Preferences with
unsaved AI edits, the app asks whether to discard them. If you keep them,
Preferences stays open.

The key never goes into the settings file. The operating system's credential
store holds it, and the page only shows whether a key is saved.

## Help & About

- **Getting started** closes Preferences and opens the first-run guide again.
  The **Audio** and **Keymap** buttons open those pages.
- **User manual · offline · English** shows the user manual inside the app.
- **Version** comes from the build, and **Supported DSL version** from the
  parser crate, so both match the running program.
- **Licence** is AGPL-3.0-or-later. **Git repository** opens
  `https://github.com/verithia/opencompose`.
- **Build** shows the source revision, build profile, operating system and
  architecture.
- **Credits and licenses** holds the AGPL text, the MIT and Apache-2.0 texts of
  the language components, and the notices of third-party software and assets.
- **Export diagnostics…** writes the support archive described in
  [Diagnostics](diagnostics.md).

## The resource browser

The **Browse** button next to **Preferences** opens the **Resources** overlay,
with three tabs:

- **Plugins** lists the free-plugin catalog, the same list the Store shows.
- **Samples** searches the sample markets, the same panel as the bottom
  panel's Samples mode.
- **Effects** lists the built-in devices by category, and the effect plugins
  found by the plugin scan. **Add to selected track** adds one to the selected
  track.

The overlay does not change the bottom panel's mode.

## Interface scale and the toolbar

A larger interface scale leaves fewer CSS pixels for the toolbar. The toolbar
keeps **Preferences** and **Browse** outside the strip that holds the transport
and file controls, so these two buttons always stay in the window. The strip
stays one row high and scrolls sideways when it runs out of width. Below
1180 CSS pixels the button labels hide and the gaps shrink, before any control
is hidden.

`npm run layout-check` renders the toolbar in headless Chrome in a 1400-pixel
window at 150% and 200% and fails if a button leaves the window or its
scrolling strip. The whole toolbar is a window drag region, so you can move the
window from any empty part of it.
