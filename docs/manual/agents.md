# Drawing from an agent

While the GUI runs, it serves a loopback HTTP API. An external agent uses it to
read and edit the open project without screen automation. The built-in AI panel
uses the same tool catalog.

## Connecting

The GUI starts the server at launch:

- Address: `http://127.0.0.1:38642`, or the first free port among the next 20.
  `OPENCOMPOSE_AGENT_PORT` changes the base port.
- Credentials: `~/.opencompose/agent.json` holds
  `{ "port", "token", "pid", "version" }`. Send the token as
  `X-OpenCompose-Token: <token>` or `Authorization: Bearer <token>`.

| Route | Purpose |
|---|---|
| `GET /health` | Liveness and version; no token required. |
| `GET /tools` | The tool catalog as JSON. |
| `POST /call` | Call one tool: `{"tool": "...", "args": { ... }}`. |
| `POST /mcp` | JSON-RPC 2.0 MCP: `initialize`, `tools/list`, `tools/call`. |
| `GET /ai/tools` | The panel's tools, narrowed by `?toolsets=a,b`. |
| `GET /ai/toolsets` | The bundled and user tool sets. |
| `POST /ai/complete` | One provider turn on the panel's behalf, for a headless driver. |
| `POST /ai/turn` | One whole agent turn on the backend loop, with nobody to ask. |
| `POST /ai/steer` | Deliver a person's message into a running turn. |
| `POST /ai/cancel` | Stop a running turn: `{request_id, session_id}`. |

If another process holds the project you ask to open, the call answers 409 with
`kind` `project_in_use`, and your own document is not replaced. If another
process has a saved conversation open, the call answers 409 with `kind`
`session_in_use`. That rule covers `/ai/complete`, a `/call` envelope that names
a session, and `POST /ai/turn`.

To run a full turn, send `POST /ai/turn` with `provider`, `messages` (system
messages and at most one message from the person), `session_id` and the
absolute `project_path`. `toolsets`, `label` and `request_id` are optional. If
you name the turn with a `request_id`, you can steer and stop it while it runs.
Nobody is there to answer, so a `Confirm` call is refused and the turn carries
on, and a validated `edit_subtree` applies inside the turn. The response is the
turn outcome with `ok`.

`/call` answers `{"ok": true, "result": ...}` or
`{"ok": false, "error": "..."}`. When one of the bundled skills explains a
refusal, the answer also carries `"skill": "<id>"`, and `read_skill` with that
id returns the note. The `error` text is the same either way. The MCP route
answers standard JSON-RPC 2.0 (`initialize`, `tools/list`, `tools/call`) at
protocol `2024-11-05`, and a failed call carries the same hint as a second
`text` block.

The catalog endpoint reports the current tools. Reads, edits, transport, render
and analysis share their implementation with the AI panel, which offers a
subset under its tool policy.

To browse production fields, call `read_skill` with
`id: "production-curriculum"`. Add `topic: "Singeli"`, or another genre, scene,
field or technique, to read that topic's session exercise and listening check.
A partial topic returns the matching labels.

For audio clips, `audio_clips_list` returns the clips and the project revision.
`audio_clip_set_playback` edits speed, pitch, formants and warp anchors; pass
`stretch_to` to scale the mapping and move the clip end with it.
`audio_clip_edit_range` splits or trims without scaling the mapping. The
`align-and-stretch-audio` skill has ratio examples. Audition the same range
before and after the edit.

## What you can ask for

| Area | Examples |
|---|---|
| Project | read the document, list tracks/presets, inspect a subtree, validate a candidate edit, set the tempo and meter maps |
| Edit | change mix, add/remove effects, add/remove buses and routes, create, duplicate and rename tracks and presets |
| Transport | play, pause, stop, seek, read transport state |
| Render | render a window of the mix with optional per-track stems, or export the whole song's stems |
| Analysis | measure a WAV, compare two renders |
| MIDI | preview, apply and export MIDI |
| Handoff | import same-origin WAV stems and optional MIDI into a saved empty project |
| History | undo, redo, read the undo state, list the undo history |
| Registry | list devices and their parameters, search the plugin catalog |

To hand an exported arrangement to the agent:

1. Save an empty project.
2. Put the aligned WAVs, and the MIDI if you have it, in a `handoff` folder next
   to the project file.
3. Ask the agent to import them with `import_aligned_stems`.

To pick files from another folder yourself, use
**File → Import Aligned Stems…**.

`render_range` behaves differently with and without `tracks`:

- Without `tracks`, it returns an exact slice of the full mix, including effect
  tails that started before the window. Give either `start`/`end` in seconds or
  `start_bar`/`end_bar` (counted from one), never both. The window is at most
  600 s. Bars resolve through the written tempo and meter maps, so use seconds
  when a section repeats.
- With `tracks`, it renders a solo window from a reset at the window start. The
  result is not a slice of the full mix. Use it to audition a group of stems, and
  do not compare it against the full render. To get stems that line up with the
  master, pass `stems: true` without `tracks`; the stems then come from the same
  full-mix render.

`audio_analyze` returns sample peak, RMS, crest, integrated LUFS (null when it is
undefined), true peak, ten octave bands, spectral peaks, stereo correlation and
mid/side ratio, silent regions and sounding sections.

`audio_compare` returns level and per-octave deltas, third-octave, MFCC and LSD
shape terms, the largest sample difference over the aligned overlap, correlation
and an alignment lag, a banded-DTW summary, a YIN-style pitch diagnostic, and
separate onset-strength and events-per-second terms. By default it refuses two
files whose sample rates or lengths differ; pass `allow_mismatch: true` to
compare them anyway. The absolute score's `value` uses the v1.3.0 formula, and
`previous_value` reproduces the v1.2.0 number.

## The transaction model

A batch of edits reaches the project as one change, or not at all.

- `transaction_begin` snapshots the project, DSL text, dirty flag, file path and
  undo mark, and returns the base revision.
- Inside an open batch, edits do not publish their own revisions. The first
  mutation marks the transaction and the revision stays at the base, and
  `expected_revision` accepts that base for every call in the batch.
- `transaction_commit` publishes exactly one revision. It squashes the batch into
  one undo entry that records the typed changes between the batch base and its
  final state. One undo restores the project as it was before the batch, and one
  redo applies the whole batch again.
- `transaction_rollback` restores the snapshot byte for byte, drops the batch's
  undo and redo entries, and moves the revision forward so that stale
  `expected_revision` guards fail.
- A failed mutation changes nothing and leaves the batch open. `dispatch`
  returns the error as the result of the call, and the earlier edits stay in the
  batch. A batch that a client owns directly is discarded only by an explicit
  `transaction_rollback`. A failed commit leaves the batch open for its owner.
- A panel turn or a headless agent turn owns its batch. The loop commits it
  however the turn ends (answer, cancel, failure, timeout or round cap), so the
  edits that landed before a stop are kept. A batch left open by a turn that no
  longer exists (`panel:` or `turn:`) is committed and never rolled back: the
  next turn commits it, and so does `ai_batch_recover`.
- Recall is `ai_recall {session_id, index, undo_edits, confirmed, token?}`. It is
  offered only on a message the person sent. It cuts the conversation before
  that message and returns the message's text and attachments.
  - With `undo_edits`, it also moves the project history cursor back to the
    position the message recorded. This is one history jump, and a redo reverses
    it.
  - If edits by anyone other than this conversation's agent would be undone, the
    first call names a token, and a second call that echoes the token confirms.
  - If the cut fails, the cursor, the document and its history are restored
    exactly.
  - Recall is refused while a turn runs or a batch is open. `undo_edits` is
    refused for a message sent against another project file, or one with no
    recorded position.
- While a batch is open, save, undo and redo are refused. A crash therefore
  cannot leave a half-applied edit on disk or rewind history under the batch.
  `project_open` and `project_new` cancel the batch, because they replace the
  whole session. `project_new` takes an absolute `.ocp` path and an optional
  `starter` template, writes the file, and refuses a path that already exists.
- `transaction_begin` counts as a mutation for `expected_revision`.

On mutation tools, pass the revision you last saw as `expected_revision`. If it
does not match, the call fails and newer work is not overwritten. The full
contract is in [Agent feedback loop](../agent-loop.md).

## The built-in AI panel

The **AI** sidebar tab is a chat client with the same tools, minus the ones that
reach outside the panel.

- It offers every catalog tool the panel policy enables, plus three panel-local
  tools: `edit_subtree`, `plan_update` and `ask_user`. The enabled set follows
  the catalog.
- The panel does not offer: plugin scan and load, native plugin windows, writing
  plugin state to a file, SoundFont install and removal, content-library writes,
  `render_offline` and `render_stems`, host audio device configuration,
  device-level mute, tools that list filesystem paths, and host capability
  queries. `render_offline` and `render_stems` render the whole song with no
  bound, which is too slow for the panel; use `render_range`, with `stems` when
  you want the parts. The panel can install a plugin with `plugin_pull`, but
  only a native plugin archive (VST3, VST2, CLAP or LV2) or a managed separator
  runtime that the signed index pins by checksum and size.
- `edit_subtree` replaces one complete subtree. It validates the replacement
  first, and then the conversation's permission mode decides what happens.
  Under **Ask** (`require_approval`) the change appears as a diff row and the card
  asks you. Under **Review** (`autoreview`) the diff row is recorded and the change
  applies. Under **Auto** (`write`) the change applies. An applied replacement is
  one undo entry inside the turn's batch. A headless turn, with nobody to ask,
  applies a validated replacement itself in the same turn (see
  [Agent feedback loop](../agent-loop.md)).
- Type `/goal <text>` to set the goal of the conversation, and use the clear
  button on the goal chip to remove it. The goal and the plan are saved with the
  conversation. The model reads them every round, the composer shows the goal
  and a plan strip, and the agent updates them through `plan_update`. Rewinding
  to before a goal or plan change restores the earlier one, and a fork keeps the
  state it copied.
- `ask_user` asks you one question when the decision is yours, with at most six
  short options or a free-text answer. An answer must name one of the offered
  options. Free text is accepted only when the question offers it, and only up to
  4,000 characters. A headless turn has nobody to answer and continues with
  `answered: false`.
- Before it guesses a name, the agent searches the project with
  `project_search` (tracks, presets, patterns, buses, sections, markers, effects
  and phrases) and gets back addressable paths.
- A turn is limited to 64 model rounds and 32 tool calls per round. A turn that
  reaches the round cap commits the edits it applied. A budget row appears for
  the last four rounds, and when the context share reaches 60 % and 75 %.
- Provider settings require HTTPS, except on loopback. Keys are stored in the OS
  credential store, never in the project or in YAML. Settings that are not
  secret live in `~/.opencompose/ai-providers.yaml`.
- The panel sends the selected project context and the recent conversation to
  your provider. For a model configured to accept audio, the request can also
  carry audio you attach and rendered excerpts returned by `audition_range`.
  [Agent feedback loop](../agent-loop.md) describes the audio limits and how audio
  is delivered. The panel gives the model no shell, no arbitrary file access and
  no unrestricted network access.

Tool contracts and the provider policy are described in
[AI and plugins](../ai-and-plugins.md).
