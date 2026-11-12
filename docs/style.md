# Documentation style

Every page on this site follows these rules. They exist so that a reader who
learns one chapter can predict the next one, and so that a contributor does not
have to guess.

## Voice

- Write in English, in the second person, present tense and active voice:
  "Press **Space** to play", not "the space bar can be pressed".
- Address the reader as "you". A page describes the system, not the history of
  building it: no "we added", no changelog tense. Design rationale goes in an
  explanation page under a stated reason.
- One idea per sentence. State the concrete case before the general rule.
- Do not use "simply", "just", "obviously", "seamless" or other filler that
  assumes the reader's experience.
- Headings are sentence case. Only product names and code identifiers keep
  their capitals.

## Terms

Use one word per concept, on every page:

| Term | Meaning |
|---|---|
| **track** | One channel of the mixer; the routing identity. |
| **pattern** | A named, reusable block of events placed by a clip. |
| **clip** | A placement of a pattern, or an `@audio_clip`, on a track at a timeline range. |
| **preset** | An instrument and its settings, referenced by name from a track or pattern. |
| **instrument** | The sound source selected as `category/name` on a track. |
| **device** | One entry in an effect chain, or one mixer processor. |
| **project** | The `.ocp` document as a whole. |

Do not drift to "channel", "loop", "region" or "patch" for these concepts. A
patch is the OpenSynth parameter document (see [The patch system](patch-system.md)),
not a preset.

## Code and links

- Fence `.ocp` examples as ` ```ocp `. Every such block is parsed by
  `scripts/check-docs.sh`; a block that only illustrates a shape uses `text`.
- Paths are relative to the repository root unless the text says otherwise.
- Link to generated API pages as `api/<crate>/index.html`; link to another
  chapter by its file name, and the build rewrites it.
- Name a source file only when the reader needs it; the rustdoc page is the
  contract.

## What may not appear

- No reference-product names except a foreign preset format or a tested host
  named for compatibility, and no reverse-engineering traces anywhere: no
  addresses, offsets, decompiled names or comparison harnesses.
- No non-English prose or table cells anywhere in `docs/`.
- No unlabelled machine-specific numbers. A timing table is an example: it
  names the machine, CPU tier and load that produced it, and says that the
  targets, not the example, are the contract.
