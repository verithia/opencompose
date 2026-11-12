---
id: route-groups-and-returns
title: Route groups and parallel returns
when: You need drum or vocal groups, shared effects, separate processing, or a route that avoids doubled dry audio.
category: mixing
version: 1
tools: [tracks_list, project_extract, buses_add, buses_routes, buses_mix, tracks_output, tracks_send_add, project_validate, audition_range]
---
Draw the signal path from source to main output. Name a bus by its job,
such as drum group, plate return or parallel compression. Inspect existing
outputs and sends before changing them.

For a group, use an exclusive track output to the bus. Route the group to
the intended downstream bus or master. The source should reach the master
through this group once. Validate the graph after each layer of routing.

For a shared reverb, keep the track main route and add a send to a return.
Set the return effect fully wet so the return adds ambience. A post-fader
send follows source fader changes. Choose a pre-fader tap only if the wet
balance needs to be independent of that fader.

Turn the send down to reduce what feeds the next tail. Move the return
fader to change everything already in that return. Keep detector routing
separate from audible routing when sidechaining an effect.

Solo or mute groups methodically and listen for doubled attacks or a dry
signal through an effects return. Check both verse and chorus if send level
changes by section. Avoid cycles and verify downstream bus names.

Leave a short route summary for the session, including the tap type and
where each return feeds. Audition the full mix after restoring solo states.
