# Preferences

Preferences is one in-app window with a page per subject. Open it from the
Preferences button on the right of the toolbar. Escape, the close button and a
click on the backdrop all close it. Leaving with unsaved provider, model or key
edits asks whether to discard them, whichever page is open; nothing here opens
an operating-system window.

The left sidebar lists the pages:

| Page | What it holds |
|---|---|
| Display | Theme family and variant, the interface scale, the language. |
| Audio | Output device and format, capture device and layout, monitoring, MIDI input. |
| Keymap | The shortcut editor: search, rebind, reset. |
| Performance | Track rendering threads, parallel rendering, and the read-only render facts. |
| AI | Provider connection and key, model and defaults, permission, edit target, project instructions, toolsets. |
| Folders | The folders searched for plugins and sounds, adding and removing your own, and a rescan. |
| Storage | SoundFont and plugin counts, the project's sample cache, the preferences file. |
| Help & About | Version, supported DSL version, repository, licence, diagnostics archive. |

Each page reads what it shows when you open it. Leaving a page stops its polling.

## Display

The theme family and variant apply immediately, and each variant offers a
preview card. The interface scale runs from 70% to 150% in 10% steps and shows
its current value beside the slider; **Reset** returns it to 100%. The scale is
a webview zoom, so it scales the whole interface, not the timeline's own zoom.

## AI

The AI page is the same surface the AI panel's **Settings** button opens; that
button lands directly on this page instead of stacking a second dialog. The page
edits the provider connection, its key, the active model and effort, the
provider defaults, the per-model limits, the conversation's permission and edit
target, the project's instructions, and the enabled toolsets.

Provider, model and default edits are drafts until you press **Save settings**;
the line above the form says so. Permission, edit target and toolsets apply at
once. **Test Connection** uses the saved provider, so save first if the form
still reports unsaved changes. Removing a provider or a saved key asks for
confirmation first. Closing the dialog, or switching to another page, with
unsaved provider edits asks whether to discard them; keeping them leaves the
dialog open.

The key itself never enters the preferences file: the OS credential store holds
it, and the page only reports whether it is saved.

## The capture rule

Viewing the Audio page never asks for the microphone. The page forwards a read
of the device state and the last measured level; it does not open a capture
stream. Capture opens on exactly three events:

- you choose an input device or a capture channel layout;
- you turn monitoring on;
- a take is armed.

Until one of those happens the level readout says **Not capturing**. This is
why the page can be opened on a laptop with a closed lid without a permission
prompt.

Capture defaults to stereo. A mono device is duplicated to two channels, and a
stereo interface keeps its image. Choose **Mono** to downmix instead. Monitoring
is off by default because a microphone through speakers feeds back; its gain
slider applies only while monitoring is on.

## The performance knob

**Track rendering threads** sets how many workers an offline render uses. The
setting is persisted and applied to the next render.

| Value | Meaning |
|---|---|
| Auto | Use the machine's reported parallelism. This is the default. |
| 1–64 | Use exactly that many workers, capped by the machine when it has fewer cores. |

**Parallel track rendering** is the second half of the same setting: off renders
every track on one worker, whatever the thread count says. Turning it off is the
honest way to ask for serial rendering.

The page also reports facts it does not change: the core count, the lookahead
window, the steady-state block size, the short first chunk, the sample rate the
output device resolved to, the contract budget for the first block, and the last
measured first block with the start path that produced it.

The value is published to the engine as `OCP_RENDER_THREADS`. The engine reads
that variable when it starts an offline render, so a change applies to the next
render rather than to one already running.

## Folders

The Folders page lists every folder OpenCompose searches, in two blocks: plugin
folders, and sound folders for SoundFont, SFZ and sampler files and samples.
Within each block the folders are grouped by why they are searched:

| Group | Where the folders come from | Editable here |
|---|---|---|
| Added by you | Folders you chose with **Add plugin folder…** or **Add sound folder…**. | Yes: **Remove** stops searching a folder and leaves its files on disk. |
| Standard locations | The folders each plugin format or sound tool names. | No |
| Common locations | Folders other music software often uses, searched when they exist. | No |
| From environment variables | `CLAP_PATH`, `VST3_PATH`, `VST_PATH`, `LXVST_PATH` and `LV2_PATH`. | No; change the variable. |
| From another app's settings | Plugin folders another audio host is set to search. | No; change them in that host. |
| Installed by OpenCompose | The folder that sound packs install into. | No |

Each row shows how many plugins, or how many instruments and samples, the
folder holds. A usual location that does not exist on this computer is counted
below the block instead of listed. [Plugins](manual/plugins.md) lists every
location.

Adding a plugin folder searches it at once and then scans it, so its plugins
appear in the plugin browser. A sound folder must hold at least one instrument
file. When a change is refused, the page says why, for example that the folder
is already searched or that the list of plugin folders is full, and the lists
stay as they were.

**Scan again** reads every plugin again, including ones that could not be read
before, and then recounts the sound folders. The page shows the same progress
for both steps as the first-run guide, and the totals update when the scan
finishes.

## Storage

Every count on this page is read from the filesystem, so it reports what is on
disk rather than what a project claims to use.

| Row | Reads | Actions |
|---|---|---|
| SoundFonts | The installed packs and their folder. | Open folder. |
| Plugins | Bundles found in the search paths, counted per format. | Open folder for each path, and Manage folders, which opens the Folders page. |
| Project data | Edit histories, AI notes, recovery copies and conversations of each project and workspace folder, with size and a gone or due state. | Scan, and Remove only for data whose file or folder is gone. |
| Conversations | Each workspace's saved conversations and the shared audition audio archive. | Delete one conversation, Clear audition audio. |
| Clip cache | Decoded and processed clip PCM, and the vocal analysis and bake cache against its 1 GiB budget. | Scan, Clear. |
| Sample cache | Files and bytes under the open project's `media/samples`. | Open folder, Clear. |
| App folders | Runtimes, the plugin index, presets, sample providers, audition scratch, preset trims and logs. | Scan, Open folder. |
| Preferences file | The settings file this build writes. | Open folder, then Export or Import. |

Project data whose file or folder is gone waits 30 days and is then removed by
the startup sweep. Removing one by hand is offered only while it is gone. See
[On-disk storage](storage-layout.md).

**Clear** deletes every file inside `media/samples` of the open project and
leaves the folder in place. It never touches the project file, an audio clip
that points outside that folder, or any other project. A project that has never
been saved has no media folder, so the row states that instead of offering the
action.

The preferences file is `~/.opencompose/gui-settings.yaml`. The app can read
older JSON preferences until the next save creates the YAML file. Desktop
network requests use the `network.proxyMode` setting (`system`, `manual`,
or `none`). Manual mode reads `network.proxyUrl`; enter the address of your
proxy in Network preferences.

**Find a proxy** checks the saved route, a direct connection and the usual
local proxy ports against the plugin index. Each route is listed with its
result and timing, and **Use** saves a working one. A local port counts as a
proxy only when it accepts a CONNECT tunnel to the index host; a web server on
the same port and a proxy whose upstream is unreachable are not offered. The
same check runs when a first-run or Store index fetch fails, where a working
route can be applied before the fetch is retried.

The portable preferences bundle is a separate file you choose. See
[Preferences bundle](preferences-bundle.md).

### Separate configuration profiles

Set `OPENCOMPOSE_CONFIG_DIR` before launching the GUI or `ocp` to select a
separate filesystem configuration root. Without it, the default remains
`~/.opencompose`. GUI and audio preferences, saved AI sessions, presets and
other files that use this root are stored directly in the selected directory.
Existing settings are not copied automatically.
Diagnostic logs requested with `--debug` or `--perf` use `logs/` inside an
explicit profile. Without an explicit profile, diagnostics keep their normal
platform log directory.

The value must be a nonempty absolute path. Its parent must already exist;
leave the profile directory itself for OpenCompose to create. For example,
in a macOS or Linux shell:

```sh
mkdir -p "$HOME/.opencompose-profiles"
OPENCOMPOSE_CONFIG_DIR="$HOME/.opencompose-profiles/review" ocp validate song.ocp
```

On Windows, choose a directory below `%LOCALAPPDATA%`, whose resolved location
must itself be within `%USERPROFILE%`. OpenCompose resolves existing parents
before checking containment and rejects unsupported locations. Windows profiles
use the containing user profile's inherited permissions.

OpenCompose marks a newly created profile for later reuse. It refuses an
unrelated existing directory, an invalid marker, a profile-directory symlink
or an insecure Unix profile rather than changing that directory's permissions. On Unix, new profile
directories use mode `0700` and their marker uses `0600`. Startup errors do not
fall back to the default profile. The selection is immutable for that process;
restart to switch profiles.

Quit any running instance before switching profiles: this option does not
change the application's single-instance identity, WebView storage, operating
system credential store, plugin installation folders or project locations.
It does not change `HOME` or other applications' settings.

## External Sources

Manage Freesound and Jamendo credentials in the operating system's credential
store, connect Freesound for original downloads, and open provider API
documentation. Sample searches run through [local provider plugins](samples.md)
in debug builds.

## Help & About

The version comes from the build, and the supported DSL version comes from the
parser crate, so neither can drift from the binary that is running. The
repository line links to `https://github.com/verithia/opencompose`. The licence
is AGPL-3.0-or-later; the language components (`oc-dsl`, `oc-device-schema`,
`oc-lsp` and the VS Code extension) are MIT OR Apache-2.0. **Export diagnostics…** writes the support archive
described in [Diagnostics](diagnostics.md).

## The resource browser

The Browse button next to Preferences opens a separate overlay with three tabs:

- **Plugins** lists the free-plugin catalogue, the same surface the Store shows.
- **Samples** searches the sample markets, the same surface the bottom panel's
  Samples mode shows.
- **Effects** lists the built-in devices grouped by category, with a one-line
  description each, and adds one to the selected track.

The browser is an overlay, not a window, and it does not change the bottom
panel's mode: the Samples mode stays where it was.

## Interface scale and the toolbar

A large interface scale shrinks the CSS viewport, which can push right-hand
buttons out of the window. The toolbar therefore keeps Preferences and
Browse outside the strip that holds the transport and file controls, so those
two buttons are never the ones that leave. Inside the strip the groups wrap onto
another row when they run out of width, the toolbar row grows with them, button
labels hide before any control does, and separators tighten at narrow widths.
`npm run layout-check` measures every toolbar button at 150% and 200% and fails
if one leaves the viewport or is covered by an open sidebar. The same check
covers the toolbar's drag region, which spans the whole bar so the window can be
moved from any empty part of it.
