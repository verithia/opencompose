# Parametric EQ curve editor

The `dynamic-eq` device panel is a curve editor: a log-frequency magnitude plot
with one node per band, a per-band list, and a selected-band inspector. The
fixed three-band `eq` device keeps its smaller curve view.

Source files:

| Path | Role |
| --- | --- |
| `crates/oc-gui/frontend/src/devices/biquad.ts` | The only TS biquad design and magnitude implementation. |
| `crates/oc-gui/frontend/src/devices/parametricEq.ts` | Band model, solo/channel rules, summed response curves. |
| `crates/oc-gui/frontend/src/components/ParametricEqEditor.tsx` | Canvas, nodes, band list, inspector, gestures. |
| `crates/oc-dsp/src/biquad.rs` | Rust biquad design and `response_db`. |
| `crates/oc-dsp/src/dynamic_eq.rs` | Six bands plus low/high cuts, dynamic detector, solo and channel targets. |
| `crates/oc-engine/src/spectrum.rs` | FFT analyser tap: folding, smoothing, reset. |
| `crates/oc-gui/frontend/src/stores/spectrum.ts` | Panel-scoped analyser subscription. |
| `crates/oc-dsp/tests/suite/eq_response_fixture.rs` | Generates and guards the shared response table. |

## Device model

The dynamic EQ has six band slots and two dedicated cut slots. "Add band"
enables the first free slot; "remove" disables a slot while keeping its values.
The type menu offers bell, low shelf, high shelf, low cut, high cut, notch, band
pass, and a "Dynamic" entry that selects a bell plus the dynamic detector.
Dynamic is a mode layered on a shape, so a band converted to a dynamic shelf in
the DSL keeps its shape and shows the dynamic badge.

Per band the editor exposes enable, type, frequency, Q, gain, dynamic
threshold/range/attack/release, solo, and a channel target (`stereo`, `mid`,
`side`, `left`, `right`). Cuts expose enable, frequency, and Q. Bypass sits in
the header. Solo is real processing state: when any enabled band is soloed only
soloed bands are processed, and the drawn curve follows the same rule.

The dynamic range of a band is drawn as a dashed overlay at `gain + range`,
the steady state the detector reaches at full drive. The solid curve is the
static response at `gain`.

## Interaction model

| Gesture | Behavior |
| --- | --- |
| Drag a node | Changes frequency (log X) and gain (linear Y). Cut nodes change frequency only. |
| Wheel over a node | Multiplies Q by `exp(-deltaY * 0.0015)`, clamped to the schema range. |
| Alt-drag a dynamic band | Changes `range` by `-dy * 0.25` dB/pixel, clamped to ±24 dB. |
| Double-click a node | Resets frequency, gain, Q (and range for dynamic bands) to schema defaults. |
| Click / focus a node | Selects it and opens the inspector. |
| Arrow keys on a focused node | Left/right shift frequency by 1% (shift: 10%); up/down shift gain by 0.5 dB (shift: 5 dB). |
| Enable | Toggles the band slot; disabling also clears solo. |
| Solo | Solos the band and enables it; other enabled bands dim and stop processing. |
| Type | Sets the filter shape; choosing a shape clears the dynamic flag, choosing Dynamic sets bell + dynamic. |
| Channel | Writes the band's `channel` enum; the canvas follows the channel view selector. |
| Channel view | `All` sums every audible band; `L`/`R`/`M`/`S` keep stereo bands plus bands targeting that component. |
| Add / remove | Enables the first free slot / disables the slot. |

Hover is shown with a halo, selection with a brighter halo, disabled bands are
dimmed, and non-soloed bands are dimmed while a solo is active. While dragging,
a readout bubble shows frequency + gain (or the range during an Alt-drag).

### Undo

Every gesture is committed in one batch through `set_effect_params`, which is a
single project mutation and therefore a single undo entry. During a drag the
editor keeps a local draft, redraws from it, and sends nothing over IPC; the
commit happens on mouse-up. Keyboard nudges and inspector fields commit one
batch per change. Wheel bursts are coalesced for 300 ms before committing, so a
trackpad flick is one edit rather than dozens.

## Live spectrum analyser

The canvas can draw a real-time FFT spectrum behind the grid. The toggle is
per panel and off by default; the engine only runs the FFT while at least one
panel holds a subscription. Closing the panel releases it through
`stores/spectrum.ts` and the render thread stops analysing on the next chunk.

The analysis runs where the audio already is: `AudioGraph` keeps an optional
`SpectrumAnalyser` and taps the selected node's **pre-effects** signal in the
same pass that mixes it. The tap targets are a track input (before its insert
chain), a bus input (before the bus chain), or the master input (before the
master chain), selected with the `set_spectrum` Tauri command. Offline renders
never construct an analyser.

| Property | Value |
| --- | --- |
| FFT | 2048-point Hann-windowed, forward complex FFT; 23.4 Hz per bin at 48 kHz |
| Overlap | 50 % (hop 1024 frames); a new analysis every 21.3 ms, 46.9/s at 48 kHz |
| Display bands | 128 log-spaced bands from 20 Hz to 20 kHz, about 1/12.8 octave |
| Band fold | Peak FFT bin in the band (tone-accurate dBFS, no Hann main-lobe double count) |
| Smoothing | Per band, one-pole in dB: attack 40 ms, release 250 ms |
| Range | −96 dBFS floor; 0 dBFS is drawn at +24 dB on the curve axis |
| Allocation | All buffers and the FFT scratch exist after construction; `push` allocates nothing |
| Cost | 13.97 µs per analysis, 0.065 % of one core, about 1527× realtime (debug build) |

The curve and the analyser share the log-frequency mapping and the dB-per-pixel
scale. The analyser has its own 48 dB window: display dB = dBFS + 24, so
0 dBFS sits on the +24 dB gridline and −48 dBFS on the −24 dB line. Spectrum
values are clamped to the plot.

### Gain-reduction display

`meter_snapshot` publishes a signed gain per dynamic band next to the existing
per-effect gain reduction. The render thread copies
`AudioEffect::band_gain_db` into atomic slots after each mixed chunk; the UI
polls at 10 Hz (`stores/meters.ts`). The editor adds each band's current offset
to its static gain, draws the resulting **current** response, and fills the area
between the static and current curves. The band list shows the same value as
`−3.4 dB` next to the range. Disabled bands and bands below the detector
threshold read `0 dB`, so the fill disappears when nothing is moving.

## Sidechain

The dynamic EQ accepts an external detector key. The DSL key is the existing
per-effect source (`dynamic_eq: sidechain=kick`). A track chain may key another
track; a bus or master chain may key a track or another bus. Each dynamic band
then opts in with `bandN_sidechain=1`, so one device can have some bands on its
own signal and some on the key. The inspector exposes the switch as "External
key" and the band list marks opted-in bands with `SC`.

Routing is real, not a UI state:

1. `AudioGraph` collects a tap of every key source at the **post-insert,
   pre-fader, pre-mute** point: the source's buffer immediately after its own
   effect chain, before polarity, routing, the fader, the ride and the mute
   gate. A bus source is tapped after its chain and before its return fader.
2. `SidechainEffect` wraps the device, aligns main and key paths through the
   automatic delay compensation plan, and calls
   `DynamicEq::process_with_sidechain`.
3. `DynamicEq` filters the key through each opted-in band's own biquad (using a
   separate filter state) and feeds that level to the band's detector. With no
   key present the band falls back to self detection.
4. The key never enters the audio output: it is only a detector input.

The header shows `SC` next to the source selector whenever a source is set; it
is highlighted when at least one enabled dynamic band uses it. One source
applies to the whole device; per-band sources are deliberately out of scope.

## Auto-gain

Auto-gain keeps the device output as loud as its input while bands move. It is
a header toggle (`auto_gain=1`), off by default.

The detector measures K-weighted power on the input and on the EQ output before
makeup, using the analytic BS.1770 stage-1 high shelf (1682 Hz, +4 dB) and
stage-2 high pass (38.1 Hz, Q 0.5003). Power is integrated with a 400 ms
one-pole window; the applied linear gain is the square root of the input/output
power ratio, smoothed with a 150 ms one-pole and clamped to ±24 dB. Makeup is
applied after the bands and cuts, so it does not change the drawn response and
does not feed back into the measurement. `DynamicEq::auto_gain_db` reports the
current makeup for meters and tests.

Measured: a 1 kHz tone through a dynamic band at `−12 dB` range holds
integrated loudness within **0.5 LU** of the input (BS.1770-4 gated integrated
loudness, 8 s, 48 kHz); without auto-gain the same band drops the output by
about 12 LU.

### Deliberate omissions

No M/S spectrum, no per-band analyser traces, no band crossovers, no per-band
sidechain sources, and no drag gestures beyond frequency, gain, and dynamic
range. The spectrum is not persisted with the project. Undo grouping does not
span separate gestures (two drags are two undo entries).

## Coordinate mapping

The canvas uses a fixed `460 × 260` viewBox with the plot at
`left = 44`, `top = 14`, `width = 404`, `height = 206`.

Frequency is logarithmic from 20 Hz to 20 kHz:

```
x = left + log(freq / 20) / log(1000) * width
freq = 20 * 1000 ^ ((x - left) / width)
```

Gain is linear from −24 dB to +24 dB:

```
y = top + (1 - (db + 24) / 48) * height
db = -24 + (1 - (y - top) / height) * 48
```

Grid lines are drawn at −12, −6, 0, +6, +12 dB and at 100 Hz, 1 kHz, 10 kHz,
with 20 Hz and 20 kHz labels at the plot edges. Dragged frequencies are
quantized to 0.1 Hz below 100 Hz, 1 Hz from 100 Hz to 1 kHz, and 5 Hz above
1 kHz; gains are quantized to 0.1 dB. The SVG is clipped to the plot rectangle
so curves near the ±24 dB limits stay inside the frame.

## Curve computation

`biquad.ts` mirrors `oc-dsp/src/biquad.rs` line for line: the same frequency
clamp (`20 .. sampleRate * 0.45`), the same `q >= 0.1` clamp, the same RBJ
coefficient sets (including band-pass), the same `a0` normalization, and the
same complex evaluation with the same zero/denominator guards. There is no
second coefficient implementation in the GUI; the legacy three-band curve
(`eqResponse.ts`) delegates to the same module.

Shape mapping used by both sides:

```
bell -> peak            low_shelf -> low_shelf     high_shelf -> high_shelf
low_cut -> high_pass    high_cut -> low_pass       notch -> notch
band_pass -> band_pass
```

The response is summed in dB across audible bands. The editor draws at 48 kHz,
the engine's default playback rate; the device response is sample-rate
dependent at the top end, so the constant is explicit (`CURVE_SAMPLE_RATE`).

## Verification

The Rust integration test `crates/oc-dsp/tests/suite/eq_response_fixture.rs` builds
the table both ways at 32 log-spaced frequencies from 20 Hz to 20 kHz:

- `analytic_db`: `BiquadCoefficients::design(...).response_db(...)` summed per
  band — the same functions the device uses.
- `measured_db`: a sine sweep through a real `DynamicEq` instance, with 0.75 s
  of settling and a Hann-windowed single-bin projection over the last 0.5 s.

The test asserts measured vs analytic stays within **0.1 dB** at every point.
The fixture also stores the per-case band definitions so the frontend test can
rebuild the exact same response:

`crates/oc-gui/frontend/src/devices/__fixtures__/eqResponseTable.json`

The vitest `src/__tests__/devices/parametricEq.test.ts` reloads that fixture,
runs `responseDb` from `parametricEq.ts`, and asserts the drawn TS curve is
within **0.1 dB** of both the Rust analytic column and the measured column at
all 32 points for every case (flat, single bell boost/cut, both shelves, both
cuts, notch + band-pass, and a six-band mastering chain). It also re-checks
measured vs analytic, so the table cannot silently drift.

Regenerate the fixture after changing the DSP (and commit it):

```sh
OC_EQ_RESPONSE_FIXTURE=write cargo test -p oc-dsp --test all devices::eq_response_fixture::
```

Without the variable the test compares the committed file byte for byte and
fails if it is stale. Current worst cases: measured vs analytic 0.00006 dB,
TS vs measured 0.00006 dB, TS vs Rust analytic 1.7e-10 dB (float noise).

Analyser tests cover display-band edges and monotonicity, the peak band for a
1 kHz tone, deterministic and monotonic attack/release, reset-to-floor, an
allocation counter around `push`, and a CPU sanity print. Sidechain coverage has
a chain-level `checked_dynamic_eq_sidechain_routes_the_key_into_the_detector`
(loud key ducks the band, silent key leaves the audio bit-identical) plus
`external_sidechain_drives_flagged_bands_without_leaking_into_audio` in the
device. Auto-gain has a tonal-level check in the device and an integrated
loudness test in `crates/oc-engine/tests/suite/dynamic_eq_loudness.rs`.

Run the verification:

```sh
cargo test -p oc-dsp --test all devices::eq_response_fixture::
cargo test -p oc-engine --test fx dynamic_eq_loudness::
cargo test -p oc-engine --test all spectrum_alloc::
cargo test -p oc-engine --lib spectrum
cd crates/oc-gui/frontend && npx vitest run src/__tests__/devices/parametricEq.test.ts
```
