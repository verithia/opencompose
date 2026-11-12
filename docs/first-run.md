# First run

A new installation opens with a short splash while the window is prepared. The
theme comes from the system on that first launch: `prefers-color-scheme` selects
the default light or dark variant, and only an explicit choice in Preferences
overrides it afterwards.

The guide then has three steps.

## 1. The bottom panel

`C`, `R` and `P` open the Clips, Piano roll and Presets views in the bottom
panel. The panel follows the selection, so the key is a view switch rather than
a separate window. The step also points at the AI assistant in the toolbar and
at Space, which auditions whatever the status bar names.

## 2. A model

**Open AI settings** opens Preferences on the AI page. The provider, model and
credential are the same controls the AI panel uses. The step is skippable: the
editor, GM instruments and effects need no AI provider.

## 3. Recommended plugins

The guide reads the plugin index the Store already uses, then offers a short
curated list of free instruments and effects (Surge XT, Dexed, sfizz, Valhalla
Supermassive and TDR Nova). It shows the entry's name, vendor, licence and
install size. Install calls the same verified `plugin_store_install` path as
the Store; an indeterminate progress bar runs while the download and unpack
finish. When the index publishes a managed separator runtime or another
recommended source, adding its id to `RECOMMENDED_IDS` is the whole change.

A source that the index does not publish does not appear: the guide does not
carry a second installer, a shell script or a hard-coded URL. This keeps the
size, SHA-256 and provenance checks in one place, and it lets the first-run list
follow the signed index instead of duplicating it.

The sample set is expected to arrive the same way. It is not a special
first-run download; when the index offers it, it becomes another recommendation
or a Store row.

## Dismissal

**Skip setup** or the final **Start making music** records a marker in
localStorage. The guide does not return unless that marker is removed. The
splash is part of the same first-run marker, so a normal later launch goes
straight to the workspace.
