# Automation

Three lane forms exist. They share one idea — a position/value list evaluated
against project time — but differ in what they drive.

| Form | Owner | Drives | Resolution |
| --- | --- | --- | --- |
| `automation:` control lane | track | an instrument control or a host parameter of the track's instrument | event stream from the interpreter |
| `automation:` effect lane | track, bus, master | one parameter of one effect in that owner's chain | per-block graph wrapper |
| `gain_automation:` clip | track, bus, master | post-insert linear gain | sample-accurate gain stage |

The rest of this document covers the effect lane; control lanes are described in
the DSL manual and gain clips in `docs/mixer-routing.md`.

## Effect lanes

```ocp
@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-18dB, ratio=4
    - plugin:"vst3.MyCompressor"
  automation: compression.threshold {1:1=-24dB, 3:1=-6dB}
  automation: fx2.Mix [ppq=480] smoothing=25ms {1:1=50%, 5:1=80% curve=smooth}

@bus drums
  effects:
    - eq: mid_freq=1kHz
  automation: eq.mid_freq {1:1=800Hz, 9:1=3000Hz}

@master
  effects:
    - limiter: ceiling=-1dB
  automation: limiter.ceiling smoothing=10ms {1:1=-3dB curve=stairs, 7:1=-1dB}
```

A lane can carry a `smoothing=` time and each point can carry a `curve=`
and, for `curve=smooth`, a `tension=`. Both are absent by default: 0 ms and
`linear`. A `curve` describes the segment from its own point to the next one;
`stairs` holds the value until the next point, the shape every lane had before
curves existed.

The lane name is `<effect>.<parameter>`. Two effect addresses are accepted:

* **By type name.** `compression`, `eq`, `limiter`, `stereo_width`,
  `ducked_send`, `plugin`, … the underscore form of the DSL type. This is only
  valid when exactly one entry of that type is in the chain; two compressors
  make `compression.threshold` ambiguous.
* **By one-based chain position.** `fx2` is the second entry of that owner's
  `effects:` list. This is the only form that addresses one plugin among
  several, and the form to use when a type repeats.

Lanes address the owner's **own** `effects:` list. Effects contributed by a
preset are not addressable from the track; the preset carries no automation of
its own.

For built-in effects the parameter must be a parameter id of that device's
schema (`ocp devices list` prints them). The value is validated in the
device's own unit and range, exactly like the same parameter in the `effects:`
entry. For plugin effects the parameter name is matched by the plugin
(normalised: case and punctuation are ignored) and the value is passed in the
plugin's plain domain, the same domain the `effects:` entry uses. A plugin
parameter that does not exist or cannot be automated produces no change.

## Time and value domain

Points use the same positions as control lanes: `bar:beat:subdivision`, or
`[ppq=N]` tick positions. A segment follows its curve from one point to the
next. A lane without a time span — the whole-timeline form — keeps the value
declared in `effects:` before its first point and holds the last value after
its last point. Every point is compiled to an absolute sample position at graph
build time.

| `curve` | Segment value at position `u` in 0..1 |
| --- | --- |
| `linear` (default) | `y0 + (y1 - y0)·u` |
| `stairs` | `y0` (the next point steps) |
| `half_sine` | `y0 + (y1 - y0)·(1 - cos(πu)) / 2` |
| `smooth` | `y0 + (y1 - y0)·(u + t·(3u² - 2u³ - u))`, `t` = `tension` in 0..1 (default 1) |

The value is evaluated analytically at each sample; nothing is read from a
table. A lane's `smoothing=` adds a one-pole low-pass on that target:

    y[n] = a·y[n-1] + (1 - a)·target[n],   a = exp(-1 / (smoothing_s · sample_rate))

so a step reaches 63.2 % of its height in `smoothing_ms` at any sample rate.
The smoother advances one sample at a time from absolute sample positions, not
from block boundaries, so a 64-frame and a 512-frame render of the same project
produce the same values. A seek reseeds it on the target, like the value
re-application below.

Rendering is per block:

* A plugin effect (VST3/CLAP) receives every changed value that falls inside
  the block as a parameter change with a **block-relative sample offset**,
  through the same `pending_events` queue the MIDI parameter path uses. A point
  at `1:1:2` in a 512-frame block that starts at sample 0 lands at offset 100,
  not at the block boundary.
* A built-in device has no event queue, so the block is split at every sample
  whose value differs from the previous one and `set_param` runs at each split.
  A `stairs` lane without smoothing still splits only at its points; a
  continuous curve or a smoothing transient splits per sample.
* A seek or reset re-applies the value the lane holds at the new position, so a
  lane resumed mid-timeline does not keep a stale parameter.
* A plugin is handed chunks no larger than its processing limit (and split at
  tempo/meter changes), so every queued offset falls inside the block the
  device is about to process.

The wrapper reports the wrapped effect's latency, tail, sidechain and meters
unchanged, so insert chains keep their PDC and metering.

## Automation clips

A lane may carry a time span. With one it is a **clip**: the same point list,
the same curve rules, but positioned and bounded in time. Without one it keeps
the whole-timeline meaning it has always had.

```ocp
@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-18dB, ratio=4
  automation: compression.threshold [3:1 - 5:1] {4:1=-24dB, 4:4=-6dB}
  automation: fx1.Mix [3:1 - 5:1] muted {3:1=100%, 5:1=60%}
```

The span is `[start - end]`, the same range syntax sections, clips and
`gain_automation:` use, with the **end exclusive**: `[3:1 - 5:1]` writes for bars
3 and 4. `muted` is a flag with no value, like `linear` and `step` on
`gain_automation:`; it appears after the span and silences the whole lane. A
lane keeps its optional `[ppq=N]`, `mode=`, `smoothing=` and cycle controls, and
they sit after the span in the serialized form.

### What a clip writes

The span decides *when* the lane writes; the point list decides *what*.

* **Before the span** the lane writes nothing, so the parameter keeps the value
  its `effects:` entry declares.
* **Inside the span, before the first point** the lane holds the first point's
  value. A clip whose points start after its head therefore opens on its
  authored opening value rather than waiting for the first anchor.
* **Inside the span, after the last point** the lane holds the last value.
* **At the end of the span** the lane stops writing. The value it wrote last
  stays on the parameter: a clip **writes and holds**, it does not revert.
* **A muted lane** writes nothing at any position, so the parameter keeps its
  declared value everywhere.
* **An LFO lane's cycle origin is the clip's head**, not its first point, so the
  wave restarts in phase at the head and a repeated clip does not carry a phase
  offset from the previous play. The points still describe one cycle, and their
  own shape is unchanged.

The release rule is a deliberate asymmetry with `gain_automation:`. A gain clip
returns to unity outside its range because unity is the value the mixer stage
declares for it. A cutoff, a ratio or a mix cannot snap back to a declared value
without an audible jump, so an automation clip leaves the parameter where the
clip ended. Read the two forms as: gain clips are gain stages, automation clips
are parameter positions.

A point may sit outside the span. It never writes there, but it still shapes the
curve inside: a point before the head sets the opening interpolation, and a point
after the end pulls the last segment towards its value even though the lane stops
writing at the end.

Two lanes on one `(owner, target)` may not write at the same time, so clips on
one target must not overlap and a clip cannot share its target with a
whole-timeline lane. Adjacent edges are allowed; the rules and their messages
are in [Validation](#validation).

## Reading a lane back

`oc_dsl::automation::value_at` is the one evaluator: it compiles a lane's points
against the project clock and returns the value at a quarter-note position,
covering the four curves, per-point tension, LFO phase and its cycle controls.
It returns no value wherever the lane writes nothing: a muted lane, a position
before the first point of a whole-timeline lane, and a position outside a clip's
span. The renderer samples the same curve code; only the one-pole smoother and
the LFO's sample clock are stateful, and they stay in the renderer. A lane's
`smoothing_ms` is therefore a property of the target the smoother is heading
for, not of the number a read-out returns.

The GUI reads it through `automation_value_at(target, lane_param, lane_index,
position_beats)`. The command answers either a driven value — in the effect's
own unit, or the normalized 0..1 position for a patch or hosted-plugin lane — or
the reason no lane drives the control: the target has no lane, the parameter
names more than one lane, the owner is muted, the lane is muted, the transport is
stopped, or the playhead is outside what the lane writes. Panels keep showing
the stored value in those cases, and mark a driven control with a badge and a
dashed ring that disappear when the lane stops driving or a drag takes over.

## Writing automation

The transport bar carries one **Write automation** button — the `W` beside the
record button — and the same action is a command in the registry, so it is
rebindable and appears in the command palette. It is one session-wide arm shared
by every surface: the OpenSynth editor, the device rack knobs and the
hosted-plugin parameter panel read the same state. The arm is session state; it
never reaches the project, the DSL or a saved file.

With the arm on, a drag on a knob records a curve:

* **While the transport rolls** the writer samples on every transport poll
  (about 20 Hz) and on every pointer move, then snaps each sample to the timeline
  grid. Each grid cell keeps the last value that landed in it, so a still finger
  records a horizontal line and a slow move records a curve.
* **While the transport is stopped** the same gesture writes one point at the
  playhead.
* A press that never changes the parameter writes nothing at all.

Sampling is bounded by the poll. A point position is stored as
`bar:beat:subdivision`, so the finest writable step is a 16th note: at 120 bpm a
1/16 is 125 ms against a 50 ms poll, which makes 1/16 and every coarser grid
complete. A finer grid (1/32, triplets) rounds each sample to the nearest
storable cell, which is a 1/16, so a finer grid records the nearest sample rather
than a fabricated intermediate.

A drag is **one undo entry**, labelled `write automation`, however many points it
produced. A drag that writes no point records nothing: no undo entry, no dirty
flag.

A parameter with no lane gets one on the first point, addressed by chain position
(`fx<N>.<param>`), so a lane can start from the knob that is being moved. A
parameter the owner cannot automate — a preset chain, a parameter with no
numeric range, or a chain entry that is a hosted plugin — is refused with the
device schema's sentence, and the ordinary edit runs instead.

After the gesture the status bar reports what happened:

```
Recorded 12 points on filter.cutoff (1/16 grid).
Recorded 1 point at 3:2:3.
Nothing was recorded: automation write is off.
```

## Lane identity

One owner may carry several lanes on one target, so a lane is keyed by
`(owner, lane_param, lane_index)`. Every editing command in the GUI takes the
optional lane index; without one the parameter must name exactly one lane. A
parameter with two lanes is refused with `Automation target 'filter.cutoff' has
2 lanes (0, 1); pass a lane index to choose one` instead of editing the first one
silently. `automation_value_at` follows the same rule and answers with that
sentence as its reason. The commands that were written before spans existed keep
their signature and reject an ambiguous parameter the same way.

## The timeline row

Each track header has an **A** button, and the track menu has **Show
automation**; both fold the automation rows under the track. Bus and master
lanes are drawn after the last track lane, under a strip that names the owner,
because those owners have no track header. A row draws every lane of its owner,
effect, patch and control lanes alike, and labels the target kind so a
`pluginparam_…` name reads as the parameter it encodes.

The row is where a point is authored: double-click adds one at the snapped
position, dragging moves one in both axes with a ghost, `Delete` removes the
selected one, and the point menu offers the four curves and a tension value.
Each of those is one command and therefore one undo entry; the drag writes once
on release, not per pointer move. A lane whose `muted` is set refuses every one
of those edits with `This lane is muted.` rather than dropping it. Moving a point
outside the lane's span is allowed, because a span gates writes, not authoring.

The row's y axis is the range the lane's target accepts, so the first point can
be placed anywhere in that range and a drag stops at its edges instead of
creeping past them. An effect lane reads its device schema, a `pluginparam_`
lane reads the patch table's physical domain, and a `volume` or `pan` lane reads
the mixer's own gain or pan range. A target with no known range — a hosted
plugin, or a control the device table does not list — scales to the points the
lane carries.

The row menu's **Delete** and the device rack's lane row both call
`remove_automation_lane`, so every lane has a delete action, with or without a
span. Deleting a lane that is playing removes it from the next graph, and undo
restores it.

### Clip gestures

A clip block on the timeline row is moved, resized, copied, split and deleted
through five commands. Each is keyed by the owner (`track:N`, `bus:name`,
`master`), the lane parameter and the lane index, and each is exactly one undo
entry. Every one of them re-runs validation before it publishes, so a project
that would stop validating is never adopted and the refusal is the validator's
own message.

| Command | Meaning |
|---|---|
| `move_automation_clip(owner, lane_param, lane_index, to)` | shift the span and every point by one delta |
| `resize_automation_clip(owner, lane_param, lane_index, edge, to)` | move one edge to `to`; the other edge stays |
| `duplicate_automation_clip(owner, lane_param, lane_index, at)` | copy the span and points to the first free position at or after `at` |
| `split_automation_clip(owner, lane_param, lane_index, at)` | cut the clip in two at `at` |
| `delete_automation_clip(owner, lane_param, lane_index)` | remove the clip lane |
| `remove_automation_lane(owner, lane_param, lane_index)` | remove one lane, with or without a span |

The rules those commands keep:

* **Move** shifts the span and every point by the same delta, so the curve keeps
  its shape and the points keep their order. A move whose new span would overlap
  another lane on the same target is refused with both spans in the message.
* **Resize** moves one edge and leaves the other edge and every point where they
  are. A resize that would cross the other edge, or overlap another lane, is
  refused with the reason. The points are never rewritten, so a point left
  outside a shrunken span still shapes the curve inside it: a point before the
  head sets the opening interpolation and a point after the end pulls the last
  segment, exactly as an authored point outside a span always has.
* **Duplicate** copies the span length, the points, the mute flag and the curve
  controls to a new lane on the same target, with every point shifted by the same
  delta, and appends it to the owner's lanes. When the requested position is
  taken the search steps right past each blocker; when no free position exists
  within 64 bars of the request, or another lane drives the target over the whole
  timeline, the copy is refused with that window in the message.
* **Split** requires a position strictly inside the span and at least one block
  grid step (a 16th note) from either edge. The left lane keeps the head and
  gains a closing point at the seam holding the value the whole clip wrote there;
  the right lane starts at the seam with the same value as its first point, so
  the curve is continuous and both halves write what the clip wrote at the seam.
  The right lane keeps every point at or after the seam, the left keeps every
  point before it, and the right lane is inserted directly after the left one.
  The seam value is interpolated in the lane's own units — the point list's
  units — because that is what a point carries. A waveform (`mode=lfo`) lane is
  refused: its points describe one cycle, and each half would restart that cycle
  at its own head.
* **Delete** removes a lane that has a span. A whole-timeline lane is not a clip,
  so these gestures refuse it with that reason; its own delete is
  `remove_automation_lane`, which removes either form and is one undo entry.
* A lane on its own tick grid (`[ppq=N]`) is refused by all five: the gestures
  address the project's notated grid, and a tick-grid lane reads its positions in
  ticks.

## Validation

`ocp validate` and `oc-dsl` validation reject a lane that names

* an effect the owner's chain does not have, naming the chain's effects,
* a type that matches more than one entry (use `fx<N>`),
* a chain position outside the list,
* a parameter the addressed built-in device does not expose, listing the ids,
* a `smoothing=` outside 0..60000 ms, or a `tension=` without `curve=smooth`
  or outside 0..1,
* a curve or smoothing on a bare control lane: a MIDI control lane is an event
  step list. A built-in patch-parameter lane ramps linearly between its points,
* a `[start - end]` span whose end is not after its start,
* two lanes on one `(owner, target)` that write at the same time. Two clips
  overlap when their spans intersect; a clip and a whole-timeline lane always
  overlap, because the whole-timeline lane has no end. Adjacent edges are
  allowed, so `[1:1 - 3:1]` and `[3:1 - 5:1]` hand over at `3:1`. The message
  names the target and both spans, and reports a whole-timeline lane as "over
  the whole timeline".

A `muted` lane is validated like any other: its span, its points, its curve
rules and its overlap are checked as if the mute were lifted.

Plugin parameter names are not known until the plugin is hosted, so they are
accepted at validation time and resolved when the effect processes.

## Limits

* A continuous curve on a built-in device sets its parameter at every sample,
  so it costs more than a `stairs` lane; the per-sample split is what keeps the
  render exact and block-size independent.
* Lane offsets are timeline positions, not compensated for the insert's own
  latency; a look-ahead device applies the change when it reaches it.
* Preset effects and pattern effects are not addressable. Pattern effects are
  already rejected as pattern content.
* Effect lanes are not exported to MIDI and do not produce instrument events.
* MIDI export of a control lane still writes every authored point: it ignores
  the clip span and the `muted` flag. A muted or clipped control lane therefore
  exports the same controller events as a whole-timeline lane.
* The clip gestures above are the timeline's; the agent-facing commands, MIDI
  export and the device rack do not move a span. A `[ppq=N]` lane and a waveform
  lane have no clip gesture, as described under [Clip gestures](#clip-gestures).
* A duplicate reserves its slot from the lane list it sees: two duplicates that
  run at the same moment can both pick the same free position. The validator
  refuses the second one, so the project stays valid and the second copy is not
  written.

## Control links

A control link is one control fanning out to many parameters. A control is a
MIDI CC, an OpenSynth macro, or an automation lane; every target carries its own
mapping formula and its own smoothing time.

```ocp
@control_link drive
  from: cc(74)
  -> track: lead param: filter.cutoff formula: midi_unit smoothing: 8ms
  -> track: lead param: output.gain formula: exponential

@control_link pad_macro
  from: macro(3)
  -> track: pad param: filter.resonance formula: logarithmic smoothing: 40ms
```

`from:` accepts `cc(controller[, channel])` (channel is 1-based, default 1),
`macro(index)` (1-8), and `lane(track, param)`, which reads one registered
automation lane's normalized value. A target names a `track` and a `param`;
`param` is an engine parameter key or a hosted plugin parameter name (quote it
when it contains spaces). `smoothing:` is optional and in milliseconds.

The link is a record; the legacy single-target spelling `target: track: ...`
still parses and loads as a one-entry fan-out, and a JSON `target` field is
merged into `targets` the same way.

### Formulas

The formula list is an enum with documented mappings. Every result is clamped
to the normalized 0..1 parameter domain.

| Name | Input | Mapping |
| --- | --- | --- |
| `linear` | normalized 0..1 | `y = x` |
| `inverted` | normalized 0..1 | `y = 1 - x` |
| `exponential` | normalized 0..1 | `y = x²` |
| `logarithmic` | normalized 0..1 | `y = √x` |
| `midi_unit` | raw 0..127 | `y = raw / 127` |
| `midi_centered` | raw 0..127 | triangle peaking at 64: `raw/64` below, `(127 - raw)/63` above |

`midi_unit` and `midi_centered` read the raw controller value, so `ocp validate`
refuses them on a macro or lane source. The other four read the control's
normalized position, which is `raw / 127` for a CC.

### Smoothing

Each target smooths independently with a one-pole filter over the block:

```
coefficient = 1 - exp(-frames / (seconds * sample_rate))
value += (goal - value) * coefficient
```

`0 ms` returns a coefficient of 1, a hard over, so a link without smoothing is
a step change like the lanes it replaces. One time constant covers 63.2% of the
remaining distance.

### Learn and last tweaked

The editor arms learn for one target, then the next CC creates the link for that
controller and writes exactly one row for the target. Learning the same target
again keeps that row and its tuning instead of appending; a link with no rows
left is removed. The panel remembers the parameter the operator moved last, so
"bind last tweaked" adds it to an existing control in one action (or arms learn
for a new CC).

### Limits

* Links are headless data plus a runtime in `oc-engine::control_link`; live MIDI
  CC dispatch into the playback graph is not wired yet.
* A target's smoothing runs at the rate the caller advances it. The runtime is
  block-based so the smoothing time does not depend on the block size.
* Link rows are not exported to MIDI and do not create automation lanes.

## Touch override

While the transport streams, dragging an automated parameter takes it over: the
hand value wins and the lane is suppressed. Releasing hands the parameter back
through a **fixed 20 ms crossfade** (not configurable) so a 20 dB hand-back
cannot click. A touch is live state, never a project edit — it writes no lane
point and changes no declared value, and offline rendering always evaluates the
lane as authored.

The rule is one function in `oc-engine::touch` (`TouchLane::plan`), shared by
effect-parameter lanes and `gain_automation:` clips. The GUI shows it as an
accent ring on the knob. Only an effect parameter that a lane actually addresses
is taken over; anything else keeps the ordinary edit path.

A held touch suspends the lane's one-pole rather than only muting it: the lane
keeps its clock, but its filtered value stays at the last sample the lane owned,
so the smoother cannot run ahead behind the hand. On release the 20 ms crossfade
blends the hand into the lane's resumed output, and from there the lane's own
`smoothing=` time constant carries the move to the current curve value. The
crossfade is a hand-back, never a second smoother: a 100 ms lane still takes
100 ms to reach a target that moved during the hold, and the fade stays 20 ms at
every lane setting.
