# Multi-output instruments and MIDI channels

Status: implemented for offline/project rendering and for streaming playback on
the CLAP and VST3 host paths. A source that declares fewer pairs than the
project routes keeps the main pair only in streaming, with one warning per send
(see §5).

## 1. DSL

### MIDI channel

```ocp
@track Kit | preset: kontakt, instrument: keys/kontakt, channel: 4
  @phrase ride [1:1 - 5:1] | channel: 6
    C4 q
```

`channel:` is 0-based (`0..=15`). A phrase overrides its track. When neither is
present the legacy default is kept, so existing documents render identically:
track index clamped to 15, or channel 9 for `drums/*` tracks.

### Auxiliary outputs

```ocp
@track Kit | preset: kontakt, instrument: keys/kontakt
  mix: send(kit.out3 -> drums = 100%)

@bus drums
  routes: [master]
```

`send(<owner>.outN -> <bus> = <level>)` extends the existing send with a source:

- `<owner>` is the hosting track's name or its preset name.
- `outN` is a 1-based output bus, `N >= 2`; `out1` is the track's main output and
  is rejected.
- `<bus>` is an existing `@bus`; `<level>` is the usual percentage/linear amount.

The aux output is captured **pre-FX and pre-fader/pan**. Only the send level and
the send pan are applied, then the audio enters the existing bus graph (bus
effects, bus routes, master) exactly like a track send. This is why the design
is minimal: no second routing system and no new destination type — only an
optional source on `Send`.

CLAP and VST3 instruments both expose their declared stereo output pairs to the
host. The source of an auxiliary send is validated against the instrument's
declared pair count before a block is rendered: requesting a pair the plugin does
not have is an error, not silence. `out1` remains the track's main pair, so a
multi-output track that uses only the main pair renders unchanged.

### Output ports

A plugin instance's outputs are its declared ports, not one stereo pair it
occupies:

```ocp
@track out3 | preset: kontakt, instrument: keys/kontakt, port: 2
```

`port:` is 0-based and selects the declared port the track reads. A port is one
declared bus: its channel count comes from the instance, and its default target
pair is the stereo pair its first channel lands on. A bus wider than two
channels spans consecutive pairs, and `pair:` (0-based, inside that port)
overrides the default pair. Both fields are absent in projects written before
ports existed, which keeps port 0 / pair 0 exactly as before.

Tracks that name a port on the same preset share one instance; the renderer
resolves each track to a pair, requests every distinct pair in one `process()`
call, and fans them into separate track buffers. Summing those tracks is
therefore equal to rendering the instance port by port. The legacy
`plugin_output_pair` parameter (1-based absolute pair) is still honoured as the
same selection.

Hosts with one stereo pair only (VST2, LV2, AudioUnit) refuse a port or pair
beyond 0 with a typed error naming the preset and the track, instead of
silently reading the main pair.

A plugin effect's sidechain input is an index too: `sidechain_input=N` selects
the N-th declared input after the main one (0 is the default). The host feeds
that input bus only; every other declared input bus stays silent, so the
plugin's process call still sees one buffer per port.

Sends without a source are channel sends. By default they tap post-fader and
post-pan into a bus; `send(bus = <level>, pre)` taps before the fader and pan
instead. Every send, including an auxiliary send, has its own `pan` balance at
the bus input. The tap point, send law, pan law and loop rule are in
`docs/mixer-routing.md`. Old projects keep the post-fader default and render
unchanged.

## 2. Bus negotiation

`Vst3Instance::load` queries `IComponent::getBusCount/getBusInfo` once and caches
one channel count per declared output bus. `set_component_state_bytes` (used for
embedded VST3 state and `.nkm`/preset state) re-queries and reactivates the buses
once after `setState`; it never runs per block.

- `output_bus_count()` — declared audio output buses.
- `output_bus_channels()` — channels per bus, in VST3 order.
- `output_ports()` / `input_ports()` — the declared buses as `AudioPort`s: channel
  count plus the default target stereo pair; empty inputs for instruments.
- `sidechain_input_count()` / `sidechain_input_bus(index)` — declared sidechain
  inputs and the absolute bus an index names.
- `output_count()` — stereo pairs available to the host (`ceil(channels / 2)`).
- `process_block_outputs_into(frames, transport, pairs, outputs)` — one
  `process()` call, appends interleaved `[left, right]` for every requested pair.

Pair `0` is the main output, so `outN` maps to pair `N - 1`. Buses wider than two
channels are flattened into consecutive stereo pairs; Kontakt's 16 stereo buses
map 1:1.

The CLAP host reads the same topology from the `audio-ports` extension:
`ClapInstance::output_count()` sums the declared output channel counts and
rounds up to stereo pairs. `process_block_with_transport_and_output_pairs` keeps
one `clap_process` call per block and fans out every requested pair, exactly like
the VST3 path, so the main pair is still pair `0`.

The planar channel buffers, per-bus pointer arrays and `AudioBusBuffers` are
allocated when the topology is negotiated, then reused every block. No bus query,
lock or buffer allocation happens in the render path because of multi-output.
(The event/parameter COM wrappers are allocated per block as before; that is a
separate pre-existing host limitation.)

## 3. Renderer integration

`RenderCache` resolves each track's declared port to a stereo pair before the
block loop, asks the project which aux pairs the track also needs, renders every
pair from the same shared instance in the same block, and caches aux pairs per
`(track, pair)`. This is shared by the CLAP and VST3 groups: one `process()`
call per block, one cache entry per requested pair, and a track with no port
still reads pair 0. The audio graph adds each aux route to its target bus with
the same PDC alignment as a regular send. Aux buffers are not passed through the
track effect chain, so track inserts affect the main output only.

Streaming playback reuses the same contract. The GUI transport graph renders
source *lanes* (one instrument instance per pattern/clip) rather than whole
tracks, so `RenderState` plans each lane's pairs from its track's sends once per
load and keeps them in `oc_engine::block_source::SourceOutput`: one preallocated
interleaved stereo buffer per pair, plus the pair list handed to the plugin. Each
block renders the main pair and every planned pair in one `process()` call
(`process_block_outputs_into` on both hosts, which appends into caller-reserved
buffers), and the mixer receives the same `(track, pair)` map offline passes to
`mix_processed_with_stems_and_aux`. The return therefore obeys the same send
level, send pan, bus mute/solo and master path; only the pair buffers differ
between the two paths.

The parity test renders one in-process two-bus project through both paths and
compares the auxiliary return sample by sample: the level agrees within 1e-6
absolute. That tolerance is f32 rounding on a constant plugin output — both
paths use the same send law and the same graph code, so only the block partition
differs and the measured difference is zero; a non-constant fixture would need a
wider window, not a different law.

Two details are streaming-specific. First, a lane's aux pairs bypass that lane's
chain, while the graph's PDC uses the lane's full latency, so the streaming path
holds each pair in a preallocated delay line for `track latency - plugin
latency` samples; that keeps a return fed by a latent plugin, or by a lane with a
lookahead effect, aligned with its main pair. Second, the pair buffers are zeroed
and refilled per block, so the steady render path allocates nothing for the fan
out; the buffers are rebuilt only when a mixer edit changes the planned pairs.

## 4. GUI

The channel strip's fader row shows a compact `Port` selector when the
instrument declares more than one output port; it lists the ports in declaration
order and writes `output_port` through one `set_track_output_port` mutation. The
list comes from `preset_output_ports` without loading the instrument; loading it
once (the `AUX` button, or transport) refreshes the list.

The add-send row lists a `Main` source plus one entry per declared auxiliary
pair. The `AUX` button asks the backend (`preset_output_pairs`) for the
instrument's pair count, loading the plugin once if it is not already loaded; the
selector then offers `out2…outN`. Creating the send is one `add_send` mutation
with `source_owner`/`source_output`, and the row shows the source and an `AUX`
badge. The main pair stays `Main` and is never rewritten by an aux send.

## 5. Still missing

- Host automation / CC-to-parameter mapping for aux levels and instrument slots.
- Pairs a streaming source cannot serve. The host activates every declared
  output bus at load, so a plugin that reports its extra outputs only after
  activation is already final before the plan is built. The remaining case is a
  project that routes a port or pair the loaded instance does not declare:
  streaming keeps pair 0 for that track and logs one warning per track per load,
  while offline rendering fails the project. AudioUnit, VST2 and LV2 hosts are
  one-pair by design, so a port or aux request beyond 0 takes this path.
- `setBusArrangements` negotiation; the host relies on the plugin's default
  stereo layout, and Kontakt's own output configuration must already be in the
  loaded state.
- MIDI 2.0 / per-note MPE routing beyond the existing profile path.
