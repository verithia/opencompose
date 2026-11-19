# Markers and the loop region

A marker names a point on the timeline. The loop region is a rehearsal window
that the transport repeats. They are stored in different places: a marker
belongs to the project, and the loop region belongs to the playback session.

## Markers

Write a marker with `@marker`, a name and an `at` position. The position uses
the same `bar:beat:subdivision` form as every other timeline position.

```ocp
@marker "Verse 2" at 9:1

@marker outro at 41:1:2
```

Markers are project data. They survive save and load, they appear in the DSL
text, and `ocp fmt` keeps them in timeline order. A project without markers
writes no marker lines.

A marker annotates the timeline and has no effect on the arrangement:

- It may sit after the last audio. The ruler shows it there, and the
  arrangement does not change.
- It never changes what `ocp render` or an export produces.
- Two markers cannot have the same name. The validator and the marker commands
  refuse a duplicate and name the marker that already uses it.
- A position with bar or beat `0` is refused, because bars and beats start at
  `1`. A subdivision of `4` or more is refused, because a beat holds
  subdivisions `0` to `3`.

## Marker controls on the ruler

Each marker is a flag on the ruler with its name beside it.

To add a marker at the playhead, press `Alt+T` or choose
**Add marker at playhead** in the ruler's context menu. The command is
available while a project is loaded. The new marker is named `Marker 1`,
`Marker 2` and so on; rename it if the name should say more.

To work with an existing flag:

- Click it to move the playhead to the marker.
- Drag it to move the marker. The flag follows the pointer, and the move is
  written as one undo entry when you release the pointer, so the whole gesture
  is one step in the history.
- Double-click it to open the **Rename marker** dialog. **OK** stays inactive
  until the name is not empty. The webview has no `window.prompt`, so the app
  draws this dialog itself.
- Right-click it for a menu with **Rename marker…** and **Delete marker**.
  Delete asks **Delete marker?** before it removes the marker.

While a change is being saved, the ruler reads `Saving marker…`. If it fails,
the ruler reads `Marker not saved: {reason}`.

## The loop region

To draw a region, drag in the strip along the bottom edge of the ruler. The
region appears as a tinted band over the ruler, and the **Loop** button in the
transport (`L`) becomes available.

The **Loop** button, `L` and **Loop the region** in the ruler's context menu
arm and disarm the loop. They work only when a region exists. Until then the
button is disabled, and its tooltip reads
`Drag in the ruler to set a loop region first.` While the transport applies the
change, the ruler status reads `Applying loop…`.

Dragging the strip again redraws the region, and the band follows the pointer.
**Clear loop region**, in the context menu of the strip or the ruler, removes
the region; the band disappears when the transport confirms. If any of these
changes fails, the ruler status and the transport read
`Loop did not change: {reason}`.

The transport refuses a region shorter than one beat with
`Loop region is shorter than one beat; drag a longer region or clear it.` It
also refuses a region that starts after the project end, because no audio
exists there. A region that starts inside the project and ends after it wraps
at the project end, and the ruler status says
`The loop end passes the project end; playback wraps at the end of the project.`
Clearing the region turns Loop off, so the button never promises a wrap that
has nowhere to go.

### Where the region lives

The loop region is session state. The app stores it, together with the armed
flag, in the webview's local storage under the project path. It survives a
reload and a restart of the app, and it never travels inside the `.ocp` file.

The design rule is that a control must show what will happen after a reload:

- The region is a rehearsal aid. It changes playback and nothing else, so it
  stays out of a file that another person can open.
- Local storage restores both the band and the armed state. What you see after
  a reload is how playback behaves after it; Loop is never off while the band
  is still drawn.
- A region that no longer applies, for example because the project got
  shorter, is removed from the screen, and the ruler status shows the
  backend's reason as `Loop did not change: {reason}`.
- The region is not part of the project, and the offline renderer and export
  receive only the project. The region cannot reach them.

### Playback

With Loop armed, streaming playback wraps at the region end. The wrap uses the
ordinary seek path: the remaining lookahead is drained, the stream seeks back to
the region start, and rendering resumes. A wrap therefore costs the same as a
backward seek. On projects whose instruments must be rebuilt, there can be a
short gap at the loop point. Without Loop, playback plays through the region
and continues to the project end.

The region is drawn on the written timeline that the streaming transport plays.
Only the project end bounds it. When the playhead is already past the region end
and Loop is armed, pressing Play jumps back to the region start.

## Sections, repeats and the loop

`@section` repeats are expanded only when the project is rendered offline. The
streaming transport plays the written timeline. A loop region wraps the same
written positions the transport plays, so a repeat never moves, duplicates or
shortens it. Arming a loop never changes the arrangement or its rendered
length.

Take a project with a repeated section and a loop near the end of that section.
The rendered arrangement is longer than the written one, so a region placed
against the rendered end would wrap on different music. The loop is therefore
mapped against written quarter notes, and the arrangement stays untouched. The
test `section_repeats_do_not_move_a_loop_region` pins that mapping.

## What the loop does not affect

Rendering and exporting ignore the loop region. The offline renderer receives a
project, and the region is never part of one, so both gate fixtures stay
byte-identical with a loop armed. The test
`a_loop_region_does_not_change_the_offline_render` pins this, together with the
byte-identity test for a project without markers.
