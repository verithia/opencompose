# Drawing from an agent

The running GUI exposes a loopback HTTP API so an external agent can read and
edit the open project without screen automation. The built-in AI panel speaks to
the same tool catalog.

## Connecting

The GUI starts the server when it launches:

- Address: `http://127.0.0.1:38642`, or the first free port in the next 20.
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
| `GET /ai/tools` | The panel's surface, narrowed by `?toolsets=a,b`. |
| `GET /ai/toolsets` | The bundled and user tool sets. |
| `POST /ai/complete` | One provider turn on the panel's behalf, for a headless driver. |
| `POST /ai/turn` | One whole agent turn on the backend loop, with nobody to ask. |

A client starts a full turn with `POST /ai/turn`: it carries `provider`,
`messages` (the system prompt and the new user message), `session_id` and the
absolute `project_path`, with optional `toolsets` and `label`. Nobody attends, so
a `Confirm` call is refused and the turn continues, and a validated
`edit_subtree` applies in the turn. The answer is the turn outcome with `ok`.

`/call` answers `{"ok": true, "result": ...}` or
`{"ok": false, "error": "..."}`. A refusal one of the bundled skills explains
also carries `"skill": "<id>"`, and `read_skill` with that id returns the note;
`error` itself never changes. The MCP route answers normal JSON-RPC 2.0
(`initialize`, `tools/list`, `tools/call`) at protocol `2024-11-05`, with the
same hint as a second `text` block on a failed call.

The catalog endpoint reports the current tools. Reads, edits, transport,
render, and analysis share implementations with the AI panel, which exposes
a subset under its tool policy.

Use `read_skill` with `id: "production-curriculum"` to browse production fields.
Pass `topic: "Singeli"` or another genre, scene, field, or technique to read its
session exercise and listening check. A partial topic returns matching labels.

Use `audio_clips_list` to inspect audio clips and their project revision.
`audio_clip_set_playback` edits speed, pitch, formants, and warp anchors.
Pass `stretch_to` to scale the mapping and move its end together. Use
`audio_clip_edit_range` to split or trim without scaling the mapping. Read the `align-and-stretch-audio` skill for ratio examples
and audition the same range before and after the edit.

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

To hand the agent an exported arrangement, save an empty project and place
the aligned WAVs and optional MIDI in a `handoff` folder beside the project.
Ask the agent to import those files with `import_aligned_stems`. You can use
**File → Import Aligned Stems** to pick files from another folder.

Two behaviours are worth designing around:

- **`render_range` without `tracks`** is an exact slice of the full mix,
  including effect tails from before the window. It accepts either
  `start`/`end` in seconds or `start_bar`/`end_bar` (one-indexed), not both, and
  the window is limited to 600 s. Bars are resolved through the written tempo and
  meter maps, so use seconds when a section repeats.
- **`render_range` with `tracks`** is a *solo window* rendered from reset at the
  start, not a slice of the full mix. Use it to audition a stem group, not to
  compare against the full render. `stems: true` without `tracks` captures the
  stems from the same full mix, so master and stems line up.

`audio_analyze` returns sample peak, RMS, crest, integrated LUFS (or null when
undefined), true peak, ten octave bands, spectral peaks, stereo correlation and
mid/side ratio, silence regions and sounding sections. `audio_compare` returns
level and per-octave deltas, third-octave/MFCC/LSD shape terms, the max sample
difference on the aligned overlap, correlation and an alignment lag, a
banded-DTW summary, a YIN-style pitch diagnostic, and separate onset-strength
and events-per-second terms; by default it refuses two files with different
sample rates or lengths unless `allow_mismatch: true`. The absolute score's
`value` is the v1.3.0 formula; `previous_value` reproduces the v1.2.0 number.

## The transaction model

Edits are guarded so a multi-step change is atomic to the user.

- `transaction_begin` snapshots the project, DSL text, dirty flag, file path and
  undo mark, and returns the base revision.
- Inside an open batch, edits do not publish revisions individually. The first
  mutation marks the transaction and the revision stays at the base;
  `expected_revision` accepts that base for every call in the batch.
- `transaction_commit` publishes exactly one revision and squashes the batch into
  one undo entry labelled `Transaction: <label>`. One undo restores the
  pre-batch project and one redo re-applies the whole batch.
- `transaction_rollback` restores the snapshot byte-for-byte, drops the batch's
  undo and redo entries, and moves the revision forward so stale
  `expected_revision` guards fail.
- A failed mutation changes nothing and keeps the batch open: `dispatch` returns
  the error as the call's result, and the edits before it stay in the batch.
  Only an explicit `transaction_rollback` discards a batch a client owns
  directly; a failed commit leaves the batch open for its owner.
- A panel or headless agent turn owns its batch: the loop commits it on every
  ending (answer, cancel, failure, timeout and round cap), so a stop keeps the
  edits that landed. `ai_batch_recover` commits a `panel:` batch a dead turn left
  open rather than rolling it back.
- Revert is `ai_revert_before {session_id, index}`, offered only from a person's
  own message. It restores the project snapshot that message was sent against as
  one undo entry (`Revert conversation`, only when the document differs) and
  cuts the conversation before the message; if the cut fails the document is put
  back. It is refused while a turn runs or a batch is open.
- While a batch is open, save, undo and redo are refused, so a crash cannot leave
  a half-applied edit on disk or rewind history under the batch.
  `project_open` and `project_new` cancel the batch because they replace the
  whole session.
- `transaction_begin` counts as a mutation for `expected_revision`.

Pass the revision you last saw as `expected_revision` on mutation tools; a
mismatch fails the call instead of overwriting newer work. The full contract is
in [Agent feedback loop](../agent-loop.md).

## The built-in AI panel

The **AI** sidebar tab is a chat client with the same tools, minus the ones that
would escape the panel.

- It advertises every catalog tool the panel policy enables, plus the
  panel-local `edit_subtree`. The enabled set follows the catalog.
- Excluded from the panel: plugin scan/load/install/sidecar-state and native
  plugin windows, content-library writes, `render_offline` and `render_stems`,
  host audio device configuration, device-level mute, tools that list filesystem
  paths, and host capability queries. `render_offline` and `render_stems` are
  excluded because an unbounded full-song render is too slow for the panel; use
  `render_range` (with `stems` when the parts are wanted).
- `edit_subtree` replaces one complete subtree. It validates the replacement
  first, then the panel's permission decides: `require_approval` shows the change
  as a diff row and asks on the card, `autoreview` records the diff row and
  applies, and `write` applies. An applied replacement is one undo entry inside
  the turn's batch. A headless turn with nobody to ask applies a validated
  replacement itself in the same turn (see
  [Agent feedback loop](../agent-loop.md)).
- A turn is limited to sixty-four model rounds and thirty-two tool calls per
  round; a turn that reaches the round cap commits the edits it applied.
- Provider settings require HTTPS, except on loopback. Keys are stored in the OS
  credential store, not in the project or in YAML; non-secret settings live in
  `~/.opencompose/ai-providers.yaml`.
- The panel sends the selected project context and recent conversation to your
  provider. For a model configured to accept audio, the request can also include
  audio you attach and rendered excerpts returned by `audition_range`. See
  [Agent feedback loop](../agent-loop.md) for audio limits and delivery.
  The panel does not grant the model a shell, arbitrary file access or
  unrestricted network access.

Tool contracts and the provider policy are written up in
[AI and plugins](../ai-and-plugins.md).
