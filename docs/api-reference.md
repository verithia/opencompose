# Navigating the API reference

The rustdoc for this commit is published at [`api/`](api/index.html), beside the
book rather than on a separate site. It is generated, never hand-written: one page
per crate, built from `cargo doc --workspace --no-deps`.

## What is there

| Page | Answers |
|---|---|
| [`api/index.html`](api/index.html) | The crate list: nineteen workspace members. |
| `api/<crate>/index.html` | The crate's front page: its own summary, its modules, its re-exports, and the items at its root. |
| `api/<crate>/<module>/index.html` | One module: what it owns, its types and functions. |
| `api/<crate>/struct.<Name>.html` | One type: fields, trait implementations, methods, and the source link. |
| `api/<crate>/fn.<name>.html`, `enum.*`, `trait.*`, `static.*`, `constant.*` | One item each. |

The crate name in the path is the Rust crate name, so `oc-dsl` is at `oc_dsl`
and the GUI backend is at `oc_gui_lib`.

`--no-deps` is deliberate: the reference documents this workspace, not the 700
crates it builds on. A dependency's own documentation is one `cargo doc -p
<dependency> --open` away, and including it here would bury the project's own
surface.

## Why the reference is generated

Signatures change daily, and a hand-copied signature is wrong the first time an
argument moves. The split is:

- **generated**: names, signatures, fields, trait implementations, module layout;
- **written**: what a module is for, which crate owns a behaviour, and the rules
  a change must respect — the pages under [Component reference](components/index.md)
  and the chapters beside them.

Every component page links into its crate's index, and the crate-level doc
comment is the summary that page builds on. If a type's meaning is not obvious
from its name, the doc comment on the type is the place to fix that, not a new
prose section here.

## Reading order for a change

1. The component page of the crate you are about to touch: what it owns and what
   must not be added to it.
2. The crate front page in the API reference: the module list, which is the
   fastest map of the crate.
3. The item page, which carries the contract in its doc comment.

## Local builds

```sh
cargo doc --workspace --no-deps --open
```

`scripts/docs.sh build` does the same and copies the output into `book/api/`, so
the links from these pages resolve exactly as they do on the deployed site. A
broken intra-doc link is a warning; the documentation job fails on it.
