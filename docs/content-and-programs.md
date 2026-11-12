# Content libraries and plugin programs

Status: implemented. Everything on this page is reachable from the backend and
from the agent surface; the GUI is one front end among them, never a
prerequisite.

## 1. Content libraries

A content library is a root that contains sampled instruments. The catalogue is
one JSON file (`opencompose.content_library.v1`) at
`~/.opencompose/content-libraries.json`, or at the path a caller passes. A row
holds the root, a stable id derived from its canonical path, timestamps, and the
instrument files the last scan found.

```json
{
  "schema": "opencompose.content_library.v1",
  "libraries": [
    {
      "id": "lib-…",
      "name": "Strings",
      "path": "/content/strings",
      "kind": "directory",
      "added_at": 1761700000,
      "scanned_at": 1761700000,
      "missing": false,
      "instruments": [
        {"name": "violins", "path": "/content/strings/violins.sf2", "format": "sf2", "size_bytes": 4096}
      ]
    }
  ]
}
```

The scan recognises `.sf2` and `.sfz` files (recursively, depth ≤ 8, ≤ 20 000
entries) and sampler asset packs whose `manifest.json` names a
`opencompose.pgen.sampler_asset.v1` entrypoint. A root can also be a single
instrument file. The scan is deliberately local: it reads files, never a
vendor database or a network index.

### Commands

| Tauri command | Agent tool | Effect |
|---|---|---|
| `list_content_libraries` | `content_libraries` | Read the catalogue and the instruments per root. |
| `register_content_library` | `content_library_register` | Scan a root, add a row, write the catalogue. |
| `remove_content_library` | `content_library_remove` | Drop a row by id; files on disk are untouched. |
| `rescan_content_libraries` | `content_library_rescan` | Refresh one row or all; a vanished root is kept and marked `missing`. |

### Write safety

Every catalogue write follows the same sequence:

1. Serialise and parse the new body — a body that cannot be read back is never
   written.
2. Copy the previous file to `<catalogue>.bak`. If that copy fails, nothing is
   written.
3. Write atomically, then read the file back and compare it byte for byte.
4. On any failure, restore the previous bytes (or remove the partial file when
   there was no previous catalogue) and report that the previous catalogue was
   restored.

The backup path is returned in the command result. Registration of a root with
no recognisable instruments is refused before the first write, and a corrupt
catalogue is reported rather than replaced.

These commands are not project mutations: they change a user catalogue, not the
project, so they do not take `expected_revision` and do not enter the project
undo stack. Their undo is the backup/rollback above.

## 2. Plugin programs

A program is one patch inside a plugin. The command surface is:

| Tauri command | Agent tool | Effect |
|---|---|---|
| `plugin_program_capabilities` | `plugins_program_capabilities` | The capability table below, as data. Never loads a plugin. |
| `get_plugin_programs` | `plugins_programs` | The programs a preset's plugin exposes, plus the capability for its format. |
| `select_plugin_program` | `plugins_select_program` | Select one program and write its state into the project. |

`select_plugin_program` takes exactly one of `index`, `name` (both resolved
against the reported list) or `state_file` (the state-injection fallback). The
result names the mechanism that was used, so a caller never has to infer it from
an empty list.

State is written the same way `plugins_save_state` writes it: embedded as
`<format>_state_hex` by default, or as a verified sidecar with `mode: file`. The
write is one `PluginState` undo entry and honours the project revision guard.

### State as a value

| Tauri command | Agent tool | Effect |
|---|---|---|
| `read_plugin_state` | `plugins_read_state` | Capture the current state as `state_hex` plus `bytes`, `sha256`, `format`, `plugin` and `truncated`. Never changes the project. |
| `apply_plugin_state` | `plugins_apply_state` | Verify the envelope's format, plugin identity, version and SHA-256, then write it into the preset. |

The read is capped: `max_bytes` defaults to 256 KiB and may rise to 3 MiB (hex
of a larger blob does not fit one agent call). Over the cap the result sets
`truncated: true` and returns **no** `state_hex` — a partial blob would be
unusable and could corrupt a later render. A state larger than the ceiling goes
through `plugins_save_state mode=file` and `plugins_select_program state_file`.

The envelope is the versioned one from `crates/oc-host/src/plugin_state.rs`, so
the value is self-describing: format, plugin identity, serde version and the
payload's SHA-256. `plugins_apply_state` refuses a mismatched identity, a
format mismatch or a digest mismatch with the message from `decode_plugin_state`
before it touches the project. `read_plugin_state`/`plugins_read_state` is the
value round-trip; `plugins_save_state` stays the live-save-to-project/sidecar
path and is excluded from the panel because of the sidecar path.

### Capability table

This is the table the backend returns; it is code (`crates/oc-host/src/programs.rs`),
not prose that can drift.

| Format | Enumerate | Select | Notes |
|---|---|---|---|
| VST3 | native | native program data, state injection | The plugin's own program list names the programs; its program data supplies the chosen state. |
| CLAP | none | state injection | No instance program list; the caller supplies a captured state blob. |
| VST2 | none | state injection | Enumeration is not wired; a captured state blob is the path. |
| AU | none | state injection | Factory-preset enumeration is not wired; an `.aupreset`-shaped state blob is the path. |
| LV2 | none | state injection | Presets are bundles, not an instance list. |
| Internal | none | none | Built-in devices have no plugin program list. |

"Native" means the list is read from the plugin. "None" means the host has no
list to read; it is not a claim that the plugin has no patches.

## 3. What is backend-doable and what is not

Backend-doable, with no GUI:

- Register, remove and rescan a content library, with a backup and rollback.
- List a VST3 plugin's programs, select one by index or name, and write its
  state into the project so a render reproduces it.
- Capture the current state as a self-describing value and apply it back later.
- Inject a state blob as a program for any format that has no native list.
- Render the result; program state is applied by the normal render path.

Genuinely human-only, and why:

- **Supplying a program state for a format with no native list.** The host
  cannot invent a patch. A caller must first capture the blob (for example with
  its own preset manager) and pass it to `state_file`.
- **Authoring new sampled content.** Selecting an existing program is backend
  work; creating a new instrument or patch is an authoring task outside the
  program-selection contract.
- **Downloading or installing a third-party library.** That needs network
  access and a license decision; it is not part of registration.
- **Vendor-specific content databases.** The catalogue here is OpenCompose's own
  JSON file. A vendor application's private database is not read or written, so
  a library that is only discoverable through a vendor browser cannot be
  registered by scanning that browser's data.

Known limitations, recorded rather than left to be discovered:

1. **VST2, AU and CLAP program enumeration is absent.** Their instance APIs do
   not expose a portable program list in this host; adding one is per-format
   work with no CI plugin to verify it. The capability table says `none` so a
   caller does not read an empty list as "this plugin has no patches".
2. **VST3 plugins that implement no program list return an empty list.** That is
   an honest empty, distinguishable from the format capability by the
   `enumerate` field; selection still works through `state_file`.
3. **A temporary plugin instance is created when the preset is not loaded.**
   Enumeration has no side effects on the project, but a large instrument pays
   its load cost once per call. Load the preset first (`plugins_load`) to reuse
   the live instance.
4. **The catalogue scan is local and extension-based.** Instruments inside an
   archive, an unsupported container, or a directory deeper than the walk caps
   are not found; the rescan reports the root, not a guess.

## 4. Fixtures and tests

- `content_library.rs` unit tests create a temporary catalogue and a temporary
  library root with fake `.sf2`/`.sfz` files. They cover registration, duplicate
  refusal, an empty root, a missing root, removal, rescan, a vanished root, a
  corrupt catalogue, a failing writer (rollback), a failing backup, a sampler
  asset manifest, a single-file root and the explicit catalogue path. No
  installed library is required.
- `commands/tests/content_programs.rs` uses the in-process fake VST3. It lists
  the fake's two programs, selects the silent one and renders silence, selects
  the audible one and renders non-silence, writes a sidecar, and pins the
  validation and state-injection paths. It also covers the state value
  round-trip (read the audible state in one project, apply it to a project that
  never loaded the plugin, render non-silence), a cap that reports
  `truncated` with no fragment, a foreign identity and a tampered SHA-256 both
  refused before the project is touched, and an empty/malformed hex. It runs in
  ordinary `cargo test`.
- A real plugin path is exercised only with `OCP_TEST_PLUGINS=1`; the default
  suite never instantiates a commercial plugin.

## 5. Re-running

```sh
cargo test -j 4 -p oc-gui --lib content
cargo test -j 4 -p oc-gui --lib program
cargo test -j 4 -p oc-host programs::
```

The agent tools are visible in `GET /tools` and callable with `POST /call`; see
[the agent surface](agent-surface.md).
