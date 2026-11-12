# The window

The GUI is a Tauri 2 app with a SolidJS frontend. The frontend lives in
`crates/oc-gui/frontend/`, the Rust backend in
`crates/oc-gui/frontend/src-tauri/`.

## Layout

```
┌──────────────────────────────────────────────────────────────┐
│ toolbar: transport, position, tempo, meter, file, MIDI, render│
├───────────────────────────────────────────────┬──────────────┤
│ Timeline (always visible)                     │  Sidebar:     │
│  track headers + ruler + clips                │  DSL          │
├───────────────────────────────────────────────┤  Plugins      │
│ Bottom panel: Clips | Piano Roll | Presets |   │  Store        │
│ Mixer | Drum Kit | Detail                     │  AI           │
├───────────────────────────────────────────────┴──────────────┤
│ status bar: state, track/bus count, selection, project, time  │
└──────────────────────────────────────────────────────────────┘
```

The split between timeline and bottom panel is draggable, and the sidebar has a
drag handle on its left edge (220–600 px).

## Transport

The toolbar has stop and play/pause, a beat-position readout, a tempo field
(20–999 BPM), and a meter menu. **Space** starts and stops playback. Clicking the
timeline ruler scrubs and seeks; playback follows the cursor until you scroll the
timeline by hand, then the next Play resumes following.

Space follows the region of the last pointer-down. If that was inside the bottom
panel, the Presets view is showing and a preset is selected, Space auditions the
preset instead of touching the transport; otherwise it plays or pauses the
timeline. The bottom tab strip gets an accent edge while it is the active region.
Auditioning never moves the timeline cursor or changes whether the transport is
playing.

The transport bar also carries **Write automation** (`W`), one arm shared by
every surface that writes a lane point. With it on, holding a knob records a
curve: one point per grid cell while the transport rolls, and one point at the
playhead while it is stopped. The whole gesture is one undo entry, and the status
bar says what happened (`Recorded 12 points on filter.cutoff (1/16 grid).`).
Without the arm a knob drag is an ordinary edit and creates no lane; a parameter
that has no lane yet gets one on the first recorded point. The button stays
enabled while the transport is stopped because writing at the playhead is
useful — the tooltip states what a stopped gesture does.

The toolbar also holds the file actions (new, open, save, save as), MIDI import
and export, offline render to WAV, stems export, the theme family/variant
pickers, and the preferences popover (output device, live MIDI input,
diagnostics, shortcut list).

## Timeline

- Track headers: colour bar, name (double-click to rename), instrument/preset
  label, **▸ Preview**, **M** mute, **S** solo.
- Clips are blocks on the lane; click one to select its pattern, double-click to
  open the **Inspector** view (source and placement information, with a piano roll for note clips). Named
  pattern clips, inline phrases and inline drum patterns all drag the same way:
  the block follows the pointer on the shared grid, the move snaps on release,
  and dropping on another lane moves the block across tracks. Drag a block's
  right edge to change its end while the start stays put. Alt-drag copies a
  named clip; Escape cancels without a project change.
- Hover a block for its detail: kind and track under the name, the bar range and
  length in bars, beats and time, its sound and note/hit count, then one state
  row only when something differs from the default (transpose, swing, colour,
  mute/solo, gain/pan, selection). Audio clips list offset, stretch, source file,
  sample rate and channels. The card hides for the whole drag or resize gesture
  and returns on the next hover with the block's new state.
- Right-click a clip for its menu: open editor, rename, copy/cut/paste,
  duplicate, split at the playhead, delete (**Mod+Z** restores it). Inline
  phrases and inline drum patterns have their own menu (open editor, copy,
  paste); audio clips offer split and delete. Right-click a track for rename,
  add track, create a pattern at the playhead, open the mixer,
  and MIDI actions. Every menu starts with the name of the object it acts on, and
  opening it never selects that object or moves the playhead: the menu acts on
  what you right-clicked, not on what was selected. Only the primary button
  selects, drags or resizes; a secondary click (Ctrl-click on macOS included)
  only opens the menu. **Open editor** is the exception that selects the clip,
  because that is what the item asks for.
- The clip menu and **Delete** act on any clip kind through one path: a pattern
  clip, an audio clip and an automation block are all named by the same clip
  pool, so a gesture the kind supports runs that kind's own command and a
  gesture it does not support is disabled with the reason. An audio clip has no
  duplicate, and a whole-timeline automation lane has no span to move, resize,
  copy, split or delete. When the backend refuses a gesture, its sentence is
  shown verbatim in the status line.
- Selected clips support cut/copy/paste/duplicate and Delete (see shortcuts).
- A compact info stripe above the ruler shows sections and key markers when the
  project has them; overlapping clips on one track are flagged.
- Zoom: buttons in the timeline footer, or **Mod+wheel** (6–120 px/beat).
- Right-click an audio clip and choose **Preview clip** to hear its current
  clip and mixer processing in isolation from other clips on the same track.
  **Space** pauses and resumes the preview at its own position.
- Double-click an audio clip or choose **Speed and pitch** from its context
  menu. Set playback speed as a multiplier, transpose in fractional semitones,
  or fit the source to the clip's timeline length. Choose Musical for sustained
  material or Percussive for sharp attacks. The edit preserves the original
  file and the clip's timeline boundaries. Use **Preserve formants** to
  compensate for pitch shifts or set **Formant shift** to change vocal colour.
  Use **Bypass processing** and **Apply** to hear the original media in the
  same clip range while retaining the processing settings.
  Add warp markers by entering source and target times measured from the
  media origin. Use **Snap** next to a marker to move its source time to a
  nearby attack while keeping its target fixed. Drag a visible marker to
  move its target on the timeline.
  Select the clip and drag its ↔ handle to stretch and resize together.
  Alt/Option-dragging the right edge does the same thing.
  A plain drag trims. Trimming or splitting a clip fitted to its range keeps
  the previously fitted speed.
- Audio clips show a waveform with trim handles, split at the playhead, and are
  placed on their exact frame.

### Picture timecode

Right-click the timeline ruler and choose **Picture timecode**. Set the
picture's frame rate and the label at project start. **Read playhead**
shows the closest picture frame. To go the other way, enter a label,
choose **Convert**, then **Move playhead**. The playhead uses the frame's
time through the current tempo map, including positions between grid lines.
Choose **Save project clock** to keep the rate and start label when the
project is saved and reopened. **Clear saved clock** removes the setting. Both
actions can be undone. Enter frame labels from the picture edit.

### Automation rows

Every track can show its automation under its lane. The **A** button in the
track header (next to **M**/**S**) and **Show automation** in the track menu fold
the area open and closed; it starts closed, and the state is per track for the
session.

- Each lane gets one row. The row draws the lane's points as a polyline and its
  name, and the header cell of the row carries the same name, the kind of target
  (**Effect**, **Patch** or **Control**) and a per-lane **M** button. A
  `pluginparam_…` lane shows the parameter name it encodes, so a synth or plugin
  lane reads like the knob it drives.
- A lane with a span (`automation: … [3:1 - 5:1]`) draws that span as a block,
  so you can see where it writes. A lane without a span is a whole-timeline lane:
  its row is striped and it has no block, because it writes from its first point
  to the end of the project.
- **A block behaves like a clip.** Drag it to move the whole clip, points
  included: a ghost follows the pointer, the position snaps to the shared grid
  on the 1/16 floor, and the move is written on release as one undo entry. Drag
  either edge to resize it — the start edge or the end edge — and the opposite
  edge stays put. **Alt-drag copies** the clip under the pointer instead of
  moving it, the same copy modifier the clip lanes use. Escape cancels a drag
  without a project change.
- **Right-click a block**, or its row, for **Copy to playhead**, **Duplicate**,
  **Split at playhead** and **Delete**. **Duplicate** puts the copy right after
  the clip; **Copy** puts it at the playhead. **Split** needs the playhead
  strictly inside the clip and at least a 16th note from either edge, and the
  two halves meet on the value the clip wrote at the seam. **Delete** removes
  the clip lane. Pressing **Delete** with a block selected does the same.
- A move, resize, duplicate or split that would overlap another lane on the same
  target is refused: the status bar names both spans, and the block stays where
  it was. A whole-timeline lane has no block, so the four menu items are
  disabled there and say **This lane drives the whole timeline; a clip gesture
  needs a span.**
- Effect, patch and control lanes all take the same gestures; a lane on its own
  tick grid (`automation: … [ppq=N]`) does not, because those gestures address
  the project's notated grid.
- **Double-click** the row to add a point at the pointer. The value comes from
  the vertical position and the position snaps to the shared grid on the 1/16
  floor that clip positions use (the footer states the floor when the grid is
  finer).
- **Drag** a point to move it in both axes: a ghost follows the pointer, the
  horizontal position snaps to the grid, and the move is written on release, as
  one undo entry.
- **Delete**/**Backspace** removes the selected point; pressing a point selects
  it. The point menu also has **Delete point**.
- **Right-click** a point for the four segment curves (**Linear**, **Stairs**,
  **Half sine**, **Smooth**) and **Tension…**, which asks for the 0–1 blend that
  only applies to a smooth segment.
- A muted lane says **This lane is muted.** and refuses every edit: adding,
  moving and deleting a point report that reason instead of writing. The lane's
  **M** button clears the mute.
- A track with no lanes shows **No automation on this track. Right-click a knob
  and choose Create automation lane.**
- Bus and master lanes have no track row, so their rows are drawn after the last
  track, under a strip that names the owner. They are always visible while they
  have lanes.

The same lane also appears in the device rack's **Automation** list, where its
mode, smoothing, curve and LFO controls live; the timeline row owns the point
positions and values.

## Tempo and meter lane

The **Tempo** chip in the ruler header opens a two-row lane above the tracks:
tempo points on the upper row, meter changes on the lower. It starts open when
the project already carries maps, opens on the first added point, and folds away
when both maps are cleared; collapsed it takes no timeline height. The sticky
header shows the tempo and meter under the playhead.

- Double-click a row to add a point at the snapped position; right-click a row
  for **Add … here** and **Clear …**. A new point starts with the effective
  value at that position, so adding it changes nothing until you edit it.
- Drag a point horizontally. Tempo points follow the shared grid; meter changes
  snap to bar starts. Alt is the timeline's copy modifier, so there is no
  modifier that bypasses snapping here. Bar numbers come from the meter map, so
  the ruler relabels exactly as the engine counts bars.
- Double-click a point (or right-click → **Set value…**) for numeric entry.
  Tempo accepts 20–400 BPM. A meter numerator is 1–32 and the denominator is 1,
  2, 4, 8, 16 or 32. Invalid values and duplicate positions are refused without
  touching the project.
- Delete removes the focused point; the context menu also deletes or clears the
  whole map. Clearing every tempo point returns to the project BPM, and clearing
  meter points returns to the project meter.
- Keyboard: **Tab** to the lane or a point, **←/→** nudges the selected point by
  one grid step (a meter change moves one bar), **↑/↓** changes tempo by 1 BPM
  (Shift: 10) or the meter numerator by 1, **Enter** edits the value,
  **Delete/Backspace** removes the point, and **Esc** clears the selection.
- Every edit is one undo entry and one dirty change. Maps persist in `.ocp` as
  `tempo_map` and `time_sig_map`, and the transport, grid and clip placement all
  resolve positions through those maps.

## Piano roll

The piano roll edits the selected pattern, or the track view opened from a
phrase/clip. The toolbar has the two tools and a zoom control (10–200 px/beat,
also **Mod+wheel**).

- The roll edits the opened clip. Its left edge is the clip's first beat and
  the clip boundary marks its exclusive end. The drum grid can show authored
  hits beyond that boundary so they remain editable.
- Bar numbers stay absolute for orientation: a clip starting at bar 9 puts the
  label "9" on the left edge. A clip that starts mid-bar shows its partial bar
  at the left edge and its first full bar line where the next bar really begins.
- Drawing, dragging, resizing, nudging and duplicating keep note starts inside
  the clip. A one-beat clip is exactly one beat wide; only a zero-length range
  falls back to one grid step so the canvas never collapses.
- **Select**: drag notes to move, drag their edges to resize, drag empty space to
  box-select, Shift-click to toggle one note in or out of the selection.
- **Draw**: click to add a note. Delete/Backspace removes the selection.
- A drum pattern whose preset is a kit (`builtin.drumkit`) gets a **drum lane**:
  one row per pad, in the kit's own order, labelled with the pad name and the
  kit name beside the instrument in the toolbar. A note the kit does not declare
  gets an `Other <n>` row, where `<n>` is its MIDI note. A semitone the kit does
  not name and no hit uses is not a row, so the lane carries the kit's pads
  rather than 72 lines; a plain drum track keeps the General MIDI rows. A hit is
  placed on the current grid, including triplets and 1/32 notes. The DSL keeps
  a sixteenth anchor and stores the exact grid displacement as
  `grid_offset_beats` in quarter notes, up to ±0.125. The written position
  determines whether the hit fits the clip. Swing and an optional
  `timing_offset_beats` then shift the playback onset.
  A hit placed on an `Other` row writes that row's MIDI note in `midi_note`, so
  the kit plays the pad that holds the note.
  Markers and velocity bars show each authored hit once, including its
  explicit timing offset. An outlined hit has no played onset in this placement
  after applying the clip's slip, repeat, swing and timing bounds. Select or
  edit the velocity of an outlined hit just like any other authored hit.
- Right-click (or Ctrl-click on macOS) opens a menu for whatever is under the
  cursor and changes nothing else: the note menu (copy, paste at the playhead,
  duplicate, delete, velocity, mute, grace) on a note, the drum-hit menu on a
  hit, and the grid menu (draw a note here, paste here, grid 1/4–1/32) on empty
  grid. Only the primary button selects, draws or scrubs; a secondary click
  never selects a note, starts a marquee or moves the playhead. The menu's first
  row names the note (pitch and velocity) or the position (pitch, bar and beat;
  the pad name on a drum lane). The hit menu names the hit under the pointer and
  deletes that hit, and a right-click also picks it, so Delete acts on the hit
  the menu showed.
- Delete/Backspace removes the selection, **Mod+D** duplicates it, **Mod+A**
  selects every note, **Esc** clears the selection.
- Left/Right nudges by one grid step; Shift+Left/Right by one beat.

## Mixer

The Mixer panel shows the selected node's detail inspector on the left and the
routing view on the right: a project-level routing toolbar above the bus graph.
The graph is the topology; the toolbar creates buses, and the inspector edits
the selected node and connection.

- **Graph**: tracks in the left column, buses in the middle, master on the right,
  with one curve per connection (solid = main output or bus route, dashed =
  send). Click a node to inspect it; click a connection to edit it. Pressing a
  connection is keyboard accessible (`Enter` / `Space`), and a click on the
  background clears the selection. Dragging the background pans the view.
- **Add a node**: the toolbar's bus field creates a bus routed to master. Type a
  name, or leave it empty for the first free `Bus N`; the new node is selected.
  A track is still added by dropping a preset on the mixer.
- **Edit a connection**: selecting a connection opens a card in the inspector
  under the source node's controls. `To` re-points it (a track's exclusive main
  output, a send, or one bus route). A send connection also exposes its dB
  level, `PRE`/`POST` tap, input pan and a remove button; `Escape` leaves the
  card, `Delete` removes the connection and focus returns to the source node.
- **Connect**: next to the channel controls, a track adds a send to a bus and a
  bus adds a route to master or another bus.
- **Delete a bus**: the delete button is in the bus actions card under the
  channel detail. An unused bus is removed immediately. A bus that is still
  used by a track output, a send or a bus route is rejected with an error naming
  those references; the confirmation card then offers an explicit re-route —
  track outputs return to master, sends into the bus are dropped, and buses that
  routed into it return to master — before deleting it.
- **Inspector**: the node detail below the channel selector shows the fader,
  pan, `M` / `S` / `SAFE` / `Ø`, the effect chain, the send list (including the
  auxiliary source picker for CLAP/VST3 instruments) and the bus route list.

Every control commits once per gesture, so one fader move, one send edit or one
connection re-point is one undo entry. See [Mixer routing](../mixer-routing.md).

## Device rack

Each track, bus and master has an effect chain. `+` opens the built-in device
picker; a slot expands into the device panel for that device. The slot's context
menu offers **Bypass** (devices that define it) and **Remove**.

- The dynamic EQ opens the full curve editor: drag a node for frequency/gain,
  wheel over it for Q, Alt-drag a dynamic band for its range, double-click to
  reset, arrow keys for fine moves, per-band solo and a channel view
  (`All`, `L`, `R`, `M`, `S`).
- The fixed EQ and the passive EQ have their own curve views.
- Device panels can show gain reduction while audio plays, and the parametric
  EQ can draw a live spectrum analyser.
- Plugin effects keep their raw parameter list.

## Patch editor

Selecting an OpenSynth preset opens the synth workbench, with **SOUND**
(OSC / FILTER / AMP pages), **MOD**, **FX** and **VOICE** views, a preset field
that opens the factory + user-library browser, a **LOAD** button that imports an
external preset file, a `⋯` patch menu, a COMPACT/EXPAND toggle, and an audition
keyboard. The patch editor writes the preset's `params`, so every edit is an
undoable project change. Details in [OpenSynth](synth.md).

Hover or focus a rotary control to read its value. Press the knob and move the
pointer vertically to adjust it: upward increases the value, downward decreases
it, and Shift makes the movement finer. The side indicator follows the gesture;
you do not need to move the pointer onto it. Releasing commits the edit. The
mouse wheel also adjusts a knob. For keyboard editing, focus it and use the arrow
keys, Home or End; Enter or Space opens a vertical editor, and Escape closes it.
Double-click restores the parameter's default value.

The patch field shows the **project preset's name**. A smaller label after it is
the patch it was loaded from — the factory patch, user-library entry or imported
file name — and survives save/load in the `.ocp` file. The `⋯` menu holds the patch lifecycle:

- **Rename…** renames the project preset and rewrites every track, pattern and
  `extends:` reference in the same undo step.
- **Duplicate** and **Save as…** create an independent copy and point the players
  at it, so editing the copy never moves the original.
- **Save to library…** stores the whole patch (parameters, effect chain and
  provenance) under `~/.opencompose/presets` for use in other projects.
  OpenSynth wavetables and sample dependencies are collected into the library
  so the source project can move independently.

The browser lists the factory bank and a **User library** section with the same
search and audition affordances; the search also matches library tags. Loading a
row replaces the whole patch — parameters, effect chain and source label — and
resets every key the row does not declare, in one undo entry. See
[Factory patches](synth.md#factory-patches) for the audition and reset rules.

## Plugin browser

The **Plugins** sidebar tab lists scanned CLAP, VST3, VST2, AU and LV2 plugins
and built-in assets, with search, format and type filters, a **Rescan** button,
and a **Check project** action that reports plugins a project references but
cannot find. LV2 and VST2 plugins have no in-app editor: LV2 control ports are
edited as preset `params` in the DSL or preset editor, and VST2 parameters are
edited in the preset editor. **Add preset** creates a project preset from the
selected source. The **Store** tab opens external plugin, sample, soundfont and
preset sites in a separate window and lists the bundled free-plugin catalog.

Each Store card shows the site's own favicon when the site offers one, and a
category glyph otherwise. The app ships no site logos: the first time the tab
shows a site, the backend asks that site's https origin for its favicon,
through the proxy chosen in Network preferences, and keeps the answer in the
app cache for a month (a site with no usable icon is asked again after a day).
Only PNG, ICO and WebP files up to 64 KB are shown; the page itself never loads
an image from the site.

## DSL editor

**D** opens the DSL sidebar: the project as text, with syntax highlighting.
**Mod+Enter** parses and applies the text; a parse error is shown inline and
nothing is applied. The editor is one undo entry per apply.

## AI panel

The **AI** sidebar tab is a chat over the open project. It has a provider
setting (endpoint, model, key or sign-in), quick prompts such as "Describe
arrangement", "Add reverb bus", "Add drum beat" and "Improve mix", a tool log,
and a diff view. Proposed edits are shown as a diff and applied only when you
press **Accept**; **Reject** discards them. One pending edit exists per turn.
Limits are listed in [Drawing from an agent](agents.md).

## Diagnostics

**Mod+Shift+D**, or the Diagnostics button in the preferences popover, writes
`opencompose-diagnostics-<stamp>.tar.gz` and reveals it in the file manager. The
archive contains `manifest.json`, `logs/recent.log`, `project/project.ocp`,
`environment.json`, `plugins/scan-summary.json` and `errors/last-error.txt`; each
missing section is recorded as `missing` with a reason in the manifest.

## Keyboard shortcuts

Every shortcut is a command in the frontend registry, so the key a surface
shows is the key that runs it (see [Remapping shortcuts](#remapping-shortcuts)).
**Mod** is Cmd on macOS, Ctrl elsewhere; letter shortcuts do nothing while a
text field has focus.

| Shortcut | Action |
|---|---|
| Space | Play/pause; auditions the selected preset when the last click was inside the Presets panel |
| Mod+N / Mod+O | New project / open project |
| Mod+S / Mod+Shift+S | Save / Save As |
| Mod+Z / Mod+Shift+Z | Undo / redo |
| C | Toggle the Clips panel |
| R | Toggle the Piano Roll |
| P | Toggle the Presets panel |
| M | Toggle the Mixer |
| K | Toggle the Drum Kit panel |
| D | Toggle the DSL sidebar |
| Esc | Close the bottom panel; in the focused piano roll, clear selection and menu |
| Delete / Backspace | Remove selected notes, the selected timeline clip, or the selected tempo/meter point |
| Mod+D | Duplicate selected notes or the selected clip |
| Mod+A | Select all notes in the focused piano roll |
| ← / → | Nudge selected notes by one grid step (Shift: one beat); move a selected tempo/meter point |
| ↑ / ↓ | Change the selected tempo point by 1 BPM (Shift: 10) or the meter numerator by 1 |
| Enter | Edit the selected tempo/meter point value |
| Mod+C / Mod+X / Mod+V | Copy / cut / paste the selected timeline clip |
| Mod+wheel | Zoom the timeline or the piano roll |
| Mod++ / Mod+- / Mod+0 | Interface scale up / down / reset |
| Mod+Enter | Parse and apply DSL edits in the DSL sidebar |
| Mod+Shift+D | Export the diagnostic bundle |
| Mod+Shift+P | Open the command palette |

### Remapping shortcuts

Open **Keyboard shortcuts…** in the preferences popover to search the command
table, pick a command and press a new combination. The command palette
(Mod+Shift+P) reads the same table, so a rebinding shows up there and in the
preferences list immediately.

- The registry lives in `crates/oc-gui/frontend/src/stores/commands.ts`. A
  command carries an id, a label key, a category, a default binding and its
  handler; components that own local state register their handler while they
  are mounted.
- Overrides are stored in `localStorage` under `opencompose.keymap` as
  `{ version: 1, bindings: { "<commandId>": "<binding>" } }`. A binding string
  uses `Mod`, `Shift`, `Alt` and the key name, for example `Mod+Shift+Z`.
- Binding a key another command already uses is refused, the message names that
  command, and the old binding stays. **Reset** drops one override; **Restore
  defaults** clears them all after a confirmation.
- Escape, Tab, modifier-only presses and the palette's own Mod+Shift+P are
  reserved and cannot be assigned.

## Files on disk

- Projects are `.ocp` text. Saving is the same document the CLI reads.
- Media cache: `~/Library/Caches/OpenCompose/media` (platform cache directory
  elsewhere). Decoded audio and stretch variants are recomputable.
- Preferences for live MIDI: `~/.opencompose/midi.json`.
- Audio devices, sample rate, buffer, capture layout and monitoring:
  `~/.opencompose/audio.json`. See [Audio devices](../audio-devices.md).
- Agent endpoint and token: `~/.opencompose/agent.json`.
- AI provider settings: `~/.opencompose/ai-providers.yaml`; keys live in the
  OS credential store, never in the project.

## Opening project files from the desktop

OpenCompose registers `.ocp` project files with the operating system. Opening a
project in Finder, Explorer or a desktop file manager brings the document window
to the front. A second launch forwards its project to the running app rather
than creating an independent editing session. File paths and local `file:` URLs
are accepted; network URLs are not project-open commands.

If a project is already open with unsaved edits, choose **Save**, **Don't Save**
or **Cancel** before the requested file replaces it. A failed or cancelled save
keeps the current project and prompt. Cancel dismisses that open request. Opening
the current file again only brings its window forward; it does not discard edits.
Requests received while the app starts or another dialog is active wait until
the document can safely answer them.

Closing the last document window exits the app after the same unsaved-work
choice. **Hide** or minimize keeps the current project and playback session in
the running app; on macOS, clicking its Dock icon restores the existing window.
**Quit** uses the application-wide unsaved-work prompt.


## Inspecting a clip

Right-click a clip to read its properties without changing the selection or
moving playback. The menu shows the placement and duration, track, effective
preset/plugin/patch or source sample file, and relevant offsets, looping and
processing settings. Source paths are selectable text: select a path and use
Copy to copy the text rather than the clip. Relative media paths are displayed
against the project file location; an unsaved project keeps its relative path.
This display does not establish that a file exists on disk.

Choose **Inspect clip** for a persistent view. The **Inspector** replaces the
old Detail clip list: a note clip retains a piano roll beside its source facts,
while audio inspection shows the file, trim, gain, fades, stretch and pitch
without an unrelated note editor.

## First launch and appearance

With no saved override, OpenCompose follows the system light/dark appearance
and negotiates its interface language from the system browser-language list.
Unsupported languages fall back to English. Setup shows these choices, with
System/Light/Dark and explicit language controls. Choosing System follows later
system changes; an explicit theme or language remains your choice. Settings
remain available in Preferences, and **Help & About → Getting started** reopens
setup without replacing the project.

Use **Play test sound** to hear the quiet built-in phrase, **Stop test sound**
to cancel it, or **Audio settings** to choose the output. No API key or plugin
installation is required. From the workspace chooser, **Create starter sketch**
asks for a new filename and writes a complete built-in example; it refuses to
overwrite an existing file.

## Help and support

Help & About contains the bundled English manual, current build/source/platform
identity, a link to the public repository, offline license and soundfont notices,
and diagnostics export. Export saves locally and includes project source, paths,
logs, plugin information and system details; inspect it before sharing.
