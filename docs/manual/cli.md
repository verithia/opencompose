# Command line

`ocp` is the command-line face of OpenCompose. It parses, checks, formats,
exports, renders and converts `.ocp` projects, and it inspects the plugin and
device registries the engine actually uses.

```sh
cargo build --release -p oc-cli
./target/release/ocp --help
```

Every subcommand exits non-zero on failure and prints the reason on stderr.

## At a glance

| Command | Purpose |
|---|---|
| `parse` | Parse one file and report (or dump) its AST. |
| `validate` | Parse plus semantic validation; decodes audio-clip media. |
| `fmt` | Canonical formatting. |
| `export` | Export the project as JSON or a Standard MIDI File. |
| `render` | Offline mixdown to WAV, optionally stems. |
| `bounce` | Render one track's post-FX signal over a beat range. |
| `clip align` | Align take clips to a reference take and write offset/stretch. |
| `level` | Bake a track leveler's gain decision into its gain automation. |
| `play` | Stream a project through the audio device, with optional live MIDI. |
| `midi` | MIDI import, export, port listing and monitoring. |
| `midi2ocp` | Convert one SMF to `.ocp` (data-side converter). |
| `batch-midi2ocp` | Convert a directory of SMFs to `.ocp`. |
| `batch-data` | Convert a directory of SMFs to (`.ocp`, WAV) training pairs. |
| `generate-midi` | Generate synthetic MIDI files. |
| `devices list` | List built-in devices and their parameter schemas. |
| `validate-builtin-effects` | Build the mixer graph without rendering audio. |
| `resolve-plugin` | Resolve a plugin id/name/path with the renderer's registry. |
| `plugin-params` | Dump a VST3 or VST2 plugin's parameter inventory. |
| `plugin-preset import` | Import a `.ffp` preset into a VST3 plugin instance. |
| `plugin conformance` | Run the VST3 conformance battery over every installed bundle. |
| `compile-project` | Compile a complete project AST JSON to canonical OCP. |
| `plugin-index` | Build, verify or key-sign the remote plugin index. |
| `sample` | Search a sample catalogue and pull one sample. |

## Inspect and check

```sh
ocp parse examples/opensynth_showcase.ocp
ocp parse --json examples/demo_ballad.ocp > /tmp/ballad.json
ocp validate examples/demo_full_featured.ocp
ocp fmt examples/demo_ballad.ocp
ocp fmt -w song.ocp
```

`parse` prints a track-by-track summary, or the AST as JSON with `--json`.
`validate` adds name/reference checks, range checks, bus-cycle detection and
audio-clip decoding, and prints `Valid: "<name>" — N tracks`. `fmt` writes the
canonical form to stdout unless `-w` is given.

## Export

```sh
ocp export examples/demo_ballad.ocp --format json -o /tmp/ballad.json
ocp export examples/demo_ballad.ocp --format midi -o /tmp/ballad.mid
```

`--format` accepts `json` and `midi`. JSON prints the AST to stdout when `-o`
is omitted. MIDI writes a Standard MIDI File through the same writer as
`ocp midi export` (format 1, 480 PPQ, whole project) and requires `--output`.
There is no MusicXML option: the command rejects `--format musicxml` while
parsing its arguments instead of failing after startup.

`compile-project` is the inverse of an AST dump: it accepts a complete,
canonical project JSON (a file path or `-` for stdin), validates it, and prints
OCP only if the serializer round-trips it byte-for-byte in state.

```sh
ocp parse --json song.ocp > song.json
ocp compile-project song.json > song2.ocp
```

## Render

`render` (including `--stems`) and `bounce` accept DSL text or an existing
OCPC project snapshot, detected by its header. Input must be a regular file,
at most 128 MiB encoded and 256 MiB decoded. Malformed or oversized snapshots
are rejected before rendering. Relative media paths remain relative to the
input file's directory. This read-only support does not change the GUI's
text project save format or the input formats of other CLI commands.

```sh
ocp render examples/opensynth_showcase.ocp -o /tmp/showcase.wav --sample-rate 48000
ocp render song.ocp -o /tmp/song.wav --soundfont soundfonts/MS_Basic.sf2
ocp render song.ocp -o /tmp/mix.wav --stems /tmp/stems
ocp render song.ocp -o /tmp/song.wav --reruns 3 --require-identical
```

| Option | Meaning |
|---|---|
| `-o, --output PATH` | Output WAV (default `output.wav`). |
| `--sample-rate HZ` | Sample rate (default 44100). |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument; also read from `OCP_SOUNDFONT`. |
| `--per-track-mix` | Render tracks independently through the full mix graph. |
| `--output-gain-db DB` | Fixed non-positive output headroom for per-track rendering. |
| `--minimum-duration-seconds S` | Minimum export length (0–600 s); never trims events. |
| `--reruns N` | Render in N isolated plugin instances and compare. |
| `--require-identical` | Fail unless every rerun is byte-identical. |
| `--pre-roll S` | Silent wall-clock warm-up per plugin instance (0–600 s, default 4). |
| `--stems DIR` | Also write one pre-master WAV per track and bus plus `stems.json`. |
| `--no-loudness-normalisation` | Ignore `loudness:` on the master. |
| `--report` | Print peak, true peak, RMS, crest and integrated loudness of the rendered file. |

`--stems` performs one export pass, so it cannot be combined with `--reruns`,
`--require-identical`, `--per-track-mix` or `--output-gain-db`.

`--report` measures the file after it is written, so it also works with
`--stems` (it reports the mix) and with `--reruns` (it reports the kept run).

Containers, bit depths and the MP3 rate and bitrate tables are in
[Export format](../export-format.md).

Plugins that load asynchronously may render silence if a note starts before their
content is ready. `--pre-roll` waits in wall-clock time, not rendered silence;
`--reruns N --require-identical` proves reproducibility but not that a load
finished. See [Troubleshooting](troubleshooting.md).

## Bounce

```sh
ocp bounce song.ocp --track lead --from 9:1 --to 17:1 --output /tmp/lead.wav
```

`bounce` renders one track's post-insert, pre-fader signal over a beat range at
the project rate and writes a 32-bit float stereo WAV. The range is a slice of
the same timeline a full mix renders, so effect state and latency handling match
the mix; sends, buses and the master chain are excluded. `--from` and `--to`
take `bar:beat[:subdivision]`, the `--to` end is exclusive, and the file holds
exactly that span.

| Option | Meaning |
|---|---|
| `--track NAME` | Track whose signal is rendered (required). |
| `--from POS` | First beat of the range, as `bar:beat[:subdivision]`. |
| `--to POS` | End of the range (exclusive), same notation. |
| `-o, --output PATH` | Output WAV, 32-bit float at the project rate (required). |
| `--sample-rate HZ` | Rate to render at; defaults to the project's export rate. |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument; also read from `OCP_SOUNDFONT`. |

The file is written exactly as rendered: no dither, no loudness normalisation and
no clipping, so a hot track keeps its headroom. This is the alignment path the
GUI bounce uses, which makes it the way to check a bounce without the app. See
[Audio clips](../audio-clip.md) section 10 for the semantics and the FX-baking
rule.

## Comping

```sh
ocp clip align song.ocp lead-take take-a take-b
ocp clip align song.ocp lead-take take-a --output aligned.ocp
ocp clip align song.ocp lead-take take-a --dry-run
ocp clip align song.ocp lead-take take-a --invert
```

`clip align` analyses the named `@audio_clip` takes against a reference clip,
then writes a head trim to `offset:` and a tempo correction to `stretch:` in the
project. It reports the aligned correlation; `--invert` writes `invert: true`
when that correlation is below `-0.25` and clears the flag otherwise.
`--sample-rate` sets the analysis rate (default 48000); the written seconds and
ratios are rate-independent. All takes are analysed before anything is written,
so a failed take leaves the project untouched. The default target is the input
file; `--output` writes elsewhere and `--dry-run` only reports.

The result is an affine correction (one offset plus one constant rate), not a
non-linear warp. A ratio outside `0.25..=4.0` is refused, not clamped; tempo
corrections beyond ±10 % are accepted with a warning. Design, measured numbers
and the remaining limits: [Comping](../comping.md).

## Leveler bake

```sh
ocp level song.ocp --track lead --write
ocp level song.ocp --track lead --output baked.ocp
ocp level song.ocp --track lead --json
```

`level` renders one track dry, steps that track's leveler over the rendered
signal with the device's own detector law, and writes the decision into the
track's `gain_automation` lane. One point lands on each 10 ms control boundary;
points a linear gain interpolation reproduces within 0.05 dB are dropped. The
leveler is then removed from the track, so the project carries the decision
instead of a realtime rider.

| Option | Meaning |
|---|---|
| `--track NAME` | Track to bake; required. |
| `-w, --write` | Replace the input project. |
| `-o, --output FILE` | Write the baked project to `FILE`. |
| `--json` | Emit the bake report as JSON. |
| `--sample-rate HZ` | Rate of the dry pass (default 48000). |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument. |

Without `--write` or `--output` the command reports and writes nothing. The dry
pass runs the track's chain without the leveler, so effects before it are
included; the leveler must be the track's last effect, and the track must own a
clip. The bake replaces any gain clips already on that track, and a different
leveler setting needs a re-bake because the lane encodes one law. The device
itself is unchanged; see [Vocal compression](../vocal-comp.md) for the law and
the measured match.

## Play and MIDI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp
ocp play song.ocp --midi-in "IAC Driver Bus 1" --midi-out "IAC Driver Bus 1" --midi-clock --midi-thru
```

`midi list` prints input and output ports with indices; names or indices are
accepted as selectors. `midi monitor` prints timestamped events.
`play` renders the project offline and streams it through the default audio
device. A hosted plugin *instrument* track is not part of live play, only
built-in/OpenSynth and SoundFont tracks; see [Live MIDI](../midi-live.md).

```sh
ocp midi import song.mid --out song.ocp --json-summary
ocp midi import song.mid --out song.ocp --split-channels --keep-empty
ocp midi export song.ocp --out song.mid --format 1 --ppq 480 --tracks 0-2 --json-report
ocp midi2ocp song.mid -o song.ocp
ocp batch-midi2ocp midi/ -o ocp_out/ --flatten
ocp batch-data midi/ -o training_data/ --soundfont soundfonts/MS_Basic.sf2
ocp generate-midi -o data/midi_raw -c 100 --seed 42
```

`midi import` is the DAW-grade importer (mapping options, tempo/meter/key maps,
CC lanes, summary). `midi2ocp` is the older bulk converter used by the data
pipeline; it writes to stdout unless `-o` is given. `batch-data` requires a
SoundFont and renders WAVs as well as OCP.

## Devices and plugins

```sh
ocp devices list
ocp devices list --json
ocp validate-builtin-effects song.ocp --sample-rate 48000
ocp resolve-plugin "org.surge-synth-team.surge-xt"
ocp plugin-params "/Library/Audio/Plug-Ins/VST3/Example.vst3"
ocp plugin-params "/Library/Audio/Plug-Ins/VST/Example.vst"
ocp plugin-params "org.example.eq" --json
ocp plugin-preset import "org.example.eq" preset.ffp
ocp plugin conformance
ocp plugin conformance --plugin "/Library/Audio/Plug-Ins/VST3/Example.vst3" --format json
```

- `devices list` prints id, category, parameter count and display name from the
  same schema the engine and GUI use; `--json` is the fixed contract.
- `validate-builtin-effects` builds the audio graph (DSP preflight) without
  rendering; it skips external plugin effects, so a project may mix built-in and
  plugin devices. Legacy effect shorthand is expanded and reported as a warning
  (see [DSL compatibility](../dsl-compat.md)).
- `resolve-plugin` expects exactly one match across VST3, VST2, CLAP, LV2 and
  built-in and prints its metadata as JSON. AudioUnit lookups are not part of
  this command.
- `plugin-params` loads a VST3 or VST2 plugin and prints every parameter with
  id/name or index, unit/label, default and current value. VST2 values are
  normalized 0..1; pass the `.vst` path to pick the VST2 build when a VST3 with
  the same name is also installed.
- `plugin-preset import` reads a `.ffp` preset, applies its parameter values to the named
  VST3 plugin, and reports parsed, mapped and applied counts, plus unmapped,
  rejected and read-back-mismatched parameters. `--json` emits the report.
- `plugin conformance` runs one child process per VST3 bundle and prints the
  per-step matrix as a table, JSON or Markdown. See
  [VST3 conformance](../vst3-conformance.md) for the battery and the JSON
  schema.

## Plugin index

```sh
ocp plugin-index keygen --out /tmp/index-keys
ocp plugin-index build --catalog catalog.json --out dist/ --key /tmp/index-keys/plugin-index.key
ocp plugin-index verify dist/ --public-key /tmp/index-keys/plugin-index.pub
```

`build` writes `index.json`, its signature and the compressed catalog; `verify`
checks the signature and catalog hash; `keygen` creates an Ed25519 keypair.

## Samples

```sh
ocp sample search "snare drum" --kind drum --limit 5
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2 --json
ocp sample pull "commons:File:Snare drum unmuffled.ogg" --dest media/samples
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound
```

`search` lists each result with its kind, duration, channels, sample rate,
licence and credit line, and says why a result's licence is refused when it is.
`pull` resolves the id, downloads the file, verifies the declared size and
sha256, and writes the audio, a `.json` sidecar with the licence and credit
line, and a receipt. Wikimedia Commons is the default provider and needs no key;
Freesound needs an API key and serves a 128 kbps preview; the GUI's Samples
panel can connect Freesound through OAuth2 and pull the original instead. The
store layout, the licence rule and the tool names are in
[Samples](../samples.md).

## Range and analysis

The windowed render, audio analysis and A/B comparison tools (`render_range`,
`audio_analyze`, `audio_compare`) are **not** CLI subcommands. They live on the
agent surface of the running GUI; see [Drawing from an agent](agents.md).

There is no `ocp serve` command. The agent HTTP surface is hosted by the running
GUI, not by the CLI; the CLI has no server to start.
