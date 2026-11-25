---
id: route-groups-and-returns
title: Route groups and parallel returns
when: You need drum or vocal groups, shared effects, separate processing, or a route that avoids doubled dry audio.
category: mixing
version: 3
tools: [tracks_list, project_extract, buses_add, buses_routes, buses_mix, tracks_output, tracks_send_add, project_validate, render_range, audition_clip_source, audition_range, read_skill, edit_subtree]
---
Draw the audible path from source to main output, then the detector/control
paths separately. Inspect outputs, sends, inserts and every downstream gain.
An enabled strip can feed a silent parent; a named empty bus proves no processing.

## Separate a group from a parallel path
For a group, set exclusive track output to that bus and route the bus onward.
Keep one intended dry path to master. A retained direct path can double the
source and bypass the group's processing. Validate downstream names and cycles.
A group compressor responds to the sum, not as separate compressors per take.
A monophonic pitch processor on several voices can mistrack the group.

For a shared reverb/delay, keep the dry main route and send to a wet return.
Set wet effects fully wet unless deliberate parallel dry gain is intended.
A pre-fader send remains after source inserts/PDC/polarity but before fader
and pan; a post-fader send follows both. Send pan is a separate route control.
Do not infer a dedicated bus-return pan or bus-send amount from another host.
A send from a source before its group does not inherit that group's inserts.
Choose tap location for the desired processed excitation, not by label alone.

## Separate feed, tail and detector
Changing a send changes new signal feeding an effect. Closing it does not
stop an existing reverb/delay tail. Return gain changes all wet audio already
there; a dry-keyed compressor after the wet effect can reveal tails in gaps.
Use choose-and-shape-reverb or place-rap-vocal-throws for phrase decisions.
Keep detector input non-summing; inspect actual device ports before connecting it.
An audio detector and a MIDI-triggered shaper are different receivers; check the actual key path.
For native compression, `route: vocal -> compression` keys from track vocal.
`route: control env -> compression` uses the declared control source env.
The native AUDIO key is post-insert, pre-fader and pre-mute; a muted source
can still drive it. A normalized CONTROL follower is a different route and
refreshes empty when muted, and a hosted MIDI receiver follows its own events.
A pre-fader send is not any of these signals.
If discovery output omits ports, do not invent a key connection from a name.

## Prove the routes you keep
Hear the source, group and return methodically for doubled attacks, dry
leakage or wrong processing ownership. Check missing/irregular keys, source
mute/fader changes and both sparse/dense sections, such as a verse and a chorus
with different send levels. Preserve intended key action.
For an isolated processed bus check, use render_range with its bus name and omit tracks.
Listen to the returned saved path with audition_clip_source; this preserves the full detector graph.
Avoid unsupported feedback cycles; a feed-forward return is not a recirculating
loop. Restore temporary solos and hear the full mix after graph validation.
Retain a concise route description with tap and detector ownership so later
edits and reopening preserve the same musical behavior.
