# oc-lsp

The language server for `.ocp` files: diagnostics, completion and hover, over
the Language Server Protocol.

## What it owns

A small server that the VS Code extension starts, and that the editor-facing
side of the product uses for validation. Every request is answered from
`oc-dsl`: the same parser and validator the renderer uses, so an editor
diagnostic and a render failure cannot disagree.

| Module | Answers |
|---|---|
| `diagnostics` | Parse and validate a buffer; returns positioned messages. |
| `completion` | Completions at a position: block keywords, properties, device names. |
| `hover` | The documentation for the token under the cursor. |
| `validate` | The reusable `validate_project` entry point, also re-exported at the crate root. |

## Key types

The binary keeps its modules private, so the only item on its API page is
[`validate_project`](../api/ocp_lsp/fn.validate_project.html), which returns a
report of errors, warnings and hints. The report and message types stay inside
the binary.

## Depends on

`oc-dsl`, `tower-lsp`, `tokio` and `serde_json`. It does not depend on the
engine, the host or the GUI.

## Rules

- The server never renders and never plays. A diagnostic that needs audio
  belongs in the app, not here.
- Every diagnostic must come from `oc-dsl`'s own error type, with its line and
  column. Do not re-implement a check in the server.
- Completion and hover read the device schema and the DSL's own lists; a new
  device or block keyword becomes visible without a second edit here.
- The binary is `ocp-lsp`; the extension finds it on `PATH` or in the workspace
  target directory (see [Troubleshooting](../manual/troubleshooting.md)).

## API

[`api/ocp_lsp/`](../api/ocp_lsp/index.html).
