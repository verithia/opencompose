# First run

A new installation opens with a short splash while the window is prepared. The
theme comes from the system on that first launch: `prefers-color-scheme` selects
the default light or dark variant, and only an explicit choice in Preferences
overrides it afterwards.

The guide then opens on a scan of the computer, may suggest free plugins and
sounds, and ends with two short steps about the editor and the AI settings.

## 1. Plugins and sounds on this computer

The first page brings the plugin inventory up to date and then searches the
sound folders, with one progress bar for each. The plugin bar counts bundles
as their helper process finishes and names the bundle being read; bundles whose
earlier result still applies count as done from the start, so a repeat scan
finishes almost at once. If the inventory scan that starts with the app is
already running, the page follows it instead of starting a second one.

When the scan ends, the page shows three counts:

- **Plugins**, by format. A bundle that the scan could not read is listed
  with that note and is not counted as usable.
- **Instruments**: SoundFont (`.sf2`, `.sf3`, checked by file header), SFZ and
  sampler-asset files.
- **Samples**: WAV, AIFF, FLAC, OGG and MP3 files in sound folders. They are
  counted but not listed.

The plugins and the instruments each have a folded list that scrolls, and the
folders searched have a third. A folder that does not exist on this computer
is not listed; a line says how many such folders were skipped. Folders that came
from another app's settings or from an environment variable are labelled.

**Add plugin folder…** and **Add sound folder…** add a folder and scan again.
**Scan again** reads every plugin again, including ones an earlier scan
quarantined. [Plugins](manual/plugins.md#where-plugins-are-found) lists the
folders that are searched.

## 2. Recommended (only when little is found)

This page appears when the scan finds fewer than 10 readable plugins, fewer
than 3 instruments, or fewer than 100 samples. It shows what is missing:

- **Free plugins**: one entry per role (synth, sampler, reverb, saturation,
  mixing, tools) from the plugin index the Store uses. The first choice is an
  entry already installed, then one that installs without a vendor installer,
  then any installable one. **Install** goes through the same verified
  `plugin_store_install` path as the Store.
- **Starter sounds**: free SoundFonts, SFZ libraries and sample packs with a
  stated licence (`stores/starterSounds.ts`). Instrument sources appear when
  instruments are few, and sample packs when samples are few. **Open download
  page** opens the publisher's page in the browser. The app never downloads
  these itself: their archives are not pinned by SHA-256 and several use
  formats the installer does not unpack. After unpacking one, add its folder
  with **Add sound folder…**. Sound packs that the signed index publishes appear
  beside them and install directly.

## 3. The bottom panel

`C`, `R` and `P` open the Clips, Piano roll and Presets views in the bottom
panel. The panel follows the selection, so the key is a view switch rather than
a separate window. The step also points at the AI assistant in the toolbar and
at Space, which auditions whatever the status bar names.

## 4. A model

**Open AI settings** opens Preferences on the AI page. The provider, model and
credential are the same controls the AI panel uses. The step is skippable: the
editor, GM instruments and effects need no AI provider.

## Dismissal

**Skip setup** or finishing the last step records a marker in
localStorage. The guide does not return unless that marker is removed. The
splash is part of the same first-run marker, so a normal later launch goes
straight to the workspace.
