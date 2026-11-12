---
id: dsl-subtree-paths
title: DSL subtree paths
when: Before extracting or replacing any subtree, or when a path fails with an unknown segment or an unsupported replace path.
category: editing
version: 1
tools: [project_extract, project_validate, edit_subtree, project_diff, sections_list, sections_add, sections_set, sections_remove]
---
A subtree path is not a file path and not a JSON pointer. It is a dot-separated
list of segments; anything else fails with `Unknown path segment: <part>`.
Names match exactly; a name with a dot cannot be addressed, since the split on
`.` happens first.

## Paths

- `project` — the whole file.
- `project.properties` (or `properties`) — tempo, key, time signature.
- `track:<name>` or `track:<index>` — one track; the index is 0-based.
- `track:<ref>.phrase:<ref>`, `.mix`, `.effects`, `.automation` — one child.
- `bus:<name>`, `preset:<name>`, `pattern:<name>`, `master`, `sections`.

## What can be replaced
`edit_subtree` and `project_validate` accept every path above.

- `project` takes a complete document, `@project` header included: patterns,
  clips, sections, automation, audio clips and `swing:`/`tempo:` at once. It
  must name at least one track. Prefer it when several level-1 blocks change.
- `pattern:<name>` takes one `@pattern` block and creates the name when it is
  new, so a pattern plus a `clip:` line is two edits at most (or one `project`).
- `sections` takes every `@section` block; an empty replacement is refused, so
  removing one goes through `sections_remove`.
- `track:<ref>.automation` takes that track's `automation:` lines only.
- `project.properties` takes only the `key: value` lines, no header.

A path that fits none of these fails with `Unsupported replace path` or
`Invalid track child for replace`.

## Order of work
1. `project_extract(path)`: read the exact text you are about to replace, then
   build the replacement in that shape (a level-1 block for a track/preset/bus,
   only the child block for a phrase/mix/effects).
2. `project_validate(path, dsl)` or `edit_subtree(path, new_dsl)`. The result
   says `pending_user_confirmation` when a user has to accept it and `applied`
   when the session has nobody to ask; trust that field.
3. `project_diff` against the old text when the replacement is large.

Never build a path from a track name you have not seen: call `tracks_list` or
`project_extract("project")` first; a guess returns `Track '<name>' not found`.
