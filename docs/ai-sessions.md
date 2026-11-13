# Saved AI conversations

The AI panel keeps its conversation on disk, so a multi-turn job survives an
application restart and a project switch. This chapter is the reference for the
store, the commands that read it, and the compaction that keeps a long
conversation inside a model's context window.

## Where conversations live

```
~/.opencompose/ai/sessions/
  <workspace key>/workspace.json            the directory this key was derived from
  <workspace key>/<session id>/meta.json
  <workspace key>/<session id>/messages.jsonl
  detached/<session id>/…                   a conversation with no workspace
```

Directories are `0700` and files are `0600`, written through the same private
filesystem layer as the provider configuration (`private_fs`).

### Audition audio

A rendered listening clip is archived beside the conversations in two
content-addressed copies, so one render costs one file per copy however many
messages carry it and every session and fork shares the archive.

- `<store root>/audition-media/<sha256>.<ext>` holds **the clip the model
  listened to** — the bounded mono 16 kHz copy, at the duration the tool asked
  for. Its descriptor is what a later request inlines, which is why the model
  hears the same duration, channel count and rate it heard the first time, and
  why the inline limit applies to the clip rather than to a render.
- `<store root>/audition-renders/<sha256>.<ext>` holds the render the clip was
  cut from, for the operator to play back. It is recorded on the message as
  `source_render`, a path and not a descriptor: no request resolves it, so a
  full-rate stereo file that exceeds the inline bound still cannot fail a turn.

A render that cannot be archived is a warning, never a failed message: the
delivery still carries its clip. A clip whose render is gone is stored once.

`Preferences ▸ Storage ▸ Session audio format` picks the container:

| Setting | What is written |
|---|---|
| WAV | the render's own bytes, unchanged |
| MP3 ≤ 160 kbps | the clip's own rate (16 kHz fits MPEG-2 directly) |
| MP3 > 160 kbps | 44.1 kHz, because MPEG-2 stops at 160 kbps; the content stays 16 kHz-limited |

An encode the encoder cannot express falls back to WAV. The archive is capped at
**256 MiB across both directories**: `audition-media/` (the bounded clip a
request replays) and `audition-renders/` (the full-rate render kept for
listening). `drop_history` and `sessions_delete` collect the files no remaining
message names, in both directories.

A full archive does not refuse a turn. Before a new clip or render is written,
the archive evicts, oldest first, its unreferenced files; if that is not enough
it evicts the oldest render before the oldest clip, because a clip is what a
later request replays and a render is the operator's copy. A file the turn in
progress wrote is never a candidate. The turn reports what it removed through
the `ai_session` event (`evicted`: directory, file and bytes per entry), and the
panel adds one transcript line naming them.

An evicted render keeps its `source_render` path on the message, and a later
load marks that row `source_render_missing` instead of failing. `Preferences ▸
Storage` reports both directories with their sizes and the shared budget, and
**Clear audition audio** removes every archived clip and render after a
confirmation. Conversations keep the audio blocks they name; those files are
reported missing until a range is asked for again.

A conversation belongs to a **workspace**, which is the directory holding the
project files — not to one `.ocp`. Projects in one directory share one set of
conversations; opening a file in another directory shows another set. A
conversation's `meta.json` records the project file it last worked on, and that
record follows the file the panel has open: switching to another `.ocp` in the
same workspace continues the same conversation instead of starting a new one.

The **workspace key** is the first eight bytes of the SHA-256 of the canonical
directory path, as hex. It is stable, opaque, and does not leak the path.
`workspace.json` records the directory — and every directory path seen for that
key — so a moved workspace can be re-associated: when the hash of the new path
has no directory yet, the store follows the recorded path, and
`Store::reassociate(from, to)` moves the directory when the old path is known.

A conversation with no workspace (an unsaved project) lives under `detached/`. A
workspace holds at most **32** conversations; creating the 33rd deletes the least
recently updated one, directory included.

At startup, with no workspace open, the app shows a start surface instead of the
timeline: the recent directories (`~/.opencompose/workspaces.json`, newest
first), **Open folder…**, **Open project file…** and **Continue without a
workspace**. A workspace is a directory, so opening any `.ocp` in it adopts that
directory. The same chooser reopens from the File menu and from the AI panel's
scope line; switching it swaps the conversation list and leaves the other
workspace's saved conversations where they are.

The surface stays until a project is open. Choosing a folder lists its project
files and adopts it as the workspace; it does not open a file, so you pick one
from the list. A folder with no project files offers **New project**, which
creates `untitled.ocp` (or the next free `untitled N.ocp`) in that folder and
opens it. The action is only offered while nothing is open, because creating a
project replaces the open one.

A directory lists at most **200** project files; when it holds more, the section
label shows the count it kept and the count it found, as in `Projects in songs
(200/240)`. The extra files stay reachable through **Open project file…**.

The recents remember whether each directory still exists. A folder that was
deleted is still listed so it can be forgotten, and its row says `not found`;
the current workspace says so too when its directory is gone. Opening such a row
reports the failure in the chooser instead of pretending it worked.

Conversations follow the scope the command layer computes: a **chosen** folder
always scopes them, and without one the directory of the open project file does,
read live. Opening another `.ocp` from the File menu therefore moves the
conversation list at once, while opening a file outside a chosen workspace does
not.

## Project versions

Before a saved provider request is sent, the native session layer attaches a
content-addressed project version (`dsl_sha`) to each newly admitted human
message. This applies to the panel and explicitly bound headless requests.
The project path, source and summary facts are captured together; a headless
request for a different open project is refused.

Replayed human turns retain their original version. Tool results, assistant
replies and typed host deliveries do not acquire a human-turn snapshot. The
version file and its first transcript reference are written under the same
store lock, so collection cannot remove a checkpoint between those writes.
A snapshot that cannot be saved prevents the new turn from reaching the
provider. Version reads are bounded by the DSL document limit and verify the
content hash before returning text.

A fork copies the versions its retained messages reference, so deleting the
parent cannot remove the fork's project checkpoints. A missing or corrupt
referenced version refuses the fork and leaves no partial child session.

## Project instructions

A project can carry a short note — "always print stems at -14 LUFS", "this song is
in D minor, do not modulate" — that the panel writes into the system prompt of
every request for **that project**, and never into another project's request.
Write it in **AI → Settings → Project instructions**, count it against the
character count under the field, and save it there.

```
~/.opencompose/ai/projects/
  index.json                    key → the project path it was derived from
  <key>.md                      the note, exactly as written
```

The key is the first eight bytes of the SHA-256 of the **canonical project
path** — the `.ocp` file, not the directory holding it. A workspace can hold
several projects, so keying by directory would share one note between them; a
file key keeps them apart. `index.json` records the readable path for the next
key and for diagnosing one, and it is written with the note, never on a read. A
damaged index loses the paths, not the notes.

A moved project keeps its note. When the exact key has no note, the read falls
back to the one index entry that records the same file name at a path that is
gone; the note is served for the project that is open now, and the index is not
rewritten by a read. Two entries with that file name are ambiguous — a note is
never borrowed from one of several candidates, so the project opens with no
note until you save one for it.

Directories are `0700` and files `0600`, written through `private_fs`
(`write_atomic`). A note that is only whitespace clears the project's note: the
file and its index entry are removed, so the prompt is byte-identical to the one
a project with no note sends.

| limit | value | behaviour at the limit |
| --- | --- | --- |
| one note | 4,000 characters | the write is refused and the message names the length and the limit; the stored note is unchanged |
| the note's prompt share | 1,000 tokens (four characters per token) | counted against the system prompt budget, so the note can never push the prompt past its ceiling |

The panel refuses an over-limit note before it calls the store, and the store
refuses it again before it writes. Nothing is ever truncated to fit. The system
prompt carries the note once, under `## Project instructions`, between the base
prompt and the tool documentation; an empty note adds no section at all.

`ai_project_instructions_get` and `ai_project_instructions_set` read and write
the note. Both accept the project path and fall back to the open project; a
project that was never saved has no path, so reading returns an empty note and
writing is refused with a message asking you to save the project first. The
stored text is trimmed, and the write answer carries `max_chars`, so the panel
shows the store's limit rather than one of its own.

A project that moves to another path gets a new key, so it loses its note; the
index is what a later version can use to offer re-association, the way
`workspace.json` does for conversations. Deleting `~/.opencompose/ai/projects`
deletes every note.

## `messages.jsonl`

One record per model turn. A record is appended whole with `O_APPEND` plus
`sync_data`, so a turn is never half-written and the file is not rewritten per
turn:

```json
{"v":1,"kind":"turn","at":"2026-10-29T07:20:11.412Z","messages":[…]}
```

`kind` is one of:

| kind | meaning |
| --- | --- |
| `turn` | messages appended for one model turn (one provider request) |
| `compaction` | replaces the whole conversation with a summary |
| `trim` | replaces the whole conversation with the newest messages that fit the size cap |

Only `compaction` and `trim` rewrite the file, and each states what it dropped
(`dropped.from`, `dropped.to`, `tokens_before`, `tokens_after`). A line that
cannot be read is skipped and counted; it never hides the rest of the file.

A `user` message may carry a `media` array: one descriptor per attached file.
The descriptor is what the panel picked, never the bytes:

```json
{"kind":"image","name":"mix.png","path":"/Users/…/songs/mix.png",
 "mime":"image/png","bytes":2048,"duration_secs":null,"sha256":"9c1f…"}
```

| field | meaning |
| --- | --- |
| `kind` | `audio` or `image` |
| `path` | absolute path the picker wrote; a client may also send a relative one |
| `mime` | the type sent on the wire |
| `bytes` | file size when it was attached |
| `duration_secs` | audio duration; `null` for an image or an unreadable container |
| `sha256` | content hash when it was attached |

The shape is canonicalised on write: an entry that does not parse is dropped
rather than failing the turn. A saved file is re-read and re-hashed when the
request is built, so a file edited on disk is sent as it is now and reported as
changed. A record written before this field existed has no `media` and loads
unchanged.

The shape is canonicalised on **read** as well. An entry an older build wrote
without `kind` gets it back from the `mime` type (`audio/…` or `image/…`), so a
conversation saved before that field existed still replays its attachment
instead of failing every turn; an entry that cannot be a descriptor is dropped.

On load, an attachment whose `path` no longer names a file is marked
`"missing":true` in the returned messages. The panel paints it as **file is
missing**. The next request skips that block and inserts one text line naming
the path, so the rest of the message still goes out; nothing about the turn
fails because a reference file was moved or deleted.

A saved descriptor outlives the model that accepted it. When the conversation
switches to a model whose limits omit that input modality, every historical
attachment becomes one text line naming the file instead of a media block, so
the request still builds and the turn runs; the saved descriptor stays, and the
transcript keeps painting its chip. Only the turn's own composer attachment can
refuse the request, which keeps the reason next to the chip that removes it.

Limits, all enforced in the store:

| limit | value | behaviour at the limit |
| --- | --- | --- |
| conversations per workspace | 32 | the least recently updated is deleted |
| `messages.jsonl` | 1 GiB | the oldest messages are replaced by a `trim` record stating the range |
| one message | 400,000 characters | the text is clipped with `…` |
| saved conversation sent to a provider | 96 messages | the request goes out as the client built it instead |
| audition archive (both directories) | 256 MiB | the oldest unreferenced file is evicted; then the oldest render, then the oldest clip. A file the turn is writing is never evicted |

## `meta.json`

```json
{"id":"18f0c3a72b100004","title":"punchy kick","created":"…","updated":"…",
 "model":"gemini-4-argon","provider":"google","workspace_key":"3f2a…",
 "workspace":"/Users/…/songs","project_path":"/Users/…/songs/song.ocp",
 "turns":7,"tokens":9321,"compacted_at":"2026-10-29T07:41:02.019Z"}
```

`tokens` is this project's estimate of the saved conversation, not a provider's
reported usage; `compacted_at` records the last compaction; `parent` names the
session a fork copied.

## Autosave

There is no save action. Every model turn writes to the store:

1. The request is appended: a turn that ends in the user's message contributes
   that message, a tool round contributes the results it added. A request that
   the conversation already contains appends nothing, so a retried request does
   not duplicate history.
2. The assistant message the provider returned is appended, so an answer is
   saved even if the panel closes before the next turn.
3. When the estimate says the conversation no longer fits, it is compacted
   before the request is sent (see below).

Once a conversation exists for the open workspace, **the saved conversation is
what goes to the provider** — the client's request supplies the system prompt,
which the panel rebuilds every turn. The panel therefore sends the system prompt
and the pending user message only: every earlier turn, tool calls and tool
results included, is replayed from the store instead of being rebuilt from the
prose rows the transcript shows.

Inline audio clips are not saved. A turn that carries one goes out as the stored
conversation plus the client's own messages, so the model still hears the clip,
and the saved copy says the clip was omitted. Files the user attached from the
composer are different: their descriptors are part of the saved user message,
so a restored conversation replays the same attachment on every later turn.

A retried or continued turn is prepared from the saved conversation, so the
request is the conversation the store already holds plus its system prompt:
preparing it appends nothing, and `sessions_load` keeps one user message for the
turn. A stop that landed mid-round is closed first: every call the model asked
for gets a result, `outcome unknown` for a dispatched call whose result was lost
and `not run` for a call that never started, so the store never holds a tool call
without a result. See `docs/agent-loop.md`, section "Retry and continue after a stop".

The panel starts a conversation with the first message of a turn and uses that
message as the title, so the conversation it saves into is the one it shows.
`sessions_new` and `sessions_load` also mark a conversation as the target for the
open project, and the first turn of a fresh process starts a new one. An external
agent creates a named conversation with `sessions_new` before its first turn.
The `/ai/complete` endpoint saves into that conversation when every request
names its session and the open project file, as described in
[Headless agent host](agent-loop.md#headless-agent-host). Requests that omit
that binding remain ephemeral.

## The conversation switcher

The panel's header carries the open conversation's title, a **New conversation**
action, and a list of this workspace's conversations newest first with the
workspace directory on one truncated line. The list shows the recorded
`meta.updated` time of each conversation; the transcript never invents a time for
a row, because the saved wire has none.

Each row offers, for that conversation:

| action | command | behaviour |
| --- | --- | --- |
| select | `sessions_load` | paints the saved transcript and makes it the autosave target; the composer and its chips are cleared |
| Rename | `sessions_rename` | inline field; Enter or blur saves, Escape cancels, an empty title is refused |
| Fork | `sessions_new(parent:)` | copies the conversation and opens the copy; the parent is unchanged |
| Compact | `sessions_compact` | folds the conversation and adds one transcript line stating what it dropped |
| Delete | `sessions_delete` | asks first, naming the conversation; cancelling leaves it in place |

A completed answer in the transcript also offers **Fork from here** in its row
menu. It runs the same `sessions_new` with `through`, the number of saved
messages the copy keeps, so the child conversation ends with that answer while
the parent keeps the turns after it. Only a row the turn ends on offers it: a
user row, a tool row, and a call whose result came later offer none, because a
fork there would cut the turn in half. The store drops a trailing call that
nothing answers, and a count that names no saved message is refused rather than
read as a copy of the whole conversation.

A restored transcript is not prose-only. Each saved tool result is joined to the
call that produced it (by `tool_call_id`, or by call order on a wire without
native tool calling), so a tool row shows the tool's name, one line of its
arguments and one line of its result, with the whole result behind the summary.
The loop's own round nudge is not painted as a user message, because the user did
not write it. Consecutive tool rows between assistant messages form one step
group: it stays open while the turn works, then folds to its duration and tool
count. A folded running group still shows the current tool or the latest
thinking line; expanding it shows every tool row. A user message, an assistant
message, or a steer that sends the next message closes the group above it and
starts the next one. Opening a tool row shows a view chosen for the result: a track
table, a section timeline, a piano roll for transcribed notes, octave-band bars
and level cards for an analysis, a waveform and player for a render or audition,
a diff for an edit, or a status card for a refusal. Raw JSON stays one toggle
away. A result bounded for the model wire recovers its full payload through
`ai_tool_result` while the backend still keeps it; the backend keeps the newest
64 results, scoped to their conversations, and drops a result over 2,000,000
characters. Otherwise the view keeps the recovered counters and shows the
preview the wire carried.

The composer draft, its attachment chips and the messages queued behind a running
turn live in `src/stores/aiSession.ts`, outside the panel: closing the sidebar or
switching tabs unmounts the panel, and all three survive that. They are also
written per conversation to `localStorage`, so a reload of the window, which
discards the panel with everything in it, brings them back. An approval card
belongs to the running turn: a reloaded panel asks `ai_turn_questions` and
paints the waiting card again. A failed or stopped turn gives the draft and the
chips back, so the typing is not lost; a conversation switch clears them, because
it is a different conversation.
The same store remembers which conversation the panel had open for each
workspace and project file, so reopening the panel paints that song's last
conversation instead of the previous song's transcript or an empty one. The
cache keeps the last 64 workspace/project pairs; starting a new conversation
forgets that pair's entry.

## A turn in flight

A strip above the composer reports the turn while it runs: the phase, the
provider and model, the seconds since the turn started, and **Cancel request**.
The phase names what the panel is waiting for — the first token, the answer being
streamed, the tool that is running, the approval card — so a provider that has
not answered yet looks like work in progress rather than nothing happening. It
sits outside the transcript scroll area, so scrolling up cannot hide that a turn
is running.

The field is a textarea: Enter sends, Shift+Enter inserts a line break, a pasted
prompt keeps its line breaks, and the field grows to 160 px before it scrolls.
Send clears it, and a failure or a stop puts the same text and the same chips
back.

Pressing Stop moves the phase to `Stopping…`. If the provider still has not
answered the cancel after ten seconds the strip says so and keeps the control, so
the stop can be sent again; it never reports a turn as finished on the provider's
behalf. Starting a new conversation or switching workspace stops the turn that
was running and drops the rows it would have painted, so a late answer cannot
land in a conversation the user has left.

## A message while a turn runs

The composer stays usable while a turn runs. Enter or the send button appends
the composed message — with the attachment chips it was typed with — to the
queue above the composer, and clears the field either way: a send is never a
click that leaves the draft behind. Every message sent while a turn runs joins
the queue in the order it was typed, so a second send is queued rather than
ignored.

| action | behaviour |
| --- | --- |
| Edit | turns that row's text into a field; Enter or blur commits the edit, Escape closes it uncommitted. The edit is what gets sent |
| Remove | drops that queued message and the chips with it; nothing is sent |
| Steer | on the oldest row only, while a turn runs: stops that turn, then sends the message |
| Send now | on the oldest row when no turn runs: sends it immediately |

The hint under the rows says what the queue is waiting for: the running turn, the
approval card a turn is waiting on, or no turn at all, which is when **Send
now** appears instead of **Steer**. A queued message is therefore never stranded
by a panel that was closed mid-turn and came back with nothing running.

One message leaves the queue per settled turn: after an answer, after a stop,
after a failure, and after an approval card is answered. It is sent once, as an
ordinary turn, and the row disappears before the request goes out. A turn that is
still waiting on an approval card keeps the queue, because the composer is
paused with it.

The queue lives in `src/stores/aiSession.ts` next to the draft, so closing the
sidebar or switching tabs keeps it. Leaving the panel cancels the running turn
through the same honest cancel; the queued messages stay queued rather than
going out from a panel that is no longer on screen.

## Scoping one request to a path

Typing `@` in the composer opens a list of project paths: the whole project, then
its tracks, presets, patterns and buses. The text after the `@` filters that
list, so `@ba` finds `track:Bass`; the arrow keys move through it. Enter picks
the highlighted path, and Escape closes the list and removes the token. A draft
that starts with `/` keeps the command palette instead, because the two lists
never open together.

The chosen path stays above the composer as a chip until the message is sent,
and the chip's × drops it. The path covers one request: that request reads the
chosen path in place of the edit target from Settings, and the send clears the
chip. A message composed while a turn runs keeps its own path in the queue, so
the turn that starts later reads the path its message was composed against.

A reference narrows what the model reads; it does not move the edit target. A
tool that applies an edit still names its own `target_path`, so scoping a
request to `track:Bass` leaves the target in Settings alone.

A path the open project no longer offers — a track renamed or deleted after the
chip was set, or a queued message restored against another project — is dropped,
and that request reads the whole project.

## A window that reloads

Reloading the window discards the panel, and with it everything that lives in
`src/stores/aiSession.ts`. Three things are written to `localStorage` under
`opencompose.ai.panel.v1`, so that the next window starts where the last one
was:

| key | what it holds |
|---|---|
| `open` | the conversation the panel had open |
| `conversations[<id>]` | that conversation's composer text, its attachment chips and its queue, oldest first |
| `turn` | the turn that was running: its conversation and when it started |

Nothing secret is written. An attachment chip is the descriptor the picker
produced — path, MIME type, size, duration and content hash — never the file's
bytes, and a saved conversation carries the same descriptor for the files it
sent. A composer that has no conversation yet is kept under the empty id, so
typing is not lost merely because the first message was never sent.

An open approval card is not part of this: it belongs to the running turn, and
`ai_turn_questions` returns it on mount. The panel also calls `ai_batch_recover`
once on mount; it commits a `panel:` batch a dead turn left open, writes the
project file and returns the document, or answers null while a turn is running.
A batch an external client owns is left alone.

### The turn that was running

The agent loop runs in the backend, so the window can be reloaded under a turn
and the answer is still saved into the conversation. The reloaded window asks
`ai_active_turn` for the turn the backend still runs and follows its `ai_turn`
events again; when the turn ends it repaints the conversation from the saved one.

`ai_active_turn` reports the one request in flight — its id and the conversation
its reply will be saved into. `session_id` is absent until the backend has
resolved the conversation, so a window never claims a session it was not told.
The panel asks on mount, and the marker is kept only when the backend reports
the turn the marker names:

* **The backend still has it.** A banner says a turn was running when the window
  was reloaded and that the backend still has it. It offers **Stop the turn**,
  which is the same cancel as Stop. The window follows the request until it
  ends, repaints the conversation from the saved one, where the answer was
  written, and adds one line saying the inherited turn finished. An approval
  card the turn was waiting on is painted again from `ai_turn_questions`. The
  composer stays usable: a send while that turn runs joins the queue, because
  the backend still admits one turn at a time.
* **The backend has no such turn.** The marker is cleared, and a banner states
  that a turn was running when the window was reloaded and that nothing is
  running now. It offers **Reload conversation** — the stored answer may have
  arrived between the mount and the question — and **Dismiss**. No row is
  painted as running and no strip appears.
* **The backend has a turn for another conversation.** This conversation has
  none, so its marker is cleared exactly as above.

An answer the store saved can ask for tools whose results never arrived, such as
after the process died. When the conversation is next prepared, the backend
closes that round: one result per call, `outcome unknown` for a dispatched call
whose result was lost and `not run` for a call that never started. The
conversation stays sendable, and the model reads what happened instead of a call
no request may carry.

## Exporting a conversation

The session bar's **Export** action offers two documents, each of which can be
copied to the clipboard or downloaded under the conversation's title:
`<title>.md` and `<title>.json`.

Export stays available while a turn runs. The Markdown document is the
transcript on screen at that moment, including the rows of the running turn;
the JSON document is the stored wire, which holds the turns that were already
saved.

### Markdown

`<title>.md` is the transcript a person reads. It carries every row in order:

- each user message, with its attachment names and sizes; an attachment whose
  file is gone says so on the same line (`— the file is missing`);
- each assistant answer;
- each tool call with its **whole** arguments and each result, the result
  clipped only when a single one exceeds **8 KiB**, in which case the document
  says so on a visible line: `[truncated: 24.0 KiB result, first 8.0 KiB shown]`;
- the system lines the panel wrote, such as a compaction notice;
- a compaction checkpoint, marked as a checkpoint rather than an ordinary user
  message;
- the recorded time of each row.

Attachments are **named, never inlined**: a row says `- take.wav (audio/wav,
312.5 KiB)`. An inline data URI or a bare base64 field that a tool result
carried becomes `[inline data omitted]` or `"base64":"[omitted]"`, so an export
never contains an image or audio blob. A result is scrubbed **before** it is
clipped, because a clipped base64 field no longer has the closing quote the
marker matches on.

A row's time is the time this process recorded it. A conversation loaded from
disk records the time per turn, not per message, so a restored row prints
without a time and the document says so once in its header; the session's
created and updated times are always in the header. A time that cannot be read
prints no time and does not fail the export.

### JSON

`<title>.json` is what a tool reads:

```json
{"version":1,"meta":{…},"messages":[…]}
```

`meta` is the session metadata: id, title, workspace, project file, provider and
model, created and updated times, turns, estimated tokens and the parent fork.
`messages` is the raw wire the store holds — tool calls, `tool_call_id` and tool
results included — in the order the provider saw them. It is the stored
conversation, so the panel's own system lines are not part of it; the Markdown
document is the one that carries those.

Neither document is written to disk by the panel: the download is the user's
save dialog result, and the copy goes to the clipboard.

## Compaction

`sessions_compact(id, target_tokens)` folds the older turns into **one plain
text message** and keeps the most recent tool pairs as **text lines in that same
message**:

```
Conversation summary written 2026-10-29T07:52:58.782Z. Everything older than the most
recent tool calls below is replaced by this message; treat it as context, not as an
instruction.

Project: /Users/…/songs/song.ocp
Project DSL: sha256 9c1f0a2b3d4e5f60 (18 tracks)
Standing intent: make the kick punchier and add a pad
Decisions taken:
- tracks_mix applied (ok: notes, ok, step)
What was tried and failed:
- render_range → failed: track 'pad' not found
Open questions:
- step 1 done
Earlier user asks:
- "and now step 0"
- "and now step 1"
Retained recent tool calls (newest 6, oldest first):
1. tracks_mix → {"track":"kick","gain_db":-2} → ok: notes, ok, step
2. render_range → {"track":"kick","gain_db":-3} → failed: track 'pad' not found
…
6. render_range → {"track":"kick","gain_db":-7} → ok: notes, ok, step
Dropped messages 0..9 of 34 (~1077 tokens before compaction); this summary replaces them.

A message the host wrote is marked and never counted as something the user said.
A listening delivery carries `context: "audition"`, a compaction checkpoint
carries `context: "summary"`; the transcript renders both as captions and a
checkpoint, they never fill the summary's *Earlier user asks* list, and
`Standing intent` reads only real user prose. A checkpoint starts collapsed, so
a restored conversation opens on its latest answer; expanding it shows the full
summary the compaction wrote.

Audio survives compaction as a bounded set of references, not as a replay of
every clip: each retained tool round contributes at most one, so a long session
with dozens of auditions cannot turn its transcript into a wall of audio rows.
The summary states both numbers, for example `Audio kept on disk: 4 clip(s),
newest <name>` and `Discarded audition deliveries: 15 clips`. Older conversations
that already carry one row per audition still open; their rows render as
captions, and the next successful compaction bounds them.
```

The summary is built from the messages themselves, deterministically: no model
call, so compaction works with the panel closed and costs nothing. It names the
project's DSL **digest** rather than carrying the DSL, which would defeat the
purpose.

Compaction leaves nothing to replay: the output is one `user` message plus any
trailing plain-text messages, each carrying only `role` and `content`. There is
no assistant message with `tool_calls`, no `provider_state`, and no signature
field (`extra_content`, `thought_signature`, `signature`) — a provider that
rejects a replayed signature accepts a compacted conversation because there is
no assistant tool-call turn left to replay. The store refuses to write a
compaction that would violate this.

`retained_pairs` states how many tool calls the summary carries as text. If the
result is still above `target_tokens`, the transcript drops pairs from the
oldest end first; the standing intent, the failure list and the latest user
message always survive. When even the smallest summary would not reduce the
estimate, the store refuses rather than writing a longer conversation.

A declined replacement is ordinary history: the call and its refusal stay in the
conversation, and the summary records it in text (`edit_subtree declined for
track:kick`). No tool-call turn is replayed for a replacement that was never
applied.

### Estimating context usage

A hosted model exposes no tokenizer, so the panel never claims an exact count.
The estimate has three parts:

1. **A documented heuristic.** Four ASCII characters or one wide character
   (CJK, kana, hangul) per token, plus four tokens of per-message overhead. A
   tool call also counts its name and argument text. The same rule runs in the
   panel and in the Rust store, so the number under the composer and the number
   the compactor acts on never disagree about the method.
2. **An anchor on reported usage.** Every response carries the provider's own
   prompt size (`usage.input`, cached tokens included). That figure is exact for
   the request it answers, so it becomes the floor for the next reading: the
   estimate may grow past it as the conversation grows, but never falls below
   what the model actually saw.
3. **A calibration ratio.** The ratio between reported and estimated prompt
   tokens is folded into an exponential moving average (weight 0.3, clamped to
   0.6–1.8). Tokenizers differ: the same English prose can cost 2.5 to 5
   characters per token, and CJK text costs more than the wide-character rule
   assumes. One sample cannot move the average, and an absurd report is ignored.

The panel also counts the **system prompt and the tool schemas**, which ride on
every request and are the largest part of a small conversation. The store's
compaction estimate covers the saved conversation only, because the store is
reached without a request in hand; the 75 % / 15 % thresholds therefore describe
the conversation, and a request that still overflows is left to the provider's
own error.

The figure under the composer is labelled an estimate, and the pre-send guard
warns rather than refuses: only the provider can reject a request for size.
The info card beside it breaks the same estimate into the system prompt, tool
schemas, conversation, project text, draft and attachments, so a large request
names what is using the window. The card's **Compact at** field sets the
fraction at which **Compact now** appears (50-95 %, stored as a panel
preference) and runs the same `sessions_compact` command as the session menu.
The backend's automatic compaction still uses its own 75 % threshold; the
manual field only decides when the card offers the action.

### Automatic compaction

A turn is compacted before it is sent when **both** hold:

- the estimate of the system prompt plus the saved conversation exceeds **75 %**
  of the model's context window, and
- the conversation alone is above **15 %** of that window, so a large system
  prompt cannot trigger a compaction on every turn.

It also compacts when the conversation passes **80 messages**, because the wire
admits 100. The target is **50 %** of the context window.

The window is the one a request uses, in this order: the per-model override the
panel stores, the bundled catalog entry, the known-window list, and only then the
provider entry's own `context_length`. Catalog and known-window lookups try the
saved id first and the id without one leading `<vendor>/` segment or a trailing
`:suffix` second, so a saved `google/gemini-4-argon:free` reads the catalog's
`gemini-4-argon` window rather than the entry's guess. The panel shows that
number next to the provider default when the two differ.

Only a request that ends with the user's message is compacted: folding the
conversation in the middle of a tool round would take away the result the model
is about to answer from. A mid-turn overflow is left to the provider's own
limit, as before.

The panel is told: an auto-compaction emits `ai_session` with `compacted: true`,
the dropped range, the token counts before and after, the retained pair count and
the messages that were sent. The panel writes one line into the transcript.

## Commands and agent tools

| command | argument | result |
| --- | --- | --- |
| `sessions_list` | `workspace?`, `all?` | newest first: id, title, turns, tokens, model, provider, workspace, project file, updated |
| `sessions_new` | `project?`, `title?`, `parent?` | the new session; `parent` forks that conversation |
| `sessions_load` | `id` | metadata plus the wire messages to replay |
| `sessions_rename` | `id`, `title` | the updated metadata |
| `sessions_delete` | `id` | removes the directory |
| `sessions_compact` | `id`, `target_tokens?` | the summary, the dropped range and the token counts |

`sessions_list` covers the open workspace; `all` covers every workspace, and each
entry names its workspace so a client can group the answer. A `project` argument
accepts an `.ocp` path or the workspace directory itself.

The agent surface exposes the same six as tools, except `sessions_delete`:
deleting a saved conversation is the user's decision, so no model is offered it.
The panel's list does offer it, behind a confirmation that names the
conversation. No command here writes to the project.

## What a summary loses

- The exact text of the older turns. Decisions, failures, the standing intent,
  the last answer and the earlier user questions are condensed, not quoted.
- Tool arguments beyond the retained pairs, and every tool result beyond a
  short summary of it.
- `provider_state`, thought signatures and reasoning items. A conversation that
  needs them to continue on the same provider continues as a fresh context.
- Attachments and inline audio clips, which compaction drops rather than
  carrying into the summary.
- The message range itself: the summary states the range and the token counts,
  not the messages.
