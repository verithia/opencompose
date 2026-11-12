# Render warnings

A render warning reports a substitute. The pass finishes, but one part of the
result is not what the project asks for: a hosted plugin that cannot start, a
drum hit whose pad does not exist, a mix that clips before quantisation. A
warning never fails a render.

The engine collects warnings while a pass runs and reports them once, when a
caller drains the list. `ocp render` writes one `warning:` line per entry to
stderr. The app shows the warnings its playback, preview and export commands
return in the status bar, joined with `; `.

## One line per problem

A warning that repeats is counted, not repeated. The engine keeps one record per
distinct message in first-occurrence order and adds the count when it drains
them:

```
warning: track 'drums': drum hit 'crash' plays General MIDI note 49, which no pad of this kit has; the hit is silent (256 occurrences)
```

A message that fired once keeps its plain text, so a single problem reads as it
always did. The rule is per message, and the message names its owner: the drum
line carries the track and the pad, so two pads on one track stay two lines and
two tracks stay apart.

Recording never logs. A normal render prints a handful of lines; a run with
`--debug` traces every occurrence instead.

## Warnings a load reports

Streaming playback reports routing problems when it rebuilds its lanes.

- A track that asks for the main output pair and gets it has not lost anything,
  so that fallback is a debug note, not a warning.
- A track that asks for a later pair or port the instrument does not declare
  loses that output, so it warns once per track.
- An auxiliary send to an output the instrument does not expose warns once per
  (track, pair).

Those rules keep a load quiet while the mix is unchanged and keep `warn` for a
signal that really disappears.

## The interpreter event dump

`OCP_TRACE_INTERPRET=1` prints the drum track's event count and the first 20
events to stderr. The dump is a tracing aid, not a diagnostic: a normal render
prints nothing of it.

## Where the code lives

- `crates/oc-engine/src/render_warnings.rs` records, counts and drains a pass.
- `crates/oc-engine/src/interpret/mapping.rs` warns that a drum hit has no pad.
- `crates/oc-engine/src/render_cache/helpers.rs` resolves a track's output pair
  and says whether the request reaches beyond the main pair.
- `crates/oc-gui/frontend/src-tauri/src/render_thread.rs` reports the routing
  fallbacks of streaming playback.
