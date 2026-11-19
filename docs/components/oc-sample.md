# oc-sample

The sample marketplace client: a provider seam, a licence rule and a local
store. The `ocp` binary and the GUI both link it, so a search on the command
line and a search from the panel use the same vocabulary.

## What it owns

| Module | Owns |
|---|---|
| `model` | `SampleQuery`, `SampleItem`, `SamplePage`, `SampleKind`, `Quality`, `SampleError`, `Failure`, `FailureKind`. |
| `provider` | The `SampleProvider` and `SampleFetch` traits, `FetchError`, query encoding, bounded JSON fetching. |
| `http` | The one network client: `reqwest` with the chosen proxy (system, manual or none), the 15-second search and 180-second download deadlines, status-to-kind mapping, `Retry-After` parsing and credential redaction in messages. |
| `license` | The licence rule: which licences need a notice, which allow commercial use, and the credit line. |
| `store` | Download, verification, file naming, the sidecar and the receipt index. |
| `registry` | One `ProviderSpec` per market, availability with the reason a market cannot answer, and the default search set. |
| `providers` | The spec of each market, including the page-only ones and their reasons. |
| `runtime`, `local_plugin` | Opening a market through a local provider process, and that process's wire protocol. |

## Key types

| Type | Role |
|---|---|
| `SampleProvider` | `search` one page and `resolve` one id into an item. |
| `SampleFetch` | The only network seam; tests implement it over recorded bytes. |
| `SampleQuery` | One search: text, kind, duration bounds, channels, tags, offset, limit and `commercial_ok`. `normalized()` bounds the text and tags, and `matches()` applies every filter to a returned item. |
| `SampleItem` | One result: id, kind, duration, channels, sample rate, licence, credit line, preview and download URL, declared size and checksum. |
| `SamplePage` | One market's page, with its total and its own `next_offset`. |
| `SampleStore` | Writes an item into a directory and returns a receipt. |
| `SampleReceipt` | The row in `index.json` and the body of the sidecar: what was saved where, with which licence and checksum. |
| `License` | Machine id, source name, URL and the four terms that change what a project may do. |
| `SampleError` | `Refused { code, message }` for a decision, or `Failed(Failure)` for a source that could not be read. A `Failure` carries a `FailureKind` (`rate_limited`, `unauthorized`, `not_found`, `unavailable`, `timeout`, `network`, `parse`, `other`), the HTTP status and the `Retry-After` wait, capped at one day. |

The multi-market search that the panel and the agent tools use (the opaque
per-market cursor, round-robin merging, duplicate removal, per-market back-off
and the preview cache) lives in the GUI backend, in
`crates/oc-gui/frontend/src-tauri/src/samples/`. This crate pages one market
at a time with offsets.

## Depends on

`serde`, `serde_json`, `sha2` and `reqwest`. It depends on no other workspace
crate, so the CLI and the GUI can both link it without pulling in the engine.

## Rules

- A provider never downloads, verifies, names or licenses a file; `store` and
  `license` do, so a market adapter cannot bring its own verification.
- User text and tags are data. Adapters build URLs only from the normalized
  query and escape or quote what a market would read as query syntax.
- Every parser is tested against recorded JSON through `SampleFetch`. No test
  reaches the network.
- A refusal is data: `SampleError::to_json` is what the agent tools return.
  A failure's kind travels as a tag, so no caller reads it out of `message`.
- A pull writes the audio, the sidecar and the receipt together, or none of
  them. It never leaves an uncredited file in a project.

## Provider runtime

`ProviderRuntime` checks whether a market is installed and opens it.
`LocalRuntime` opens local provider processes from an explicit directory or,
at the application boundary, from the process configuration; only debug builds
load them. The public provider and registry APIs always use this runtime, and
the `*_with` variants take an explicit runtime for deterministic callers.
`FixtureBuiltinRuntime` exists only for tests and the `test-providers` feature,
and must be passed in explicitly; enabling that feature never changes how the
application selects markets. The optional `local-provider-runner` program runs
the documented adapters as a separate process. A provider process reports
failures with a `kind`, `http_status` and `retry_after_secs`; only the
`unknown_id` and `no_download` codes are read as refusals.
