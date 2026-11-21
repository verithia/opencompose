# On-disk storage

A project is one plain-text `.ocp` file. Everything else OpenCompose keeps for
it lives outside the project folder, under the configuration folder
`<config>`. By default `<config>` is `~/.opencompose`. Set
`OPENCOMPOSE_CONFIG_DIR` to use a separate profile instead; see
[Preferences](preferences.md#separate-configuration-profiles).

Almost everything here can be rebuilt. An edit history, a recovery copy, a
conversation, an AI note or a cache comes back from use or restores the
project, but none of them is the project. To keep a project, keep its `.ocp`
file.

## The path key

Each per-project store is named by a path key: the first 8 bytes of the
SHA-256 of the project's canonical path, written as 16 lowercase hex digits.
One key names the edit history, the AI note and the recovery copies of one
project file. The conversations of a workspace folder use the same rule on the
folder's path. A file that does not exist yet is resolved through its folder,
so its key is the same before and after the first save.

You cannot read the path from the key. Each store keeps a small file with the
readable path, so a moved item can be found again and the Storage page can
show what each item belongs to.

## Layout

```text
<config>/
  locks/                        open-project and open-conversation locks
  history/<path key>/
    project.json                marker: version, canonical path, content SHA-256
    entries.jsonl               one stored entry per line
    index.bin                   fixed-size rows: the live linear history
    state.json                  cursor and the document at the cursor
  recovery/
    <key>/
      <stamp>-<key>.ocp         one whole project document
    <key>.owner                 the path and content SHA-256 it belongs to
  ai/
    projects/
      index.json                key to project path
      <key>.md                  the project's AI note
    sessions/
      <workspace key>/
        workspace.json          the folder this key comes from
        <session id>/
          meta.json             title, times, provider, model, project path
          messages.jsonl        one record per model turn
          view.json             where the current view starts in messages.jsonl
          results/<sha256>.txt  a full tool result that a saved row names
          versions/<sha256>.ocp the project version a human turn was sent with
      audition-media/           clips that a listening request replays
      audition-renders/         full-rate renders kept for listening
      detached/<session id>/    a conversation with no workspace
      locks/session-<id>.lock   the process that has one conversation open
  plugin-folders.json           plugin folders you added
  content-libraries.json        sound folders you added
  runtimes/<runtime id>/        installed helper runtimes
  plugin-index/scopes/          cached signed plugin catalogs
  presets/                      the preset library
  sample-providers/             sample provider plugins
  gui-settings.yaml             GUI settings
  cache/                        caches, only in a selected profile
  logs/                         app and diagnostic logs, only in a selected profile
```

## Open locks

The process that has a project open holds
`<config>/locks/project-<path key>.lock`. Only one process at a time can have a
project open. A second desktop or headless process that tries to open the same
project is refused before anything changes: its document, history and files
stay as they were. The lock is released when another project replaces it or
the process exits. The lock file is not project data, so listings and sweeps
skip it.

The edit history has no lock of its own, because only the process that holds
the project lock opens its history. A sweep, or a manual removal of another
project's history, note or recovery copies, takes that project's lock first and
skips the data while another process holds it. Save As, **Move Project** and
opening a project at a different path take the destination's lock before they
move anything there.

A saved conversation has its own lock, `ai/sessions/locks/session-<id>.lock`.
See [Saved AI conversations](ai-sessions.md).

## Edit history

`<config>/history/<path key>/` holds one project's edit history.

| File | Contents |
|---|---|
| `project.json` | The marker `{version: 1, path, sha256}`: the canonical project path, and the SHA-256 of the file's content as the app last read or wrote it. Every successful write of the project updates it, including an agent publish, the flush at exit and a project created with **New**. |
| `entries.jsonl` | One stored entry per line, with its compressed forward and inverse patches, author and scope. The optional `cause` is left out for an ordinary edit, and a cause this build does not know loads as an ordinary edit. Display text is not stored. The file only grows: a line replaced by a redo branch stays until the history is rewritten. |
| `index.bin` | Fixed-size rows for the live linear history, oldest first. Dropping a redo branch truncates it. |
| `state.json` | The cursor, the id of the entry it rests on, and the document text at the cursor, written in one atomic replace. If a shorter index moved that entry's row, opening the history follows the id. Only an id the index no longer has discards the head, so a half-written file never pairs a cursor with the wrong document. |

The app looks up a history by path first. If the path has no history, it
compares the file's content SHA-256 with the marker of every history. A
history whose project file is gone follows a file with the same content, so a
file moved or copied outside the app keeps its undo. The history is copied if
the old file still exists and moved if it is gone. A file on a volume that is
not mounted still owns its history. A project created with **New** starts a
fresh history and never takes over another file's.

If a rewrite of a history was interrupted, the app finishes it from the `.old`
folder the next time it opens the history. It removes the staging folder of an
interrupted copy and any other leftover folder instead of using it.

**Move Project** moves the history to the destination path. **Save As** copies
it and keeps the source history. Both update the marker to the destination
path.

The journal is written to disk once a second, after an agent tool changes the
project, and when the app exits, so an edit made right before you quit keeps
its undo entry. An open agent batch reaches the journal only when it commits.

## Recovery copies

`<config>/recovery/<key>/` holds whole-document recovery copies. For a saved
project the key is its path key. For a project that has never been saved, it
is the app session. A copy is named `<stamp>-<key>.ocp`, and the `<key>.owner`
file beside the folder records the canonical path and content SHA-256.

The app keeps the 10 newest copies of each project. It also reads copies stored
under the keys of earlier builds (the file identity and an older 64-bit path
hash), so those copies still restore. **Move Project** moves the copies to the
destination's key. See [Autosave and recovery](autosave.md).

## AI notes and conversations

`<config>/ai/projects/` holds one Markdown note per project file. `index.json`
maps each key to the readable path, and `<key>.md` holds the note. The key is
the path key of the `.ocp` file, not of its folder. **Move Project** moves the
note; **Save As** copies it and keeps the source note.

`<config>/ai/sessions/` holds saved conversations. A conversation belongs to a
workspace folder, not to one `.ocp` file, and is stored under the workspace
key:

- `workspace.json` records the folder and every path seen for that key, so a
  moved workspace can be matched again.
- `<session id>/meta.json` records the title, times, provider, model, workspace
  and project path.
- `<session id>/messages.jsonl` is the transcript. Records are only appended.
- `<session id>/view.json` names the newest record that replaced the
  conversation, so a read parses only the current view. A read checks it and
  rebuilds it if it does not match the transcript.
- `<session id>/results/<sha256>.txt` holds a tool result too large for its
  message row. The row stores `result_sha` instead of the text. The sweep
  removes a result file that no saved row of its session names, and a fork
  copies the files its inherited rows name. A read checks the hash, limits how
  many bytes it loads and refuses a file whose text does not match its name.
- `<session id>/versions/<sha256>.ocp` holds a project version attached to a
  human turn, named by its content hash.
- `detached/<session id>/` holds a conversation with no workspace, such as one
  for an unsaved project.
- `audition-media/` holds the short clips that a later request replays, and
  `audition-renders/` holds the full-rate renders kept for listening. The two
  folders share one 256 MiB budget.

On Unix, these folders have mode `0700` and these files `0600`. A workspace
keeps at most 32 conversations. **Clear old conversation history** in
**Preferences > Storage** rewrites each conversation's transcript from its
second-newest summary, then removes the files that only the dropped records
used. The AI panel's `/clear` command does the same for the open conversation.
Both report how many records were dropped and how much space was freed. See
[Saved AI conversations](ai-sessions.md).

## Caches

Caches can be rebuilt, and you can delete them while the app is closed. In a
selected profile they live in `<config>/cache/`, so two profiles never share or
evict each other's files. Without a profile they live in the platform's cache
folder:

| Platform | Media cache without a profile |
|---|---|
| macOS | `~/Library/Caches/OpenCompose/media` |
| Windows | `%LOCALAPPDATA%\OpenCompose\cache\media` |
| Linux | `$XDG_CACHE_HOME/opencompose/media`, or `~/.cache/opencompose/media` |

The other caches sit beside the media cache, in the same parent folder.

| Folder | Contents |
|---|---|
| `media/` | Decoded media, resampled buffers and derived clip audio, keyed by source path, modification time and size. |
| `media/` | Vocal analyses (`.ocn`) and tune and harmony bakes, within a 1 GiB budget. |
| `preset-trims/` | Measured loudness trims of presets. |
| `audition-scratch/` | A render waiting to be handed to a conversation. |

The Storage page shows decoded audio, processed audio and the vocal cache on
separate lines, and **Clear unused clip audio** clears all three. The media
cache and the derived audio cache each have their own size limit on disk.

## Application folders

| Folder | Contents |
|---|---|
| `<config>/runtimes/` | Installed helper runtimes, such as the stem separator. |
| `<config>/plugin-index/scopes/` | Cached signed plugin catalogs, one scope per signing authority and origin. `OCP_PLUGIN_INDEX_DIR` moves the folder without changing which keys are trusted. |
| `<config>/presets/` | The preset library, including patches saved from the patch editor. |
| `<config>/sample-providers/` | Installed sample provider plugins. `OPENCOMPOSE_SAMPLE_PLUGIN_DIR` points at another folder. |
| `<config>/logs/` | In a selected profile: `opencompose-app.log` from every run, and the diagnostic logs of runs started with diagnostic flags. Without a profile, both go to the platform's log folder (`~/Library/Logs/OpenCompose` on macOS). The Storage page shows the folder in use. |

A log folder keeps at most 20 diagnostic logs per program, and each run removes
the oldest ones beyond that. The app log starts over when it passes 40 KB.

## Folder lists

Two small files hold the folders you add. The Folders page in Preferences edits
both.

| File | Contents |
|---|---|
| `<config>/plugin-folders.json` | The plugin folders you added, at most 32. `OCP_PLUGIN_FOLDERS` points at another file. |
| `<config>/content-libraries.json` | The sound folders you added. Each one is a content library with the instrument files its last scan found; a folder of samples alone lists none. A change keeps the previous version in `content-libraries.json.bak`. `OPENCOMPOSE_CONTENT_CATALOGUE` points at another file. |

If the app cannot read one of these files, it reports the problem and never
overwrites the file. Removing a folder from a list leaves its files on disk.

## Stale data

The app removes the data of projects and workspace folders that are gone, and
of nothing else. A file on a volume that is not mounted is out of reach, not
gone, so its data stays.

An item is gone when the file or folder it belongs to does not exist on a
reachable volume. A gone item is due when nothing has touched it for 30 days.
A sweep runs 60 seconds after the app starts, away from the UI and audio
threads, and removes every due item. It never removes the open project's data
or a conversation with a turn in progress.

**Preferences > Storage** lists the data of each project and folder: edit
history, AI note, recovery copies and conversations, each with its size and
whether it is gone or due. Only gone items offer **Remove**, and the app checks
again that the item is gone before it deletes anything. The page also lists the
application folders with their sizes, and the clip and vocal caches.
