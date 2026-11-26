# Export: containers, sample depth and dither

The mixer carries 32-bit float samples from each source to the master. When you
export, you choose a container and, inside it, how much of that precision
reaches the file. The project header, the CLI and the encoder share one
`EncodeSpec`, so every front end writes the same bytes for the same choice.

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

`--output` follows the container. Without a path the file is `output.wav`,
`output.flac` or `output.mp3`. A path without an extension gets the container's
extension. A path whose extension names a different container is refused, so a
file never carries an extension that misdescribes its bytes.

## WAV depth

| Flag | File | What it carries |
|---|---|---|
| `--sample-format i16` (alias `--bit-depth i16`) | signed 16-bit PCM | The default. |
| `--sample-format i24` (alias `--bit-depth i24`) | signed 24-bit PCM | 8 more bits of the same mix, at no measurable runtime cost. |
| `--sample-format f32` (alias `--bit-depth f32`) | IEEE 32-bit float | Bit-identical to the internal mix buffer. |

`--bit-depth` and `--sample-format` are two names for the same option. Use
whichever reads better next to the container.

The default is `i16`. The render gate is defined on 16-bit bytes (48 kHz,
`--no-loudness-normalisation`, two fixtures at 4,248,432 bytes), and a default
of `f32` would double the file size and change every stored gate hash with no
audible gain. Higher depths are therefore opt-in.

Choose `f32` when the file goes into another digital stage, such as another DAW,
a mastering chain or an analysis pass: nothing is quantised. Choose `i24` for
delivery: it sounds the same as `f32` at three quarters of the size. Keep `i16`
for compatibility and for players that only read 16-bit PCM.

`hound` writes the WAV header at every depth. A 16-bit export keeps the same
bytes, truncation included, that the 16-bit writer has always produced, which
is what the render gate checks.

## FLAC depth and compression

FLAC is lossless over integers, so it stores the same integer grid as WAV at the
same depth. `--sample-format` (or `--bit-depth`) selects 16 or 24 bits. `f32` is
refused because FLAC has no float representation.

`--flac-level 0..=8` sets the compression level; the default is 5. Level 0
searches least and writes the most bytes, level 8 searches longest and writes
the fewest. The level changes the file size and never the decoded samples.

On the 20-track reference project at 48 kHz:

| Encode | Size |
|---|---|
| 16-bit WAV | 4.25 MB |
| 16-bit FLAC, level 0 | 2.18 MB |
| 16-bit FLAC, levels 5-8 | 2.07 MB |
| 24-bit WAV | 6.37 MB |
| 24-bit FLAC, level 5 | 4.47 MB |

Most of the saving appears at level 4, where LPC starts. The levels above it
trade encode time for a few kilobytes.

FLAC never dithers. A lossless container would store the dither as permanent
noise. `--dither` with `--format flac` is refused, and the export dialog
disables the dither checkbox and shows that reason. The float-to-integer
quantisation still happens on the same grids as undithered WAV (`i16`
truncates, `i24` rounds), so a FLAC render and a WAV render at the same depth
decode to bit-identical integers.

The encoder is the pure-Rust `flacenc` crate (Apache-2.0). It needs no C
toolchain and no external process. It writes a fixed-block stream, and the
STREAMINFO block keeps the nominal block size even when the last frame is
short. A strict reader such as symphonia would otherwise treat the stream as
variable-block and stop at the first frame.

## MP3 bitrate, rate and mode

Layer III takes 16-bit PCM at one of seven sample rates. The rates belong to
three MPEG versions, each with its own CBR table:

| MPEG version | Sample rates | CBR bitrates |
|---|---|---|
| MPEG-1 | 44100, 48000 Hz | 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320 kbps |
| MPEG-2 | 16000, 22050, 24000 Hz | 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160 kbps |
| MPEG-2.5 | 11025, 12000 Hz | the MPEG-2 table |

The export dialog reads the same seven rates from the encoder through
`export_capabilities`, so it refuses a rate the container cannot write before
the render starts.

The default is 192 kbps CBR. `--mp3-bitrate <kbps>` picks another table entry.
`--mp3-vbr <0..9>` asks for a variable-bitrate encode instead. The two flags
conflict, because a VBR encode chooses its own average bitrate. Quality 0 is the
best and 9 the smallest. The VBR ladder aims for average bitrates close to those
of the reference encoder, so `--mp3-vbr 2` lands near 190-200 kbps on music.

A sample rate with no Layer III form is refused. The renderer never resamples
to reach one:

```
96000 Hz is not an MP3 sample rate: MPEG Layer III writes 44100, 48000, 16000,
22050, 24000, 11025 or 12000 Hz and this build never resamples to reach one
```

A bitrate outside the version's table is refused with that table. It is never
snapped to a neighbouring value:

```
MP3 bitrate 200 kbps is not in the MPEG-1 Layer III table; MPEG-1 writes 32-320
kbps: 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256 or 320
```

MP3 has no sample depth and no dither step. The encoder consumes 16-bit PCM, so
the CLI refuses `--sample-format` and `--dither` with `--format mp3`, and the
dialog hides both controls. `--report` is refused as well, because it re-reads
the written file as WAV.

The dialog remembers an MP3 export like any other container. The
[preferences bundle](preferences-bundle.md) stores it as
`export.format: "mp3"` with `export.mp3_bitrate` (a CBR table entry in kbps,
`32`-`320`) and `export.mp3_vbr` (quality index `0`-`9`, or `null` for CBR).
Importing the bundle on another machine restores the bitrate and mode.

### Encoder and quality

Quality is measured against the source mix on the two render-gate fixtures
(22.13 s each, 48 kHz stereo), so the numbers describe OpenCompose's own output:

| Encode | Residual RMS | LSD vs source | Energy above 15 kHz |
|---|---|---|---|
| CBR 192, fixture A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| CBR 192, fixture B | -42.50 dBFS | 1.66 dB | -0.35 dB |
| VBR q2, fixture A | -52.11 dBFS | 1.72 dB | -0.35 dB |
| VBR q2, fixture B | -42.49 dBFS | 1.67 dB | -0.35 dB |
| CBR 320, fixture B | -50.18 dBFS | 0.74 dB | -0.31 dB |
| CBR 96, fixture B | -36.33 dBFS | 3.30 dB | -7.74 dB |

The residual falls as the bitrate rises. Third-octave differences across the
band stay within 0.12 dB RMS, so apart from the top octave no band is lifted or
dropped. Above 15 kHz the encoder loses about 0.35 dB of energy; a common MP3
low-pass loses 2.5-3 dB there. At CBR 192 and VBR q2 the encoder stays within a
1 dB log-spectral-distance margin of an external reference encoder, which is the
condition for MP3 shipping as a default feature.

The stream carries an `Info`/`Xing` tag frame with an extension block that
records the encoder delay (528 samples) and the end padding. A player that reads
the tag decodes exactly the mix length: 1062097 input frames become 923 audio
frames of 1152 samples and trim back to 1062097. A player that ignores the tag
plays up to one frame of extra silence at each end.

## Sample rate

`ocp render --sample-rate` accepts 8000-192000 Hz, the range the sampler runs.
A rate outside it is refused:

```
sample rate 0 Hz is outside the supported 8000-192000 Hz range; enter a rate the
sampler can run, such as 44100 or 48000
```

A project can state the rate it exports at:

```ocp
export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000}
```

`ocp render` uses `--sample-rate` when you pass it, then `export.sample_rate`,
then 44100. The export dialog writes the rate it rendered at into the same
field, so the project re-renders at the rate it was mixed for.

## Dither

Dither is an on/off switch, and it applies only to integer output.

- `--dither` turns it on. `--no-dither` turns it off even when the project
  turns it on. The two flags conflict.
- On `i16` it is TPDF at 1 LSB, added immediately before rounding to the
  nearest integer. That amount decorrelates the quantisation error from the
  signal. More is audible noise; less only partly decorrelates.
- On `i24` it is the same 1 LSB TPDF, which is 48 dB quieter in absolute terms.
- On `f32` it is refused. The file already carries the internal samples, so
  dither would only add noise. The error reads
  `dither is meaningless for f32: 32-bit float already carries the internal mix
  precision; use i16 or i24, or turn dither off`.
- On FLAC it is refused, because the lossless container would store the noise.
- On MP3 it is refused, because Layer III takes 16-bit PCM and has no dither
  step.

Rounding depends on the depth and is a separate matter from dither:

- `i16` without dither truncates, so existing exports and the render gate stay
  byte-identical. With dither it rounds to nearest.
- `i24` always rounds to nearest, with or without dither.

### Determinism and seeds

The dither comes from a counter-based generator. The value for a sample is a
pure function of the frame index, the channel and a seed
(`WavOptions::dither_seed`, 0 by default). Two renders of the same project
produce the same bytes, which makes a dithered export testable: you never have
to compare a re-render with a tolerance. Changing the seed changes only the
least significant bit of each sample.

Each file in a set gets its own seed, derived from its file name. The mix keeps
the spec's seed, and each stem derives one from its base name. Stems that
shared a seed would carry the same noise sequence, and their sum would carry N
times the dither power of one file. With separate seeds the noise is
decorrelated and the sum carries roughly √N times the power. The derivation is
deterministic, so the same project and stem names reproduce the same bytes.

## Clipping

Nothing clamps the mix before the encoder. Only the integer writer clamps, at
full scale. The export measures the mix before quantisation and reports what it
found:

- `ocp render --report` prints the level of the written file and the level of
  the mix before quantisation, so a clipped `i16` file cannot pass for a clean
  0.00 dBFS master:
  `Mix before quantisation: peak +3.20 dBFS, true peak +3.55 dBTP (clipped at full scale in the written file)`.
- Every clipped export prints a warning, with or without `--report`:
  ``warning: <file> reaches +3.20 dBFS before quantisation and was clipped at full scale; lower the level or add a `loudness:` target``.
- A mix more than 6 dB above full scale is refused before any file is opened:
  ``the mix reaches +8.00 dBFS, more than 6 dB above full scale; export refused (lower the master fader or add a `loudness:` target)``.
  A peak at twice full scale is a gain-staging mistake, and writing it would
  produce a square wave.

With a `loudness:` target the master limiter holds the mix at or below
−1 dBFS, so a normalised export neither clips nor warns.

## Stem names

Stem file names keep letters and digits from every script, in lower case, and
take the container's extension. A project named in Chinese characters exports
`<that name>-mix.wav` and `<that name>-<track>.wav` (or `.flac`) with the
characters intact. `Café Déjà` exports `café-déjà-mix.wav`. A bus stem is
named `<song>-bus-<bus>`. A run of characters that is unsafe in a path (`/`,
`\`, `:`, quotes, control characters) becomes one dash. If nothing is left, the
set falls back to `song` and a single stem to `track`.

## Where the setting lives

The canonical project form nests the options for each container:

```ocp
@project "My song"
  tempo: 120
  export: {container: wav, wav: {depth: i24, dither: true}, sample_rate: 48000, resample: 6}
```

A FLAC project adds a `flac:` block:

```ocp
export: {container: flac, wav: {depth: i16, dither: false}, flac: {depth: i24, level: 8}}
```

The `flac:` block defaults to `{depth: i16, level: 5}`. The serializer omits it
while it holds those defaults, so a WAV project keeps its usual spelling. The
CLI equivalent is `--format flac --sample-format i24 --flac-level 8`, and the
flags win over the project block.

An MP3 project states a bitrate or a VBR quality:

```ocp
export: {container: mp3, wav: {depth: i16, dither: false}, mp3: {bitrate: 256, vbr_quality: 2}}
```

The `mp3:` block defaults to `{bitrate: 192}` with no `vbr_quality`, and is
omitted while it holds those defaults. `bitrate` is the CBR bitrate in kbps.
`vbr_quality` is the 0-9 index; when present it replaces the CBR bitrate. The
CLI equivalent is `--format mp3 --mp3-bitrate 256` or `--format mp3 --mp3-vbr 2`.

`resample:` is optional and selects the offline resampling step (0-7).
`sample_rate:` is optional and records the rate the project exports at. Without
it the caller's choice applies.

The older flat block still loads and means the same thing:

```ocp
@project "Legacy"
  export: {format: i24, dither: true}
```

A project without an `export:` block exports 16-bit WAV without dither, and the
serializer writes no block for it. `ocp fmt` rewrites an existing block in the
container form. The flat form stays accepted, so an older file loads unchanged.

Settings resolve in this order: CLI flags, then the project header, then 16-bit
WAV without dither. A stems export records the effective choice in `stems.json`
as `container`, `sample_format`, `dithered`, `sample_rate` and `channels`, and
records the frame count of the mix and of each stem.

Stems are exclusive. A track that feeds a bus appears only inside that bus's
stem, not as a stem of its own as well. Only stems whose `mix_sum_member` is
true sum to the pre-master mix, and only when all of them are exported. Those
stems include their final route delay to the master; every other stem is taken
at its track or bus output, before its outgoing route delays. A manifest
therefore never carries the same dry signal twice, and a receiving session can
rebuild the mix from the marked stems without doubling a group and its
children.

## The export dialog

The GUI has one export dialog. Three actions open it: the toolbar's render
button, **Export audio…** in the File menu, and `Cmd/Ctrl+E`. The stems button
opens the same dialog with the scope set to **Mix + stems**.

In the dialog you choose the container, depth, dither, bitrate or VBR quality,
sample rate, resampling step, loudness normalisation and stem folder. The
dialog writes those choices into the project's `export:` block through the same
undoable edit path as any other project change. It also remembers the last
choices on this machine and uses them only for options the project does not set.

The format picker asks the backend which encoders the build has. In a build
without the MP3 encoder, MP3 is listed as disabled with the reason
`This build has no MP3 encoder`. MP3 controls appear only while MP3 is selected.
The size estimate for MP3 is the CBR size at the chosen bitrate, and the dialog
says so, because a VBR file's size depends on the material.

Renders run off the command thread and report progress at checkpoints. You can
cancel a render; it stops at the next checkpoint and publishes nothing.
