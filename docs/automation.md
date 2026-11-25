# Automation

An automation lane is a list of position/value points evaluated against project
time. There are three lane forms, and they differ in what they drive:

| Form | Owner | Drives | Resolution |
| --- | --- | --- | --- |
| `automation:` control lane | track | an instrument control or a host parameter of the track's instrument | event stream from the interpreter |
| `automation:` effect lane | track, bus, master | one parameter of one effect in that owner's chain | per-block graph wrapper |
| `gain_automation:` clip | track, bus, master | post-insert linear gain | sample-accurate gain stage |

For a scoped `automation:` clip, `playlist_lane=3` shows the clip in playlist
row 3. A `gain_automation:` clip takes the same attribute after its time range.
The clip keeps its track, bus or master target.

Most of this page is about effect lanes. Control lanes are described in
[The .ocp file](manual/dsl.md) and gain clips in
[Mixer routing](mixer-routing.md).

## Insert controls

Every track, bus and master insert has its own wet level, from zero to full
wet. Set the starting level with `slot/wet` in the effect declaration. A lane
can drive the same control with normalized values or percentages. The insert
mixes the effect output with a latency-aligned copy of its input, so the
device's own Mix parameter remains independently adjustable.

```ocp
@bus motion
  effects:
    - gain: factor=2, slot/wet=25%, bypass=true
  automation: fx1.slot/enabled {1:2=true, 3:1=false}
  automation: fx1.slot/wet {1:2=25%, 2:1=100%}
```

The enable lane holds each switch value until the next anchor. An animated
insert keeps its device running when it outputs dry audio. This preserves its
history and keeps compensation fixed as the switch moves. A static
`bypass=true` without an active enable lane leaves the device unloaded.

## Compact exponential segments

A segment can store its analytic curvature instead of a dense list of linear
samples. Its coefficient belongs to its outgoing anchor:

```ocp
@bus motion
  effects:
    - gain: factor=1
  automation: gain.factor {1:1=0.2 curve=exponential curvature=-4, 3:1=0.8}
```

For progress `u` in `[0,1]`, the shape is
`E(k,u) = expm1(k*u) / expm1(k)`. At `k=0`, it is exactly `u`.
Positive coefficients accelerate towards the destination; negative coefficients
move earlier. The schema owns the finite coefficient range, currently `[-64,64]`.
The existing `smooth` curve and its `tension` retain their previous meaning.
An omitted curvature is zero; supplying curvature on another curve is invalid.

A mirrored exponential uses three anchors: one at each endpoint and one at the
midpoint. Give the first half coefficient `k` and the second half `-k`.
Restriction to a fraction `[a,b]` of an exponential segment is exact with the
sampled endpoint values and coefficient `k*(b-a)`; a crop crossing a mirrored
midpoint keeps that midpoint. No stored phase window or dense approximation is
needed. Source-format importers must translate incoming/outgoing anchor
conventions before storing these segments.

Effect lanes and encoded hosted-instrument parameter lanes support exponential
segments. Other instrument/MIDI lanes refuse them. A `gain_automation:` clip
requires the explicit `segments` interpolation described below; legacy `linear`
and `step` gain modes retain their previous behavior.

### Explicit discontinuities

An envelope or repeating LFO lane can opt into `discontinuities` before its points. Coincident
knots retain their authored order: the first knot is the preceding segment's
left limit and the final knot is the value written at that sample. This stores
an instantaneous jump without adding another lane or a fictitious one-tick ramp.
The renderer selects the rightmost knot at an exact sample and never divides by
a zero-length segment. Without the flag, existing lane behavior is unchanged.
Use a repeating lane to store one cycle of rhythmic gain:

```ocp
@bus rhythm
  effects:
    - gain: factor=1
  automation: fx1.factor [1:1 - 9:1] mode=lfo speed=1/2 discontinuities {1:1=1, 1:2=0, 1:2=1, 1:3=1}
```

Here each half note repeats the same fade and jump. You can change the rhythm
by editing the four cycle points once. The cycle follows tempo changes and
keeps the same phase when you seek. An exclusive clip-end reset still needs an
explicit write at that boundary; the flag does not extend a clip's write span.

Automate the Gain device's `mix` to vary the cycle's depth independently.
At `mix=0` the input passes through unchanged. At `mix=1` the full gain
applies. Intermediate values blend the two linearly. Gain defaults to full
mix, so existing projects retain their levels.

### Analytic post-insert gain clips

`gain_automation: segments` explicitly selects the same prepared segment evaluator
for a post-insert gain stage. Existing `linear` and `step` clips keep their legacy
interpretation and refuse mappings or the discontinuity flag.

```ocp
@bus level
  gain_automation: segments ppq=48 [1:1 - 3:1] discontinuities mapping(exponential, curvature=2, input_quantization_steps=128) {1:1=0.2 curve=exponential curvature=-3, 2:1=0.8, 2:1=0.4, 3:1=1}
```

The clip multiplies audio after its insert effects and before the separate static
fader. It returns unity outside its exclusive range. Original anchors outside a
segment clip's range remain interpolation context, not additional writes. Without
a mapping its numeric anchors are physical linear gains; with one they are raw
receiver inputs and the mapped result must remain in the gain owner's range.
The device catalog owns that range. Touch arbitration, seek and block partition
semantics are unchanged; no dense control-point table is prepared.

### Numeric receiver mappings

An optional `input_quantization_steps=N` maps the transfer input to
`floor(value*N)/N` before the transfer. The schema permits positive integer steps
from 1 to 1,000,000. Negative inputs use mathematical floor without clamping.
Preparation rejects nonfinite products and transformed values outside the target
range. Omit this field for the previous continuous-input behavior.

An effect lane can apply one compact value transfer after interpolation and
any LFO depth. The transfer produces a value in the recipient's unit before
the lane's smoother runs.

```ocp
@bus blend
  effects:
    - gain: factor=1
  automation: gain.factor mapping(linear, scale=-1, offset=1) {1:1=0, 3:1=1}
```

`mapping(linear, scale=s, offset=o)` computes `o+s*v`.
`mapping(exponential, curvature=k, scale=s, offset=o)` computes `o+s*E(k,v)`.
Defaults are linear, zero curvature, unit scale, and zero offset. Negative scale
supports complementary controls; zero scale is a valid constant transfer but
cannot be inverted by a point-drag gesture.

Mapped lanes require unitless numeric anchors and effect recipients. The curve
interpolates the raw anchors, then applies the mapping to produce values such
as Hz or dB. The prepared curve validates finite mapped extrema, including LFO
depth, against the recipient's range. The schema bounds exponential arguments
and reports overflows or unsupported inputs as errors.

Splitting a mapped lane samples its **unmapped** seam anchor and retains the
mapping on both halves. Exponential coefficients are restricted with each half's
fraction, so mapping is applied once. Time remains governed by ProjectClock's
sample quantization; no arbitrary timing shift is introduced. Ranged clips
still stop writing at their exclusive end, and duplicate sample positions retain
the final authored write rather than creating a fabricated interpolated ramp.

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

### Moving delay read positions

Enable `interpolate=true` on a delay when a time curve should scan its
buffer continuously. This is useful for tape slowdown, pitch sweeps, and
moving between earlier beats of an input loop. Set both channels to the same
curve for stereo-linked motion and set feedback to zero when you only want
to replay the input. For example:

```ocp
@bus motion
  effects:
    - delay: time_l=1ms, time_r=1ms, feedback=0%, mix=100%, interpolate=true
  automation: fx1.time_l {1:1=1ms, 1:2=126ms}
  automation: fx1.time_r {1:1=1ms, 1:2=126ms}
```

The delay holds up to two seconds of audio. A jump between taps jumps in the
audio too, so use a short ramp when that transition should be smooth. Use
`gain_automation:` for a separate rhythmic level curve. Existing delay
patches keep integer-sample taps by default.

For tempo-synced echoes, set `tempo_sync=true` and write `quarters_l` and
`quarters_r` as fractions of a quarter note. The taps follow tempo changes.
Each tap holds at most two seconds. A delay used without a transport assumes
120 BPM.

```ocp
@bus echoes
  effects:
    - delay: tempo_sync=true, quarters_l=0.75, quarters_r=0.75, routing=ping_pong, feedback=45%, feedback_filter=low_pass, feedback_cutoff=2400Hz, mix=100%
```

Stereo keeps independent channels. Mono averages the input before the two
taps. Swapped exchanges the wet outputs. Ping Pong exchanges the feedback
channels, so the first echo stays on its input side and later echoes alternate.
The feedback filter shapes later echoes after the first tap. Use `dry_gain`
and `wet_gain` to scale their respective Mix outputs. For independently set
dry and wet levels, start at `mix=50%` and set each gain to twice its desired
output level.

## Saved plugin effect state

An external insert accepts `state_hex` (or the format-specific
`vst3_state_hex`, `vst2_state_hex`, `clap_state_hex`, `au_state_hex` or
`lv2_state_hex`) as a quoted hexadecimal string. It restores the plugin's
opaque state before applying numeric control overrides. This preserves settings
that the plugin does not expose as numeric parameters, such as custom curves.
State aliases share one last-write-wins value. The instrument preset's bounded
decoder and identity/integrity envelope checks also apply to inserts.

Use `state_file` or the corresponding format-specific `*_state_file` to keep a
large state in a separate file. Relative paths resolve from the project
directory. Save As collects these files with the other project media. Missing
files, corrupt state, and native restore failures stop the render and identify
the affected insert.

For VST3, `vst3_state_file` also accepts a `.vstpreset` file for the selected
plugin class. OpenCompose restores its processor state before applying the
project's parameter overrides. A plugin that supports separate controller state
also receives its saved controller settings.

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
  step (a 16th note) from either edge. Both halves keep every original point,
  unit and coefficient, including points that share a position; only their
  spans change. The left span ends at the seam and the right span starts there,
  so every sample keeps its value at any sample rate and through tempo changes,
  and no point is added at the seam. Points outside a half's span still shape
  its curve. The right lane is inserted directly after the left one. Moving a
  half is refused, with nothing changed, if it would push one of its points
  before the start of the project; no point is dropped or resampled. A waveform
  (`mode=lfo`) lane is refused: each half would restart its cycle at its own
  head.
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
```

`from:` accepts three sources:

- `cc(controller[, channel])`, with a one-based channel that defaults to 1;
- `macro(index, preset)`, macro 1 to 8 of a named OpenSynth preset;
- `lane(track, param)`, which reads the normalized value of one registered
  automation lane.

A target names a `track` and a `param`. `param` is an engine parameter key or a
hosted plugin parameter name; quote it when it contains spaces. `smoothing:` is
optional and in milliseconds.

In a lane link, `param: fx1.ParameterName` targets a mixer effect, and
`track:` names the track or bus that owns it. One recorded motion can, for
example, drive inserts on two parallel returns:

```ocp
@control_link echo_feedback
  from: lane(feedback_control, "fx1.factor")
  -> track: echo_left param: fx1.Feedback formula: linear
  -> track: echo_right param: fx1.Feedback formula: linear
```

The target receives a value in its effect's automation range: 0 to 1 for
hosted effects. For a built-in parameter in a physical unit, add the same
`mapping(...)` an automation lane takes. The mapping runs after the formula
and before the target's smoothing. This link spans an EQ's frequency range:

```ocp
@control_link brightness
  from: lane(brightness_control, "fx1.factor")
  -> track: return param: fx1.high_cut_freq formula: linear mapping(exponential, curvature=6.907755278982137, scale=19980, offset=20)
```

A destination takes either one link or its own automation, not both. The graph
checks the mapped range and refuses a second writer.

During desktop playback, a CC from a MIDI input can drive the same mixer
controls. Here CC 74 on channel 2 raises the first insert's gain on `lead`
while lowering it on `return`:

```ocp
@control_link crossfade
  from: cc(74, 2)
  -> track: lead param: fx1.factor formula: midi_unit
  -> track: return param: fx1.factor formula: inverted
```

A named OpenSynth preset can hold the controls for a rack. The link reads the
preset's saved macro, including inherited values and patch files:

```ocp
@preset RackControls
  source: builtin
  plugin: "builtin.open_synth"
  params: {macro.8: 0.25}

@control_link feedback
  from: macro(8, RackControls)
  -> track: echo_left param: fx1.Feedback formula: linear
  -> track: echo_right param: fx1.Feedback formula: linear
```

To change the value, edit the macro in that preset or in its `params:`. For a
control that changes over time, use a `lane(track, param)` source. A
`macro(index)` link without a preset still loads and keeps its spelling.

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

Each target is smoothed on its own by a one-pole filter. Effect lane links
move the target every sample, after the source lane's smoothing and the
target's formula:

```
coefficient = 1 - exp(-frames / (seconds * sample_rate))
value += (goal - value) * coefficient
```

At `0 ms` the coefficient is 1, so the target takes the mapped value at once.
One time constant covers 63.2% of the remaining distance.

### Learn and last tweaked

In the editor you arm learn for one target with **Learn CC**. The next CC
creates a link for that controller with exactly one row for the target. Learning
the same target again keeps that row and its settings and adds no new row. A
link with no rows left is removed. The panel remembers the parameter you moved
last, so **+ last** binds it to an existing control in one action, and
**Learn CC** binds it to a new CC.

### Limits

- Effect lane links run in both export and playback. The source stays one
  editable lane.
- While the desktop app monitors a MIDI input, a CC link can drive track and
  bus insert parameters. Each block reads each controller once for all
  targets. The written value holds until the first matching CC arrives, and
  each target can smooth the changes after it. Offline renders use the written
  values and automation.
- Named preset macros run in desktop playback and export. During playback,
  dragging a macro previews the linked values; releasing saves the value as one
  edit, and cancelling restores it. Saving a preset that only serves as a rack
  control keeps the playing graph, with its delay and reverb tails. Export
  always reads the saved preset. Per-voice modulation stays inside the synth.
- Link rows are not exported to MIDI and do not create automation lanes; MIDI
  export takes its events from the written MIDI and automation lanes.

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
