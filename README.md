# OpenCompose

OpenCompose is a desktop DAW, a text format for music projects, and an audio
engine. The same `.ocp` project can be edited on the timeline, changed as text
or through the agent tools, and rendered from the command line.

Source and issue tracker are on [GitHub](https://github.com/verithia/opencompose).

- **Arrange and edit** with the timeline, piano roll, pattern clips, automation,
  audio waveforms, and clip fade handles.
- **Design sounds** with OpenSynth, the sampler, drum kits, and SoundFonts. Host
  CLAP, VST3, VST2, AudioUnit, and LV2 plugins where supported.
- **Mix and render** with track and bus meters, sends, built-in effects,
  sidechain routing, and offline mix and stem export.
- **Work with an assistant** through the built-in AI panel or the local MCP
  surface. The panel can propose project edits for review.

## Quick start

Install Rust 1.95.0, Node.js LTS with npm, and your platform's Tauri build
dependencies. See [Getting started](docs/manual/getting-started.md) for details.

Run the desktop app from source:

```sh
cd crates/oc-gui/frontend
npm ci
npm run tauri dev -- --release
```

Choose **File → Open** and select
[`examples/opensynth_showcase.ocp`](examples/opensynth_showcase.ocp). This
project uses only built-in instruments. Press **Space** to play.

From the repository root, build the command-line tool and render the same
project:

```sh
cargo build --release -p oc-cli
./target/release/ocp validate examples/opensynth_showcase.ocp
./target/release/ocp render examples/opensynth_showcase.ocp -o showcase.wav
```

On Windows, run `target\release\ocp.exe`. For a standalone CLI render that
uses a SoundFont, pass the bank with `--soundfont <path>`.

## Documentation

- [User manual](docs/manual/README.md) for desktop workflows, shortcuts, and CLI commands.
- [The OCP format](docs/manual/dsl.md) for project syntax and examples.
- [Plugins](docs/manual/plugins.md) for host compatibility and the plugin catalog.
- [AI and plugins](docs/ai-and-plugins.md) for provider setup and instrument presets.
- [Component reference](docs/components/index.md) for the Rust workspace.
- [Contributing](docs/contributing.md) and [Release packaging](docs/release.md) for development and distribution.

## License

OpenCompose is licensed under the
[GNU Affero General Public License v3.0 or later](LICENSE) (AGPL-3.0-or-later).
If you distribute a modified version, or let others use it over a network, you
must publish its complete source code under the same license.

The language components are licensed under either [Apache 2.0](LICENSE-APACHE)
or [MIT](LICENSE-MIT), at your option, so editors and other tools can read and
write `.ocp` projects without adopting the AGPL:

- `crates/oc-dsl` — the `.ocp` grammar, parser, formatter and serializer
- `crates/oc-device-schema` — the device and synth parameter schema the parser validates against
- `crates/oc-lsp` — the language server
- `crates/oc-test-support` — test instrumentation the language crates use
- `editors/vscode-ocp` — the VS Code extension

Each crate's `Cargo.toml` states its license. Third-party dependencies and
bundled assets keep their own licenses; their notices are collected in
[`THIRD_PARTY_LICENSES.txt`](crates/oc-gui/frontend/src-tauri/resources/THIRD_PARTY_LICENSES.txt),
which the app shows on the **Help & About** page.

**Your music is yours.** Projects, audio, MIDI and other works you create with
OpenCompose belong to you. The license covers the software, not what you make
with it, and places no restriction on commercial use of your work.

"OpenCompose" and the OpenCompose logo identify this project. The software
licenses do not grant permission to use them; a modified version must use a
different name.
