# Agent surface

A running OpenCompose exposes one HTTP server on the loopback interface so a
script, an editor or a model can read and edit the open project without screen
automation. The built-in AI panel speaks to the same catalog. This page covers
how the three surfaces relate; the user-facing walkthrough is
[Drawing from an agent](manual/agents.md) and the design contract is
[Agent feedback loop](agent-loop.md).

## One server, three surfaces

| Surface | Route | What it is |
|---|---|---|
| MCP | `POST /mcp` | JSON-RPC 2.0 at protocol `2024-11-05`: `initialize`, `tools/list`, `tools/call`. |
| Direct call | `POST /call` | One tool per request: `{"tool": "...", "args": { ... }}`. |
| Panel | `GET /ai/tools`, `GET /ai/toolsets`, `POST /ai/complete` | What the built-in AI panel is offered, its tool sets, and one provider turn on its behalf. |

`GET /health` answers without a token. `GET /tools` returns the full catalog.

All three read `tool_registry::catalog`. Each `ToolSpec` combines its description
and JSON schema with a typed `ToolId`. The identity table owns the wire name,
area, panel visibility, mutation flag and approval policy. Backend dispatch
matches `ToolId` exhaustively, while each provider receives the same advertised
schema. Bundled tool sets reference those identities; saved user tool sets keep
their existing wire names.

Reference and transport tools use typed request structs for both decoding and
schema generation. Their external decoder retains optional `null` values and
ignored extension fields for compatibility; the panel applies its advertised
object schema and rejects unknown fields. Requests without parameters retain
ignored external payloads, while their panel schema accepts only an empty object.
Decode failures keep a typed tool identity, error code and known field internally;
existing transport responses and reference results keep their wire shape.

```text
tool_registry::catalog  ──►  /tools, /mcp tools/list, /ai/tools
        │                              │
        ▼                              ▼
agent_api::dispatch  ◄──  POST /call, POST /mcp tools/call, panel calls
```

## The catalog

<!-- tool-registry: mcp=126 panel=98 -->

The catalog holds one entry per MCP tool. The built-in panel adds one
panel-local tool: its handler lives in the frontend, not in `dispatch`, because
it proposes a replacement for the user to accept, or applies it in the turn when
the session has nobody to ask (see [Agent feedback loop](agent-loop.md)).

Tool areas: project and arrangement, tracks, presets, effects and devices,
buses, transport, render, analysis, MIDI, plugins, content, undo and
transactions, skills, and system capability.

## What the panel may call

`ToolId` metadata decides that, independently of the transport. The panel advertises only enabled tools and adds `edit_subtree`. A refusal names
the reason the tool is excluded.

The panel excludes what would escape it: reading or writing an arbitrary file
path (`project_open`, `project_save`, `midi_import_preview`, `midi_import_apply`, `midi_export`,
`plugins_save_state`, `sampler_assets`), process-level plugin work (`plugins_scan`,
`plugins_load`, `plugin_pull`, `plugins_open_gui` and the editor calls),
host audio configuration (`audio_devices`, `audio_config`, `transport_mute`),
unbounded renders (`render_offline`, `render_stems`), whole-subtree replacement
(`project_edit`, which must go through the `edit_subtree` proposal), and host
capability queries (`system_capabilities`).

Use `render_range` instead of the unbounded renders; with `stems: true` it
covers the same ground for a window.

## Toolsets

A conversation can narrow the panel surface to named sets. `toolset` is a list
of tool names; the conversation offers the union of its selected sets, so
deselecting a set removes its tools from both the advertised list and the
request.

- The bundled sets partition the whole panel surface, so every panel tool stays
  reachable.
- `~/.opencompose/ai/toolsets.json` adds sets of your own and may not redefine a
  bundled id.
- A broken file never blocks the panel: the bundled sets are used and the
  problem is logged.
- A selection can only narrow the policy. An excluded tool stays excluded even
  if a set names it.

## Skills

A skill is a short reference note the model can read on demand. Eight are
compiled in; `~/.opencompose/ai/skills/<id>.md` adds your own and may not
redefine a bundled id. The index carries no bodies: `list_skills` returns id,
title, category, version and the tools a skill mentions, and `read_skill` returns
one note.

A refusal that one of the bundled skills explains carries `"skill": "<id>"`
beside the error, so a caller can read the note that says how to do it instead.
The error text itself never changes.

## Transactions and revisions

Every mutation accepts `expected_revision`. A mismatch fails the call instead of
overwriting newer work.

- `transaction_begin` snapshots the project, the DSL text, the dirty flag, the
  file path and the undo mark, and returns the base revision.
- Inside an open batch, mutations do not publish revisions individually;
  `expected_revision` accepts the base for every call in the batch.
- `transaction_commit` publishes one revision and squashes the batch into one
  undo entry.
- `transaction_rollback` restores the snapshot byte-for-byte, drops the batch's
  undo and redo entries, and moves the revision forward so stale guards fail.
- A failed mutation changes nothing and keeps the batch open: `dispatch` returns
  the error as the call's result, and the edits before it stay in the batch.
  Only a rollback, a cancel, a stop or a failed commit discard a batch.
- While a batch is open, save, undo and redo are refused.

The full contract, including what an agent may assume about a render that
finishes after an edit, is in [Agent feedback loop](agent-loop.md).

## Credentials

The server binds `127.0.0.1`, on port 38642 or the first free port in the next
twenty; `OPENCOMPOSE_AGENT_PORT` changes the base port. `~/.opencompose/agent.json`
holds `{ "port", "token", "pid", "version" }`. Send the token as
`X-OpenCompose-Token: <token>` or `Authorization: Bearer <token>`.

The credential is per process and is never written into a project or a log.

## The panel's provider path

`POST /ai/complete` runs one provider turn through the same encoder the panel
uses: body `{provider, messages, tools?, max_tokens?}`, answer the decoded wire
message with `"ok": true` beside it. A failed turn answers the status its class
implies and a body with `{"ok": false, "error": "<message>", "kind",
"retryable", "reauth"}`. The status is 400 only for a malformed request, 401 for
a rejected credential, 403 when the provider denies the model, 409 when the
saved provider or endpoint needs fixing, 429 for a rate limit, 502 when the reply
is unusable or truncated, and 503 when the provider times out or is unreachable.
`error` stays a plain string, so a caller that predates the class keeps working;
`retryable` says whether sending the same turn again can land, and `reauth` says
whether the operator has to sign in again. The endpoint never logs the request
or the credential. Keys live in the OS credential store, not in the project; see
[Drawing from an agent](manual/agents.md) for provider setup.

## Adding a tool

1. Add the identity and metadata in `tool_registry/identity.rs`.
2. Add its `ToolSpec` in `tool_registry::catalog` with the input schema.
3. Add the exhaustive `ToolId` handler in `agent_api::dispatch`.
4. Set panel visibility in the identity metadata, with a reason when it is
   excluded.
5. Add its `ToolId` to the bundled toolset that owns its area.
6. Describe it in [Drawing from an agent](manual/agents.md) when it changes what
   a user can ask for. The site's `scripts/check-docs.sh` fails if a page names a
   tool the catalog does not have.
