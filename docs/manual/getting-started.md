# Getting started

In this tutorial you build OpenCompose from the repository and play the bundled
sample project. You do not need a signed installer.

## What you need

- Rust 1.95.0 (edition 2024). This is the supported build baseline.
- Node.js LTS with npm. The GUI frontend uses SolidJS and Vite.
- The Tauri build dependencies for your platform. On macOS, install the Xcode
  Command Line Tools.
- Optionally, a SoundFont bank. The GUI bundle carries the MIT-licensed
  `FluidR3Mono_GM.sf3`, so GM presets play without a download. To render with
  an external bank from the CLI, pass `--soundfont <path>`. Projects that use
  only the native synth and plugins need no bank.

Rust 1.95.0 is the locally tested baseline, and CI checks this compiler on
macOS, Linux and Windows. Whether an older compiler can build the project is
not tested.

The native CI jobs also compile the GUI and CLI without MP3 export, the engine
once with plugin hosting and once with MP3 export, and the local
sample-provider runner. These jobs only compile the code. They do not show that
every platform, architecture or combination of optional features works at run
time.

SoundFont presets need no system library. The engine has its own reader and
player for them.

## Build and run the GUI

From the repository root, install the frontend dependencies and run the Tauri
development command. Tauri starts Vite and waits for it before it opens the
app.

```sh
cd crates/oc-gui/frontend
npm ci
npm run tauri dev -- --release
```

In a separate terminal, build the CLI from the repository root.

```sh
# CLI
cargo build --release -p oc-cli
./target/release/ocp --help
```

The binary is `ocp`. A plain `cargo build --release` also builds the language
server `ocp-lsp`, which the VS Code extension uses.

## The sample project

Start with `examples/opensynth_showcase.ocp`. It is about a hundred lines long
and uses three OpenSynth patches, with no SoundFont and no plugins.

```sh
./target/release/ocp validate examples/opensynth_showcase.ocp
./target/release/ocp render examples/opensynth_showcase.ocp \
    --output /tmp/showcase.wav --sample-rate 48000
```

The second sample, `examples/dynamic_eq.ocp`, is 25 lines: one held tone and a
six-band dynamic EQ on the master.

```sh
./target/release/ocp render examples/dynamic_eq.ocp \
    --output /tmp/dynamic_eq.wav --sample-rate 48000
```

## The other commands

You will use these four commands from the start:

```sh
ocp validate song.ocp                    # parse plus semantic checks; prints a reason on failure
ocp fmt -w song.ocp                      # canonical formatting, in place
ocp export song.ocp --format midi -o song.mid
ocp midi2ocp input.mid -o song.ocp       # a MIDI file to a project
```

Without `-w`, `ocp fmt` writes to stdout. Run `ocp fmt song.ocp > /tmp/x` to
compare the result with your file before you keep it. `ocp parse --json` prints
the AST, and `ocp export --format json` prints the render-ready project with
notation such as hairpins expanded. [Command line](cli.md) lists every
subcommand.

## Build the documentation site

```sh
scripts/docs.sh build    # the book into book/, the API reference into book/api/
scripts/docs.sh serve    # the book on http://127.0.0.1:3000
```

## Five things to try first

1. Check the document. `ocp validate examples/opensynth_showcase.ocp` parses
   and validates the project. For a project with audio clips it also decodes
   every referenced media file. On success it prints the project name and the
   track count.
2. Render it. The `ocp render` command above writes a 48 kHz WAV that you can
   open in any player. The whole mix, master effects included, is rendered
   offline.
3. Open it in the GUI. Click **Open file** in the toolbar (or press **Mod+O**)
   and select `examples/opensynth_showcase.ocp`. Press **Space** to play from
   the cursor. Click the ruler to seek.
4. Change one number. Press **D** to open the DSL sidebar, change `amp.release`
   in a preset, and press **Mod+Enter** to parse and apply the text. **Mod+Z**
   undoes the change.
5. Export stems. `ocp render examples/opensynth_showcase.ocp -o /tmp/mix.wav
   --stems /tmp/stems` writes one pre-master WAV per track and bus, plus a
   `stems.json` manifest.

Next, read [The .ocp file](dsl.md). If you prefer to start in the app, read
[The window](gui.md).
