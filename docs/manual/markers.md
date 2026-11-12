# Markers and the loop region

A marker names a point on the timeline. The loop region is a rehearsal window
the transport repeats. The two are stored in different places on purpose: a
marker is part of the project, and a loop region is part of the playback
session.

## Markers

Write a marker with `@marker`, a name and an `at` position. The position uses
the same `bar:beat:subdivision` form as every other timeline position.

```ocp
@marker "Verse 2" at 9:1

@marker outro at 41:1:2
```

Markers are project data. They survive save and load, they appear in the DSL
text, and `ocp fmt` keeps them in timeline order. A project without markers
serialises exactly as it did before markers existed.

A marker is an annotation, not arrangement data:

- It may sit past the last audio. The ruler shows it there, and the arrangement
  is unaffected.
- It never changes what `ocp render` or an export produces.
- Two markers cannot share a name. The validator and the marker commands refuse
  a duplicate and name the marker that already holds it.
- A position with bar or beat `0` is refused, because bars and beats start at
  `1`. A subdivision of `4` or more is refused, because a beat holds `0` to `3`.

## Marker controls on the ruler

Each marker is a flag on the ruler with its name beside it.

| Control | Enabled when | Disabled reason | Pending | Failure |
| --- | --- | --- | --- | --- |
| Add marker at playhead (`Alt+T`, or the ruler context menu) | A project is loaded | — | The ruler reads `Saving marker…` | `Marker not saved: {reason}` |
| Marker flag, single click | Always | — | — | `Marker not saved: {reason}` |
| Marker flag, drag | Always | — | The flag follows the pointer | `Marker not saved: {reason}` |
| Marker flag, double click | Always | — | The rename dialog keeps `Add` inactive until the name is non-empty | `Marker not saved: {reason}` |
| Marker flag, right click | Always | — | The confirmation reads `Delete marker?` | `Marker not saved: {reason}` |

A click moves the playhead to the marker. A drag moves the marker and writes one
undo entry when the pointer is released, so a whole gesture is one step in the
history. A double click opens the in-app rename dialog; the webview has no
`window.prompt`, so the application owns that dialog. A right click asks for
confirmation before it deletes.

`Alt+T` adds a marker named `Marker 1`, `Marker 2`, and so on at the playhead.
Rename it afterwards if the name should say more.

## The loop region

Drag the strip along the bottom edge of the ruler to draw a region. The region
is drawn as a tinted band over the ruler, and the transport gains a **Loop**
button (`L`).

| Control | Enabled when | Disabled reason | Pending | Failure |
| --- | --- | --- | --- | --- |
| `Loop` button, `L`, or the ruler menu item | A region exists | `Drag in the ruler to set a loop region first.` | The button reads `Loop armed` (or `Loop off…` when disarming) until the transport confirms | `Loop did not change: {reason}` |
| Loop strip drag | Always | — | The band follows the pointer | `Loop did not change: {reason}` |
| Clear loop region (strip context menu) | A region exists | `Drag in the ruler to set a loop region first.` | The band disappears when the transport confirms | `Loop did not change: {reason}` |

The transport refuses a region shorter than one beat with
`Loop region is shorter than one beat; drag a longer region or clear it.` It
also refuses a region that starts past the project end, because no audio exists
there. A region that starts inside the project and ends past it wraps at the
project end. Clearing the region turns Loop off, so the button never claims a
wrap that has nowhere to go.

### Where the region lives

The loop region is **session state**, not project data. The application stores it
in the webview's local storage, keyed by the project path, together with the
armed flag. It survives a reload and an application restart, and it never
travels inside the `.ocp` file.

The rule is that a control must not lie about what will
happen on reload:

- The region is a rehearsal aid. It changes playback and nothing else, so it
  does not belong in a file that another person can open.
- Because it is restored from local storage, a reload brings back both the band
  and the armed state. The picture after a reload is the behaviour after a
  reload; Loop is not silently off while the band is still drawn.
- A region that no longer applies (for example, the project got shorter) is
  dropped with the backend's sentence, rather than left on screen. Read
  `Loop did not change: {reason}` in the ruler status.
- Because the region is not in the project, it cannot reach the offline
  renderer or an export. That is a property of where the data lives, not a
  convention: the renderer only ever receives the project.

### Playback

With Loop armed, streaming playback wraps at the region end. The wrap uses the
existing seek path: the remaining lookahead is drained, the stream seeks back to
the region start, and rendering resumes. A wrap therefore pays the same cost as
a backward seek, and there can be a short gap at the loop point on projects
whose instruments must be rebuilt. Without Loop, playback plays through the
region and continues to the project end.

The region is drawn in the written timeline that the streaming transport plays.
It is bounded by the project end and by nothing else. When the playhead is
already past the region end, pressing Play jumps back to the region start,
because Loop is armed and that is the window it repeats.

## Sections, repeats and the loop

`@section` repeats expand only when the project is rendered offline; the
streaming transport plays the written timeline. A loop region wraps the same
written positions the transport plays, so a repeat does not move it, duplicate
it, or shorten it. Arming a loop never changes the arrangement or its rendered
length.

The case that could bite is a project that combines a repeated section with a
loop near the section's end: the rendered arrangement is longer than the written
one, and a region mapped against the rendered end would wrap on different music.
The loop is mapped against written quarters instead, and the arrangement stays
untouched. `section_repeats_do_not_move_a_loop_region` pins that mapping.

## What the loop does not affect

Rendering and exporting ignore the loop region. The offline renderer receives a
project, and the region is never part of a project, so both gate fixtures stay
byte-identical with a loop armed. `a_loop_region_does_not_change_the_offline_render`
pins this, together with the byte-identity test for a project without markers.
