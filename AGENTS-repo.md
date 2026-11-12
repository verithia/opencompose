# AGENTS-repo.md

What belongs in the repository, and how to name, license, document and commit
it. The hard rules in [AGENTS.md](AGENTS.md) still apply.

## Public tree vs `./private/`

The repository is public. `./private/` is ignored by git and holds everything
else. If you are unsure, put it in `./private/`.

| Goes in `./private/` (or outside the tree) | Never committed anywhere |
|---|---|
| Review reports, task boards, sprint logs, agent notes and plans | Secrets, API keys, OAuth client secrets, signing keys (`plugin-index.key`) |
| Rendered audio, UI screenshots, A/B listening material | Absolute paths with a user name, private hosts and domains |
| Vocal and sample corpora, datasets, ML checkpoints | Third-party presets, samples or libraries you have no right to redistribute |
| Machine-specific benchmark logs and profiles | |
| Notes about third-party products | |

Committed binaries are limited to tiny generated test fixtures, the app's
icons and brand art, and the licensed bundled resources listed with their
license files (for example the GM SoundFont in
`crates/oc-gui/frontend/src-tauri/resources/soundfonts/`). Build output
(`target/`, `dist/`, `out/`, `*.vsix`, `book/`) is never committed.

`.gitignore` lists only standard build, cache and dependency patterns, plus
`/private/` and the signing key. Put personal patterns in
`.git/info/exclude`.

## Licensing

- Application, engine and libraries: **AGPL-3.0-or-later** (`LICENSE`).
- Language components: **MIT OR Apache-2.0** (`LICENSE-MIT`,
  `LICENSE-APACHE`). These are `oc-dsl`, `oc-device-schema`, `oc-lsp`,
  `oc-test-support` and `editors/vscode-ocp`. They must not depend on an AGPL
  crate. `scripts/check-licenses.py` enforces this.
- A new crate inherits AGPL from `[workspace.package]`. Making it permissive is
  a deliberate decision: update `check-licenses.py` and the README.
- A new dependency must be compatible with the license of the crate that uses
  it. Never add GPL-only code to a permissive crate. Never copy code, presets,
  samples or artwork without a compatible license and its attribution.
- Shipped dependencies must use a license on the allow list in
  `scripts/third-party-licenses.py`. Copyleft (MPL-2.0) is allowed only outside
  the permissive crates' graph; GPL and LGPL-only are refused.
- The app ships every dependency's notices in
  `crates/oc-gui/frontend/src-tauri/resources/THIRD_PARTY_LICENSES.txt`, shown
  on the Help & About page. After changing `Cargo.lock`, the frontend
  `package-lock.json` or a bundled asset, regenerate it with
  `python3 scripts/third-party-licenses.py`. CI fails when it is stale.
- A bundled third-party asset ships with its license file next to it and an
  entry in `ASSETS` in that script.

## Originality and third-party names

Follow [docs/compatibility.md](docs/compatibility.md).

- A third-party product name may appear only where it selects a file format
  (`.SerumPreset`, `.ffp`, `.sf2`) or identifies a tested plugin host.
- No product or hardware trademarks in the names of our devices, presets,
  filter models or UI labels.
- Never present our output as another vendor's product.
- No reverse-engineering artifacts, field maps annotated from vendor files,
  reference constants, comparison scores or competitor tables.

## Naming

- Crates use `oc-*` (`oc_*` in Rust). The product is **OpenCompose**, the CLI is
  **`ocp`** and the language server is **`ocp-lsp`**.
- Never rename stable identifiers: wire tags such as
  `opencompose_plugin_scan_receipt_v1`, the Tauri bundle identifier, or the
  `OCPC` header.

## Documentation

- `docs/` is the mdBook source. Every page is listed in `docs/SUMMARY.md` and
  follows [docs/style.md](docs/style.md): English, second person, present
  tense, no changelog voice.
- Examples run from the repository or a standard install. Use portable commands
  (`cargo`, `npm`, `npx`), never private wrappers or local paths.
- Public docs hold design rationale and reproducible measurements. Work logs
  belong in `./private/`.
- A public page may show one clearly labelled example measurement per claim:
  named hardware, CPU tier, load, profile and median, presented as an example
  and not as the contract. Raw session logs (every repeat, unfinished runs,
  per-run load averages) stay in `./private/`.

## Commits

- Stage exact paths. Never run `git add -A` without first reviewing
  `git status`, and leave unrelated edits alone.
- Commit subjects name the area and result, for example
  `fix(transport): resume playback from the requested cursor`.
- Enable the hook with `git config core.hooksPath .githooks`. It formats fully
  staged `.rs` and `.py` files. For partially staged files it only checks the
  staged content.
