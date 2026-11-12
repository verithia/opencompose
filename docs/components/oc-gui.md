# oc-gui

The desktop application: a Tauri 2 shell around the engine, with a SolidJS
frontend, a streaming playback path, the editor state and the agent server.

## What it owns

| Area | Modules | What it does |
|---|---|---|
| Transport and audio | `audio`, `render_thread`, `meters`, `live_synth` | Streams the mix through `cpal` with a lock-free ring buffer, keeps the playhead, meters and MIDI preview alive, and re-renders on a seek generation. |
| Editing | `state`, `commands`, `project_extent`, `project_resources` | The project, the undo stack, the patch system and the per-command handlers the frontend calls over IPC. |
| Rendering for the UI | `render_grid`, `audition`, `ab_audition`, `audition_figure` | Range and grid renders, audition windows, and the figures the panels draw. |
| Agent surface | `agent_api`, `tool_registry`, `ai`, `ai_service` | The loopback HTTP server, the shared tool catalog and the AI panel's provider client. See [Agent surface](../agent-surface.md). |
| Plugins | `plugin_store`, `plugin_keepalive`, `plugin_index`, `editor_helper`, `content_library` | Installed plugins, the editor bridge, the remote index and the content library. |
| Support | `diagnostics`, `private_fs`, `startup`, `window_guard`, `midi_bridge`, `midi_commands`, `midi_live`, `beat_grid` | Logs, credential storage, startup order, window placement, MIDI wiring and the beat grid. |

The frontend (`crates/oc-gui/frontend`) is SolidJS + Vite: timeline, piano roll,
mixer, device rack, patch editor, plugin browser, DSL editor and AI panel. It
holds no audio code; every audio decision is a command into this backend.

## Key types

| Type | Role |
|---|---|
| `PlaybackState` | `Playing`, `Paused`, `Stopped`; the transport's single source of truth. |
| `StreamingPlayback` | The render thread: block production, ring buffer and seek generation. |
| `UndoStack` | Project history, with patch coalescing. |
| `ToolDef`, `Panel` | One agent tool and its panel policy; the registry both surfaces read. |
| `SilenceReport`, `RenderGrid` (see `render_grid`) | What the grid draws for a range. |

## Depends on

`oc-dsl`, `oc-engine` (with `plugin-host`), `oc-host`, `oc-synth`, `oc-diag` and
`oc-build-parity`, plus Tauri, `cpal`, `rtrb` and `tokio`.

## Rules

- The GUI composes; it does not process audio. A DSP decision belongs in
  `oc-engine` or `oc-dsp`, and the command calls that.
- Every mutation goes through the undo stack, and every command that changes the
  project reports the new revision; the agent surface and the frontend both rely
  on it.
- The audio callback never allocates, locks or blocks; see
  [Playback latency](../playback-latency.md).
- A new command needs a `#[tauri::command]` handler, an entry in the frontend
  API layer and a test at the layer that owns the behaviour.
- Do not start a second HTTP server or a second render path. One agent server,
  one `RenderCache`.

## API

[`api/oc_gui_lib/`](../api/oc_gui_lib/index.html).
