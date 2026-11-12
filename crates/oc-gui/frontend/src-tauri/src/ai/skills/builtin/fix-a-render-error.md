---
id: fix-a-render-error
title: Fix a render error
when: A render or audition fails; read the message before changing anything.
category: render
version: 1
tools: [render_range, render_compat_report, tracks_list, project_extract]
---
Read the whole message first. The substring that names the cause decides the
fix, and it is usually one field rather than a rewrite. A syntax error is not a
render error: `Parse error` / `Syntax error at line` means read `writing-notes`.

## `output directory `

    output directory '/tmp/renders' does not exist

`render_range` writes a file and never creates directories. Use an existing
parent such as `/tmp` or the project's `renders/`, and keep the path absolute.

## `cannot be used as an effect`

    <name>: an instrument plugin (0 audio inputs, 1 stereo output) cannot be
    used as an effect; put it on an instrument track, or wrap it in a bus

An instrument was put in an `effects:` chain. Remove it there; put it on its own
track with `presets_add` + `tracks_preset`, or route the effect through a bus.

## `needs a SoundFont`

Read `playable-instruments`: it quotes the exact message and the preset fix.

## `refused: ` / `unknown tool '`

The tool is not offered here: `refused: '<name>' is excluded from the built-in
AI panel`, or `unknown tool '<name>'` when the name is not a tool at all. Do not retry it;
use the listed tool or say which toolset switch would enable it. `render_offline`
is always excluded: use `render_range`.

## `stale revision` / `Project changed`

The project moved while you were thinking. `project_get` for the new
`revision`, reapply the edit with `expected_revision`, and do not resend the
stale call unchanged. The refused edit changed nothing; the edits before it in
the same batch are still there.
