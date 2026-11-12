# Export: containers, sample depth and dither

The mixer keeps 32-bit float samples from source to master. An export states
which container to write and, inside that container, how much of the mix's
precision reaches the file. The project header, the CLI and the encoder share
one `EncodeSpec`, so every front end writes the same bytes for the same choice.

## Containers

| `--format` | `export.container` | Status |
|---|---|---|
| `wav` | `wav` | RIFF/WAVE, uncompressed. |
| `flac` | `flac` | Native FLAC, lossless. |
| `mp3` | `mp3` | MPEG Layer III, lossy. |

An unknown container is refused with the list this build supports:

```
unknown export container 'ogg': this build writes wav, flac, mp3
```

`--output` follows the container: a missing path becomes `output.wav`,
`output.flac` or `output.mp3`, a path without an extension gets the container's
extension, and a name that disagrees with the container is refused rather than
written as bytes the extension lies about.

## WAV depth

| Flag | File | What it carries |
|---|---|---|
| `--sample-format i16` (alias `--bit-depth i16`) | signed 16-bit PCM | The historical export default. |
| `--sample-format i24` (alias `--bit-depth i24`) | signed 24-bit PCM | 8 more bits of the same mix; no runtime cost worth measuring. |
| `--sample-format f32` (alias `--bit-depth f32`) | IEEE 32-bit float | Bit-identical to the internal mix buffer. |

`--bit-depth` and `--sample-format` are two spellings of the same option; use
whichever reads better next to the container.

The default is **`i16`**, because that is what every existing export produced and
the render gate is defined on those bytes (48 kHz, `--no-loudness-normalisation`,
two fixtures at 4,248,432 bytes). Changing the default to `f32` would double the
file size and change every stored gate hash for no audible gain; the format is
opt-in instead.

`f32` is the right choice when the file is going into another digital stage —
another DAW, a mastering chain, an analysis pass — because nothing is quantised.
`i24` is the right choice for delivery: the same audible result as `f32` with a
third the storage. `i16` stays for compatibility and for players that only read
16-bit PCM.

The WAV header is written by `hound` in all three cases, so a 16-bit export is
byte-for-byte the file the old writer produced, truncation included.

## FLAC depth and compression

FLAC is lossless over integers, so it writes the same integer grid as WAV at the
same depth. `--sample-format` (or `--bit-depth`) selects 16 or 24 bits; `f32` is
refused because FLAC has no float representation. The compression level is
`--flac-level 0..=8`, default **5**: 0 searches least and writes the most bytes,
8 searches longest and writes fewest. The level changes the file size and never
the decoded samples. On the 20-track reference project at 48 kHz, 16-bit FLAC
writes 2.18 MB at level 0 and 2.07 MB at levels 5-8 against 4.25 MB for WAV;
24-bit FLAC writes 4.47 MB at level 5 against 6.37 MB for 24-bit WAV. Most of
the gain comes when LPC starts at level 4; the remaining levels trade time for
a few kilobytes.

FLAC **never dithers**. A lossless container stores the dither as permanent noise
instead of removing it, so dither is a WAV option: `--dither` with `--format flac`
is refused, and the dialog disables the checkbox with that reason. The float to
integer quantisation still happens, and it uses the same grids as undithered WAV
(`i16` truncates, `i24` rounds), so a FLAC render and a WAV render at the same
depth decode to bit-identical integers.

The encoder is the pure-Rust `flacenc` crate (Apache-2.0). It adds no C toolchain
step and no external process. It writes a fixed-block stream, so the STREAMINFO
block keeps the nominal block size even when the last frame is short; a strict
reader such as symphonia otherwise reads the stream as variable-block and stops
at the first frame.

## MP3 bitrate, rate and mode

Layer III takes 16-bit PCM at one of seven rates, and the rates split into three
MPEG versions with different CBR tables:

| MPEG version | Sample rates | CBR bitrates |
|---|---|---|
| MPEG-1 | 44100, 48000 Hz | 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320 kbps |
| MPEG-2 | 16000, 22050, 24000 Hz | 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160 kbps |
| MPEG-2.5 | 11025, 12000 Hz | the MPEG-2 table |

The dialog offers this same seven-entry table: it reads the rates from the
encoder through `export_capabilities`, so a rate the container cannot write is
refused before the render starts.

The default is **192 kbps CBR**. `--mp3-bitrate <kbps>` states another entry,
`--mp3-vbr <0..9>` asks for a variable-bitrate encode instead; the two conflict,
because a VBR encode chooses its own average bitrate and the CBR value does not
apply. Quality 0 is the best and 9 the smallest; the ladder targets average
bitrates close to the reference encoder's, so `--mp3-vbr 2` lands near
190-200 kbps on music.

A rate with no MPEG Layer III form is refused, never resampled silently:

```
96000 Hz is not an MP3 sample rate: MPEG Layer III writes 44100, 48000, 16000,
22050, 24000, 11025 or 12000 Hz and this build never resamples to reach one
```

A bitrate outside the version's table is refused with that range rather than
snapped to a neighbour:

```
MP3 bitrate 200 kbps is not in the MPEG-1 Layer III table; MPEG-1 writes 32-320
kbps: 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256 or 320
```

MP3 has **no sample depth and no dither**. The encoder consumes 16-bit PCM, so
`--sample-format` with `--format mp3` is refused rather than ignored, `--dither`
is refused because there is no dither step, and the dialog hides both controls.
`--report` is refused too: it re-reads the written file as WAV.

The dialog remembers an MP3 export like any other container. The
[preferences bundle](preferences-bundle.md) carries that choice as
`export.format: "mp3"` with `export.mp3_bitrate` (kbps, `64`–`320`) and
`export.mp3_vbr` (quality index, `0`–`9`, or `null` for CBR), so exporting and
importing preferences on another machine restores the MP3 bitrate and mode.

### Encoder and quality

Quality is measured on the two render-gate fixtures (22.13 s each, 48 kHz
stereo) against the source mix, so the numbers describe our own output:

| Encode | Residual RMS | LSD vs source | Energy above 15 kHz |
|---|---|---|---|
| CBR 192, fixture A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| CBR 192, fixture B | -42.50 dBFS | 1.66 dB | -0.35 dB |
| VBR q2, fixture A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| VBR q2, fixture B | -42.49 dBFS | 1.67 dB | -0.35 dB |
| CBR 320, fixture B | -50.18 dBFS | 0.74 dB | -0.31 dB |
| CBR 96, fixture B | -36.33 dBFS | 3.30 dB | -7.74 dB |

The residual falls with the bitrate as expected, and third-octave differences
across the band stay inside 0.12 dB RMS, so no band is lifted or dropped beyond
the top-octave choice: the encoder keeps roughly 0.35 dB of the energy above
15 kHz, where a common MP3 low-pass drops 2.5-3 dB. Before MP3 became a default
feature the encoder was benchmarked at CBR 192 and VBR q2 against an external
reference encoder; it stayed inside the 1 dB log-spectral-distance margin, so
the feature ships on.

The stream carries the de-facto `Info`/`Xing` tag frame and extension block
recording the encoder delay (528 samples) and the end padding, so a player that
understands the tag decodes
exactly the mix length: 1062097 input frames become 923 audio frames of 1152
samples and trim back to 1062097. A player that ignores the tag gets up to one
frame of extra silence at each end.

## Sample rate

`ocp render --sample-rate` accepts 8000-192000 Hz, the range the sampler runs.
A rate outside it is refused with that range:

```
sample rate 0 Hz is outside the supported 8000-192000 Hz range; enter a rate the
sampler can run, such as 44100 or 48000
```

A project states the rate it exports at, and the flag overrides it:

```ocp
export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000}
```

`ocp render` uses `--sample-rate` when you pass it, then `export.sample_rate`,
then 44100. The export dialog writes the rate it rendered at into the same
field, so a project re-renders at the rate it was mixed for.

## Dither

Dither is a switch, and it only applies to integer output:

- `--dither` enables it; `--no-dither` turns it off even when the project enables
  it. The two conflict.
- On `i16` it is **TPDF at 1 LSB**, added immediately before rounding to the
  nearest integer. That is enough to decorrelate the quantisation error from the
  signal; larger amounts are audible noise, smaller amounts only half-work.
- On `i24` it is the same 1 LSB TPDF, which is 48 dB quieter in absolute terms.
- On `f32` it is **refused**, not ignored: the file already carries the internal
  samples, so dither would only add noise.
- On FLAC it is **refused** for the opposite reason: the container is lossless,
  so the noise would be stored rather than removed.
- On MP3 it is **refused**: Layer III takes 16-bit PCM and has no dither step.
  The `f32` error reads
  `dither is meaningless for f32: 32-bit float already carries the internal mix
  precision; use i16 or i24, or turn dither off`.

Rounding differs by depth and is not the same switch as dither:

- `i16` **without** dither keeps the historical truncation, so existing exports
  and the render gate stay byte-identical. With dither it rounds to nearest.
- `i24` **always rounds** to nearest, with or without dither.

### Determinism and seeds

The dither is a counter-based generator, not a stream: the value for a sample is
a pure function of the frame index, the channel and a seed (`WavOptions::dither_seed`,
0 by default). Two renders of the same project therefore produce the same bytes,
which is what makes a dithered export testable; changing the seed moves only the
least significant bit of each sample. A re-render never has to be compared with
tolerance because the noise was freshly drawn.

A file set gets one seed per file, derived from the file name. The mix keeps the
spec's seed; each stem derives its own from its base name. Stems that shared a
seed would carry the same noise sequence and sum coherently to N× the noise of
one file; naming them apart decorrelates the set, so a stem sum carries roughly
√N times the dither power rather than N times. The derivation is deterministic,
so the same project and stem names reproduce the same bytes.

## Clipping

The mix is never clamped before the encoder; only the integer writer clamps at
full scale. An export therefore measures the mix before quantisation and says
what it saw:

- `ocp render --report` prints the written file's level and the mix level before
  quantisation, so a clipped `i16` file does not pass for a clean 0.00 dBFS
  master:
  `Mix before quantisation: peak +3.20 dBFS, true peak +3.55 dBTP (clipped at full scale in the written file)`.
- Any clipped export warns, whether or not `--report` is set:
  `warning: <file> reaches +3.20 dBFS before quantisation and was clipped at full scale; lower the level or add a loudness: target`.
- A mix more than **6 dB** above full scale is refused before a file is opened:
  `the mix reaches +8.00 dBFS, more than 6 dB above full scale; export refused (lower the master fader or add a loudness: target)`.
  Twice full scale is a gain-staging mistake, not a loud master, and writing it
  would only produce a square wave.

With a `loudness:` target the master limiter keeps the mix at or below −1 dBFS,
so a normalised export does not clip and does not warn.

## Stem names

Stem file names keep letters and digits from every script and take the
container's extension. A project named in Chinese characters exports
`<that name>-mix.wav` and `<that name>-<track>.wav` (or `.flac`) with those
characters intact instead of `untitled-mix.wav`; `Café Déjà` exports
`café-déjà-mix.wav`. Runs of characters that are hostile in a path (`/`, `\`,
`:`, quotes, control characters) collapse to one dash. A name with nothing left
falls back to `song` for the set and `track` for one stem, never to `untitled`.

## Where the setting lives

The canonical project form nests the per-container options:

```ocp
@project "My song"
  tempo: 120
  export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000, resample: 6}
```

A FLAC project states its own container block:

```ocp
export: {container: flac, wav: {depth: i16, dither: false}, flac: {depth: i24, level: 8}}
```

The `flac:` block defaults to `{depth: i16, level: 5}` and is omitted from the
serialised form while it equals those defaults, so a WAV project keeps its
historical spelling. The CLI takes `--format flac --sample-format i24
--flac-level 8`; the flags win over the project block.

An MP3 project states the bitrate or the VBR quality it wants:

```ocp
export: {container: mp3, wav: {depth: i16, dither: false}, mp3: {bitrate: 256, vbr_quality: 2}}
```

The `mp3:` block defaults to `{bitrate: 192}` with no `vbr_quality` and is
omitted while it equals those defaults. `bitrate` is the CBR bitrate in kbps;
`vbr_quality` is the 0-9 index and, when present, replaces the CBR bitrate. The
CLI takes `--format mp3 --mp3-bitrate 256` or `--format mp3 --mp3-vbr 2`.

`resample:` is optional and selects the offline resampling rung (0-7).
`sample_rate:` is optional and records the rate the project exports at; a
project that omits it keeps the caller's choice. The flat
block written by earlier versions still loads and means the same thing:

```ocp
@project "Legacy"
  export: {format: i24, dither: true}
```

A project without the `export:` block keeps the historical 16-bit undithered WAV
and serialises exactly as before, with no block at all. An existing block is
rewritten in the container form by `ocp fmt`; the flat form remains accepted so
an older file loads unchanged.

Resolution order is: CLI flags, then the project header, then 16-bit WAV without
dither. The stems export records the effective choice as `container`,
`sample_format`, `dithered`, `sample_rate` and `channels` in `stems.json`. The
manifest also records the frame count for the mix and each stem.

Stems are exclusive. A track that feeds a bus appears as that bus's return, not
as both a track stem and the bus stem, and only stems whose `mix_sum_member` is
true sum to the pre-master mix. A manifest therefore never carries the same dry
signal twice, and a receiving session can rebuild the mix from the marked stems
without duplicating a group and its children.

The GUI has one export dialog. The toolbar's render button, the File menu's
"Export audio…" item and `Cmd/Ctrl+E` open it, and the stems button opens the
same dialog with the scope preset to "Mix + stems". The dialog chooses the
container, depth, dither, bitrate or VBR quality, rate, resampling rung, loudness
normalisation and stem folder, then writes those choices into the project's `export:` block through the
same undoable edit path as any other project change; the last used choices are
also remembered on the machine, and only fill options the project leaves out.
The format picker asks the backend which encoders this build has. A build
without the MP3 encoder lists MP3 disabled with the reason `This build has no
MP3 encoder`, and MP3 controls appear only while MP3 is selected. The size
estimate for MP3 is the CBR size at the chosen bitrate, and the dialog says so;
a VBR encode follows the material. Renders run off the command thread, report
progress at checkpoints and can be cancelled at the next one, in which case
nothing is published.
