# Automation

An automation lane is a list of position/value points evaluated against project
time. There are three lane forms, and they differ in what they drive:

| Form | Owner | Drives | Resolution |
| --- | --- | --- | --- |
| `automation:` control lane | track | an instrument control or a host parameter of the track's instrument | event stream from the interpreter |
| `automation:` effect lane | track, bus, master | one parameter of one effect in that owner's chain | per-block graph wrapper |
| `gain_automation:` clip | track, bus, master | post-insert linear gain | sample-accurate gain stage |

Most of this page is about effect lanes. Control lanes are described in
[The .ocp file](manual/dsl.md) and gain clips in
[Mixer routing](mixer-routing.md).

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

A lane can carry a `smoothing=` time. Each point can carry a `curve=`, and a
`curve=smooth` point can also carry a `tension=`. By default smoothing is 0 ms
and the curve is `linear`. A point's `curve` shapes the segment from that point
to the next one. `stairs` holds the value until the next point.

The lane name is `<effect>.<parameter>`. You can address the effect in two ways:

- By type name: `compression`, `eq`, `limiter`, `stereo_width`, `ducked_send`,
  `plugin` and so on, the underscore form of the DSL type. This works only when
  the chain holds exactly one entry of that type. With two compressors,
  `compression.threshold` is ambiguous.
- By one-based chain position: `fx2` is the second entry of the owner's
  `effects:` list. Use this form when a type repeats. It is the only way to
  address one plugin among several.

A lane addresses the owner's own `effects:` list. Effects that come from a
preset cannot be addressed from the track, and a preset carries no automation
of its own.

For a built-in effect, the parameter must be a parameter id from that device's
schema (`ocp devices list` prints them). The value is validated in the device's
unit and range, the same way as the parameter in the `effects:` entry. For a
plugin effect, the plugin matches the parameter name with case and punctuation
ignored, and the value is passed in the plugin's plain domain, the same domain
the `effects:` entry uses. A plugin parameter that does not exist or cannot be
automated produces no change.

## Time and value domain

Points use the same positions as control lanes: `bar:beat:subdivision`, or
`[ppq=N]` tick positions. Each segment follows its curve from one point to the
next. A lane without a time span (the whole-timeline form) keeps the value
declared in `effects:` before its first point and holds the last value after its
last point. Every point is compiled to an absolute sample position when the
graph is built.

| `curve` | Segment value at position `u` in 0..1 |
| --- | --- |
| `linear` (default) | `y0 + (y1 - y0)·u` |
| `stairs` | `y0` (the next point steps) |
| `half_sine` | `y0 + (y1 - y0)·(1 - cos(πu)) / 2` |
| `smooth` | `y0 + (y1 - y0)·(u + t·(3u² - 2u³ - u))`, `t` = `tension` in 0..1 (default 1) |

The value is computed from the formula at each sample; there is no lookup
table. A lane's `smoothing=` adds a one-pole low-pass on its target:

    y[n] = a·y[n-1] + (1 - a)·target[n],   a = exp(-1 / (smoothing_s · sample_rate))

A step therefore reaches 63.2 % of its height in `smoothing_ms` at any sample
rate. The smoother advances one sample at a time from absolute sample
positions, independent of block boundaries, so a 64-frame and a 512-frame render
of the same project produce the same values. A seek reseeds the smoother on the
target, in the same way as the value re-application described below.

Rendering works per block:

- A plugin effect (VST3/CLAP) receives every changed value inside the block as
  a parameter change with a block-relative sample offset, through the same
  `pending_events` queue the MIDI parameter path uses. A point at `1:1:2` in a
  512-frame block that starts at sample 0 lands at offset 100, inside the block.
- A built-in device has no event queue. The block is split at every sample whose
  value differs from the previous one, and `set_param` runs at each split. A
  `stairs` lane without smoothing splits only at its points. A continuous curve
  or a smoothing transient splits at every sample.
- A seek or reset re-applies the value the lane holds at the new position, so a
  lane that resumes mid-timeline never leaves a stale parameter.
- A plugin receives chunks no larger than its processing limit, also split at
  tempo and meter changes, so every queued offset falls inside the block the
  device is about to process.

The wrapper reports the wrapped effect's latency, tail, sidechain and meters
unchanged, so insert chains keep their delay compensation and metering.

## Automation clips

A lane can carry a time span. A lane with a span is a clip: it has the same point
list and the same curve rules, but it is placed and bounded in time. A lane
without a span covers the whole timeline.

```ocp
@track bass | instrument: synth/bass
  effects:
    - compression: threshold=-18dB, ratio=4
  automation: compression.threshold [3:1 - 5:1] {4:1=-24dB, 4:4=-6dB}
  automation: fx1.Mix [3:1 - 5:1] muted {3:1=100%, 5:1=60%}
```

The span is `[start - end]`, the same range syntax that sections, clips and
`gain_automation:` use, and the end is exclusive: `[3:1 - 5:1]` writes for bars
3 and 4. `muted` is a flag with no value, like `linear` and `step` on
`gain_automation:`. It follows the span and silences the whole lane. A clip
lane keeps the optional `[ppq=N]`, `mode=`, `smoothing=` and cycle controls,
which follow the span in the serialized form.

### What a clip writes

The span decides when the lane writes, and the point list decides what it
writes.

- Before the span, the lane writes nothing, so the parameter keeps the value its
  `effects:` entry declares.
- Inside the span and before the first point, the lane holds the first point's
  value. A clip whose points start after its head opens on that value at once.
- Inside the span and after the last point, the lane holds the last value.
- At the end of the span, the lane stops writing. The last value it wrote stays
  on the parameter: a clip writes and holds, and does not revert.
- A muted lane writes nothing anywhere, so the parameter keeps its declared
  value everywhere.
- An LFO lane's cycle starts at the clip's head, which may come before its first
  point. The wave restarts in phase at the head, and a repeated clip carries no
  phase offset from the previous play. The points still describe one cycle, with
  the same shape.

This release rule differs from `gain_automation:` on purpose. A gain clip
returns to unity outside its range, because unity is the value the mixer stage
declares for it. A cutoff, a ratio or a mix cannot snap back to a declared value
without an audible jump, so an automation clip leaves the parameter where the
clip ended. Think of gain clips as gain stages and automation clips as parameter
positions.

A point may lie outside the span. It never writes there, but it still shapes the
curve inside the span. A point before the head sets the opening interpolation,
and a point after the end pulls the last segment towards its value even though
the lane stops writing at the end.

Two lanes on one `(owner, target)` may not write at the same time. Clips on one
target must not overlap, and a clip cannot share its target with a
whole-timeline lane. Clips may touch at their edges. The rules and their
messages are under [Validation](#validation).

## Reading a lane back

`oc_dsl::automation::value_at` is the single evaluator. It compiles a lane's
points against the project clock and returns the value at a quarter-note
position, covering the four curves, per-point tension, the LFO phase and its
cycle controls. It returns no value wherever the lane writes nothing: on a muted
lane, before the first point of a whole-timeline lane and outside a clip's span.
The renderer samples the same curve code. Only the one-pole smoother and the
LFO's sample clock hold state, and they stay in the renderer. A lane's
`smoothing_ms` therefore applies to what the renderer outputs; the value that
`value_at` returns is the unsmoothed target.

The GUI reads lanes through `automation_value_at(target, lane_param,
lane_index, position_beats)`. The command answers with one of two things:

- the driven value, in the effect's own unit, or as the normalized 0..1 position
  for a patch or hosted-plugin lane;
- the reason no lane drives the control: the target has no lane, the parameter
  names more than one lane, the owner is muted, the lane is muted, the
  transport is stopped, or the playhead is outside what the lane writes.

When there is no driven value, panels show the stored value. A driven control
gets a badge and a dashed ring, which disappear when the lane stops driving it
or when you drag the control.

## Writing automation

**Write automation** is a toggle in the toolbar's **Transport settings**
popover. The same action is a registry command, so you can rebind it and run it
from the command palette. It is a single arm for the whole session. The
OpenSynth editor, the device rack knobs and the hosted-plugin parameter panel
all read it. The arm is session state and is never written to the project, the
DSL or a saved file.

With the arm on, dragging a knob records a curve:

- While the transport rolls, the writer takes a sample on every transport poll
  (about 20 Hz) and on every pointer move, then snaps each sample to the
  timeline grid. Each grid cell keeps the last value that landed in it. Holding
  the knob still records a flat line; a slow move records a curve.
- While the transport is stopped, the same gesture writes one point at the
  playhead.
- A press that never changes the parameter writes nothing.

The poll limits the sampling. Point positions are stored as
`bar:beat:subdivision`, so the finest writable step is a 16th note. At 120 bpm a
16th lasts 125 ms and the poll runs every 50 ms, so a 1/16 grid and every
coarser grid are filled completely. On a finer grid (1/32, triplets) each sample
rounds to the nearest storable cell, which is a 16th, so the lane gets the
nearest real sample and no invented values in between.

A drag is one undo entry, labelled `write automation`, however many points it
writes. A drag that writes no point adds no undo entry and does not mark the
project as changed.

If the parameter has no lane, the first point creates one, addressed by chain
position (`fx<N>.<param>`). A parameter the owner cannot automate (a preset
chain, a parameter with no numeric range, or a hosted-plugin chain entry) is
refused with the device schema's message, and the drag becomes an ordinary edit.

After the gesture the status bar reports the result:

```
Recorded 12 points on filter.cutoff (1/16 grid).
Recorded 1 point at 3:2:3.
Nothing was recorded: automation write is off.
```

## Lane identity

One owner can carry several lanes on one target, so a lane is identified by
`(owner, lane_param, lane_index)`. Every editing command in the GUI takes an
optional lane index. Without it, the parameter must name exactly one lane. A
parameter with two lanes is refused with `Automation target 'filter.cutoff' has
2 lanes (0, 1); pass a lane index to choose one`, so the first lane is never
edited by accident. `automation_value_at` follows the same rule and gives that
sentence as its reason. The commands that predate spans keep their signature
and refuse an ambiguous parameter in the same way.

## The timeline row

Choose **Show automation** in a track's menu to unfold the automation rows under
that track. Bus and master lanes appear after the last track lane, under a strip
that names the owner, because those owners have no track header. A row shows
every lane of its owner, whether effect, patch or control lane, and labels the
target kind, so a `pluginparam_…` name reads as the parameter it encodes.

You author points in the row:

- Double-click to add a point at the snapped position.
- Drag a point to move it on both axes; a ghost shows the new position.
- Press `Delete` to remove the selected point.
- Open the point menu to pick one of the four curves or set a tension.

Each of these is one command and one undo entry. A drag writes once, on release.
On a lane with `muted` set, every one of these edits is refused with
`This lane is muted.` You may move a point outside the lane's span, because the
span limits when the lane writes, and points outside it still shape the curve.

The row's vertical axis is the range the lane's target accepts. You can place
the first point anywhere in that range, and a drag stops at its edges. An effect
lane reads the range from its device schema, a `pluginparam_` lane from the
patch table's physical domain, and a `volume` or `pan` lane from the mixer's
gain or pan range. A target with no known range, such as a hosted plugin or a
control the device table does not list, scales to the points the lane holds.

**Delete** in the row menu and the **Delete lane** button on the device rack's
lane row both call `remove_automation_lane`, so you can delete any lane, with or
without a span. Deleting a lane that is playing removes it from the next graph,
and undo restores it.

### Clip gestures

On the timeline row you can move, resize, copy, split and delete a clip block.
Each gesture is one command keyed by the owner (`track:N`, `bus:name`,
`master`), the lane parameter and the lane index, and each is one undo entry.
Every command re-runs validation before it publishes. A result that would not
validate is never adopted, and the refusal carries the validator's message.

| Command | Meaning |
|---|---|
| `move_automation_clip(owner, lane_param, lane_index, to)` | shift the span and every point by one delta |
| `resize_automation_clip(owner, lane_param, lane_index, edge, to)` | move one edge to `to`; the other edge stays |
| `duplicate_automation_clip(owner, lane_param, lane_index, at)` | copy the span and points to the first free position at or after `at` |
| `split_automation_clip(owner, lane_param, lane_index, at)` | cut the clip in two at `at` |
| `delete_automation_clip(owner, lane_param, lane_index)` | remove the clip lane |
| `remove_automation_lane(owner, lane_param, lane_index)` | remove one lane, with or without a span |

Rules for each gesture:

- Move shifts the span and every point by the same delta, so the curve keeps its
  shape and the points keep their order. If the new span would overlap another
  lane on the same target, the move is refused and the message names both spans.
- Resize moves one edge and leaves the other edge and every point in place. A
  resize that would cross the other edge or overlap another lane is refused with
  the reason. Points are never rewritten, so a point left outside a shrunken span
  still shapes the curve inside it, like any authored point outside a span.
- Duplicate copies the span length, the points, the mute flag and the curve
  controls to a new lane on the same target, shifts every point by the same
  delta, and appends the lane to the owner's lanes. If the requested position is
  taken, the search steps right past each blocker. If there is no free position
  within 64 bars of the request, or another lane drives the target over the
  whole timeline, the copy is refused and the message names that window.
- Split needs a position strictly inside the span and at least one block grid
  step (a 16th note) from either edge. The left lane keeps the head and gets a
  closing point at the seam that holds the value the whole clip wrote there. The
  right lane starts at the seam with that same value as its first point, so the
  curve stays continuous. The right lane keeps every point at or after the seam,
  the left lane keeps every point before it, and the right lane is inserted
  directly after the left one. The seam value is interpolated in the point
  list's units, because those are the units a point carries. A waveform
  (`mode=lfo`) lane is refused: its points describe one cycle, and each half
  would restart that cycle at its own head.
- Delete removes a lane that has a span. A whole-timeline lane is not a clip, so
  the clip gestures refuse it and say why. To delete it, use
  `remove_automation_lane`, which removes either form as one undo entry.
- All five gestures refuse a lane on its own tick grid (`[ppq=N]`). The gestures
  work on the project's notated grid, and a tick-grid lane stores its positions
  in ticks.

## Validation

`ocp validate` and `oc-dsl` validation reject a lane that names:

- an effect the owner's chain does not have (the message lists the chain's
  effects);
- a type that matches more than one entry (use `fx<N>`);
- a chain position outside the list;
- a parameter the addressed built-in device does not expose (the message lists
  the ids);
- a `smoothing=` outside 0..60000 ms, or a `tension=` without `curve=smooth` or
  outside 0..1;
- a curve or smoothing on a bare control lane, because a MIDI control lane is a
  list of event steps (a built-in patch-parameter lane ramps linearly between
  its points);
- a `[start - end]` span whose end is not after its start;
- two lanes on one `(owner, target)` that write at the same time.

Two clips overlap when their spans intersect. A clip and a whole-timeline lane
always overlap, because the whole-timeline lane has no end. Clips that only
touch are allowed: `[1:1 - 3:1]` and `[3:1 - 5:1]` hand over at `3:1`. The
message names the target and both spans, and describes a whole-timeline lane as
"over the whole timeline".

A `muted` lane is validated like any other. Its span, points, curve rules and
overlaps are checked as if it were not muted.

Plugin parameter names are unknown until the plugin is hosted. Validation
accepts them, and they are resolved when the effect processes.

## Limits

- A continuous curve on a built-in device sets its parameter at every sample, so
  it costs more than a `stairs` lane. The per-sample split keeps the render
  exact and independent of the block size.
- Lane positions are timeline positions and are not compensated for the
  insert's own latency. A look-ahead device applies the change when the change
  reaches it.
- Preset effects and pattern effects cannot be addressed. Pattern effects are
  already rejected as pattern content.
- Effect lanes are not exported to MIDI and do not produce instrument events.
- MIDI export of a control lane writes every authored point and ignores the
  clip span and the `muted` flag. A muted or clipped control lane therefore
  exports the same controller events as a whole-timeline lane.
- Only the timeline has clip gestures. Agent-facing commands, MIDI export and
  the device rack do not move a span. A `[ppq=N]` lane and a waveform lane have
  no clip gestures, as described under [Clip gestures](#clip-gestures).
- A duplicate reserves its slot from the lane list it sees, so two duplicates
  that run at the same moment can pick the same free position. The validator
  refuses the second one. The project stays valid, and the second copy is not
  written.

## Control links

A control link connects one control to many parameters. The control is a MIDI
CC, an OpenSynth macro or an automation lane. Each target has its own mapping
formula and its own smoothing time.

```ocp
@control_link drive
  from: cc(74)
  -> track: lead param: filter.cutoff formula: midi_unit smoothing: 8ms
  -> track: lead param: output.gain formula: exponential

@control_link pad_macro
  from: macro(3)
  -> track: pad param: filter.resonance formula: logarithmic smoothing: 40ms
```

`from:` accepts three sources:

- `cc(controller[, channel])`, with a one-based channel that defaults to 1;
- `macro(index)`, with an index from 1 to 8;
- `lane(track, param)`, which reads the normalized value of one registered
  automation lane.

A target names a `track` and a `param`. `param` is an engine parameter key or a
hosted plugin parameter name; quote it when it contains spaces. `smoothing:` is
optional and in milliseconds.

The single-target spelling `target: track: ...` still parses and loads as a
link with one target. A JSON `target` field is merged into `targets` in the same
way.

### Formulas

The formula is one of a fixed set of names, each with a documented mapping.
Every result is clamped to the normalized 0..1 parameter domain.

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
normalized position, which for a CC is `raw / 127`.

### Smoothing

Each target is smoothed on its own by a one-pole filter over the block:

```
coefficient = 1 - exp(-frames / (seconds * sample_rate))
value += (goal - value) * coefficient
```

At `0 ms` the coefficient is 1, so the value jumps straight to the goal, and a
link without smoothing steps like a lane without smoothing. One time constant
covers 63.2% of the remaining distance.

### Learn and last tweaked

In the editor you arm learn for one target with **Learn CC**. The next CC
creates a link for that controller with exactly one row for the target. Learning
the same target again keeps that row and its settings and adds no new row. A
link with no rows left is removed. The panel remembers the parameter you moved
last, so **+ last** binds it to an existing control in one action, and
**Learn CC** binds it to a new CC.

### Limits

- Links are headless data with a runtime in `oc-engine::control_link`. Live
  MIDI CC input is not yet dispatched into the playback graph.
- A target's smoothing runs at the rate the caller advances it. The runtime is
  block-based, so the smoothing time does not depend on the block size.
- Link rows are not exported to MIDI and do not create automation lanes.

## Touch override

While the transport streams, dragging an automated parameter takes it over: the
value under your hand wins and the lane is suppressed. When you release, the
parameter returns to the lane through a fixed 20 ms crossfade, so even a 20 dB
jump does not click. The crossfade length cannot be changed. A touch is live
state and never edits the project. It writes no lane point and changes no
declared value, and offline rendering always evaluates the lane as authored.

The rule is one function in `oc-engine::touch` (`TouchLane::plan`), shared by
effect-parameter lanes and `gain_automation:` clips. The GUI shows a touch as an
accent ring on the knob. Only an effect parameter that a lane actually addresses
is taken over. Any other parameter is edited as usual.

A held touch suspends the lane's one-pole smoother as well as muting the lane.
The lane keeps its clock, but its filtered value stays at the last sample the
lane owned, so the smoother cannot drift towards the curve while your hand holds
the parameter. On release, the 20 ms crossfade blends your value into the lane's
resumed output, and from there the lane's own `smoothing=` time constant carries
the value to the current curve value. The crossfade only hands the parameter
back and adds no smoothing of its own. A 100 ms lane still takes 100 ms to reach
a target that moved during the hold, and the fade stays 20 ms at every lane
setting.
