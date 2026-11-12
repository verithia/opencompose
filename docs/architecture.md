# The engine pipeline

This page follows one `.ocp` document from text to a WAV file and names the
stage that owns each decision. The types it mentions are documented in the
`oc-engine` and `oc-dsl` API pages.

```text
.ocp text
  │ parse + validate                     oc-dsl
  ▼
Project (AST)                            oc-dsl::ast
  │ interpret                            oc-engine::interpret
  ▼
timed channel events per track
  │ sources                              oc-engine::block_source, oc-synth, oc-host
  ▼
per-track stereo buffers
  │ graph                                oc-engine::graph
  ▼
track effects → sends → buses → master
  │ loudness                             oc-engine::render
  ▼
master limiter → 32-bit float mix
  │ WAV writer                           oc-engine::wav
  ▼
.wav file
```

## 1. Parse and validate

`oc_dsl::parse` turns the text into an [`ast::Project`](api/oc_dsl/ast/struct.Project.html);
`oc_dsl::validate` rejects what the grammar
accepts but the engine cannot honour: a bus cycle, a section that overlaps its
neighbour, a note outside the project tempo map, a media file that does not
exist. `ocp validate` runs both and prints the first failure with its line.

Validation runs before any audio work, so a project that reaches the renderer is
structurally sound. A render error after this point is a device, plugin or media
failure, not a syntax one.

## 2. Interpret the arrangement

`oc_engine::interpret` expands the arrangement into a flat, absolute timeline:
sections and `repeat:` decide the length, patterns placed as clips expand in
place, and the tempo and meter maps convert bar:beat positions into quarter
notes and then into samples. Two inputs are merged here:

- note and hit events from phrases, patterns and drum kits;
- `performance:` blocks, which are raw channel commands and are never merged
  with note-derived events on the same track.

The output is a per-track event list at sample-accurate positions. Nothing is
rendered yet, which is why a dry interpretation is cheap enough for the GUI to
run on every edit.

## 3. Render the sources

`oc_engine::block_source` asks each track's instrument for audio in blocks:
OpenSynth and the drum-kit engine render natively, the SoundFont player renders
`source: soundfont` presets, and `oc_host` drives a hosted plugin in its own
format. A source is stateful and owns its renderer; nothing is cached at
whole-song length.

Plugin instances are host objects. They are created per render, kept alive for
the length of that render, and handed to the graph, so a plugin's own latency
and state stay consistent between the tracks that share it.

## 4. Mix through the graph

`oc_engine::graph::AudioGraph` is the only place that sums audio. For every
block it applies, in order:

1. the track's effect chain;
2. the track fader, pan and polarity;
3. sends, each of which is post-fader by default or pre-fader when written
   `pre`;
4. the bus tree, depth-first, with each bus's own effects and fader;
5. the master fader and master effects.

Solo, mute and solo-safe are resolved to a single set of audible tracks before
the block loop, so a solo change does not rebuild the graph. Plugin delay
compensation is applied at the same stage: each channel reports its latency and
the graph delays its siblings to match.

The graph never allocates in the block loop. Buffers are sized when the graph is
built, which is what lets the same code run in the realtime callback and in an
offline render.

## 5. Loudness, limiting and the sample rate

The mix stays 32-bit float until the WAV writer. If the project declares
`loudness:`, `oc_engine::render` measures the finished mix and applies the gain
in up to two deterministic passes, each starting from the unmodified mix and
each followed by the master limiter at a −1 dBFS true-peak ceiling. The report
per pass is available to the caller, so a shortfall is visible instead of
silent.

Sample rate is a property of the render, not of the project: `ocp render`
defaults to 44.1 kHz and accepts 8 kHz to 192 kHz, the range the sampler runs.
Media that was decoded at another rate is converted with a Kaiser-windowed sinc
converter whose tap count comes from the quality rung; `export: { resample: N }`
selects the render rung and `live_resample: N` selects the cheaper rung used for
streaming preview. Sample-rate conversion runs on the offline and media-loading
paths only, never in the audio callback.

## 6. Write the file

`oc_engine::encode` is the only module that decides how float precision reaches
the file: it measures the mix before quantisation, then a container encoder
writes it. The container (`wav`, `flac`, `mp3`) and its options — depth (`i16`,
`i24`, `f32`) and TPDF dither for WAV, a compression level for FLAC, a CBR
bitrate or VBR quality for MP3 — come from the project's `export:` block or from
the command line, and the same seed and length always produce the same bytes.

## Where the caches sit

Two caches make repeated renders cheap, and neither changes the result:

- `oc_engine::media_cache` holds decoded and resampled media, keyed by path,
  modification time and the resample rung. Audio clips are decoded once.
- `oc_engine::render_cache` holds plugin-side state and per-track render
  products for the GUI's streaming path. `render_offline` takes a snapshot,
  releases the project lock, renders, and publishes afterwards, so a render
  never blocks an edit and an edit never lands inside a render.

## Realtime rules

The audio callback runs one graph instance on one thread:

- no allocation, no locks and no thread creation in the block loop;
- no per-voice threads: OpenSynth is single-threaded by design;
- parallel work exists only in the offline renderer, where tracks render on a
  worker pool before the graph sums them.

See [Playback latency](playback-latency.md) for the measurements behind these
rules, and [oc-engine](components/oc-engine.md) for the modules that implement
each stage.
