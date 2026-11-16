# On-disk storage

OpenCompose keeps project files as plain `.ocp` text and everything else it
needs outside the project folder, under the configuration root `<config>`. The
default is `~/.opencompose`; `OPENCOMPOSE_CONFIG_DIR` selects a dedicated
profile. A selected profile holds its caches too, so two profiles never share or
evict each other's files.

Most of these files are replaceable. A history, a recovery copy, a conversation,
a note or a cache is rebuilt, restored or re-created from use, and none of them
is the project document. Keep the `.ocp` file to keep the project.

## The path key

Every per-project store is named by a **path key**: the first 8 bytes of the
SHA-256 of the canonical path, as 16 lowercase hex digits. The same key names
the edit history, the AI note and the recovery copies of one project file. The
conversations of a workspace folder apply the same rule to the folder path. A
file that does not exist yet resolves through its folder, so its key is the same
before and after the first save.

The key is opaque and does not reveal the path. Each store keeps a small file
that records the readable path, so a moved item can be found again and the
Storage page can show what an item belongs to.

## Layout

```text
<config>/
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
        workspace.json          the directory this key was derived from
        <session id>/
          meta.json             title, times, provider, model, project path
          messages.jsonl        one record per model turn
          view.json             where the current view starts in messages.jsonl
          results/<sha256>.txt  full tool result a saved row names
          versions/<sha256>.ocp project version a human turn was sent against
      audition-media/           clip a listening request replays
      audition-renders/         full-rate render kept for listening
      detached/<session id>/    a conversation with no workspace
  cache/
    media/                      decoded, resampled and derived audio; vocal analyses
    preset-trims/               measured loudness trims of presets
    audition-scratch/           render waiting to be handed to a conversation
  runtimes/<runtime id>/        installed helper runtimes
  plugin-index/scopes/          cached signed plugin catalogs
  presets/                      the preset library
  sample-providers/             sample provider plugins
  logs/                         app and diagnostic logs, in a selected profile
```

## Edit history

`<config>/history/<path key>/` holds the edit history of one project.

| File | Contents |
|---|---|
| `project.json` | Marker `{version: 1, path, sha256}`: the canonical project path and the file's content SHA-256 as the app last read or wrote it. |
| `entries.jsonl` | One stored entry per line, with its compressed forward and inverse patches, author, scope and an optional `cause` (`conversation_revert`; an ordinary edit omits it). Display text is not stored. The file only grows; a line a redo branch replaced stays as garbage until the history is rewritten. |
| `index.bin` | Fixed-size rows for the live linear history, oldest first. Dropping a redo branch truncates it. |
| `state.json` | The cursor and the document text at the cursor, written in one atomic replace, so an open can tell whether the file still matches. |

The history is matched by path first. When the path has no history, the file's
content SHA-256 is compared with the marker of every history: a history whose
project file is gone follows the file, which is how a file moved or copied
outside the app keeps its undo. The adoption is a copy when the old file still
exists and a move when the old file is gone. A project created with **New**
starts a fresh history and never adopts another file's.

**Move Project** moves the history to the new path. **Save As** copies it and
leaves the source history in place. Both keep the marker in step with the new
path.

The journal is synced once a second, after an agent tool mutation, and when the
app exits, so an edit made just before a quit keeps its undo entry. An open
agent batch stays provisional and reaches the journal only when it commits.

## Recovery copies

`<config>/recovery/<key>/` holds whole-document recovery copies. The key is the
path key for a saved project and the app session for a project that has never
been saved. A copy is named `<stamp>-<key>.ocp`, and the sibling `<key>.owner`
marker records the canonical path and content SHA-256.

Each project keeps the 10 newest copies. The app also reads copies written under
earlier keys, the file identity and the older 64-bit path hash, so copies from
an earlier build still restore. **Move Project** moves the copies to the new
key. See [Autosave and recovery](autosave.md).

## AI notes and conversations

`<config>/ai/projects/` holds one markdown note per project file.
`index.json` maps each key to the readable path, and `<key>.md` holds the note.
The key is the path key of the `.ocp` file, not of its folder. A moved project
keeps its note through **Move Project**; **Save As** copies the note and leaves
the source note in place.

`<config>/ai/sessions/` holds saved conversations. A conversation belongs to a
workspace directory, not to one `.ocp`, and lives under the workspace key:

- `workspace.json` records the directory and every path seen for that key, so a
  moved workspace can be re-associated.
- `<session id>/meta.json` records the title, times, provider, model, workspace
  and project path.
- `<session id>/messages.jsonl` is the append-only transcript.
- `<session id>/view.json` names the newest record that replaced the
  conversation, so a read parses only the current view. A read checks it and
  rebuilds it when it does not match the file.
- `<session id>/results/<sha256>.txt` holds a full tool result that is too large
  for its message row. The row keeps `result_sha` in place of the text. The
  sweep removes a result file no saved line of its session names, a fork copies
  the files its inherited rows name, and a read validates the hash, bounds the
  bytes it loads and refuses a file whose text does not hash to its name.
- `<session id>/versions/<sha256>.ocp` holds a content-addressed project version
  attached to a human turn.
- `detached/<session id>/` holds a conversation with no workspace, such as one
  for an unsaved project.
- `audition-media/` holds the bounded clip a later request replays, and
  `audition-renders/` holds the full-rate render kept for listening. One 256 MiB
  budget covers both directories.

Directories are `0700` and files are `0600` on Unix. A workspace holds at most
32 conversations. **Clear old conversation history**, in Preferences > Storage,
rewrites each conversation's transcript from its second-newest compaction and
then removes the assets only the dropped records named; the panel's `/clear`
command does the same for the open conversation. Both report the records
dropped and the space freed. See [Saved AI conversations](ai-sessions.md).

## Caches

These files are recomputable and safe to delete while the app is closed.

| Location | Contents |
|---|---|
| `<config>/cache/media/` | Decoded media, resampled buffers and derived clip audio, keyed by source path, modification time and size. With no profile selected, the platform cache directory holds the same files (`~/Library/Caches/OpenCompose/media` on macOS). |
| `<config>/cache/media/` | Vocal analyses (`.ocn`) and tune or harmony bakes, against a 1 GiB budget. |
| `<config>/cache/preset-trims/` | Measured loudness trims of presets. |
| `<config>/cache/audition-scratch/` | A render waiting to be handed to a conversation. With no profile selected, it sits beside the platform media cache. |

The Storage page reports the decoded and processed clip caches separately from
the vocal cache, and offers one **Clear** action for all three. The media cache
and the derived cache each have their own on-disk bound.

## Application folders

| Location | Contents |
|---|---|
| `<config>/runtimes/` | Installed helper runtimes, such as the stem separator. |
| `<config>/plugin-index/scopes/` | Cached signed plugin catalogs, one scope per signing authority and origin. `OCP_PLUGIN_INDEX_DIR` moves the root without changing trust. |
| `<config>/presets/` | The preset library, including patches saved from the patch editor. |
| `<config>/sample-providers/` | Installed sample provider plugins. `OPENCOMPOSE_SAMPLE_PLUGIN_DIR` overrides the directory. |
| `<config>/logs/` | The logs of a selected profile: `opencompose-app.log` from every run and the diagnostics logs of a run started with diagnostics flags. Without a profile, both use the platform log directory (`~/Library/Logs/OpenCompose` on macOS). The Storage page reports the same folder. |

A log directory keeps at most 20 diagnostics logs per program. A new run
removes the oldest beyond that bound, so diagnostics never grow without limit.
The app log starts over once it passes 40 KB.

## Stale data

The app removes data for projects and workspace folders that are gone, and only
for those. A file on a volume that is not mounted is out of reach, not gone, so
its data stays.

An item is **gone** when the file or folder it belongs to does not exist on a
reachable volume. A gone item is **due** when nothing has touched it for 30 days.
A sweep runs 60 seconds after start, off the UI and audio threads, and removes
every due item. The open project's data is never removed, and a conversation
with a running turn is never removed.

**Preferences > Storage** lists the project data of each project and folder: the
edit history, AI note, recovery copies and conversations, each with its size and
its gone or due state. Only gone items offer **Remove**, and the backend checks
that state again before it deletes. The page also lists the app folders above
with their sizes, and reports the clip PCM and vocal caches.
