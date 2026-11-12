# Autosave and recovery

OpenCompose keeps a bounded set of recovery copies of the open project. A copy
is a complete project document, so restoring one goes through the ordinary
project-load path; no separate decoder exists.

## Where copies live

A recovery copy is written under the app data directory, never beside the
project:

```
~/.opencompose/recovery/<project-key>/<stamp>-<project-key>.ocp
~/.opencompose/recovery/<project-key>.owner
```

`<project-key>` names the document:

- A saved project is keyed by its file identity: the device and inode number on
  Unix, and the creation time and size on Windows, where the standard library
  exposes no file id. Renaming or moving the file keeps that identity, so its
  copies stay reachable. When the file is gone, the key falls back to a 64-bit
  hash of the canonical path.
- A project that has never been saved is keyed by the app session. Two windows
  with unsaved documents therefore get separate directories, and neither can
  list, restore or prune copies the other wrote.

The `.owner` marker beside a directory records the project path. A save replaces
the project file by rename, which gives it a new identity; the marker lets the
save clear the directory the old identity left behind. A copy written by an
older build lives in a directory named by the path hash alone, and still lists
and restores. Its copies of a never-saved document live in the shared
`untitled` directory, which every session lists because no session can claim
them; a save from an untitled document retires that directory.

Copies hold the project document only. Media files declared by the project stay
in the project folder and are not duplicated; restoring a copy assumes those
files are still where the project references them.

## When a copy is written

- After the project has been dirty for 60 seconds, and then once per further 60
  seconds while it stays dirty.
- Immediately before an offline mix render or a stems export, because a crash
  during a long render would otherwise lose the state the render started from.
- Immediately before the open project is replaced: New, Open, and restoring a
  copy. This keeps unsaved work recoverable when a destructive command replaces
  it.

Clip and range previews do not write a copy: they are frequent and short, and
the timer covers the same edits.

A copy is skipped while an agent transaction is open, because the project is in
a half-applied state. The writer leaves the dirty flag, the undo stack, the
media folder and the audio graph untouched.

## Retention and a clean save

Each project keeps the 10 newest copies; writing an 11th removes the oldest. A
successful save deletes every copy of that project, including copies made while
it was untitled: the saved file holds the document, so they would only take
space.

## Discovery and restore

`Recover autosave…` in the File menu lists the copies of the open project,
newest first. A copy's stamp is monotonic within its directory: it is the wall
clock, raised above every existing stamp, so a backwards clock step cannot make
a new copy older than an old one and two copies in one millisecond get distinct
names. A copy is offered for restore only when its stamp is later than the saved
file's modification time. An older copy stays listed with Restore disabled and a
reason; a copy that does not parse says so. A project that has never been saved
has no file to compare against, so its copies are always offered.

Restoring parses the copy and replaces the open project through the same path
`Open file` uses, so undo history starts clean, previews and caches see the
restored content, and analysis results match the document. The copy stays on
disk after a restore, and the restored project is marked dirty; only a save
retires the copy. A restore that fails leaves the copy in place and reports the
file and the reason.

A copy whose file name carries another project's key is refused instead of
being loaded, so a copy moved between recovery directories cannot be restored
into the wrong project. The same rule keeps two unsaved sessions apart.

## Orphaned directories

Listing sweeps the recovery root first. A directory whose newest copy is older
than 30 days is removed when its owner marker names a file that is gone or no
longer matches the directory's identity or legacy path key; a directory without
a marker, such as a crashed-session directory, is removed on age alone. The
directories of the project being listed are never swept. The bound keeps a copy
alive while a project sits on a volume that is temporarily unmounted.
