# OpenCompose documentation

OpenCompose is a digital audio workstation that stores a song as a text file.
An `.ocp` document lists the tracks, patterns, notes, mixer settings and effects
as readable lines. You can edit that file by hand, have a model generate it,
render it with the command line or play it in the desktop app.

## What you work with

The `.ocp` language is the project format. It is plain text that you can read
and diff in version control, and it does not depend on one engine. See
[The .ocp file](manual/dsl.md).

The `ocp` command line parses, validates, formats, exports and renders a
project. It also analyses audio and converts MIDI. See
[Command line](manual/cli.md).

The desktop app is a Tauri 2 window over the same engine. It has a timeline,
piano roll, mixer, device rack, patch editor, plugin browser, DSL editor and AI
panel. See [The window](manual/gui.md).

The twenty crates of the Rust workspace implement the language, the command
line and the app. See [Component reference](components/index.md).

## How this site is organised

The sidebar groups pages by the four documentation kinds of
[Diátaxis](https://diataxis.fr), so you can tell what a page is for before you
open it. Tutorials take you from a fresh checkout to a working render. How-to
guides solve one concrete problem, such as a blank window, a failing render or
a broken check. Reference pages give the exact syntax, control name,
subcommand, type or tool. Explanation pages describe why the engine is built
the way it is.

`cargo doc` generates the API reference from the source, and the site publishes
it at [`api/`](api/index.html). The API reference is the authority for every
public type and function. The written pages describe behaviour and link into
it.

## Where to go next

- To build the CLI and render a sample project, read
  [Getting started](manual/getting-started.md).
- To write or read a project by hand, read [The .ocp file](manual/dsl.md).
- Before you change a crate, read its entry in the
  [Component reference](components/index.md).
- To follow a project on its way to a WAV file, read
  [The engine pipeline](architecture.md).
- To drive the running app from a script or a model, read
  [Agent surface](agent-surface.md).
- To work on the repository itself, read [Contributing](contributing.md) and
  [Testing and verification](testing.md).
- For the wording rules of this site, read [Documentation style](style.md).

## The full map

The tables below list the product and design documents in `docs/`. Each one is
also a chapter of this site. A design document says how a feature works and
why, and is meant to be read next to the code. Measurement logs and design
provenance stay outside the repository.

Read [Compatibility and originality](compatibility.md) first. It sets what may
be written where, and every synthesis and device document assumes it.

### Sound and instruments

| Doc | What it answers |
|---|---|
| [`patch-system.md`](patch-system.md) | What a patch is, and how the DSL, the synth and the GUI share it. |
| [`modulation.md`](modulation.md) | Envelopes, the curve LFOs (480-point shapes, generators, sync ladder) and the mod matrix. |
| [`filter-character.md`](filter-character.md) | The filter families, the damping and drive laws, the resonance ceiling. |
| [`osc-granular-spectral.md`](osc-granular-spectral.md) | The granular and spectral source oscillators. |
| [`synth-character.md`](synth-character.md) | Oscillator readout, unison, filters, modulation and sample layers. |
| [`fx-laws.md`](fx-laws.md) | Signal flow and effect order in the synth rack. |
| [`fx-delay-distortion.md`](fx-delay-distortion.md) | The delay line, its time mapping and reported latency; the multi-stage shaper. |
| [`fx-hyperd-conv.md`](fx-hyperd-conv.md) | The widener and the convolution insert. |
| [`fx-utils-split-bode.md`](fx-utils-split-bode.md) | Utility processing, signal splits and frequency shifting. |
| [`drum-kit.md`](drum-kit.md) | The pad-grid instrument: layers, choke groups, round-robin. |
| [`drum-synthesis.md`](drum-synthesis.md) | Synthesized drum voices: what a pad without a sample layer does today, and the voice design that replaces it. |
| [`sampler.md`](sampler.md) | The sampler and multisample engine. |
| [`sf2-player.md`](sf2-player.md) | The SoundFont 2 reader and player: chunks, generator subset, level laws, what it does not do. |
| [`factory-presets.md`](factory-presets.md) | The 66 OpenSynth factory patches and what each one is built from. |

### Devices

| Doc | What it answers |
|---|---|
| [`vocal-chain.md`](vocal-chain.md) | The vocal chain order and its rationale; the latency ledger. Start here. |
| [`vocal-analysis.md`](vocal-analysis.md) | The offline monophonic pitch analyzer: frame and note contract, cache key, measured accuracy and CPU. |
| [`vocal-tune.md`](vocal-tune.md) | The `@vocal_tune` note model: stored decisions, target resolution, the amount=0 pass-through and `ocp vocal apply`. |
| [`vocal-comp.md`](vocal-comp.md) | The leveler (including `ocp level`) and the optical compressor. |
| [`vocal-tone.md`](vocal-tone.md) | The iron stage and the passive EQ. |
| [`vocal-fx.md`](vocal-fx.md) | The doubler, the air exciter and the ducked send. |
| [`vocal-fet.md`](vocal-fet.md) | The FET compressor: the channel-conductance law that fixes ratio, knee and distortion. |
| [`deesser-modes.md`](deesser-modes.md) | The de-esser's relative-threshold, full-band, detector-placement, selectable-band, automatic-band and segment-gate modes, with the bit-identity and latency proofs. |
| [`plosive.md`](plosive.md) | The plosive detector and dynamic low shelf: thresholds, band shape, measured acceptance, zero latency. |
| [`resonance.md`](resonance.md) | The spectral resonance suppressor: STFT, prominence detector, cepstral minimum-phase FIR, measured depth and cost. |
| [`multiband.md`](multiband.md) | The multiband compander and its crossovers. |
| [`limiter.md`](limiter.md) | The true-peak mastering limiter: ceiling, lookahead, release and the measured gain reduction. |
| [`eq-editor.md`](eq-editor.md) | The parametric EQ and the curve editor, including the dynamic bands. |
| [`fx-compressor.md`](fx-compressor.md) | The synth rack's three-band RMS compander, the lookahead limiter at its output, and the mixer compressor's detector sidechain EQ. |
| [`fx-reverb-fdn.md`](fx-reverb-fdn.md) | The synth rack's FDN reverb: topology, the two band-decay trims and the recorded failure modes. |
| [`breath-split.md`](breath-split.md) | The breath splitter: a complementary linear-phase split plus the detector that decides which path dominates. |
| [`tuner.md`](tuner.md) | The scale-snapped pitch corrector: YIN detection, note tracking and resynthesis. |
| [`reverb-fdn.md`](reverb-fdn.md) | The long-form FDN reverb: two networks, per-band decay and the early-reflection table. |
| [`plugin-support.md`](plugin-support.md) | Hosted plugins: how parameters are discovered and modelled, what "verified" means, and the verified list. |
| [`devices/params/compression.md`](devices/params/compression.md) | Per-device parameter tables for the seven long-form signal devices: name, unit, range, default and scale. |

### Projects, arrangement and editing

| Doc | What it answers |
|---|---|
| [`arrangement.md`](arrangement.md) | Sections, repeats, and how the render length is decided. |
| [`autosave.md`](autosave.md) | Recovery copies: where they live, when they are written, retention, restore and discard. |
| [`export-format.md`](export-format.md) | Sample formats (`i16`/`i24`/`f32`), dither, determinism, and which setting wins. |
| [`audio-clip.md`](audio-clip.md) | Audio clips: import, peaks, trim, split, streaming. |
| [`audio-clip-stretch.md`](audio-clip-stretch.md) | Time-stretch: the derived-media cache, WSOLA, the phase vocoder. |
| [`mixer-routing.md`](mixer-routing.md) | Track, bus and master routing, sends, and the pan and balance laws. |
| [`multi-output-routing.md`](multi-output-routing.md) | Multi-output instruments and aux returns. |
| [`midi-import-export.md`](midi-import-export.md) | MIDI in and out, including the cancellable import. |
| [`midi-live.md`](midi-live.md) | Live MIDI devices and routing. |
| [`mpe.md`](mpe.md) | Per-note expression. |
| [`automation.md`](automation.md) | The three automation lane forms, their curves and smoothing, and how they reach an effect parameter. |
| [`comping.md`](comping.md) | Offline take alignment and `ocp clip align`: onsets, offset, stretch, polarity and the comp crossfade. |
| [`dsl-compat.md`](dsl-compat.md) | Retired effect shorthand the parser still accepts, and the warning it prints. |
| [`playback-latency.md`](playback-latency.md) | How a space press, seek or preview reaches its first block, and what each start path costs. |

### Platform and product

| Doc | What it answers |
|---|---|
| [`branding.md`](branding.md) | The mark, the window chrome and the wordmark. |
| [`plugin-index.md`](plugin-index.md) | The remote plugin catalogue: format, signing, publishing. |
| [`security.md`](security.md) | Credential storage and the agent's permission model. |
| [`release.md`](release.md) | How to build the bundle, smoke test it, sign and notarise it, and publish an update. |
| [`agent-loop.md`](agent-loop.md) | The agent's analysis, range-render and transaction tools. |
| [`content-and-programs.md`](content-and-programs.md) | Content-library registration and plugin program selection, with the per-format capability table and the known limitations. |
| [`au-hosting.md`](au-hosting.md) | AudioUnit (AUv2) hosting in process: which components render offline and which stay scan-only. |
| [`vst2.md`](vst2.md) | The VST 2.4 host: discovery, parameters, presets and the known limits. |
| [`vst3-editor.md`](vst3-editor.md) | The VST 3 editor: the host contracts it needs and the lock-free monitor that carries its audio. |
| [`vst3-conformance.md`](vst3-conformance.md) | The per-bundle conformance matrix: the battery, the child process, the JSON schema and the contract decisions. |
| [`diagnostics.md`](diagnostics.md) | The diagnostics switches: which build can report what, and which flag turns it on. |
| [`compatibility.md`](compatibility.md) | Compatibility scope and the originality rules for code and docs. |

### Measurements

| Doc | What it answers |
|---|---|
| [`performance-targets.md`](performance-targets.md) | The throughput and latency targets, the reference session and the measurement protocol. |
