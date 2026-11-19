# Autosave and recovery

OpenCompose keeps a bounded set of recovery copies of the open project. A copy
is a complete project document, so restoring it goes through the ordinary
project-load path. No separate decoder exists.

## Where copies live

Recovery copies are written under the app data directory, never next to the
project:

```text
~/.opencompose/recovery/<project-key>/<stamp>-<project-key>.ocp
~/.opencompose/recovery/<project-key>.owner
```

`<project-key>` identifies the document:

- A saved project is keyed by its path key: the first 8 bytes of the SHA-256 of
  its canonical path, in hex. The edit history, the AI note and the recovery
  copies of one file share that key. When you rename or move the file with
  **Move Project…**, its copies stay reachable. A file moved outside the app is
  matched by the content SHA-256 recorded in its owner marker.
- A project that has never been saved is keyed by the app session. Two windows
  with unsaved documents therefore get separate directories, and neither can
  list, restore or prune the copies the other wrote.

The `.owner` marker next to a directory records the project path and the
content SHA-256. A save replaces the project file by rename. The marker lets a
move adopt the copies, and it lets a save clear the directories that an earlier
identity key left behind.

A copy written by an older build lives in a directory named by the file
identity or by the older 64-bit path hash, and it can still be listed and
restored. Older builds kept the copies of a never-saved document in the shared
`untitled` directory. Every session lists that directory, because no session
can claim it, and a save from an untitled document retires it.

Copies hold the project document only. Media files that the project declares
stay in the project folder and are not duplicated. Restoring a copy assumes
those files are still where the project references them.

## When a copy is written

A copy is written:

- after the project has been dirty for 60 seconds, and again every 60 seconds
  while it stays dirty;
- immediately before an offline mix render or a stems export, so a crash
  during a long render does not lose the state the render started from;
- immediately before the open project is replaced by New, Open or a restore, so
  unsaved work stays recoverable.

Clip and range previews do not write a copy. They are frequent and short, and
the timer already covers the same edits.

No copy is written while an agent transaction is open, because the project is
then half-applied. Writing a copy leaves the dirty flag, the undo stack, the
media folder and the audio graph untouched.

## Retention and a clean save

Each project keeps its 10 newest copies; writing an 11th removes the oldest. A
successful save deletes every copy of that project, including the copies made
while it was untitled, because the saved file now holds the document.

**Move Project…** merges the copies under the project's current key with those
under the keys that earlier builds wrote, so copies from either directory
survive the move. Files in those directories that are not recovery copies stay
where they are.

## Discovery and restore

When a project loads and one of its copies is newer than the saved file, the
app asks **Recover unsaved work?**. You can also choose **Recover autosave…**
in the file menu of the toolbar (the **More file actions** arrow) at any time.
The item is unavailable when there is nothing to recover.

The dialog lists the copies of the open project, newest first. Each copy's
stamp is the wall-clock time, raised above every existing stamp in its
directory. A clock that steps backwards therefore cannot make a new copy look
older than an old one, and two copies written in the same millisecond get
distinct names.

**Restore** is offered only for a copy whose stamp is later than the
modification time of the saved file. An older copy stays in the list with
Restore disabled and a reason, and a copy that does not parse says so. A
project that has never been saved has no file to compare against, so all of its
copies can be restored.

Restoring parses the copy and replaces the open project through the same path
that **Open file** uses. The undo history starts clean, previews and caches see
the restored content, and analysis results match the document. The copy stays
on disk, and the restored project is marked dirty. A save retires the copy. A
restore that fails leaves the copy in place and reports the file and the
reason.

**Discard** deletes one copy after you confirm. The saved project is not
affected.

A copy whose file name carries another project's key is refused and never
loaded. A copy moved into the wrong recovery directory therefore cannot be
restored into the wrong project. The same rule keeps two unsaved sessions
apart.

## Orphaned directories

Listing first sweeps the recovery root. A directory whose newest copy is older
than 30 days is removed when its owner marker names a file that is gone, or a
file that no longer matches the directory's identity or legacy path key. A
directory without a marker, such as one left by a crashed session, is removed
on age alone. The directories of the project being listed are never swept. The
age limit keeps the copies of a project alive while it sits on a volume that is
temporarily unmounted.
