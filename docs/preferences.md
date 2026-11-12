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

## Storage

Every count on this page is read from the filesystem, so it reports what is on
disk rather than what a project claims to use.

| Row | Reads | Actions |
|---|---|---|
| SoundFonts | The installed packs and their folder. | Open folder. |
| Plugins | Bundles found in the search paths, counted per format. | Open folder for each path. |
| Sample cache | Files and bytes under the open project's `media/samples`. | Open folder, Clear. |
| Preferences file | The settings file this build writes. | Open folder, then Export or Import. |

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
