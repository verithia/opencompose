# Mixer routing: sends, taps, bus strips and channel overrides

Status: implemented in the DSL, the offline/streaming audio graph and the mixer
GUI. `.ocp` v0.2 documents without the new keys keep their old meaning.

```
instrument → preset FX → track insert FX → PDC → polarity
   ├─ pre send  (tap) ─► send pan ─► send level ─► bus input
   └─ fader + pan
         ├─ post send (tap) ─► send pan ─► send level ─► bus input
         └─ main route ─► master / output bus

bus input ─► bus FX (+ gain automation) ─► return fader ─► mute/solo
   └─► routes (buses) / master
```

## 0. Device inputs: audio key and control

A device can declare more than one input. The device catalog (`devices_catalog`)
prints each device's `ports`; a port carries either `audio` or `control`, and a
port with `sums: false` is silent by construction (it never reaches the output
sum).

`compression` declares three inputs: `in` (the chain audio, sums), `key` (an
audio detector input, does not sum) and `control` (one normalized `0..1` value
per frame, does not sum). A channel `route:` line connects one of them:

```ocp
@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-24dB, ratio=4:1
  route: control kick_env -> compression
```

`route: kick -> compression` is the same line with the default `audio` kind and
feeds the device's key input instead. A device takes one detector input, so the
two kinds are alternatives on the same effect.

A bus chain can take its detector from a track, which ducks the whole return
from that track's level without copying the key into the bus:

```ocp
@bus low
  routes: [master]
  effects:
    - compression: threshold=-35dB, ratio=8:1, attack=1ms, release=40ms
  route: kick -> compression
```

Measured with a 65 Hz bass sent to `low` and 1 kHz key hits on `kick`, the bass
return drops about 4 dB on each hit and returns to unity between them. The same
bus without the route self-compresses to about −6 dB under the bass alone, so
the drop above is the key's work and not the bass triggering its own detector.
The rule is not band-selective: it attenuates the whole return.

Bus and master chains accept the same line. A control route is a graph edge,
not a field on the effect: the routing graph draws it with its own dashed line
and a diamond end, dragging it onto a device with no control port is refused
with a message, and the inspector's control row disconnects it. Control is
evaluated after its feed track's chain and before the consumer's chain, so it
is sample-accurate; it does not create a signal-path cycle, and one source may
fan out to several consumers. A track cannot feed its own control input.

## 1. The send tap point

A channel send taps the **post-insert channel signal**:

```
instrument → preset FX → track insert FX → PDC → polarity → ┬─ pre send ─► bus
                                                            │
                                              fader + pan ──┴─ main route ─► master / bus
                                                            └─ post send ─► bus
```

Rules, in order:

1. The tap is taken after the track's preset effects, its insert chain and PDC,
   so every send carries the channel inserts.
2. Polarity inversion is applied before the tap, so both pre and post sends
   carry it.
3. `pre` sends tap **before the fader and pan**. They carry only the send pan
   and send level; the channel fader and pan do not reach them.
4. `post` sends (the default) tap **after the fader and pan**. They follow both,
   including fader automation, and therefore also carry the pan law. The send's
   own pan is an extra balance stage on top of the channel pan.
5. The main route (master or an exclusive `output` bus) always passes through
   fader and pan regardless of send taps.
6. A send never includes another channel's signal, the bus chain, or the master
   chain. Plugin auxiliary outputs are a separate tap: `send(<owner>.outN -> bus)`
   is captured pre-insert and pre-fader, see `docs/multi-output-routing.md`.
   Streaming playback captures the same tap.

The tap is summed into the target bus buffer once per block. PDC aligns the send
path to the bus input, so a send that carries insert latency waits for the rest
of the bus instead of leading it. See §9 for the reference PDC uses.

**Gain moves.** The channel fader and pan, the send level, the bus return and the
master gain each move to a newly written value over 10 ms at the point they are
applied. A block is 4096 frames (about 85 ms) during streaming, so a control that
stepped once per block would be heard as a click; the ramp turns the same edit
into a short slope and lands exactly on the written value. A gain that does not
move is applied as a single constant, so a project without a live edit renders
the samples it rendered before the ramp existed. The mute and solo gate moves on
the same ramp, and a channel that has finished fading out is skipped instead of
ramping its silence into the next block. This stage is separate from automation:
a gain clip keeps its own per-sample curve, and the manual ramp only starts on a
manual change, so a moving lane is never smoothed twice.

## 2. Send level and headroom

The project stores the send as a linear gain `amount` in `0.0..=4.0` so old
documents keep their exact value. `1.0` is 0 dB; `4.0` is +12.02 dB. The mixer
and DSL present it in dB:

```
level_db = 20 * log10(amount)
amount   = 10 ^ (level_db / 20)
```

- `amount = 0` (−inf) is silence.
- Every renderer applies exactly one gain: `bus += tap * amount`. For a post
  send the tap already contains the fader and pan; for a pre send it does not.
  The send level is never applied twice and the channel pan law is never
  re-applied behind the send pan.
- A send and the channel's main route are parallel paths; the channel is not
  removed from the master when it sends.
- **Headroom rule.** The engine mixes in `f32` and applies no internal clipping;
  the WAV writer clamps only at the final conversion. A send may therefore reach
  +12 dB, but the rule is that the **bus return, after its own fader, must stay
  at or below 0 dBFS**. A send above unity is a deliberate drive into the bus
  effects (a saturator or compressor input), not a level control; compensate at
  the bus return fader. Raising a send above +12 dB is rejected by the validator
  and the GUI, so the worst case stays bounded at +12 dB per send.

On a track, `mix: pan_law=balance` uses stereo balance even when its two
channels contain identical samples. At center, both channels keep unity
gain. Aligned stem import sets this for stereo WAVs. The default
`pan_law=auto` detects the channel width of the post-insert audio.

## 3. Per-send pan

Each send has its own balance at the bus input, independent of the channel pan:

- `pan = 0` is exactly unity for every width, so a send written before this key
  renders bit-identically (the engine returns `(1.0, 1.0)` without arithmetic).
- **Mono tap** (a mono channel carrying the same content on both sides) uses the
  channel constant-power curve, renormalised so that centre is unity:
  `gL = √2·cos((pan+1)·π/4)`, `gR = √2·sin((pan+1)·π/4)`. Hard left is
  +3.01 dB on the left and silence on the right, and `gL² + gR²` stays constant.
- **Stereo tap** (independent left/right content) uses the channel balance law:
  `gL = clamp(1−pan, 0, 1)`, `gR = clamp(1+pan, 0, 1)`. The near side stays at
  unity and only the far side attenuates (hard pan mutes the far side; pan
  `−0.5` is `(1.0, 0.5)`, −6.02 dB on the far side).
- Plugin auxiliary sends are stereo pairs and always use the balance law.

**Mono-compatibility rule.** Both branches keep the two channels in phase, with
no delay and no polarity flip, so a mono fold-down of a send never cancels. The
mono branch is power-preserving (`L² + R²` constant); the stereo branch is a
pure attenuation of one side, so its fold-down level changes with the pan, as it
does for a stereo channel. The send pan is applied **at the bus input**, after
the tap and before the bus effects; it is not a second channel pan.

Measured: a mono send at hard left is exactly `√2` (1.41421) on the left and
`0.0` on the right, with the same total power as the centre send; a stereo send
at `−0.5` is `(1.0, 0.5)`. The render tests assert both laws sample by sample.

## 4. Bus strip: return fader, mute and solo

A bus has the same three controls as a channel, applied **at the bus return**:

| Control | Position in the chain | Range |
| --- | --- | --- |
| `vol` | after bus FX and gain automation, before mute/solo | −120…+24 dB, linear gain |
| `mute` | after the return fader | boolean |
| `solo` | part of the graph-wide solo rule | boolean |

The bus buffer is always processed (so a live unmute does not restart the FX
tail), then multiplied by the return fader. Muting a bus ramps its return to
silence over 10 ms and keeps fanning the fading tail; once the ramp has arrived
the buffer is zeroed before the stem capture and the bus is not fanned into its
routes. The return fader is not automation: `gain_automation` stays a chain
stage, so automation runs before the return fader, like a channel insert.

**Solo rule.** `any_solo` spans channels and buses:

```
any_solo = any track.solo || any bus.solo

audible(bus)   = !bus.mute && (!any_solo || bus.solo || keep(bus))
audible(track) = !track.mute
                 && (!any_solo || track.solo || track.solo_safe
                     || (any_bus_solo && track routes into a kept source bus))
```

The project header's `solo_mode` selects what a solo propagates to. The default
is `both`, which is the historical graph closure; the property is omitted from
the DSL and from serialized JSON when it holds that default.

| `solo_mode` | `keep` / source buses |
| --- | --- |
| `upstream` | the soloed buses and every bus that can reach them (their inputs); channels feeding those buses stay audible |
| `both` | everything reachable in either direction over bus edges; every channel feeding a kept bus stays audible |

The soloed node's own route to master is always kept: a soloed channel keeps its
output bus and both send taps, and a soloed bus keeps its feeds and its returns.
Soloed and solo-safe channels contribute their send targets to `keep` as well.

Destinations-only is deliberately not shipped as a third mode. A bus has no
generator: muting the buses that feed it mutes its content, and making its
returns audible without its feeds gives silence. Keeping the feeds makes it the
`both` closure, so a "downstream" setting would be either silent or identical to
`both` — a selectable mode that can never be heard. The two shipped modes are
therefore the two behaviours this graph actually has.

Sibling sources are only unmuted when a bus solo is active, so a channel-only
solo renders identically in either mode. The audible difference is at bus solos:
`both` keeps a return that hangs off a bus feeding the soloed bus, `upstream`
drops it.

Consequences, each covered by a render test:

- A soloed bus keeps its inputs audible in both modes; the input channels'
  parallel main route stays audible too (a send does not remove the channel from
  the master).
- A soloed bus is never silent: `upstream` keeps the feeds and drops the side
  return, `both` keeps the side return with its tail.
- A direct-to-master channel that does not route into the kept component is
  silenced, including its sends.
- A solo-safe channel keeps running under another channel's solo and keeps its
  bus returns running.
- `mute` wins over `solo`: a muted bus stays silent even while soloed.
- A channel solo does not silence the bus returns that carry it, and its pre and
  post send taps stay audible in either mode.
- A channel that feeds no bus still reaches master under either mode.

Measured: bus `−6 dB` scales the return by exactly `0.5011872336272722`; a muted
bus return is peak `0.0` while the direct path is untouched; a bus solo renders
sample-identically (1e-7) to the same graph with its hand-computed audible set
expressed as explicit mutes. With `src_a`/`src_b` feeding `main` through `bus_a`
and `bus_a` also feeding a reverb return, soloing `main` renders `both` minus
`upstream` with max sample delta `0.988592` and RMS delta `0.206224` (test
`upstream_drops_the_return_that_both_keeps`).

## 5. Loop rule

The routing graph is a DAG with three node kinds:

- tracks: sources, no inbound routing edges;
- buses: receive sends, route to buses or master;
- master: sink.

Edges are `track → bus` (send, auxiliary send or exclusive output),
`bus → bus` (`routes`) and `bus → master`.

A track send cannot close a cycle because tracks have no inbound edges. An
auxiliary send is an edge of the same kind, so it is covered by the same rule; a
glitch that made an auxiliary source a track input would still be caught by the
bus cycle check. A **bus route** can close a cycle: `A → B → A`. Such a route is
rejected with an error in three places:

1. the DSL validator reports `Bus routing cycle`;
2. GUI bus-route edits validate the candidate project and return the error
   without mutating the project;
3. the checked engine graph build returns `bus routing cycle: A -> B` before any
   buffer is allocated.

The renderer additionally orders buses with a bounded topological pass, so even
the unchecked legacy constructor cannot hang on a malformed graph. It is a
defensive fallback only; checked builds fail closed.

## 6. DSL

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

Every new key is optional. A v0.2 file without them parses to the legacy state:
post-fader sends, no send pan, unity bus return, no mute/solo, no polarity
inversion, no overrides. `fmt` writes a key only when it differs from that
default, so old files round-trip byte-compatible. `display_name` is presentation
only: the track `name` remains the routing and reference identity (presets,
sidechains and stems keep using it).

JSON export (`ocp parse --json`, project snapshots) exposes the same state:

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

## 7. GUI

The mixer is two columns: the selected node's inspector on the left and the
routing view on the right. The routing view is a project-level toolbar above the
bus graph. The graph is the topology (tracks, buses, master and the edges
between them). Editing is split by level: the toolbar creates bus nodes, the
inspector edits the selected node, and a dedicated connection card edits the
selected edge. Selecting an edge never changes what the top row means.

### 7.1 Routing toolbar, node actions and the connection card

- **Add a node (project level).** The `Bus` field creates a bus routed to master.
  A typed name is used as-is; an empty field takes the first free `Bus N`.
  Creation is one `add_bus` command, one undo entry, and the new node becomes
  selected. Tracks are still added by dropping a preset on the mixer; a track is
  a source, not a routing node the toolbar can invent. The toolbar holds only
  this action, so its content does not change with the selection.
- **Select a connection.** Edges are clickable and keyboard focusable
  (`Enter`/`Space`). The invisible hit stroke is 12 px wide, so the thin curve
  does not have to be hit exactly; a press that starts on an edge selects it and
  never starts a background pan. Selecting an edge selects its source node too,
  so the inspector shows the channel that owns it. Clicking a node, or the graph
  background, clears the edge selection.
- **Re-point (connection card).** Selecting an edge opens a connection card
  under the source node's controls in the inspector column. Its `To` control
  moves the selected connection: a track's exclusive main output
  (`set_track_output`), a send (`set_send_target`) or one bus route
  (`set_bus_routes`). A send keeps its level, tap and pan; a re-point cannot
  duplicate an existing send from the same tap or route the same target twice,
  and those targets are not offered.
- **Edit a send (connection card).** A send edge exposes its level in dB, its
  `PRE`/`POST` tap (channel taps only; an auxiliary tap is fixed pre-insert and
  shows `AUX`), its input pan and a remove button. `Escape` leaves the card,
  `Delete` removes the connection, and focus returns to the source node.
- **Connect (node level).** Next to the channel controls, a track adds a send to
  any bus that tap does not already feed, and a bus adds a route (master or
  another bus). Both are one command.
- **Delete a node (node level).** The delete button sits in the bus actions card
  under the channel detail, and its confirmation is a modal card rather than a
  row in the toolbar. A bus with no incoming edge is deleted immediately. A bus
  that still receives a track output, a send or a bus route is **rejected** with
  an error that names every reference; the confirmation then shows exactly what
  the explicit re-route does: track outputs that targeted it return to master,
  sends into it are dropped, and buses that routed into it return to master when
  that was their only route. Nothing is orphaned silently. The confirmation is
  one `remove_bus` command with `reroute: true`.

The channel detail below the inspector keeps the per-send list, including the
auxiliary-source picker: for a CLAP/VST3 preset the add-send row has a source
selector (`Main`, `out2`, `out3`, …) and an `AUX` button. The button loads the
instrument if needed and returns its declared stereo output pair count; the
selector then offers one entry per pair beyond the main one. A multi-output
instrument keeps its main pair unchanged: `Main` is still `out1`.

Every control commits through the project mutation/undo path once per gesture:
dB fields commit on change (not per keystroke), toggles commit on click, and a
send, route or edge re-point is one command. A bus fader, mute or solo edit is
one `update_bus_mix` command and one undo entry.

A routing edit is saved like any other: the command rewrites the `.ocp`, and the
routing operations round-trip through save/load and render the same audio as
the equivalent hand-written DSL routing.

## 8. Deliberately out of scope

- **Bus inserts.** A bus has `effects` and gain automation, but no insert-chain
  editor of its own; effects are edited in the inspector and bus effects are
  inline rather than a separate pre/post pair. A per-bus insert rack is the next
  mixer step.
- **Bus pan.** Returns are summed in stereo; there is no bus balance control.
  This keeps the solo/return path free of a second width law.
- **Bus-level sends.** A bus cannot send into another bus at a send level; the
  only bus edge is `routes`, which passes the whole return.
- **Aux outputs in streaming playback.** The offline/project renderer captures
  every requested pair; the live streaming graph still mixes the main pair only.
- **Renaming the routing identity from the mixer**; `display_name` is
  presentation only and leaves references untouched.
- **Send level above +12 dB.** The ceiling is enforced in the validator and the
  GUI; the headroom rule in section 2 explains the bound.
- **Drag-a-cable editing.** Connections are edited by selecting an edge and
  choosing a target, not by dragging a cable between nodes. The keyboard path is
  the same selection, so no operation is pointer-only.
- **Auxiliary nodes.** A plugin's extra output pairs are not project nodes; they
  are send sources (`owner.outN`) shown as `AUX` edges. Creating one needs the
  instrument's pair count, so it stays in the channel detail's source picker
  rather than the node's routing section.
- **Renaming a bus.** The bus name is its reference identity; the toolbar's
  create field is the only place it is chosen.

## 9. Latency compensation: referenced to the destination

A route is delayed by the latency its destination already carries, minus the
source's own:

```
delay(src -> dst) = latency(dst) - latency(src)       # never negative
```

`latency(track)` is the source's reported latency plus its insert chain;
`latency(bus)` is the accumulated arrival at the bus input from its sources. A
source that feeds a later destination therefore waits for that destination
instead of for the graph's slowest path. Direct routes and bus returns into
**master** are the exception: every path has to arrive on the same sample there,
so they keep referencing the global maximum. That maximum is the smallest
reference that aligns the mix without advancing a path, and it is also what
`pre_master_latency_samples` reports to the renderers.

A route whose destination already carries **more** latency than the source
cannot be made up at the source. It is not clamped silently: the plan records a
`LatencyDiagnostic { source, destination, delta }` (`delta` is
`latency(source) - latency(destination)`), the render carries it as a warning,
and `AudioGraph::validate_latency_compensation()` hands it to a validator as a
typed error. The destination's own reference absorbs the difference, so the
master mix stays sample-aligned either way.

Per channel, `mix: pdc=on | off | when_recording` selects the policy:

| Policy | Behaviour |
| --- | --- |
| `on` | compensate every route (default; a document without the key behaves this way) |
| `off` | leave this channel's output undelayed |
| `when_recording` | leave it undelayed only on a recording pass (`AudioGraph::set_recording`) |

Bypassing is per source channel: a bypassed channel's sends and direct route are
not delayed, so a monitored recording path lines up with the timeline instead of
with the master reference.

### Manual latency offset

`mix: latency=<samples>` declares extra output latency for a track (signed, in
samples). It is added to the track's reported latency:

```
effective(track) = reported(track) + latency_offset
delay(src -> dst) = max(0, latency(dst) - effective(src))
```

The offset is a calibration, not a delay effect: it tells the plan how late the
track really is (an external instrument's round trip, a plugin that reports the
wrong number). The plan then removes exactly that much from the route, so a
correct offset leaves the destination alignment unchanged and a missing one
shows up as a fixed sample shift.

The offset only redistributes the delay the plan already applies. It is clamped
to `-reported <= offset <= min(destination_reference - reported)` over the
track's main-output routes, which is the range where every route's compensation
stays inside `[0, pre_master_latency_samples]`: no delay line grows, the master
reference does not move and the render length does not change. A requested value
outside that budget is clamped with one warning per track, and the readout
reports both numbers. Auxiliary plugin outputs bypass the track output, so they
do not constrain the offset and are not offset themselves.

The offset is part of the declared latency, so it stays applied when
`pdc=off`/`when_recording` drops the compensation; those passes simply do not
delay the route. `AudioGraph::latency_readout()` reports the three parts per
track: reported latency, requested/applied manual offset, and for each
destination the reference it is compared against plus the compensation applied
there (and the diagnostic delta, when there is one). The GUI inspector shows
that readout for the selected track through the `track_latency_readout`
command.
