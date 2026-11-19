# The window

The desktop app is built with Tauri 2 and a SolidJS frontend. The frontend
source is in `crates/oc-gui/frontend/`, and the Rust backend is in
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
│ Mixer | Drum Kit | Samples | History |         │  AI           │
│ Inspector                                     │               │
├───────────────────────────────────────────────┴──────────────┤
│ status bar: state, track/bus count, selection, project, time  │
└──────────────────────────────────────────────────────────────┘
```

Drag the split between the timeline and the bottom panel to resize them. The
sidebar has a drag handle on its left edge and can be 220 to 600 px wide. The
**Inspector** tab appears in the bottom strip only while a clip is open in it.

## Transport

The toolbar holds stop and play/pause, a beat-position readout, a tempo field
(20–999 BPM) and a meter menu. Press **Space** to start and stop playback.
Click the timeline ruler to scrub and seek. The timeline follows the cursor
during playback until you scroll it by hand; the next play resumes following.

Space acts on the region where you last pressed the pointer. If that was the
bottom panel, the Presets view is showing and a preset is selected, Space
auditions that preset and leaves the transport alone. Anywhere else it plays or
pauses the timeline. The bottom tab strip shows an accent edge while it is the
active region. Auditioning never moves the timeline cursor and never starts or
stops the transport.

**Write automation** sits in the **Transport settings** popover. It is one arm
for every surface that writes lane points. It has no default key; you can bind
one in **Preferences > Keymap**. With the arm
on, holding a knob records a curve. While the transport rolls, you get one point
per grid cell. While it is stopped, the gesture writes one point at the
playhead, which is why the button stays enabled when stopped (its tooltip says
so). The whole gesture is one undo entry, and the status bar reports the result,
for example `Recorded 12 points on filter.cutoff (1/16 grid).`. With the arm
off, a knob drag is an ordinary edit and creates no lane. A parameter with no
lane gets one on its first recorded point.

The toolbar also has the file actions (new, open, save, save as, move
project), MIDI import and export, offline render to WAV, stems export, the
**AI** button and the **Preferences** button. **Preferences** opens a window
with the pages **Display** (theme family and variant), **Audio** (output device
and live MIDI input), **Keymap** (the shortcut list), **Performance**,
**Storage**, **Network**, **External Sources**, **AI** and **Help & About**
(diagnostics export).

A project has a file from the moment it exists. **New** asks where to put the
project, writes the file there and refuses a path that already holds a file.
With no project open, the start surface offers to open or create one. **Move
Project** gives the open project a new file and takes its edit history and
backup along, so nothing stays at the old place.

Only one process can have a project open. If another OpenCompose process
already has the project, opening it is refused, your current project stays
open, and the window tells you which case it is. Saved conversations follow
the same rule: opening one that another process holds keeps the conversation
the panel already shows.

## Edit history

Each project keeps its edit history in the app data folder, under
`<config>/history/<path key>/`, so undo reaches edits from earlier sessions.
Nothing is written beside the `.ocp` file. `<config>` is the app profile
directory: `~/.opencompose`, or the directory named by
`OPENCOMPOSE_CONFIG_DIR`. The path key is the first 8 bytes of the SHA-256 of
the project's canonical path, in hex.

The folder holds four files:

- `project.json` is the marker: version, canonical project path and the SHA-256
  of the file's content.
- `entries.jsonl` has one entry per line.
- `index.bin` is the live linear history.
- `state.json` holds the cursor, the id of the applied entry and the document at
  that entry.

Every entry records its author: you, the agent of a named conversation, an
external client, or a change made to the file outside OpenCompose. It also
records the typed changes its patches describe, and a cause that marks an entry
written by an older conversation restore. A recall moves the cursor and writes
no entry.

Press **Mod+Z** and **Mod+Shift+Z** to step through the history. The status bar
describes what the step took back or reapplied, built from the entry's typed
changes in the app's language, and names the author. While an agent turn is
running or holds a batch, undo waits and the status bar says
`Agent is editing the project, cannot undo.`

The **History** tab in the bottom panel lists entries newest first, 64 at a
time; **Load older entries** shows the rest. Click an entry to move the project
to the state after it. Undone entries stay in the list until a new edit
replaces them.

The tab also shows how much space the history uses. **Clear older history**
keeps the number of newest entries you choose and drops the rest after asking.
Undo then stops at the oldest kept entry. Entries you undid are never cleared.
When a history grows past the size set in **Preferences > Storage** (1 GB by
default), the tab and the status bar offer to clear it. Nothing is cleared
without your confirmation.

The app finds a project's history by path. When a file's path has no history,
the app compares the file's SHA-256 with the marker of every history, and a
history whose project file is gone follows the file. This is how a file you
moved or copied outside the app keeps its history: the history is copied if the
old file still exists and moved if it is gone. A project created with **New**
starts a fresh history and never adopts another file's.

**Move Project** moves the history, the AI note, the conversations scoped to
the project folder and the recovery copies to the new path, and leaves nothing
at the old path. **Save As** copies the history and the AI note and leaves the
conversations and recovery copies with the old file.

When a project file or workspace folder is gone, its data is kept for 30 days
and then removed by a sweep that runs 60 seconds after the app starts. A file
on a volume that is not mounted does not count as gone. The open project's data
and a running conversation are never removed.

**Preferences > Storage** lists the data of each project and folder (history,
AI note, recovery copies and conversations) with its size and whether it is gone
or due for removal. Only gone data has a **Remove** button. The page also
reports the other app folders and the clip PCM cache. See
[On-disk storage](../storage-layout.md).

## Timeline

A track header has a colour bar, the name (double-click to rename), the
instrument or preset label, **▸ Preview**, **M** (mute) and **S** (solo).

Clips are blocks on the track's lane. Click a block to select its pattern;
double-click it to open the **Inspector** with the clip's source and placement,
plus a piano roll for note clips.

Named pattern clips, inline phrases and inline drum patterns all drag the same
way. The block follows the pointer on the shared grid, the move snaps on
release, and dropping the block on another lane moves it to that track. Drag the
right edge to change the end while the start stays put. Alt-drag copies a named
clip. Press Escape to cancel a drag without changing the project.

Hover a block to see its details. Under the name you get the kind and track,
then the bar range, the length in bars, beats and time, the sound and the note
or hit count. A state row follows only when something differs from the
default: transpose, swing, colour, mute or solo, gain or pan, selection. Audio
clips also list offset, stretch, source file, sample rate and channels. The card
hides during a drag or resize and comes back on the next hover with the block's
new state.

Right-click a clip for its menu: open editor, rename, copy, cut, paste,
duplicate, split at the playhead and delete (**Mod+Z** restores a deleted
clip). Inline phrases and inline drum patterns have a shorter menu (open
editor, copy, paste), and audio clips offer split and delete. Right-click a
track to rename it, add a track, create a pattern at the playhead, open the
mixer or reach the MIDI actions.

Every menu starts with the name of the object it acts on. Opening a menu never
selects that object or moves the playhead, so the menu acts on what you
right-clicked even when something else is selected. Only the primary button
selects, drags or resizes; a secondary click, including Ctrl-click on macOS,
only opens the menu. **Open editor** is the one item that also selects the
clip.

The clip menu and **Delete** reach every clip kind through one path. Pattern
clips, audio clips and automation blocks are all named by the same clip pool.
A gesture that the kind supports runs that kind's own command; a gesture it does
not support is disabled and shows the reason. An audio clip cannot be
duplicated, and a whole-timeline automation lane has no span to move, resize,
copy, split or delete. When the backend refuses a gesture, the status line shows
its message word for word.

Selected clips support cut, copy, paste, duplicate and Delete (see
[Keyboard shortcuts](#keyboard-shortcuts)). When the project has sections or
key markers, a thin info stripe above the ruler shows them, and overlapping
clips on one track are flagged.

Zoom with the buttons in the timeline footer or with **Mod+wheel**, from 6 to
120 px per beat.

### Audio clips

An audio clip shows its waveform with trim handles, can be split at the
playhead, and sits on its exact frame.

To hear one clip alone, right-click it and choose **Preview clip**. You hear
its current clip and mixer processing without the other clips on the track.
**Space** pauses and resumes the preview at its own position.

To change speed or pitch, double-click the clip or choose **Speed and pitch…**
from its menu:

1. Set the playback speed as a multiplier, transpose in fractional semitones, or
   fit the source to the clip's timeline length.
2. Choose **Musical** for sustained material or **Percussive** for sharp
   attacks.
3. Turn on **Preserve formants** to compensate for a pitch shift, or set
   **Formant shift** to change the vocal colour.
4. Choose **Apply**.

The edit keeps the original file and the clip's timeline boundaries. To compare,
turn on **Bypass processing** and choose **Apply**: you hear the original media
in the same clip range, and the processing settings are kept.

Warp markers map source time to target time, both measured from the media
origin. Enter the two times to add a marker. **Snap** next to a marker moves
its source time to a nearby attack and keeps its target fixed. Drag a visible
marker to move its target on the timeline.

To stretch and resize together, select the clip and drag its ↔ handle, or
Alt/Option-drag its right edge. A plain edge drag trims. Trimming or splitting
a clip that is fitted to its range keeps the speed it was fitted to.

### Picture timecode

Right-click the timeline ruler and choose **Picture timecode**. Set the
picture's frame rate and the timecode at project start, as `HH:MM:SS:FF`
(`HH:MM:SS;FF` for drop-frame at 29.97 fps). Enter frame labels as the picture
edit shows them.

- **Read playhead** shows the picture frame closest to the playhead.
- To go the other way, enter a label, choose **Convert**, then **Move
  playhead**. The playhead goes to the frame's time through the current tempo
  map, including positions between grid lines.
- **Save project clock** keeps the rate and start label in the project when it
  is saved and reopened. **Clear saved clock** removes them. Both can be undone.

### Automation rows

Every track can show its automation under its lane. The **A** button in the
track header (next to **M** and **S**) and **Show automation** in the track
menu open and close the area. It starts closed, and the state is kept per track
for the session.

Each lane gets one row. The row draws the lane's points as a polyline with the
lane name. The row's header cell repeats the name, shows the kind of target
(**Effect**, **Patch** or **Control**) and has a per-lane **M** button. A
`pluginparam_…` lane shows the parameter name it encodes, so a synth or plugin
lane reads like the knob it drives.

A lane with a span (`automation: … [3:1 - 5:1]`) draws that span as a block,
so you can see where it writes. A lane without a span is a whole-timeline lane.
Its row is striped and has no block, because the lane writes from its first
point to the end of the project.

You handle an automation block the way you handle a clip:

- Drag the block to move it, points included. A ghost follows the pointer, the
  position snaps to the shared grid with a 1/16 floor, and the move is written
  on release as one undo entry.
- Drag the start or end edge to resize the block; the opposite edge stays put.
- Alt-drag copies the block under the pointer, the same copy modifier the clip
  lanes use.
- Press Escape to cancel a drag without changing the project.
- Right-click the block, or its row, for **Copy to playhead**, **Duplicate**,
  **Split at playhead** and **Delete**. **Duplicate** puts the copy right after
  the block; **Copy to playhead** puts it at the playhead. **Split at
  playhead** needs the playhead inside the block and at least a 16th note from
  either edge, and the two halves meet at the value the block wrote at the
  seam. **Delete** removes the clip lane, as does pressing **Delete** with the
  block selected.

A move, resize, duplicate or split that would overlap another lane on the same
target is refused. The status bar names both spans, and the block stays where it
was. A whole-timeline lane has no block, so those four menu items are disabled
there with the hint **This lane drives the whole timeline; a clip gesture needs
a span.**

Effect, patch and control lanes all accept these gestures. A lane on its own
tick grid (`automation: … [ppq=N]`) does not, because the gestures work on the
project's notated grid.

To edit points:

- Double-click the row to add a point at the pointer. The vertical position sets
  the value, and the time snaps to the shared grid with the same 1/16 floor that
  clip positions use. The footer states the floor when the grid is finer.
- Drag a point to move it in both directions. A ghost follows the pointer, the
  horizontal position snaps to the grid, and the move is written on release as
  one undo entry.
- Click a point to select it. **Delete** or **Backspace** removes the selected
  point, and the point menu has **Delete point**.
- Right-click a point to choose one of the four segment curves (**Linear**,
  **Stairs**, **Half sine**, **Smooth**) or **Tension…**, which asks for the
  0–1 blend used by a smooth segment.

A muted lane shows **This lane is muted.** and refuses every edit; adding,
moving or deleting a point reports that reason and writes nothing. The lane's
**M** button clears the mute. A track with no lanes shows **No automation on
this track. Right-click a knob and choose Create automation lane.**

Bus and master lanes have no track row, so their rows come after the last
track, under a strip that names the owner. They stay visible while they have
lanes.

The same lane also appears in the device rack's **Automation** list, which
holds its mode, smoothing, curve and LFO controls. Point positions and values
are edited on the timeline row.

## Tempo and meter lane

The **Tempo** chip in the ruler header opens a two-row lane above the tracks,
with tempo points on the upper row and meter changes on the lower one. The lane
starts open when the project already has maps, opens when you add the first
point, and folds away when both maps are cleared. Folded, it takes no timeline
height. The sticky header shows the tempo and meter at the playhead.

- Double-click a row to add a point at the snapped position, or right-click it
  for **Add tempo point here**, **Add meter change here** and the clear items.
  A new point starts with the value already in effect at that position, so
  adding it changes nothing until you edit it.
- Drag a point horizontally. Tempo points follow the shared grid, and meter
  changes snap to bar starts. Alt is the timeline's copy modifier, so no
  modifier turns snapping off here. Bar numbers come from the meter map, so the
  ruler numbers bars the same way the engine counts them.
- Double-click a point, or right-click it and choose **Set value…**, to type a
  value. Tempo accepts 20–400 BPM. A meter numerator is 1–32, and the
  denominator is 1, 2, 4, 8, 16 or 32. Invalid values and duplicate positions
  are refused and the project is left untouched.
- Delete removes the focused point, and the context menu can also clear a whole
  map. Clearing every tempo point returns to the project BPM; clearing the meter
  changes returns to the project meter.
- From the keyboard, **Tab** to the lane or a point. **←/→** nudges the selected
  point by one grid step (a meter change moves by one bar). **↑/↓** changes a
  tempo point by 1 BPM (10 with Shift) or a meter numerator by 1. **Enter**
  edits the value, **Delete/Backspace** removes the point, and **Esc** clears
  the selection.

Every edit is one undo entry and one dirty change. The maps are saved in `.ocp`
as `tempo_map` and `time_sig_map`, and the transport, the grid and clip
placement all resolve positions through them.

## Piano roll

The piano roll edits the selected pattern, or the clip you opened from a phrase
or the timeline. Its toolbar has the two tools and a zoom control (10–200 px per
beat, also **Mod+wheel**).

The roll shows the opened clip. Its left edge is the clip's first beat, and the
clip boundary marks the clip's exclusive end. The drum grid can show authored
hits beyond that boundary so you can still edit them.

Bar numbers stay absolute: in a clip that starts at bar 9, the left edge is
labelled "9". A clip that starts mid-bar shows its partial bar at the left edge
and draws its first full bar line where the next bar really begins.

Drawing, dragging, resizing, nudging and duplicating keep note starts inside the
clip. A one-beat clip is exactly one beat wide. Only a zero-length range falls
back to one grid step, so the canvas never collapses.

With the **Select** tool, drag notes to move them, drag their edges to resize,
drag empty space to box-select, and Shift-click to add or remove one note from
the selection. With the **Draw** tool, click to add a note. Delete or Backspace
removes the selection with either tool.

### Drum lane

A drum pattern whose preset is a kit (`builtin.drumkit`) gets a drum lane with
one row per pad, in the kit's own order. Each row carries the pad name, and the
toolbar shows the kit name beside the instrument. A note that the kit does not
declare gets an `Other <n>` row, where `<n>` is its MIDI note. A semitone that
the kit does not name and no hit uses gets no row, so the lane shows the kit's
pads instead of 72 lines. A plain drum track keeps the General MIDI rows.

Hits are placed on the current grid, including triplets and 1/32 notes. The DSL
stores a sixteenth anchor and the exact grid displacement as
`grid_offset_beats`, in quarter notes, up to ±0.125. The written position
decides whether the hit fits the clip; swing and an optional
`timing_offset_beats` then shift the onset you hear. A hit placed on an `Other`
row writes that row's MIDI note in `midi_note`, so the kit plays the pad that
holds the note.

Markers and velocity bars show each authored hit once, including its explicit
timing offset. An outlined hit has no onset that plays in this placement once
the clip's slip, repeat, swing and timing bounds are applied. You can still
select an outlined hit and edit its velocity.

### Menus and keys

Right-click (Ctrl-click on macOS) opens a menu for whatever is under the
pointer and changes nothing else. A note gets the note menu (copy, paste at the
playhead, duplicate, delete, velocity, mute, grace), a drum hit gets the hit
menu, and empty grid gets the grid menu (draw a note here, paste here, grid
1/4–1/32). The first row names the note (pitch and velocity) or the position
(pitch, bar and beat, or the pad name on a drum lane). The hit menu names and
deletes the hit under the pointer, and the right-click also picks that hit, so
Delete acts on the hit the menu showed.

Only the primary button selects, draws or scrubs. A secondary click never
selects a note, starts a marquee or moves the playhead.

Delete or Backspace removes the selection, **Mod+D** duplicates it, **Mod+A**
selects every note and **Esc** clears the selection. Left/Right nudges by one
grid step, and Shift+Left/Right by one beat.

## Mixer

The Mixer panel has the selected node's inspector on the left and the routing
view on the right, with a project-level routing toolbar above the bus graph.
The graph shows the topology, the toolbar creates buses, and the inspector
edits the selected node and connection.

The graph puts tracks in the left column, buses in the middle and master on the
right. Each connection is one curve: solid for a main output or bus route,
dashed for a send. Click a node to inspect it, or a connection to edit it. A
connection can also be pressed from the keyboard with `Enter` or `Space`.
Clicking the background clears the selection, and dragging the background pans
the view.

To add a bus, type a name in the toolbar's bus field, or leave it empty to get
the first free `Bus N`. The new bus is routed to master and selected. To add a
track, drop a preset on the mixer.

Selecting a connection opens a card in the inspector under the source node's
controls. Use `To` to re-point the connection: a track's exclusive main output,
a send, or one bus route. A send also shows its dB level, `PRE`/`POST` tap,
input pan and a remove button. `Escape` leaves the card; `Delete` removes the
connection and returns focus to the source node.

Next to the channel controls, a track can add a send to a bus, and a bus can add
a route to master or to another bus.

The delete button for a bus is in the bus actions card under the channel
detail. An unused bus is removed at once. A bus that a track output, a send or a
bus route still uses is refused with an error that lists those references. The
confirmation card then offers to re-route before deleting: track outputs return
to master, sends into the bus are dropped, and buses that routed into it return
to master.

Below the channel selector, the node detail shows the fader, pan, `M` / `S` /
`SAFE` / `Ø`, the effect chain, the send list (with the auxiliary source picker
for CLAP and VST3 instruments) and the bus route list.

Every control commits once per gesture, so one fader move, one send edit or one
connection re-point is one undo entry. See [Mixer routing](../mixer-routing.md).

## Device rack

Each track, bus and master has an effect chain. `+` opens the built-in device
picker, and a slot expands into the panel for its device. The slot's context
menu has **Bypass** (for devices that define it) and **Remove effect**.

The dynamic EQ opens the full curve editor. Drag a node to set frequency and
gain, scroll over it for Q, Alt-drag a dynamic band to set its range, and
double-click to reset. Arrow keys make fine moves. Each band has a solo, and a
channel view switches between `All`, `L`, `R`, `M` and `S`.

The fixed EQ and the passive EQ have their own curve views. Device panels can
show gain reduction while audio plays, and the parametric EQ can draw a live
spectrum analyser. Plugin effects show their raw parameter list.

## Patch editor

Selecting an OpenSynth preset opens the synth workbench. It has the views
**SOUND** (with OSC, FILTER and AMP pages), **MOD**, **FX** and **VOICE**; a
preset field that opens the factory and user-library browser; a **LOAD** button
that imports an external preset file; a `⋯` patch menu; a **COMPACT**/**EXPAND**
toggle; and an audition keyboard. The editor writes the preset's `params`, so
every edit is an undoable project change. Details are in [OpenSynth](synth.md).

Hover over or focus a rotary control to read its value. Press the knob and move
the pointer up to increase the value or down to decrease it; hold Shift for
finer steps. The side indicator follows the gesture, so you do not have to move
the pointer onto it. Releasing commits the edit. The mouse wheel also turns a
knob. From the keyboard, focus the knob and use the arrow keys, Home or End.
Enter or Space opens a vertical editor, and Escape closes it. Double-click
restores the parameter's default.

The patch field shows the name of the project preset. The smaller label after it
names the patch it was loaded from (a factory patch, a user-library entry or an
imported file name), and that label is saved in the `.ocp` file. The `⋯` menu
manages the patch:

- **Rename…** renames the project preset and rewrites every track, pattern and
  `extends:` reference in the same undo step.
- **Duplicate** and **Save as…** create an independent copy and point the
  players at it, so editing the copy leaves the original alone.
- **Save to library…** stores the whole patch (parameters, effect chain and
  provenance) under `~/.opencompose/presets` for use in other projects. The
  OpenSynth wavetables and samples it depends on are copied into the library,
  so the source project can move on its own.

The browser lists the factory bank and a **User library** section with the same
search and audition controls; the search also matches library tags. Loading a
row replaces the whole patch (parameters, effect chain and source label) and
resets every key the row does not declare, in one undo entry. See
[Factory patches](synth.md#factory-patches) for the audition and reset rules.

## Plugin browser

The **Plugins** sidebar tab lists scanned CLAP, VST3, VST2, AU and LV2 plugins
and the built-in assets. It has search, format and type filters, a **Rescan**
button, and **Check project**, which reports plugins the project references but
the app cannot find. **Add preset** creates a project preset from the selected
source.

LV2 and VST2 plugins have no in-app editor. Edit LV2 control ports as preset
`params` in the DSL or the preset editor, and VST2 parameters in the preset
editor.

The **Store** tab opens external plugin, sample, SoundFont and preset sites in
a separate window and lists the bundled catalog of free plugins.

## DSL editor

Press **D** to open the DSL sidebar, which shows the project as highlighted
text. **Mod+Enter** parses and applies the text. A parse error is shown inline
and nothing is applied. Each apply is one undo entry.

## AI panel

The **AI** sidebar tab is a chat about the open project. It has provider
settings (endpoint, model, key or sign-in), quick prompts such as **Describe
arrangement**, **Add reverb bus**, **Add drum beat** and **Improve mix**, a tool
log and a diff view.

**Project edit permission** decides how the agent's edits land. With **Ask**,
each change to the project DSL waits on a card with **Approve**, **Always
allow** and **Reject**, one change at a time. **Review** validates each change,
records its diff in the conversation and applies it in the same turn. **Auto**
applies changes directly, though a destructive call still asks first. When a
proposed edit is open for preview, the preview bar offers **Accept** and
**Decline**. Limits are listed in [Drawing from an agent](agents.md).

## Diagnostics

Press **Mod+Shift+D**, or choose **Export diagnostics…** on the **Help &
About** page of Preferences, and pick where to save
`opencompose-diagnostics-<stamp>.tar.gz`. The app writes the archive and reveals
it in the file manager. The archive contains `manifest.json`,
`logs/recent.log`, `project/project.ocp`, `environment.json`,
`plugins/scan-summary.json` and `errors/last-error.txt`. A section that could
not be collected is recorded in the manifest as `missing`, with a reason.

## Keyboard shortcuts

Every shortcut is a command in the frontend registry, so the key a surface shows
is the key that runs it (see [Remapping shortcuts](#remapping-shortcuts)).
**Mod** is Cmd on macOS and Ctrl elsewhere. Letter shortcuts do nothing while a
text field has focus.

| Shortcut | Action |
|---|---|
| Space | Play/pause; auditions the selected preset when the last click was inside the Presets panel |
| L | Toggle the loop |
| Mod+N / Mod+O | New project / open project |
| Mod+S / Mod+Shift+S | Save / Save As |
| Mod+E | Export audio |
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
| Mod+G / Mod+Shift+G | Group / ungroup clips |
| Alt+T | Add a marker at the playhead |
| F2 | Rename the selection |
| Mod+wheel | Zoom the timeline or the piano roll |
| Mod++ / Mod+- / Mod+0 | Interface scale up / down / reset |
| Mod+Enter | Parse and apply DSL edits in the DSL sidebar |
| Mod+Shift+D | Export the diagnostic bundle |
| Mod+Shift+P | Open the command palette |

### Remapping shortcuts

Open **Preferences > Keymap** to search the command table, pick a command and
press a new combination. The command palette (Mod+Shift+P) reads the same
table, so a new binding shows up there and in the Keymap list at once.

The registry is in `crates/oc-gui/frontend/src/stores/commands.ts`. A command
has an id, a label key, a category, a default binding and a handler. Components
that own local state register their handler while they are mounted.

Overrides are stored in `localStorage` under `opencompose.keymap` as
`{ version: 1, bindings: { "<commandId>": "<binding>" } }`. A binding string
uses `Mod`, `Shift`, `Alt` and the key name, for example `Mod+Shift+Z`.

If you bind a key that another command already uses, the binding is refused,
the message names that command, and the old binding stays. **Reset** drops one
override; **Restore defaults** clears them all after you confirm. Escape, Tab,
modifier-only presses and the palette's own Mod+Shift+P are reserved and cannot
be assigned.

## Files on disk

Projects are `.ocp` text, and a saved project is the same document the CLI
reads. The other files the app uses:

- Media cache: `<config>/cache/media` when a profile is selected, otherwise the
  platform cache directory (`~/Library/Caches/OpenCompose/media` on macOS). It
  holds decoded audio, stretch variants and vocal analyses, all of which can be
  recomputed.
- Edit history, recovery copies, AI conversations and caches: see
  [On-disk storage](../storage-layout.md).
- Live MIDI preferences: `~/.opencompose/midi.json`.
- Audio devices, sample rate, buffer, capture layout and monitoring:
  `~/.opencompose/audio.json`. See [Audio devices](../audio-devices.md).
- Agent endpoint and token: `~/.opencompose/agent.json`.
- AI provider settings: `~/.opencompose/ai-providers.yaml`. Keys are kept in the
  OS credential store and never in the project.

## Opening project files from the desktop

OpenCompose registers `.ocp` files with the operating system. Opening a project
in Finder, Explorer or a desktop file manager brings the document window to the
front. A second launch hands its project to the running app instead of starting
a separate editing session. File paths and local `file:` URLs are accepted; a
network URL does not open a project.

If the open project has unsaved edits, choose **Save**, **Don’t Save** or
**Cancel** before the requested file replaces it. If the save fails or you
cancel it, the current project and the prompt stay. **Cancel** dismisses that
open request. Opening the current file again only brings its window forward and
keeps your edits. Requests that arrive while the app is starting or another
dialog is open wait until the document can answer them safely.

Closing the last document window quits the app after the same unsaved-work
choice. Hiding or minimizing the window keeps the project and the playback
session in the running app; on macOS, clicking the Dock icon brings the window
back. **Quit OpenCompose** uses the app-wide unsaved-work prompt.

## Inspecting a clip

Right-click a clip to read its properties without changing the selection or
moving playback. The menu shows the placement and duration, the track, the
effective preset, plugin, patch or source sample file, and any relevant offsets,
looping and processing settings. Source paths are selectable text: select a
path and use Copy to copy the text, which leaves the clip clipboard alone.
Relative media paths are shown against the project file's location, and an
unsaved project shows the relative path as written. The menu does not check
that the file exists on disk.

Choose **Inspect clip** to keep the view open in the **Inspector**. A note clip
shows a piano roll beside its source details. An audio clip shows the file,
trim, gain, fades, stretch and pitch, without a note editor.

## First launch and appearance

Until you choose otherwise, OpenCompose follows the system light or dark
appearance and picks its interface language from the system's preferred browser
languages. A language the app does not support falls back to English. The setup
screen shows these choices, with **Follow system**, **Light** and **Dark** and
an explicit language control. **Follow system** keeps following later system
changes; an explicit theme or language stays as you set it. The same settings
are in Preferences, and **Help & About > Getting started** reopens setup without
replacing the project.

Choose **Play test sound** to hear a quiet built-in phrase, **Stop test sound**
to end it, or **Audio settings** to choose the output. You need no API key and
no plugins. In the workspace chooser, **Create starter sketch** asks for a new
file name and writes a complete built-in example; it refuses to overwrite an
existing file.

## Help and support

The **Help & About** page holds the bundled English manual, the build, source
and platform identity, a link to the public repository, the license and
SoundFont notices (available offline) and diagnostics export. The diagnostics
file is saved locally and contains the project source, paths, logs, plugin
information and system details, so read through it before you share it.
