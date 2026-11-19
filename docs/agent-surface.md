# Agent surface

A running OpenCompose serves one HTTP server on the loopback interface. A
script, an editor or a model uses it to read and edit the open project without
screen automation, and the built-in AI panel calls the same tool catalog. For a
walkthrough from the user's side, read [Drawing from an agent](manual/agents.md);
the design contract is in [Agent feedback loop](agent-loop.md).

## One server, three surfaces

| Surface | Route | What it is |
|---|---|---|
| MCP | `POST /mcp` | JSON-RPC 2.0 at protocol `2024-11-05`: `initialize`, `tools/list`, `tools/call`. |
| Direct call | `POST /call` | One tool per request: `{"tool": "...", "args": { ... }}`. |
| Panel | `GET /ai/tools`, `GET /ai/toolsets`, `POST /ai/complete` | The tools the built-in AI panel is offered, its tool sets, and one provider turn on its behalf. |

`GET /health` answers without a token. `GET /tools` returns the full catalog.

All three surfaces read `tool_registry::catalog`. Each `ToolSpec` pairs a
description and a JSON schema with a typed `ToolId`. The identity table owns
the wire name, the area, panel visibility, the mutation flag and the approval
policy. Backend dispatch matches `ToolId` exhaustively, and every provider
receives the same advertised schema. Bundled tool sets refer to these
identities; saved user tool sets keep their wire names.

Reference and transport tools use typed request structs both to decode a
request and to generate its schema. For compatibility, their external decoder
keeps optional `null` values and ignores extension fields. The panel applies its
advertised object schema and rejects unknown fields. A tool without parameters
ignores whatever payload an external caller sends, while its panel schema
accepts only an empty object. A decode failure keeps a typed tool identity, an
error code and the known field internally, and the existing transport responses
and reference results keep their wire shape.

```text
tool_registry::catalog  ──►  /tools, /mcp tools/list, /ai/tools
        │                              │
        ▼                              ▼
agent_api::dispatch  ◄──  POST /call, POST /mcp tools/call, panel calls
```

## The catalog

<!-- tool-registry: mcp=127 panel=99 -->

The catalog has one entry per MCP tool. The built-in panel adds three
panel-local tools whose handlers live in the agent loop and not in `dispatch`:

- `edit_subtree` proposes a subtree replacement for you to accept. In a turn
  with nobody to ask, it applies the replacement in that turn.
- `plan_update` records the conversation's goal and plan.
- `ask_user` asks you one question when the decision is yours. In a headless
  turn it answers at once that no user is available (see
  [Agent feedback loop](agent-loop.md)).

The tools are grouped by area: project and arrangement, tracks, presets,
effects, devices, buses, transport, render, analysis, MIDI, plugins and the
plugin index, SoundFonts, samples, content, sessions, undo, transactions,
skills, audio, and system capability.

`project_search` is the read-only name lookup on both surfaces. Before a caller
guesses the name of a track, preset, pattern, bus, section, marker, effect or
phrase, it searches with the words it knows. It gets back a bounded list of hits,
each with an addressable `path`; for sections and markers the `path` points
into `sections`.

## What the panel may call

`ToolId` metadata decides what the panel may call, whatever the transport. The
panel advertises only the enabled tools, plus `edit_subtree`, `plan_update` and
`ask_user`. When it refuses a tool, the refusal gives the reason the tool is
excluded.

The panel excludes these tools:

- Tools that read or write an arbitrary file path or list filesystem paths:
  `project_open`, `project_save`, `midi_import_preview`, `midi_import_apply`,
  `midi_export`, `plugins_save_state` and `sampler_assets`.
- `project_new`, because it replaces the current session.
- Process-level plugin work and native windows: `plugins_scan`, `plugins_load`,
  `plugins_open_gui`, `plugins_close_gui` and `plugins_has_gui`.
- Writes to the app's content catalogue and SoundFont folder:
  `content_library_register`, `content_library_remove`,
  `content_library_rescan`, `soundfont_install` and `soundfonts_remove`, along
  with the network refresh `plugin_index_refresh`.
- Host audio configuration and device-level mute: `audio_devices`,
  `audio_config`, `transport_mute` and `transport_unmute`.
- Unbounded renders: `render_offline` and `render_stems`. Use `render_range`
  instead; with `stems: true` it covers the same ground for a window.
- Whole-subtree replacement through `project_edit`, which must go through the
  `edit_subtree` proposal.
- `sessions_delete`, which removes a saved conversation.
- Host capability queries: `system_capabilities`.

## Toolsets

A conversation can narrow the panel's tools to named sets. A set is a list of
tool names. The conversation offers the union of the sets you select, so
deselecting a set removes its tools from both the advertised list and the
request.

The bundled sets partition the whole panel surface, so every panel tool can be
reached. `~/.opencompose/ai/toolsets.json` adds sets of your own; it may not
redefine a bundled id. If that file is broken, the panel still opens with the
bundled sets and logs the problem. A selection can only narrow the policy: an
excluded tool stays excluded even when a set names it.

## Skills

A skill is a short reference note the model reads when it needs it. The bundled
skills are compiled in, and `~/.opencompose/ai/skills/<id>.md` adds your own; a
user skill may not redefine a bundled id. The index carries no note bodies.
`list_skills` returns the id, title, category and version of each skill and the
tools it mentions, and `read_skill` returns one note.

When a refusal is one that a bundled skill explains, the answer carries
`"skill": "<id>"` beside the error, so the caller can read the note on how to do
the task. The error text stays the same.

## Transactions and revisions

Every mutation accepts `expected_revision`. If it does not match, the call fails
and newer work is left in place.

- `transaction_begin` snapshots the project, the DSL text, the dirty flag, the
  file path and the undo mark, and returns the base revision.
- Inside an open batch, mutations do not publish revisions one by one, and
  `expected_revision` accepts the base revision for every call in the batch.
- `transaction_commit` publishes one revision and squashes the batch into one
  undo entry.
- `transaction_rollback` restores the snapshot byte for byte, drops the batch's
  undo and redo entries, and moves the revision forward so stale guards fail.
- A failed mutation changes nothing and keeps the batch open. `dispatch`
  returns the error as the call's result, and the edits before it stay in the
  batch. A batch that a client owns directly is discarded only by an explicit
  `transaction_rollback`; a failed commit leaves the batch open for its owner.
  A panel or headless turn commits its own batch on every ending, including a
  cancel or a stop.
- While a batch is open, save, undo and redo are refused.

The full contract, including what an agent may assume about a render that
finishes after an edit, is in [Agent feedback loop](agent-loop.md).

## Credentials

The server binds `127.0.0.1` on port 38642, or on the first free port among the
next twenty. `OPENCOMPOSE_AGENT_PORT` changes the base port.
`~/.opencompose/agent.json` holds `{ "port", "token", "pid", "version" }`.
Send the token as `X-OpenCompose-Token: <token>` or
`Authorization: Bearer <token>`.

The credential belongs to one process and is never written into a project or a
log.

## The panel's provider path

`POST /ai/complete` runs one provider turn through the encoder the panel uses.
The body is `{provider, messages, tools?, max_tokens?}`, and a successful
answer is the decoded wire message with `"ok": true` beside it.

A failed turn answers with the HTTP status of its failure class and the body
`{"ok": false, "error": "<message>", "kind", "retryable", "reauth"}`:

| Status | Meaning |
|---|---|
| 400 | The request is malformed. |
| 401 | The provider rejected the credential. |
| 403 | The provider denies the model. |
| 409 | The saved provider or endpoint needs fixing. |
| 429 | Rate limit. |
| 502 | The reply is unusable or truncated. |
| 503 | The provider timed out or is unreachable. |

`error` stays a plain string, so a caller written before the failure classes
keeps working. `retryable` says whether sending the same turn again can
succeed, and `reauth` says whether the operator has to sign in again. The
endpoint never logs the request or the credential. Keys are kept in the OS
credential store and never in the project; provider setup is described in
[Drawing from an agent](manual/agents.md).

## Adding a tool

1. Add the identity and its metadata in `tool_registry/identity.rs`.
2. Add its `ToolSpec`, with the input schema, in `tool_registry::catalog`.
3. Add the `ToolId` arm in the exhaustive match of `agent_api::dispatch`.
4. Set panel visibility in the identity metadata. An excluded tool needs a
   reason.
5. Add the `ToolId` to the bundled toolset that owns its area.
6. If the tool changes what a user can ask for, describe it in
   [Drawing from an agent](manual/agents.md). `scripts/check-docs.sh` fails
   when a page names a tool the catalog does not have.
