# Getting started

This chapter builds OpenCompose from the repository and plays the bundled sample
project. Nothing here needs a signed installer.

## What you need

| Requirement | Notes |
|---|---|
| Rust | 1.95.0, the supported build baseline (edition 2024). |
| Node.js LTS + npm | The GUI frontend is SolidJS + Vite. |
| Tauri build dependencies | Use your platform’s Tauri build dependencies. On macOS, install the Xcode Command Line Tools. |
| SoundFont bank (optional) | The GUI bundle carries the MIT-licensed `FluidR3Mono_GM.sf3`, so GM presets play without a download. For a CLI render with an external bank, pass `--soundfont <path>`. Native synth and plugin projects need no bank. |

Rust 1.95.0 is the locally tested build baseline. CI is configured to check this
compiler on macOS, Linux, and Windows. This is a supported compiler version,
not a claim about the oldest compiler that could build the project.

The native CI jobs are also configured to check the GUI and CLI without MP3
export, the engine with plugin hosting and MP3 enabled separately, and the
local sample-provider runner. These are compilation checks; they do not establish
native runtime coverage for every platform, architecture, or optional feature
combination.

SoundFont presets need no system library: the engine's own reader and player
render them.

## Build and run the GUI

From the repository root, install the frontend dependencies and use the Tauri
development command. Tauri starts Vite and waits for it before opening the app.

```sh
cd crates/oc-gui/frontend
npm ci
npm run tauri dev -- --release
```

Build the CLI from the repository root in a separate terminal.

```sh
# CLI
cargo build --release -p oc-cli
./target/release/ocp --help
```

The binary is `ocp`. A plain `cargo build --release` also builds the language
server, `ocp-lsp`, used by the VS Code extension.

## The sample project

`examples/opensynth_showcase.ocp` is the best first project: three OpenSynth
patches, no SoundFont and no plugins, about a hundred lines.

```sh
./target/release/ocp validate examples/opensynth_showcase.ocp
./target/release/ocp render examples/opensynth_showcase.ocp \
    --output /tmp/showcase.wav --sample-rate 48000
```

`examples/dynamic_eq.ocp` is the second sample: one held tone and a six-band
dynamic EQ on the master, in 25 lines.

```sh
./target/release/ocp render examples/dynamic_eq.ocp \
    --output /tmp/dynamic_eq.wav --sample-rate 48000
```

## The other commands

The four commands you need before anything else:

```sh
ocp validate song.ocp                    # parse plus semantic checks; prints a reason on failure
ocp fmt -w song.ocp                      # canonical formatting, in place
ocp export song.ocp --format midi -o song.mid
ocp midi2ocp input.mid -o song.ocp       # a MIDI file to a project
```

`ocp fmt` writes to stdout unless `-w` is given, so `ocp fmt song.ocp > /tmp/x`
shows the diff before you keep it. `ocp export --format json` prints the AST,
which is what the editor tooling reads. Every subcommand is in
[Command line](cli.md).

## Build the documentation site

```sh
scripts/docs.sh build    # the book into book/, the API reference into book/api/
scripts/docs.sh serve    # the book on http://127.0.0.1:3000
```

## First five things to try

1. **Check the document.** `ocp validate examples/opensynth_showcase.ocp` parses,
   validates and (for projects with audio clips) decodes every referenced media
   file. It prints the project name and track count.
2. **Render it.** The `ocp render` command above writes a 48 kHz WAV. Open it in
   any player; the whole mix, including master effects, is rendered offline.
3. **Open it in the GUI.** Choose **File → Open** and select
   `examples/opensynth_showcase.ocp`. Press **Space** to play from the cursor. Click the ruler to seek.
4. **Change one number.** Press **D** for the DSL sidebar, change `amp.release`
   in a preset, and press **Mod+Enter** to parse and apply. **Mod+Z** undoes it.
5. **Export stems.** `ocp render examples/opensynth_showcase.ocp -o /tmp/mix.wav
   --stems /tmp/stems` writes one pre-master WAV per track and bus plus a
   `stems.json` manifest.

Next: [the .ocp file](dsl.md), or [the GUI](gui.md) if you would rather click
first.
