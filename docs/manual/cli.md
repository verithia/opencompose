# Command line

`ocp` is the command-line tool for OpenCompose. It parses, checks, formats,
exports, renders and converts `.ocp` projects. It also inspects the plugin and
device registries that the engine uses.

```sh
cargo build --release -p oc-cli
./target/release/ocp --help
```

Every subcommand exits with a non-zero status on failure and prints the reason
on stderr. Two global flags work with any subcommand: `--debug` writes a
diagnostics log with debug detail, and `--perf` adds render and stage timings to
that log. `ocp` prints the log path on stderr.

## Subcommands

| Command | Purpose |
|---|---|
| `parse` | Parse one file and print a summary or its AST. |
| `validate` | Parse, run semantic validation and decode audio-clip media. |
| `fmt` | Print or write the canonical formatting. |
| `export` | Export the project as JSON or a Standard MIDI File. |
| `render` | Offline mixdown to an audio file, optionally with stems. |
| `bounce` | Render one track's post-FX signal over a beat range. |
| `clip align` | Align take clips to a reference take and write offset and stretch. |
| `vocal analyze`, `vocal apply` | Analyse one audio clip's pitch, or bake its stored note decisions. |
| `level` | Bake a track leveler's gain decision into its gain automation. |
| `randomize` | Write seeded variations of a project's OpenSynth patch. |
| `play` | Stream a project through the audio device, with optional live MIDI. |
| `midi` | MIDI import, export, port listing and monitoring. |
| `midi2ocp` | Convert one SMF to `.ocp` (data-side converter). |
| `batch-midi2ocp` | Convert a directory of SMFs to `.ocp`. |
| `batch-data` | Convert a directory of SMFs to (`.ocp`, WAV) training pairs. |
| `generate-midi` | Generate synthetic MIDI files. |
| `devices list` | List built-in devices and their parameter schemas. |
| `validate-builtin-effects` | Build the mixer graph without rendering audio. |
| `scan` | Scan installed plugin bundles; `--rescan` retries quarantined ones. |
| `resolve-plugin` | Resolve a plugin id, name or path with the renderer's registry. |
| `plugin-params` | Dump a VST3 or VST2 plugin's parameter inventory. |
| `plugin-preset import` | Import a `.ffp` preset into a VST3 plugin instance. |
| `plugin install` | Resolve, download, verify and install one catalog entry. |
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

`parse` prints a track-by-track summary. With `--json` it prints the AST as
JSON.

`validate` adds name and reference checks, range checks, bus-cycle detection
and audio-clip decoding, and refuses effect parameter values the renderer cannot
use. On success it prints `Valid: "<name>" — N tracks`.

`fmt` writes the canonical form to stdout. With `-w` it rewrites the file.

## Export

```sh
ocp export examples/demo_ballad.ocp --format json -o /tmp/ballad.json
ocp export examples/demo_ballad.ocp --format midi -o /tmp/ballad.mid
```

`--format` accepts `json` and `midi`.

- JSON is the render-ready state of the project, so a hairpin appears as the
  gain clip it expands to. Use `parse --json` when you want the source AST.
  Without `-o` the JSON goes to stdout.
- MIDI requires `--output`. It writes a Standard MIDI File through the same
  writer as `ocp midi export`: format 1, 480 PPQ, whole project.

There is no MusicXML option. The command rejects `--format musicxml` while it
parses its arguments, before any work starts.

`compile-project` goes the other way. It reads a complete, canonical project
JSON (a file path, or `-` for stdin), validates it, and prints OCP. It fails
unless the serializer reproduces exactly the same project state when the OCP is
parsed again.

```sh
ocp parse --json song.ocp > song.json
ocp compile-project song.json > song2.ocp
```

## Render

`render` (including `--stems`) and `bounce` accept DSL text or an OCPC project
snapshot; the header tells them apart. The input must be a regular file of at
most 128 MiB encoded and 256 MiB decoded. A malformed or oversized snapshot is
rejected before rendering starts. Relative media paths stay relative to the
input file's directory. Reading snapshots here does not change the GUI's text
save format or the input formats of other subcommands.

```sh
ocp render examples/opensynth_showcase.ocp -o /tmp/showcase.wav --sample-rate 48000
ocp render song.ocp -o /tmp/song.wav --soundfont soundfonts/MS_Basic.sf2
ocp render song.ocp -o /tmp/mix.wav --stems /tmp/stems
ocp render song.ocp -o /tmp/song.wav --reruns 3 --require-identical
```

| Option | Meaning |
|---|---|
| `-o, --output PATH` | Output file. Defaults to `output.` plus the container's extension, for example `output.wav`. |
| `--format CONTAINER` | Container to write; defaults to the project's export setting, then WAV. |
| `--sample-rate HZ` | Sample rate, 8000 to 192000. Defaults to the project's export rate, then 44100. |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument; also read from `OCP_SOUNDFONT`. |
| `--per-track-mix` | Render tracks independently through the full mix graph. |
| `--output-gain-db DB` | Fixed non-positive output headroom for per-track rendering. |
| `--minimum-duration-seconds S` | Minimum export length (0 to 600 s); events are never trimmed. |
| `--reruns N` | Render in N isolated plugin instances and compare the outputs. |
| `--require-identical` | Fail unless every rerun is byte-identical. |
| `--pre-roll S` | Silent wall-clock warm-up per plugin instance (0 to 600 s, default 4). |
| `--stems DIR` | Also write one pre-master file per track and bus, plus `stems.json`. |
| `--no-loudness-normalisation` | Ignore `loudness:` on the master. |
| `--sample-format`, `--bit-depth` | WAV or FLAC sample depth. |
| `--flac-level`, `--mp3-bitrate`, `--mp3-vbr` | FLAC compression and MP3 rate settings. |
| `--dither`, `--no-dither` | Turn WAV dither on, or off even when the project enables it. |
| `--resample-quality STEP` | Resampling quality step, 0 (fastest) to 7 (best). |
| `--report` | Print peak, true peak, RMS, crest and integrated loudness of the rendered file. |

`--stems` performs a single export pass. You cannot combine it with
`--reruns`, `--require-identical`, `--per-track-mix` or `--output-gain-db`.

`--report` measures the file after it is written. With `--stems` it reports the
mix; with `--reruns` it reports the run that was kept.

Containers, bit depths, dither and the MP3 rate and bitrate tables are in
[Export format](../export-format.md). The flags override the project's export
settings.

A plugin that loads its content asynchronously can render silence if a note
starts before the content is ready. `--pre-roll` waits in wall-clock time; it
does not add rendered silence. `--reruns N --require-identical` proves that the
output is reproducible. It does not prove that a load finished. See
[Troubleshooting](troubleshooting.md).

## Bounce

```sh
ocp bounce song.ocp --track lead --from 9:1 --to 17:1 --output /tmp/lead.wav
```

`bounce` renders one track's post-insert, pre-fader signal over a beat range
and writes a 32-bit float stereo WAV. The range is a slice of the same timeline
a full mix renders, so effect state and latency handling match the mix. Sends,
buses and the master chain are left out. `--from` and `--to` take
`bar:beat[:subdivision]`. The `--to` end is exclusive, and the file holds
exactly that span.

| Option | Meaning |
|---|---|
| `--track NAME` | Track whose signal is rendered (required). |
| `--from POS` | First beat of the range, as `bar:beat[:subdivision]`. |
| `--to POS` | End of the range (exclusive), same notation. |
| `-o, --output PATH` | Output WAV, 32-bit float (required). |
| `--sample-rate HZ` | Rate to render at. Defaults to the project's export rate, then 44100. |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument; also read from `OCP_SOUNDFONT`. |

The file is written exactly as rendered, with no dither, no loudness
normalisation and no clipping, so a hot track keeps its headroom. The GUI bounce
uses the same path, so you can check a bounce without opening the app. The
semantics and the FX-baking rule are in
[Audio clips, section 11](../audio-clip.md#11-bounce-and-consolidate).

## Comping

```sh
ocp clip align song.ocp lead-take take-a take-b
ocp clip align song.ocp lead-take take-a --output aligned.ocp
ocp clip align song.ocp lead-take take-a --dry-run
ocp clip align song.ocp lead-take take-a --invert
```

`clip align` analyses the named `@audio_clip` takes against a reference clip.
It writes a head trim to `offset:` and a tempo correction to `stretch:` in the
project, and reports the aligned correlation. With `--invert` it writes
`invert: true` when that correlation is below `-0.25`, and clears the flag
otherwise.

`--sample-rate` sets the analysis rate (default 48000). The seconds and ratios
it writes do not depend on that rate. Every take is analysed before anything is
written, so one failed take leaves the project untouched. By default the
command rewrites the input file. `--output` writes somewhere else, and
`--dry-run` only reports.

The correction is affine: one offset plus one constant rate. It does not warp
the take non-linearly. A ratio outside `0.25..=4.0` is refused, and the command
does not clamp it. A tempo correction beyond ±10 % is accepted with a warning.
The design, the measured numbers and the remaining limits are in
[Comping](../comping.md).

## Leveler bake

```sh
ocp level song.ocp --track lead --write
ocp level song.ocp --track lead --output baked.ocp
ocp level song.ocp --track lead --json
```

`level` renders one track dry, then steps that track's leveler over the
rendered signal with the device's own detector law. It writes the result into
the track's `gain_automation` lane, one point on each 10 ms control boundary,
and drops points that linear gain interpolation reproduces within 0.05 dB. It
then removes the leveler from the track, so the project stores the gain
decision and no longer rides the level in real time.

| Option | Meaning |
|---|---|
| `--track NAME` | Track to bake (required). |
| `-w, --write` | Replace the input project. |
| `-o, --output FILE` | Write the baked project to `FILE`. |
| `--json` | Emit the bake report as JSON. |
| `--sample-rate HZ` | Rate of the dry pass (default 48000). |
| `--soundfont PATH` | Fallback SoundFont for tracks with no project-owned instrument. |

Without `--write` or `--output` the command reports and writes nothing.

The dry pass runs the track's chain without the leveler, so effects before it
are included. The leveler must be the track's last effect, and the track must
own a clip. The bake replaces any gain clips already on that track. The lane
encodes one setting, so if you change the leveler's settings you bake again. The
device itself does not change. The law and the measured match are in
[Vocal compression](../vocal-comp.md).

## Play and MIDI

```sh
ocp midi list
ocp midi monitor --input "IAC Driver Bus 1"
ocp play song.ocp
ocp play song.ocp --midi-in "IAC Driver Bus 1" --midi-out "IAC Driver Bus 1" --midi-clock --midi-thru
```

`midi list` prints the input and output ports with their indices. Wherever a
port is expected you can give its name or its index. `midi monitor` prints
timestamped events.

`play` renders the project offline and streams the result through the audio
device the desktop app has saved in its settings. Live play covers built-in,
OpenSynth and SoundFont tracks. A track whose instrument is a hosted plugin is
not part of it; see [Live MIDI](../midi-live.md).

```sh
ocp midi import song.mid --out song.ocp --json-summary
ocp midi import song.mid --out song.ocp --split-channels --keep-empty
ocp midi export song.ocp --out song.mid --format 1 --ppq 480 --tracks 0-2 --json-report
ocp midi2ocp song.mid -o song.ocp
ocp batch-midi2ocp midi/ -o ocp_out/ --flatten
ocp batch-data midi/ -o training_data/ --soundfont soundfonts/MS_Basic.sf2
ocp generate-midi -o data/midi_raw -c 100 --seed 42
```

`midi import` is the full importer, with mapping options, tempo, meter and key
maps, CC lanes and a summary. `midi2ocp` is the older bulk converter that the
data pipeline uses; it writes to stdout unless you pass `-o`. `batch-data`
requires a SoundFont and renders a WAV for each `.ocp` it writes.

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

`devices list` prints each device's id, category, parameter count and display
name from the schema the engine and the GUI use. The `--json` output is the
fixed contract.

`validate-builtin-effects` builds the audio graph as a DSP preflight without
rendering. It skips external plugin effects, so a project may mix built-in and
plugin devices. Legacy effect shorthand is expanded and reported as a warning
(see [DSL compatibility](../dsl-compat.md)).

`resolve-plugin` expects exactly one match across VST3, VST2, CLAP, LV2 and the
built-in devices, and prints the match's metadata as JSON. It does not look up
AudioUnits.

`plugin-params` loads a VST3 or VST2 plugin and prints every parameter. For a
VST3 plugin the table shows id, name, unit, range, default, step count and
flags. For a VST2 plugin it shows index, name, label, default and current
value; VST2 values are normalized to 0..1. When a VST3 with the same name is
also installed, pass the `.vst` path to select the VST2 build.

`plugin-preset import` reads a `.ffp` preset and applies its parameter values
to the named VST3 plugin. It reports how many parameters it parsed, mapped and
applied, and lists the unmapped, rejected and read-back-mismatched ones.
`--json` emits the report.

`plugin conformance` runs one child process per VST3 bundle and prints the
per-step results as a table, JSON or Markdown. The battery and the JSON schema
are in [VST3 conformance](../vst3-conformance.md).

## Plugin index

```sh
ocp plugin-index keygen --out /tmp/index-keys
ocp plugin-index build --catalog catalog.json --out dist/ --key /tmp/index-keys/plugin-index.key
ocp plugin-index verify dist/ --public-key /tmp/index-keys/plugin-index.pub
```

`keygen` creates an Ed25519 keypair. `build` writes `index.json`, its
signature and the compressed catalog. `verify` checks the signature and the
catalog hash.

## Samples

```sh
ocp sample search "snare drum" --kind drum --limit 5
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2 --json
ocp sample pull "commons:File:Snare drum unmuffled.ogg" --dest media/samples
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound
```

`search` lists each result with its kind, duration, channels, sample rate,
licence and credit line. When a result's licence is refused, it says why.

`pull` resolves the id and downloads the file, then verifies the declared size
and sha256. It writes the audio, a `.json` sidecar with the licence and credit
line, and a receipt.

Wikimedia Commons is the default provider and needs no key. Freesound needs an
API key and serves a 128 kbps preview. The GUI's Samples panel can connect to
Freesound through OAuth2 and pull the original file. `ocp sample providers`
lists the providers this build knows. The store layout, the licence rule and
the tool names are in [Samples](../samples.md).

`ocp sample slices loop.wav --first-key 60` prints an SFZ patch with one region
per WAVE cue point; `--gated` follows note length and `--json` prints the frame
ranges instead. See [Sampler](../sampler.md).

## Range render and analysis

The windowed render, audio analysis and A/B comparison tools (`render_range`,
`audio_analyze`, `audio_compare`) are not CLI subcommands. They belong to the
agent surface of the running GUI; see [Drawing from an agent](agents.md).

There is no `ocp serve` command. The running GUI hosts the agent HTTP surface,
and the CLI has no server to start.
