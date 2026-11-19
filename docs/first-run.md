# First run

On a new installation, OpenCompose shows a short splash while it prepares the
window. On that first launch the theme follows the system: `prefers-color-scheme`
selects the light or dark default. From then on, only a choice you make in
Preferences changes it.

A setup guide then opens. It scans the computer, may suggest free plugins and
sounds, and ends with one step about the editor and one about the AI settings.

## 1. Plugins and sounds on this computer

The first page brings the plugin inventory up to date and then searches the
sound folders. Each of the two jobs has its own progress bar. The plugin bar
names the bundle being read and counts a bundle when its helper process
finishes. A bundle whose earlier scan result still applies counts as done from
the start, so a repeat scan finishes almost at once. If the inventory scan that
starts with the app is already running, the page follows that scan and does not
start a second one.

When the scan ends, the page shows three counts:

- **Plugins**, by format. A bundle the scan could not read is listed with that
  note and does not count as usable.
- **Instruments**: SoundFont files (`.sf2`, `.sf3`, recognised by their file
  header), SFZ files and sampler-asset files.
- **Samples**: WAV, AIFF, FLAC, OGG and MP3 files in sound folders. They are
  counted but not listed.

The plugins and the instruments each have a folded, scrolling list, and a third
list shows the folders that were searched. A usual location that does not exist
on this computer is left out of that list, and one line says how many were
skipped. A folder is labelled when it came from another app's settings, from an
environment variable or from a sound pack that OpenCompose installed.

**Add plugin folder…** and **Add sound folder…** add a folder and scan again.
**Scan again** reads every plugin again, including plugins an earlier scan
quarantined. [Plugins](manual/plugins.md#where-plugins-are-found) lists the
folders that are searched.

## 2. Recommended (only when little is found)

This page appears when the scan finds fewer than 10 readable plugins, fewer
than 3 instruments or fewer than 100 samples. It shows what is missing.

The suggestions and their install steps come from the signed plugin index. The
application carries only the ids on the recommendation list. Neither the app
nor `ocp` embeds the catalog: the index supplies the names, builds, addresses,
sizes, checksums and install scripts. If no verified copy of the index is
cached when the guide opens, the guide fetches it once, by the same automatic
route the Store uses. After that, the cached copy lets the suggestions work
offline. If no index can be downloaded, the page says so, shows the reason and
offers **Try again**.

When a fetch fails, the page tests the saved route, a direct connection and the
usual local proxy ports against the plugin index. It offers a route only after
that route has answered the index. One click applies the route and retries the
fetch. A local proxy that is running but cannot reach the index is reported as
such and is not offered.

**Free plugins** lists one entry for each role: synth, sampler, reverb,
character and mixing. The entries come from the plugin index the Store uses. For
each role the page first picks an entry that is already installed, then one
that installs without a vendor installer, then any installable one. An entry
already shown for an earlier role is skipped. **Install** uses the same verified
`plugin_store_install` path as the Store. **Cancel** stops the install, and
**Skip setup** leaves it running.

**Starter sounds** lists free SoundFonts, SFZ libraries and sample packs that
the index recommends. Instrument packs appear when you have few instruments,
and sample packs when you have few samples. Each card shows the publisher, the
content kind, the download and unpacked sizes and the licence. The licence name
opens the licence text, and the small print carries any attribution the licence
requires. **Install** downloads the archive from the publisher, checks its size
and SHA-256 against the signed catalog, and unpacks it into the OpenCompose
sounds folder, where the next scan finds it. While it runs, the card shows the
download, verify and unpack progress and a **Cancel** action. For sounds kept
somewhere else, **Add sound folder…** is still available.

## 3. The bottom panel

`C`, `R` and `P` open the **Clips**, **Piano Roll** and **Presets** views in
the bottom panel. The panel follows the selection, and each key switches the
view inside the panel; no separate window opens. The step also points at the
AI assistant in the toolbar, and at Space, which auditions whatever the status
bar names.

## 4. A model

**Open AI settings** opens Preferences on the AI page. The provider, model and
credential controls there are the ones the AI panel uses. You can skip this
step: the editor, the GM instruments and the effects work without an AI
provider.

## Dismissal

**Skip setup**, or finishing the last step, records a marker in localStorage.
The guide does not come back while that marker exists. To open it again, choose
**Preferences > Help & About > Getting started**. The splash uses the
same first-run marker, so a normal later launch goes straight to the workspace.
