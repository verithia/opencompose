# oc-sample

The sample marketplace client: a provider seam, a licence rule and a local
store. The `ocp` binary and the GUI both link it, so a search on the command
line and a search from the panel answer with the same vocabulary.

## What it owns

| Module | Owns |
|---|---|
| `model` | `SampleQuery`, `SampleItem`, `SamplePage`, `SampleKind`, `Quality`, `SampleError`. |
| `provider` | The `SampleProvider` and `SampleFetch` traits, query encoding, JSON fetching. |
| `http` | The one network client; `reqwest` with the system proxy and a fixed user agent. |
| `license` | The licence rule: which licences may enter a project, and the credit line. |
| `store` | Download, verification, file naming, the sidecar and the receipt index. |
| `providers::commons` | Wikimedia Commons: MediaWiki search, no key. |
| `providers::freesound` | Freesound: API key, preview-quality pulls. |

## Key types

| Type | Role |
|---|---|
| `SampleProvider` | `search` one page and `resolve` one id into an item. |
| `SampleFetch` | The only network seam; tests implement it over recorded bytes. |
| `SampleItem` | One result: id, kind, duration, channels, sample rate, licence, credit line, preview and download URL, declared size and checksum. |
| `SampleStore` | Writes an item into a directory and returns a receipt. |
| `SampleReceipt` | The row in `index.json` and the body of the sidecar: what landed where, with which licence and checksum. |
| `License` | Machine id, source name, URL and the four terms that change what a project may do. |
| `SampleError` | `Refused { code, message }` for a decision, `Failed` for a source that could not be read. |

## Depends on

`serde`, `serde_json`, `sha2` and `reqwest`. It depends on no other workspace
crate, so the CLI and the GUI can both link it without pulling the engine in.

## Rules

- A provider never downloads, verifies, names or licenses a file; that belongs
  to `store` and `license`, so a new source cannot invent its own verification.
- Every parser is tested against recorded JSON through `SampleFetch`. No test
  reaches the network.
- A refusal is data: `SampleError::to_json` is what the agent tools return, and
  `message` is the sentence a person acts on.
- A pull writes the sidecar and the receipt, or removes the audio again. It
  never leaves an uncredited file in a project.

## Provider runtime

`ProviderRuntime` owns both installation checks and opening a provider.
`LocalRuntime` selects real local-process plugins from an explicit directory or,
at the application boundary, process configuration. The public provider and
registry APIs always use this runtime; `*_with` variants accept an explicit
runtime for deterministic callers. `FixtureBuiltinRuntime` is available only
for tests or the `test-providers` feature and must be injected explicitly.
Enabling that feature never changes application provider selection. The optional
`local-provider-runner` executable invokes the documented adapters as a separate
process. Host and wire code compile in test builds as well as application builds.
