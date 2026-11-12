# oc-cli

The `ocp` binary: parsing, validation, formatting, export, rendering, MIDI
conversion and the plugin and device registries.

## What it owns

The command surface described in [Command line](../manual/cli.md). The crate is
both a binary and a thin library, so integration tests can drive the real build
path instead of a copy of it:

| Module | Owns |
|---|---|
| `main` | The clap command tree and one function per subcommand. |
| `midi` | The `midi` subcommands: import, export, port listing, monitoring. |
| `play` | Streaming playback through the audio device, with optional live MIDI. |
| `plugin_index` | Building, verifying and signing the remote plugin index: catalog parsing, archive download, manifest and signature. |

`render`, `validate`, `fmt`, `export` and `midi2ocp` are command-line wrappers
over `oc-dsl` and `oc-engine`. The CLI decides arguments and output paths; it
does not decide audio behaviour.

## Key types

| Type | Role |
|---|---|
| `Commands` | The clap enum: one variant per subcommand. |
| [`Catalog`](../api/oc_cli/plugin_index/struct.Catalog.html), `Entry`, `Manifest`, `SignatureInfo` | The signed plugin index format. |
| `BuildOptions`, `InstallKind` | How an index is built and how an entry is installed. |
| `PlayOptions` | The playback command's resolved settings. |

## Depends on

Every runtime crate: `oc-dsl`, `oc-engine`, `oc-synth`, `oc-host`, `oc-dsp`,
`oc-data`, `oc-diag` and `oc-build-parity`.

## Rules

- The CLI is a wrapper. If a subcommand needs behaviour the engine does not
  have, add it to the engine (or the crate that owns it) and call it from here.
- Exit codes are part of the contract: `0` success, non-zero with a reason on
  stderr for every failure. Tests assert on both.
- A flag that changes rendered output must say so in `--help` and be covered by
  the render tests; `--no-loudness-normalisation` and `--resample` are the
  examples the render gate depends on.
- Do not print absolute workspace paths in normal output. The commands are used
  from scripts.
- Keep `--json` output stable; the agent surface and the tests both parse it.

## API

[`api/oc_cli/`](../api/oc_cli/index.html).
