# Mixer routing

Sends, bus returns and per-track overrides work the same way in the DSL, in
the offline and streaming audio graph, and in the mixer. A `.ocp` v0.2
document that does not use the keys on this page keeps its old meaning.

The signal flow for one track and one bus:

```
instrument → preset FX → track insert FX → PDC → polarity
   ├─ pre send  (tap) ─► send pan ─► send level ─► bus input
   └─ fader + pan
         ├─ post send (tap) ─► send pan ─► send level ─► bus input
         └─ main route ─► master / output bus

bus input ─► bus FX (+ gain automation) ─► return fader ─► mute/solo
   └─► routes (buses) / master
```

## Device inputs: audio key and control

A device can declare more than one input. The device catalog
(`devices_catalog`) lists each device's `ports`. A port carries either `audio`
or `control`. A port with `sums: false` never reaches the output sum, so it is
silent by construction.

`compression` declares three inputs:

- `in`, the chain audio, which sums;
- `key`, an audio detector input, which does not sum;
- `control`, one normalized `0..1` value per frame, which does not sum.

A `route:` line on a track connects one of them:

```ocp
@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-24dB, ratio=4:1
  route: control kick_env -> compression
```

`route: kick -> compression` uses the default `audio` kind and feeds the key
input. A device takes one detector input, so on a given effect you choose one
kind or the other.

A bus chain can take its detector from a track. The whole return then ducks
under that track's level, and the key is not copied into the bus:

```ocp
@bus low
  routes: [master]
  effects:
    - compression: threshold=-35dB, ratio=8:1, attack=1ms, release=40ms
  route: kick -> compression
```

In a measurement with a 65 Hz bass sent to `low` and 1 kHz key hits on `kick`,
the bass return drops about 4 dB on each hit and comes back to unity between
hits. Without the route, the same bus compresses itself to about −6 dB under
the bass alone. The drop on each hit therefore comes from the key, and the bass
does not trigger its own detector. The route is not band-selective: it
attenuates the whole return.

Bus and master chains accept the same line. A control route is an edge in the
routing graph and is not stored on the effect. The graph draws it as a dashed
line with a diamond end. Dropping it on a device without a control port is
refused with a message, and the control row in the inspector disconnects it.

Control is evaluated after the feeding track's chain and before the consuming
chain, so it is sample-accurate. It does not create a cycle in the signal path,
and one source can feed several consumers. A track cannot feed its own control
input.

## The send tap point

A channel send taps the post-insert channel signal:

```
instrument → preset FX → track insert FX → PDC → polarity → ┬─ pre send ─► bus
                                                            │
                                              fader + pan ──┴─ main route ─► master / bus
                                                            └─ post send ─► bus
```

The rules, in signal order:

1. The tap comes after the track's preset effects, its insert chain and PDC.
   Every send carries the channel inserts.
2. Polarity inversion happens before the tap, so pre and post sends both carry
   it.
3. A `pre` send taps before the fader and pan. It carries only the send pan and
   send level. The channel fader and pan do not reach it.
4. A `post` send, the default, taps after the fader and pan. It follows both,
   including fader automation, and so it also carries the pan law. The send's
   own pan is an extra balance stage on top of the channel pan.
5. The main route (master, or an exclusive `output` bus) always passes through
   the fader and pan, whatever the sends do.
6. A send never contains another channel's signal, the bus chain or the master
   chain. Plugin auxiliary outputs are a separate tap:
   `send(<owner>.outN -> bus)` is captured before the inserts and before the
   fader. See [Multi-output instruments](multi-output-routing.md). Streaming
   playback captures the same tap.

The tap is summed into the target bus buffer once per block. PDC aligns the
send path to the bus input, so a send that carries insert latency waits for the
rest of the bus. [Latency compensation](#latency-compensation) describes the
reference PDC uses.

### Gain changes

The channel fader and pan, the send level, the bus return and the master gain
each move to a newly written value over 10 ms, at the point where they are
applied. During streaming a block is 4096 frames (about 85 ms). A control that
stepped once per block would click. The ramp turns the same edit into a short
slope that lands exactly on the written value.

A gain that does not move is applied as one constant. A project with no live
edit therefore renders the same samples as a graph without the ramp stage. The
mute and solo gate uses the same ramp. A channel that has finished fading out
is skipped, so its silence is not ramped into the next block.

The ramp is separate from automation. A gain clip keeps its own per-sample
curve, and the manual ramp starts only on a manual change, so a moving lane is
never smoothed twice.

## Send level and headroom

The project stores a send as a linear gain `amount` in `0.0..=4.0`, so older
documents keep their exact value. `1.0` is 0 dB and `4.0` is +12.02 dB. The
mixer and the DSL show the level in dB:

```
level_db = 20 * log10(amount)
amount   = 10 ^ (level_db / 20)
```

- `amount = 0` (−inf) is silence.
- Every renderer applies one gain: `bus += tap * amount`. For a post send the
  tap already contains the fader and pan; for a pre send it does not. The send
  level is never applied twice, and the channel pan law is never applied again
  behind the send pan.
- A send runs in parallel with the channel's main route. Sending does not take
  the channel out of the master.

The engine mixes in `f32` and does not clip internally. The WAV writer clamps
only at the final conversion. A send can reach +12 dB, but the bus return,
after its own fader, has to stay at or below 0 dBFS. Use a send above unity to
drive the bus effects harder (a saturator or a compressor input) and bring the
level back down at the bus return fader. The validator and the GUI reject a
send above +12 dB, so the worst case is +12 dB per send.

On a track, `mix: pan_law=balance` uses stereo balance even when the two
channels hold identical samples. At center both channels stay at unity gain.
Aligned stem import sets this for stereo WAVs. The default, `pan_law=auto`,
detects the channel width of the post-insert audio.

## Per-send pan

Each send has its own balance at the bus input, independent of the channel
pan.

`pan = 0` is exactly unity for every width. The engine returns `(1.0, 1.0)`
without any arithmetic, so a send with no `pan` adds no processing and renders
bit-identically to an unpanned send.

A mono tap (a mono channel with the same content on both sides) uses the
channel's constant-power curve, renormalized so that center is unity:

```
gL = √2·cos((pan+1)·π/4)
gR = √2·sin((pan+1)·π/4)
```

Hard left is +3.01 dB on the left and silence on the right, and `gL² + gR²`
stays constant.

A stereo tap (independent left and right content) uses the channel balance
law:

```
gL = clamp(1−pan, 0, 1)
gR = clamp(1+pan, 0, 1)
```

The near side stays at unity and only the far side is attenuated. A hard pan
mutes the far side; pan `−0.5` gives `(1.0, 0.5)`, which is −6.02 dB on the far
side.

Plugin auxiliary sends are stereo pairs and always use the balance law.

### Mono compatibility

Both laws keep the two channels in phase, with no delay and no polarity flip,
so a mono fold-down of a send never cancels. The mono law preserves power
(`L² + R²` is constant). The stereo law attenuates one side, so its fold-down
level changes with the pan, as it does for a stereo channel. The send pan is
applied at the bus input, after the tap and before the bus effects. It does not
act as a second channel pan.

A mono send at hard left measures exactly `√2` (1.41421) on the left and `0.0`
on the right, with the same total power as the center send. A stereo send at
`−0.5` measures `(1.0, 0.5)`. The render tests check both laws sample by
sample.

## Bus strip: return fader, mute and solo

A bus has the same three controls as a channel, applied at the bus return:

| Control | Position in the chain | Range |
| --- | --- | --- |
| `vol` | after bus FX and gain automation, before mute/solo | −120…+24 dB, linear gain |
| `mute` | after the return fader | boolean |
| `solo` | part of the graph-wide solo rule | boolean |

The bus buffer is always processed, so unmuting during playback does not
restart the effect tail. The buffer is then multiplied by the return fader.

Muting a bus ramps its return to silence over 10 ms and keeps sending the
fading tail on to its routes. Once the ramp has finished, the buffer is zeroed
before the stem capture and the bus is no longer passed to its routes.

The return fader is not automation. `gain_automation` stays a chain stage, so
automation runs before the return fader, as a channel insert does.

### The solo rule

`any_solo` covers channels and buses:

```
any_solo = any track.solo || any bus.solo

audible(bus)   = !bus.mute && (!any_solo || bus.solo || keep(bus))
audible(track) = !track.mute
                 && (!any_solo || track.solo || track.solo_safe
                     || (any_bus_solo && track routes into a kept source bus))
```

The project header's `solo_mode` sets what a solo propagates to. The default is
`both`, the full graph closure. The DSL and the serialized JSON omit the
property when it has this default value.

| `solo_mode` | `keep` / source buses |
| --- | --- |
| `upstream` | the soloed buses and every bus that can reach them (their inputs); channels feeding those buses stay audible |
| `both` | everything reachable in either direction over bus edges; every channel feeding a kept bus stays audible |

The soloed node's own route to master is always kept. A soloed channel keeps
its output bus and both send taps, and a soloed bus keeps its feeds and its
returns. Soloed and solo-safe channels also add their send targets to `keep`.

There is no destinations-only mode. A bus generates no sound of its own:
muting the buses that feed it mutes its content, and keeping its returns
audible without its feeds gives silence. Keeping the feeds as well makes it the
`both` closure. A "downstream" mode would therefore be either silent or the
same as `both`, so the two shipped modes cover every behaviour the graph has.

Sibling sources are unmuted only while a bus solo is active, so a channel-only
solo renders the same in either mode. You hear the difference on bus solos:
`both` keeps a return that hangs off a bus feeding the soloed bus, and
`upstream` drops it.

Each of these consequences has a render test:

- A soloed bus keeps its inputs audible in both modes. The input channels'
  parallel main route also stays audible, because a send does not take the
  channel out of the master.
- A soloed bus is never silent. `upstream` keeps the feeds and drops the side
  return; `both` keeps the side return with its tail.
- A direct-to-master channel that does not route into the kept component is
  silenced, sends included.
- A solo-safe channel keeps running under another channel's solo, and its bus
  returns keep running too.
- `mute` wins over `solo`: a muted bus stays silent even while soloed.
- A channel solo does not silence the bus returns that carry the channel, and
  its pre and post send taps stay audible in either mode.
- A channel that feeds no bus reaches master in either mode.

Measured values:

- A bus at `−6 dB` scales its return by exactly `0.5011872336272722`.
- A muted bus return has a peak of `0.0`, and the direct path is untouched.
- A bus solo renders sample-identically (within 1e-7) to the same graph with
  the hand-computed audible set written as explicit mutes.
- With `src_a` and `src_b` feeding `main` through `bus_a`, and `bus_a` also
  feeding a reverb return, soloing `main` gives a `both` minus `upstream`
  difference of `0.988592` max sample delta and `0.206224` RMS delta (test
  `upstream_drops_the_return_that_both_keeps`).

## Loop rule

The routing graph is a DAG with three kinds of node:

- tracks: sources, with no inbound routing edges;
- buses: receive sends, and route to buses or master;
- master: the sink.

The edges are `track → bus` (send, auxiliary send or exclusive output),
`bus → bus` (`routes`) and `bus → master`.

A track send cannot close a cycle, because tracks have no inbound edges. An
auxiliary send is the same kind of edge and follows the same rule. If a fault
ever made an auxiliary source a track input, the bus cycle check would still
catch it.

A bus route can close a cycle: `A → B → A`. Such a route is rejected with an
error in three places:

1. The DSL validator reports `Bus routing cycle detected`.
2. A bus-route edit in the GUI validates the candidate project and returns the
   error without changing the project.
3. The checked engine graph build returns `bus routing cycle: A -> B` before it
   allocates any buffer.

The renderer also orders buses with a bounded topological pass, so the
unchecked legacy constructor cannot hang on a malformed graph. This is only a
defensive fallback. Checked builds fail closed.

## DSL

```ocp
@track keys | instrument: synth/lead_square
  mix: vol=-6dB, pan=0.25, send(space=-6dB, pre), send(delay=25%, pan=-0.4), polarity=invert, display_name="Keys Wide", color="#ff8800", solo_safe=true

@bus space
  mix: vol=-3dB, mute=false, solo=true
  routes: [master]
@bus delay
  routes: [master]
```

| Key | Values | Default |
| --- | --- | --- |
| `send(bus=<level>)` | percentage, linear number, or dB up to +12 dB | post-fader |
| `send(bus=<level>, pre)` | `pre` or explicit `post` | `post` |
| `send(bus=<level>, pan=<x>)` | signed number, `-1.0`…`1.0` | `0` (unity) |
| `send(<owner>.outN->bus=<level>)` | plugin auxiliary output, `N >= 2` | channel tap |
| `polarity=` | `invert`, `normal`, `true`, `false` | `normal` |
| `display_name=` | quoted string, 1-64 chars | track name |
| `color=` | quoted CSS colour, 1-64 chars | strip palette |
| `solo_safe=` | `true` / `false` | `false` |
| bus `mix: vol=<dB>` | −120…+24 dB | `0` dB |
| bus `mix: mute=true` | boolean | `false` |
| bus `mix: solo=true` | boolean | `false` |

Every key is optional. A v0.2 file without them parses to the legacy state:
post-fader sends, no send pan, unity bus return, no mute or solo, no polarity
inversion and no overrides. `fmt` writes a key only when it differs from that
default, so old files round-trip byte for byte.

`display_name` only changes what the mixer shows. The track `name` stays the
identity for routing and references: presets, sidechains and stems keep using
it.

JSON export (`ocp parse --json`, project snapshots) carries the same state:

```json
{
  "mix": {
    "sends": [{
      "target": "space",
      "amount": 0.5011872336272722,
      "pre_fader": true,
      "pan": -0.4
    }],
    "polarity_inverted": true,
    "overrides": { "display_name": "Keys Wide", "color": "#ff8800", "solo_safe": true }
  },
  "buses": [{
    "name": "space",
    "routes": ["master"],
    "effects": [],
    "gain_automation": [],
    "volume_db": -3.0,
    "mute": false,
    "solo": true
  }]
}
```

## GUI

The mixer has two columns. On the left is the inspector for the selected node;
on the right is the routing view. The routing view is a project-level toolbar
above the bus graph, and the graph shows the topology: tracks, buses, master
and the edges between them.

Each level has its own place to edit. The toolbar creates buses, the inspector
edits the selected node, and a connection card edits the selected edge.
Selecting an edge does not change what the toolbar does.

### Routing toolbar, node actions and the connection card

Create a bus:

1. Type a name in the **New bus name** field, or leave it empty to take the
   first free `Bus N`.
2. Press **+ Bus** or **Enter**.

The new bus routes to master and becomes the selected node. Creating it is one
`add_bus` command and one undo entry. The toolbar holds only this action, so
its content does not change with the selection. To add a track, drop a preset
on the mixer: a track is a source, and the toolbar does not create one.

Select a connection by clicking an edge, or by focusing it and pressing
**Enter** or **Space**. The invisible hit stroke is 12 px wide, so you do not
have to hit the thin curve exactly. A press that starts on an edge selects it
and never starts a background pan. Selecting an edge also selects its source
node, so the inspector shows the channel that owns the edge. Clicking a node or
the graph background clears the edge selection.

Selecting an edge opens a connection card under the source node's controls in
the inspector column:

- **To** moves the selected connection. Depending on the edge, this is a
  track's exclusive main output (`set_track_output`), a send
  (`set_send_target`) or one bus route (`set_bus_routes`). A moved send keeps
  its level, tap and pan. The list leaves out targets that would duplicate an
  existing send from the same tap or route to the same target twice.
- A send edge shows its level in dB, its **PRE**/**POST** tap, its input pan
  and a remove button. Only channel taps can switch between **PRE** and
  **POST**; an auxiliary tap is fixed before the inserts and shows **AUX**.
- **Escape** leaves the card, **Delete** (or **Backspace**) removes the
  connection, and focus returns to the source node.

Next to the channel controls, a track can add a send to any bus that the tap
does not already feed, and a bus can add a route to master or to another bus.
Each is one command.

The **Delete bus** button sits in the **Bus actions** card under the channel
detail. A bus with no incoming edge is deleted at once. A bus that still
receives a track output, a send or a bus route is refused with an error that
names every reference. A confirmation card then shows what **Re-route &
delete** does:

- track outputs that pointed at the bus return to master;
- sends into the bus are dropped;
- buses that routed into it return to master, if that was their only route.

Nothing is left orphaned without notice. The confirmation runs one
`remove_bus` command with `reroute: true`.

The channel detail below the inspector keeps the per-send list, including the
auxiliary-source picker. For a CLAP or VST3 preset, the add-send row has a
source selector (**Main**, `out2`, `out3`, …) and an **AUX** button. The button
loads the instrument if needed and returns its declared number of stereo output
pairs. The selector then offers one entry for each pair after the main one. A
multi-output instrument keeps its main pair: **Main** is still `out1`.

Every control commits through the project's mutation and undo path once per
gesture. dB fields commit on change, not on each keystroke. Toggles commit on
click. A send, a route or an edge re-point is one command. A bus fader, mute or
solo edit is one `update_bus_mix` command and one undo entry.

A routing edit is saved like any other edit: the command rewrites the `.ocp`.
Routing operations round-trip through save and load, and render the same audio
as the equivalent routing written by hand in the DSL.

## Out of scope

- Bus inserts. A bus has `effects` and gain automation but no insert-chain
  editor of its own. You edit its effects in the inspector, and bus effects are
  one inline chain with no separate pre/post pair. There is no per-bus insert
  rack.
- Bus pan. Returns are summed in stereo and there is no bus balance control.
  This keeps a second width law out of the solo and return path.
- Bus-level sends. A bus cannot send into another bus at a send level. The only
  bus edge is `routes`, which passes the whole return.
- Renaming the routing identity from the mixer. `display_name` changes only the
  label and leaves references untouched.
- Send levels above +12 dB. The validator and the GUI enforce the ceiling; see
  [Send level and headroom](#send-level-and-headroom).
- Dragging audio cables. You edit an audio connection by selecting its edge and
  choosing a target. The keyboard uses the same selection, so every operation
  works without a pointer.
- Auxiliary nodes. A plugin's extra output pairs are not project nodes. They
  are send sources (`owner.outN`) drawn as **AUX** edges. Creating one needs the
  instrument's pair count, so it lives in the source picker of the channel
  detail and not in the node's routing section.
- Renaming a bus. The bus name is its reference identity, and the toolbar's
  create field is the only place you choose it.

## Latency compensation

A route is delayed by the latency its destination already carries, minus the
source's own:

```
delay(src -> dst) = latency(dst) - latency(src)       # never negative
```

`latency(track)` is the source's reported latency plus its insert chain.
`latency(bus)` is the accumulated arrival time at the bus input from its
sources. A source that feeds a later destination waits for that destination,
and not for the slowest path in the graph.

Direct routes and bus returns into master are the exception. Every path has to
arrive at master on the same sample, so these routes keep the global maximum as
their reference. That maximum is the smallest reference that aligns the mix
without advancing any path, and it is the value `pre_master_latency_samples`
reports to the renderers.

If a route's destination already carries more latency than the source, the
source cannot make up the difference. The plan does not clamp this silently. It
records a `LatencyDiagnostic { source, destination, delta }`, where `delta` is
`latency(source) - latency(destination)`. The render carries it as a warning,
and `AudioGraph::validate_latency_compensation()` passes it to a validator as a
typed error. The destination's own reference absorbs the difference, so the
master mix stays sample-aligned in either case.

On each channel, `mix: pdc=on | off | when_recording` selects the policy:

| Policy | Behaviour |
| --- | --- |
| `on` | compensate every route (default; a document without the key behaves this way) |
| `off` | leave this channel's output undelayed |
| `when_recording` | leave it undelayed only on a recording pass (`AudioGraph::set_recording`) |

The `off` and `when_recording` policies apply per source channel. When a
channel skips compensation, its sends and direct route are not delayed, so a
monitored recording path lines up with the timeline and not with the master
reference.

### Manual latency offset

`mix: latency=<samples>` declares extra output latency for a track, as a signed
number of samples. It is added to the track's reported latency:

```
effective(track) = reported(track) + latency_offset
delay(src -> dst) = max(0, latency(dst) - effective(src))
```

The offset is a calibration and does not add a delay. It tells the plan how
late the track really is, for example the round trip of an external instrument
or a plugin that reports the wrong number. The plan then removes exactly that
much delay from the route. A correct offset leaves the destination alignment
unchanged; a missing one shows up as a fixed sample shift.

The offset only redistributes delay the plan already applies. It is clamped to
`-reported <= offset <= min(destination_reference - reported)` over the track's
main-output routes. In that range every route's compensation stays inside
`[0, pre_master_latency_samples]`: no delay line grows, the master reference
does not move, and the render length does not change. A requested value outside
that range is clamped with one warning per track, and the readout reports both
numbers. Auxiliary plugin outputs bypass the track output, so they do not limit
the offset and do not receive it.

The offset is part of the declared latency, so it still applies when
`pdc=off` or `when_recording` drops the compensation; those passes do not delay
the route. `AudioGraph::latency_readout()` reports three parts per track:

- the reported latency;
- the requested and applied manual offset;
- for each destination, the reference it is compared against, the compensation
  applied there, and the diagnostic delta when there is one.

The GUI inspector shows this readout for the selected track through the
`track_latency_readout` command.
