# Remote plugin index

Distribution layer for the free-plugin marketplace. This document covers the index
format, hosting, compression, integrity, caching, and the `ocp plugin-index` builder.
It does not cover the local catalog/store UI or the plugin scanner.

Status: builder and verifier implemented in `crates/oc-cli`; the Tauri
client (fetch, pinned-signature verification, anti-rollback, atomic cache, refresh
policy) is implemented in `crates/oc-gui/frontend/src-tauri/src/plugin_index.rs`.
The signed index is published from `plugin-index/` (sections 1 and 7).

## 1. Hosting

The signed index lives in the repository, in `plugin-index/`, and is served
from two origins compiled into the app and the CLI as `PRIMARY_BASE_URL` and
`FALLBACK_BASE_URL` in `crates/oc-market/src/index/verified.rs`:

| Origin | Base URL | Updated by |
|---|---|---|
| Primary | `https://verithia.github.io/opencompose/plugin-index/` | The documentation workflow, on every push to `main` that touches `plugin-index/` |
| Fallback | `https://raw.githubusercontent.com/verithia/opencompose/main/plugin-index/` | The push itself |

Both serve the same committed bytes. The primary is the documentation site:
`scripts/docs.sh build` copies `plugin-index/` into `book/plugin-index/`
unchanged, and `.github/workflows/docs.yml` deploys `book/` to GitHub Pages.
The fallback reads the default branch directly, so the index stays reachable
while a site deployment is pending or failing.

Hosting the files next to the code keeps a publish reviewable: the signed bytes
are a commit, and `cargo test -p oc-market` verifies the committed index
against the release keys on every CI run. The hosts are trusted only to serve
bytes; authenticity comes from the signature (section 4).

### Required file layout

Everything sits directly at the base; see section 2 for the format:

```
<base>/index.json
<base>/index.json.sig
<base>/catalog-<sha256>.json.zst
```

`index.json` and `index.json.sig` have fixed names. Catalog names are
content-addressed. `plugin-index/` keeps the current catalog and the previous
one, so a client that read the old manifest just before a deployment can still
finish its fetch.

`.gitattributes` marks `plugin-index/` as binary, so checkouts never convert
line endings and the signature keeps matching the exact manifest bytes.

### Requests the client makes

Exactly three kinds of request, all `GET`, all with
`User-Agent: OpenCompose/<version> plugin-index`, `Accept-Encoding: identity`,
a 10-second connect timeout, and a 30-second total timeout:

1. `GET <base>index.json` with `Cache-Control: no-cache`, plus `If-None-Match`
   when the cached copy came from the same base. A `304 Not Modified` keeps the
   cache. The body is capped at 512 KiB.
2. `GET <base>index.json.sig?v=<sha256-of-manifest>`: the detached Ed25519
   signature over the exact manifest bytes, exactly 64 raw bytes (capped at
   4 KiB). The query keeps a cache from pairing a stale signature with a new
   manifest; a static host ignores it.
3. `GET <base>catalog-<sha256>.json.zst`, only when the manifest names a catalog
   the client does not already have in its content-addressed cache. Capped at
   16 MiB compressed.

A manifest request that fails on the primary is retried on the fallback.
Catalog requests try the serving base, then the signed manifest's `mirrors[]`.

### Transfer encoding

Signatures and hashes cover the published bytes, so verification never sees a
transfer encoding. Both clients send `Accept-Encoding: identity`, and both
refuse a response that still carries a `Content-Encoding` other than identity
(`oc_market::index::check_content_encoding`). GitHub Pages and
`raw.githubusercontent.com` compress only when a client asks, so they serve the
files unchanged.

### Freshness

GitHub Pages sends `Cache-Control: max-age=600` and an `ETag` for every file,
so a publish reaches every client within about ten minutes of its deployment.
The raw origin caches for a few minutes. Catalog names are content-addressed,
so a cached catalog is never wrong, only possibly absent.

The Ed25519 signature guarantees integrity and authenticity. Freshness comes
from TLS on both origins and from anti-rollback: the client rejects a manifest
whose `generated_at` is older than the highest one it has committed, with stage
`rollback`, and keeps its last-good catalog. Residual risks:

- **First fetch.** A fresh install has no cache to compare against; TLS is the
  defense.
- **Freezing.** Withheld updates look like an idle publisher. The app shows
  staleness (`stale_warning` at 14 days).

### Moving the index

A manifest cannot advertise its own base, so moving the index means changing
the two constants and shipping an app release. The files are portable and
content-addressed; no catalog or key change is needed. Keep publishing to the
old origins until the release that drops them has reached users.

## 2. File layout

```
plugin-index/
  index.json                      # small, uncompressed, signed manifest
  index.json.sig                  # 64-byte detached Ed25519 signature over index.json bytes
  catalog-<sha256>.json.zst       # compressed catalog, name = sha256 of these bytes
```

`catalog.path` is relative to the base URL and is validated to be exactly
`catalog-<64 lowercase hex>.json.zst`; no absolute catalog URLs are accepted. Mirrors
are base URLs, and the same relative path is appended.

### Manifest

```json
{
  "catalog": {
    "bytes": 4779,
    "compression_level": 9,
    "encoding": "zstd",
    "path": "catalog-417c59af81d3762527e8a2d11c522a15e3caad56c858cb957928d01294f933c2.json.zst",
    "sha256": "417c59af81d3762527e8a2d11c522a15e3caad56c858cb957928d01294f933c2",
    "uncompressed_bytes": 72784
  },
  "catalog_version": "2026.10.22.080000",
  "generated_at": "2026-10-22T08:00:00Z",
  "min_app_version": "0.1.0",
  "mirrors": [
    {
      "base_url": "https://mirror.example.com/plugin-index/",
      "name": "mirror"
    }
  ],
  "schema": 1,
  "signature": {
    "algorithm": "ed25519",
    "encoding": "raw",
    "key_id": "ed25519:2d1a75fbd38fff78",
    "path": "index.json.sig"
  }
}
```

Fields are emitted in sorted-key order with two-space indentation and a trailing
newline. `generated_at` is RFC 3339 UTC. `catalog_version` is opaque to clients; the
builder defaults it to `YYYY.MM.DD.HHMMSS` from `generated_at`. `min_app_version` is
a semantic version; pre-release precedence is honored and build metadata does not change compatibility.

### Catalog

```json
{
  "entries": [
    {
      "description": "A lush algorithmic reverb with three decay models ...",
      "homepage": "https://plugins.example.com/example-reverb",
      "id": "example-audio.reverb-1",
      "license": "GPL-3.0",
      "installs": [
        {
          "archive": "zip",
          "bytes": 12000000,
          "format": "vst3",
          "kind": "github_release",
          "platform": "macos-aarch64",
          "sha256": "0000000000000000000000000000000000000000000000000000000000000001",
          "url": "https://github.com/example/plugin/releases/download/v1.2.3/plugin-mac.zip"
        },
        {
          "archive": "installer",
          "format": "vst2",
          "kind": "direct",
          "platform": "windows-x86_64",
          "url": "https://downloads.example.net/plugin/setup.exe"
        },
        {
          "archive": "zip",
          "format": "lv2",
          "kind": "github_release",
          "platform": "linux-x86_64",
          "plugin_id": "urn:example:reverb",
          "url": "https://github.com/example/plugin/releases/download/v1.2.3/plugin-linux.zip"
        }
      ],
      "name": "Example Reverb",
      "tags": ["reverb", "ambience"],
      "vendor": "Example Audio",
      "version": "1.2.3"
    }
  ],
  "generated_at": "2026-10-26T15:30:00Z",
  "schema": 2
}
```

Required: `schema`, `generated_at`, `entries`; per entry `id`, `name`, `vendor`,
`installs`; per install `kind`, `platform`, `format`, `url`. Optional: entry
`version`, `description`, `homepage`, `license`, `aliases`,
`known_issues`; install `plugin_id`, `sha256`, `bytes`, `archive`, `wrapped`,
`runtime`, `companion_content`. Empty optional arrays/objects are omitted.

`wrapped: true` says the archive holds a vendor installer rather than bundle
directories (u-he ships a `.pkg` inside its macOS zip, for example). The client
downloads and verifies the archive, unpacks the one installer it holds, and opens
it. An archive with several installers is refused rather than guessed.
`runtime` and `companion_content` use shared typed descriptors validated before
publication and again when clients read the catalog; see section 9.

Enumerations:

- `kind`: `direct` (vendor direct download), `github_release` (URL host must be
  exactly `github.com`), `vendor_page` (landing page; the app opens a browser).
- `platform`: `macos-aarch64`, `macos-x86_64`, `windows-x86_64`, `linux-x86_64`.
- `format`: `vst3`, `vst2`, `clap`, `au`, `lv2`, `standalone`.
- `archive`: `zip`, `dmg`, `pkg`, `tar-gz`, `tar-xz`, `7z`, `installer`. The
  client unpacks the zip, tar and 7z containers into bundle directories. A
  `.deb` or `.rpm` download is recorded as `installer`, so the client downloads
  it and hands the file to the system instead of unpacking it itself.

### Instrument packs

A catalog may also offer downloadable instruments in a top-level `packs` array.
The array is optional and additive: a catalog written before packs existed still
parses, and a kind a client does not know is skipped instead of failing the whole
catalog. Two kinds exist: `soundfont` for a single bank and `sound_archive` for
an archive of instruments and samples.

```json
{
  "packs": [
    {
      "kind": "soundfont",
      "id": "ms-basic",
      "name": "MS Basic",
      "version": "1.0.0",
      "description": "General MIDI sound set for the built-in SoundFont engine.",
      "url": "https://plugins.example.com/packs/ms-basic-1.0.0.sf3",
      "bytes": 50003968,
      "sha256": "0000000000000000000000000000000000000000000000000000000000000001",
      "license": {
        "name": "MIT",
        "url": "https://plugins.example.com/packs/MS%20Basic_License.md",
        "text": "MuseScore_General is MIT; derived from FluidR3Mono and FluidR3."
      }
    },
    {
      "kind": "sound_archive",
      "id": "freepats.upright-piano-kw-small",
      "name": "Upright Piano KW",
      "publisher": "FreePats",
      "homepage": "https://freepats.example.com/piano",
      "version": "2019-07-03",
      "description": "Compact upright piano.",
      "content": "soundfont",
      "archive": "7z",
      "url": "https://freepats.example.com/packs/piano.7z",
      "bytes": 6093984,
      "sha256": "0000000000000000000000000000000000000000000000000000000000000002",
      "unpacked_bytes": 12345678,
      "license": {
        "name": "CC0-1.0",
        "url": "https://freepats.example.com/packs/license",
        "text": "CC0 public domain dedication."
      }
    }
  ]
}
```

Required per `soundfont` pack: `kind`, `id`, `name`, `version`, `description` (at
most 280 characters), `url`, `bytes`, `sha256`, and `license` with `name`, `url`
and `text`. Installed packs live in the user's soundfont folder as `<id>.sf3` and
a preset plays one through `source: soundfont` and
`file: "<folder>/<id>.sf3"`.

A `sound_archive` pack adds `publisher`, `homepage`, `content`, `archive` and
`unpacked_bytes`. `content` is `soundfont`, `sfz` or `samples`. `archive` is
`file`, `zip`, `tar_gz`, `tar_xz` or `7z`. The bundled catalog carries its
starter `sound_archive` packs, so they list and install without remote trust. An
installed archive lives in the managed sounds folder as `<id>/`, and its files
enter the instrument search as they are laid out in the archive. `file` stores the single download as the last
path segment of `url`. A `soundfont` archive must contain at least one `.sf2` or
`.sf3` file with a `RIFF....sfbk` header, `sfz` at least one `.sfz`, and
`samples` at least one `.wav`, `.aif`, `.aiff`, `.flac`, `.ogg` or `.mp3`.
`tar_bz2` and other packed forms are not read.

Every pack URL and homepage must be absolute `https://` with a dotted host. The
signed catalog pins `bytes`, `sha256` and `unpacked_bytes`, so an install is
refused unless the downloaded size and hash match. `bytes` is greater than zero
and at most 2 GiB; `unpacked_bytes` is at least one and at most 8 GiB. Extraction
streams under the declared expansion budget plus one percent, never trusts a
header's size, and refuses absolute paths, `..`, drive prefixes, NUL names,
links, devices and case-insensitive duplicate paths. Entry count, path length and
path depth are bounded as well. The publisher validates these fields before
signing, and `license.text` must carry the acknowledgement the pack's own
license requires. A `content` or `archive` value this build does not know keeps
the whole pack opaque, so a newer catalog stays loadable; an older GUI skips
that pack instead of failing the catalog.

### Formats and scanner ids

An entry describes one download per format. The `format` value matches the
`PluginFormat` the host scans, and `plugin_id` records the id a project uses for
that format when the download contains exactly one plugin:

| `format` | What is installed | Scanner id in a project |
|---|---|---|
| `vst3` | `Name.vst3` bundle | `vst3:<lowercase name>` (spaces become `_`) |
| `vst2` | `Name.vst` bundle on macOS, a `.dll` on Windows or a `.so` on Linux | `vst2:<lowercase name>` (non-alphanumerics become `_`) |
| `clap` | `Name.clap` file or bundle | the CLAP descriptor id, e.g. `com.vendor.product` |
| `au` | `Name.component` bundle | the AudioComponent description |
| `lv2` | `Name.lv2` bundle | the plugin URI from `manifest.ttl`, e.g. `https://…` or `urn:…` |
| `standalone` | a runnable application | not addressable from a project |

`plugin_id` is optional and follows the same rules: a `vst2:`/`vst3:` prefix plus
a lowercase slug, or an absolute URI for `lv2`. LV2 URIs are read from
`manifest.ttl`, so they are recorded when the download ships one plugin; a VST2 id
is derived from the name the plugin reports at load time, so VST2 installs
generally omit it. Archives that ship several plugins (a plugin suite, a reverb
family) have no single id either; after installation the plugin browser lists the
ids the scanner produced. See [`docs/vst2.md`](vst2.md) and
[`docs/manual/dsl.md`](manual/dsl.md) for how those ids are used in a project.

### Licenses

`license` is an SPDX identifier (`GPL-3.0`, `MIT`, `BSD-2-Clause`, `LGPL-3.0`,
`BSL-1.0`, …) or `Freeware` for a free proprietary download. It describes the
plugin build, not the catalog document, and must match the project's stated
license. The value is validated as printable ASCII up to 64 characters.

Validation is enforced by `build` (and repeated by `verify`):

- unique entry ids, `[A-Za-z0-9._-]{1,128}`, non-empty `name`/`vendor`;
- all URLs `https://` with a dotted host; all `homepage` URLs too (plain
  `http://` is rejected as well, not only non-HTTP schemes);
- lowercase 64-hex `sha256` when present; `bytes > 0` when present;
- `license` printable ASCII, at most 64 characters, when present;
- `plugin_id`, when present, is `vst2:`/`vst3:` plus a lowercase slug or an
  absolute URI for `lv2`; unknown format strings fail to parse;
- no duplicate `(kind, platform, format)` within an entry;
- between 1 and 64 installs per entry; between 1 and 128 non-empty tags, each at most 64 bytes;
- catalog `schema` must be in `1..=2`; timestamps must be canonical UTC `YYYY-MM-DDTHH:MM:SSZ` values with valid calendar dates.

Unknown fields in catalog, entry, install and manifest records are ignored. The
builder drops those fields from canonical output. Known runtime and companion
descriptors reject unknown fields rather than accepting settings the installer
cannot honor. Packs of an unknown kind retain their complete opaque JSON payload
for newer clients and are skipped by older GUIs. A catalog contains at most 1,024
packs; each pack is limited to 64 KiB, and unknown payload nesting must fit the
JSON parser's recursion limit.

`oc-market::catalog_wire` owns the raw types and validation used by the publisher,
CLI and GUI. Validation failures carry a stable code and a field path, such as
`pack` at `packs[0].license.url` or `runtime` at
`entries[0].installs[1].runtime`. JSON decoding failures identify the containing
entry or pack; their message describes the missing, unknown or invalid field.
GUI installation flags are computed separately and never become signed catalog
fields.

### Why the manifest is not compressed

The manifest is the polling unit. It must be small, directly parseable, and
byte-identical for signature verification. At ~0.6-0.8 KB it costs almost nothing to
fetch, and compressing it would force the client to decompress before it can even
learn which catalog to fetch. It also keeps a failed signature cheap to diagnose
(`xxd index.json`, diff two versions) without a decompression step.

The catalog is 60-100x larger and changes as a unit, so it is compressed and cached
forever by content hash. The clients request identity encoding for every index
file (section 1), so neither file is ever transfer-compressed.

### Catalog content

The catalog (`crates/oc-gui/frontend/src/data/plugin-catalog.json`, also compiled
into the app as the bundled fallback) carries entries for `vst3`, `vst2`, `clap`,
`au`, `lv2` and `standalone` downloads, all `https://`. Every install is a link to
the vendor's own download or release asset; **no plugin binary is stored in the
repository or served by us**. `bytes` is the release asset size where the host
publishes one, and `sha256` is recorded only when the vendor publishes a digest.

The VST2 and LV2 additions record how the format was verified: archives were
listed (or, for macOS packages, expanded) to confirm the bundle names, and the
release asset URLs were fetched. That check is manual — see section 10.

A macOS `.dmg` install is recorded with `archive: "dmg"` and no `wrapped` flag
when its volume carries the declared bundle directories. An image that carries
only a vendor `.pkg` sets `wrapped: true` and stays an installer hand-off.

## 3. Compression and bandwidth

Measured on `crates/oc-cli/tests/fixtures/plugin-catalog-60.json`: 60
fabricated entries, 120 installs, canonical JSON. The test asserts the fixture stays
canonical so these numbers stay reproducible.

| Artifact | Bytes | % of raw |
|---|---:|---:|
| Catalog JSON (canonical, pretty) | 72,784 | 100% |
| zstd level 3 | 5,682 | 7.8% |
| **zstd level 9 (default)** | **4,779** | **6.6%** |
| zstd level 19 | 4,451 | 6.1% |
| gzip -9 (comparison only) | 5,194 | 7.1% |
| brotli -q 11 (comparison only) | 3,812 | 5.2% |
| `index.json` (one mirror) | 715 | 1.0% |
| `index.json.sig` | 64 | 0.1% |

Level 9 is the default: level 3 saves build CPU but costs ~19% more bytes; level 19
saves another 328 bytes (~7% of the compressed size) for a much slower build. A
real catalog with mostly unique descriptions/URLs will land around 8-12 KB, still
under the 12 KB test ceiling. Brotli wins on size but adds a dependency and cannot be
content-addressed cleanly through HTTP content negotiation, so zstd is the shipped
format. gzip/brotli numbers exist only for comparison.

### Bandwidth math

Per refresh, a client fetches `index.json` (715 B) + `index.json.sig` (64 B) = **779 B**
if both changed, or ~0 bytes of body on a `304 Not Modified`. A catalog download is
**4,779 B** and happens only when `catalog.sha256`/path changes (weekly in the normal
case).

| Scenario | Calculation | Monthly bandwidth |
|---|---|---:|
| 1k clients, manifest polled 4x/day, all 200 | 1,000 x 779 x 4 x 30 | 93 MB |
| 10k clients, manifest polled 4x/day, all 200 | 10,000 x 779 x 4 x 30 | 935 MB |
| 1k clients, weekly catalog | 1,000 x 4,779 x 4.3 | 21 MB |
| 10k clients, weekly catalog | 10,000 x 4,779 x 4.3 | 206 MB |
| 10k clients, worst case daily catalog | 10,000 x 4,779 x 30 | 1.43 GB |

With ETag revalidation most manifest polls return 304 and the real total is lower.
Even the pathological case is ~2.4 GB/month, trivial for a small VPS and well
within a modest object-store bill at 10k daily clients. At 100k daily clients the
same math reaches ~24 GB/month, before a CDN absorbs repeat catalog traffic.

**Plugin binaries are never hosted by us.** Catalog entries contain only vendor
`https://` URLs (or a vendor page). A single 20 MB plugin download is ~4,300 catalog
downloads; 10k clients installing one plugin each is ~200 GB, which is exactly the
cost this design avoids. Our bandwidth is index-sized only.

## 4. Integrity, authenticity and freshness

Three independent checks:

1. **Integrity:** `sha256` of the compressed catalog bytes is in the filename, the
   manifest, and is compared after download. The manifest also carries `bytes` and
   `uncompressed_bytes`; both are checked, and decompression is capped by the
   declared uncompressed size (16 MiB hard ceiling) to avoid compression bombs.
2. **Authenticity:** `index.json.sig` is a 64-byte raw Ed25519 signature over the
   exact `index.json` bytes. The trusted public keys are **pinned in the app**
   (`crates/oc-market/plugin-index.pub`, compiled in), never fetched over the
   network. The manifest's `key_id` (`ed25519:` + first 8 bytes of SHA-256 of the
   public key) selects exactly one trusted key, and the signature must verify
   under it; a `key_id` outside the list fails with stage `key_id`.
3. **Freshness (anti-rollback):** each signing-authority/origin scope records its
   highest committed signed manifest. Older `generated_at` values are rejected
   with stage `rollback`. Equal timestamps are accepted only for the exact same
   manifest bytes; different bytes at the same timestamp fail with stage
   `equivocation`. Every changed publication must use a fresh UTC timestamp. A
   deliberate rollback to older catalog contents is supported when republished
   with a fresh `generated_at` (section 7).

Verification first authenticates the bounded manifest bytes and signing authority,
then validates schema, canonical UTC timestamps, app compatibility, content paths
and size limits. Freshness is checked before requesting the catalog. The catalog
then passes compressed hash/length, exact decompressed length, schema and entry
validation. Install URLs from an unverified catalog are never surfaced to the UI.
The shared limits are 512 KiB for a manifest, exactly 64 bytes for its signature,
and 16 MiB each for compressed and expanded catalog bytes. Catalog entry arrays
are limited to 100,000 items during deserialization.

Failure contract:

- Any signature, key-id, rollback, schema, hash, size, decompression, or
  catalog-validation failure rejects the downloaded set.
- Keep the last known-good manifest + catalog on disk; keep serving it with a visible
  "catalog refresh failed" indicator.
- If no cached copy exists, fall back to the catalog bundled with the app (also
  signed and verified at app build time).
- Never use partially verified data, and never downgrade to "unsigned mode".
- Surface the stage that failed (signature / key_id / rollback / hash / schema /
  network) and retry with exponential backoff, capped at the normal polling interval.

Tests cover a tampered catalog, tampered manifest, tampered signature, a foreign
signing key, a missing signature, a 404, and an older-manifest replay. The
anti-rollback check has no effect on the first fetch (no cache to compare); TLS is
the defense there, not the signature.

## 5. Caching and offline behavior

| Object | Cache policy | Client behavior |
|---|---|---|
| `index.json` | `max-age=600` and an `ETag` on GitHub Pages | Store `ETag`; send `If-None-Match`. 304 keeps the cache and updates `checked_at`. 200 with identical bytes is treated as unchanged. |
| `index.json.sig` | Same as the manifest | Fetch with `Cache-Control: no-cache` and a manifest-hash query (`index.json.sig?v=<sha256-of-manifest>`) so a cached old signature cannot be paired with a new manifest. |
| `catalog-<hash>.json.zst` | Same as the manifest; the name never changes meaning | Cache forever by hash; never revalidate. |
| Installed plugin binaries | N/A | Downloaded from vendor URLs, not proxied or cached by us. |

The GUI and CLI use the same verifier and scoped cache root,
`~/.opencompose/plugin-index/scopes/<authority-and-origin-hash>/`. Each scope has
immutable signed catalog snapshots under `catalogs/<manifest-hash>/`, an atomic
`head` pointer, and a signed high-water mark selected by `highest`. Snapshot
`state.json` carries the fetch/check time, ETag, source origin and trust scope;
manifest metadata is always read from authenticated bytes.

Readers hold a shared per-scope OS lock while loading snapshots. Writers acquire
an exclusive lock and recheck freshness before committing.
They atomically commit the signed high-water mark before publishing the catalog
head. If publication is interrupted, the last-good catalog remains available and
an exact retry can finish publication. After a successful head commit, only the
current and previous catalog snapshots are retained. Unreferenced staging files
and obsolete snapshots are removed while readers are excluded; the highest
signed watermark is preserved. Signature, schema, app compatibility,
bounds, compressed hash and exact decompressed length are rechecked on disk load.
A damaged signed watermark reports an error instead of silently resetting rollback
protection. Conditional HTTP replies and identical manifest bytes use the same
verification and freshness policy.

A matching, fully verified legacy `last-good/` cache can migrate into its scope;
legacy files remain intact. If an older app changes those files during migration,
verification fails without publishing a mixed snapshot; the GUI reports the
failure and uses its bundled fallback. Unscoped or foreign-authority/origin caches are not
adopted. `OCP_PLUGIN_INDEX_DIR` overrides the cache root without changing trust.
The app's bundled catalog remains the final fallback.

Offline UX contract:

- Offline or refresh failure with a cache: show the catalog, labelled
  **"Using cached plugin catalog from `<generated_at date>` (refresh failed: `<reason>`)"**,
  with a retry action.
- Cache older than 14 days: add a stale-catalog warning; do not block installs, since
  vendor URLs are external and may still work.
- No cache and no network: use the bundled catalog and label it as bundled.
- A remote manifest requiring a newer app leaves a compatible last-good catalog
  intact. An older app also refuses to load an incompatible catalog from disk.

Polling policy: the app fetches the index when the Store's plugin list opens, and
at no other time; a cold start reads the last-good cache from disk and makes no
request. An open inside the 6-hour interval answers from the cache without network
I/O, and the explicit Refresh action is rate-limited to once per 5 minutes.
Content-addressed catalogs mean a refresh never re-downloads an unchanged
catalog.

### Client API and policy

`plugin_index_status` returns `source` (`bundled` / `cache` / `remote`),
`generated_at`, `fetched_at` (Unix seconds), `stale_days`, `stale_warning`
(true at 14 days), `last_error`, `catalog_version`, `min_app_version`, and
`next_refresh_in_seconds` (the manual-floor countdown). `plugin_index_refresh`
takes `force`; the open path uses `force=false` and returns without network I/O
when the 6-hour interval has not elapsed. `force=true` bypasses that interval but
never the 5-minute manual floor, which returns stage `policy`. A failed fetch is
reported through `last_error` (and the panel's warning), never as a background log
line.

The transport is a small `IndexFetch` trait with a `reqwest` implementation and a
scripted fake in tests. The HTTP client sends `User-Agent: OpenCompose/<version>
plugin-index`, a 10-second connect timeout, a 30-second total timeout, and
`Cache-Control: no-cache`; the manifest is capped at 512 KiB, the signature at
4 KiB, and the compressed catalog at 16 MiB. The signature request carries
`?v=<sha256-of-manifest>` so a stale signature cannot pair with a new manifest.
Manifest fetches fall back to the compiled fallback base (the raw origin in
section 1); catalog fetches try the serving base, then the signed manifest's
`mirrors[]` filtered to `https://`. Every request sends `Accept-Encoding:
identity`, and a response with another `Content-Encoding` fails at stage
`network`.

Failure stages are reported as `network`, `signature`, `key_id`, `rollback`,
`schema`, `min_app_version`, `catalog_size`, `catalog_hash`, `decompression`,
`catalog_validation`, `storage`, and `policy`. Any failure keeps the last-good
cache (and its files) untouched and never enables unsigned data.

The MCP surface exposes `plugin_index_refresh`; the built-in AI panel does not,
so conversational AI can search the catalog but cannot trigger network refreshes.

## 6. Versioning and compatibility

- `schema` is an integer. The manifest is `1`; the catalog is `2` since the VST2
  and LV2 formats and the `license`/`plugin_id` fields were added. Clients accept
  catalog schema `1..=2`, so an older published catalog stays readable.
- Why the catalog bump: an old client cannot parse `"format": "vst2"`/`"lv2"` (an
  unknown enum variant fails the whole document), so bumping lets it reject the
  catalog at the `schema` stage and keep its last-good copy, instead of failing
  while parsing. Old clients therefore do **not** get the new formats; they keep
  their cached catalog until the app is updated. The manifest schema did not
  change, and manifest `schema` is still checked as `<= supported`.
- A **newer manifest schema** is not guessed at: keep the last-good catalog, show
  "Update OpenCompose to refresh the plugin catalog", and continue using cached data.
  A newer **catalog schema** is handled the same way (stage `schema`), and the
  last-good catalog is kept; nothing is ever read past a schema this build knows.
- An **older schema** is accepted for backward compatibility (`1..=2`); the
  client's parser ignores fields it does not know.
- Unknown fields are ignored at every level. This is tested for catalog, entry, and
  install; consumers must not reject a document because of extra keys.
- `min_app_version` is compared as `MAJOR.MINOR.PATCH`. If the running app is older,
  the client does not apply the new catalog; it keeps the last-good/bundled catalog
  and prompts for an update. Pre-release suffixes are treated as their numeric core.
- `catalog_version` is an opaque string for display, logs, and rollback bookkeeping.
  Clients must not parse or order it.
- `generated_at` is used **only** for anti-rollback ordering: a validly signed
  manifest older than the active cache is rejected with stage `rollback`. It is
  never used for expiry, staleness, or cache invalidation. A legitimate rollback
  must therefore set a fresh `generated_at`.
- Canonical key order is a producer guarantee, not a consumer contract. Clients parse
  by field name.

## 7. Publishing cadence and rollback

- Publish whenever the catalog changes, plus immediately for security issues,
  delistings, or a vendor URL fix. An unchanged catalog produces the same hash,
  so a publish that only moves `generated_at` adds no catalog file.
- Every publish needs a `generated_at` later than the committed one; the script
  refuses anything else, because clients would reject it with stage `rollback`
  or `equivocation`.
- **Never revert an index commit.** A reverted manifest carries an older
  `generated_at`, and every client that saw the newer one refuses it. Roll back
  by publishing again.
- **Rollback:** restore the older catalog JSON (for example with
  `git checkout <commit> -- crates/oc-gui/frontend/src/data/plugin-catalog.json`)
  and publish it. The build is deterministic, so the catalog gets its old hash
  and file name back, and the manifest gets a fresh `generated_at`. Propagation
  is the ten-minute origin cache.
- A broken documentation build blocks the primary origin, not the fallback: the
  raw origin serves the pushed files immediately, and clients fall back to it
  when the primary fails. Fix the docs build so both origins agree again.
- Because clients keep the last-good catalog and reject bad signatures or
  hashes, a faulty publish degrades to "stale but working" rather than a broken
  marketplace.

### Publishing workflow

Publishing is a local action on the machine that holds the private key:

```sh
cargo build --release -p oc-cli
scripts/publish-plugin-index.sh --key ~/.config/opencompose/plugin-index-release/plugin-index.key
git add plugin-index
git commit -m "Publish the plugin index for <generated_at>"
git push origin HEAD:main            # or merge a pull request
scripts/check-plugin-index-live.sh --wait 900
```

The script signs the catalog, verifies the result against
`crates/oc-market/plugin-index.pub`, checks that `generated_at` moves forward,
and writes `index.json`, `index.json.sig` and the catalog into `plugin-index/`,
keeping the previous catalog and deleting older ones. It never commits, pushes
or uploads. A signing key that the app does not trust fails verification before
any file is written.

The push is the publish. GitHub Pages deploys the whole site atomically, so the
manifest, its signature and its catalog change together. The live check
downloads each file from both origins, compares it with `plugin-index/` byte
for byte, and verifies the downloaded set; `--wait` keeps retrying while the
deployment and the CDN catch up.

The private key never enters CI. CI only verifies the committed result: the
`the_committed_plugin_index_verifies_against_the_release_keys` test in
`oc-market` authenticates the manifest, checks the catalog hash and size, and
checks that the directory holds only the expected files.

### Keys and rotation

`crates/oc-market/plugin-index.pub` lists two release keys, one hex public key
per line with `#` comments:

| Role | Key id | Use |
|---|---|---|
| Primary | `ed25519:bcb4e9972d740701` | Signs every publish |
| Spare | `ed25519:450b5ca039023d95` | Kept offline; signs only after the primary is lost or retired |

Because every shipped app already trusts the spare, losing or retiring the
primary needs no emergency release:

1. Publish with the spare key. Every client accepts it immediately.
2. Generate a new spare offline with `ocp plugin-index keygen --out <offline-dir>`.
3. In the next app release, list the spare as the primary and the new key as the
   spare, removing the old primary.

A leaked primary is handled the same way, but until the release that removes it
has reached users, a holder of the leaked key can still sign an index those
users accept. Anti-rollback limits the damage to manifests newer than the last
legitimate publish, which is why publishing promptly with the spare matters.

Losing both private keys requires a release with a new list; installed apps keep
their last-good catalog and bundled catalog until they update.

## 8. CLI

`ocp plugin-index` was chosen over a separate binary: the CLI already ships, users
and CI already have it, and the subcommand keeps one install target and one version
to pin. A dedicated bin would add a second artifact for three commands.

```sh
# one-time key generation (private key stays 0600 and out of the repo)
ocp plugin-index keygen --out ~/.config/opencompose

# build; SOURCE_DATE_EPOCH makes the output reproducible
SOURCE_DATE_EPOCH=1792656000 \
ocp plugin-index build \
  --catalog catalog.json \
  --out dist \
  --key ~/.config/opencompose/plugin-index.key \
  --min-app-version 0.1.0 \
  --catalog-version 2026.10.22.1

# release gate
ocp plugin-index verify dist --public-key plugin-index.pub
```

`build` prints raw/compressed/manifest/signature sizes, the catalog SHA-256, the
catalog version, the key id, and the exact upload list. It rejects invalid catalogs
before writing anything. `verify` exits non-zero on any signature/hash/schema error
and prints a one-line summary on success.

`--level` (1-22, default 9), `--generated-at` (RFC 3339 UTC), and repeated `--mirror
NAME=URL` are available for CI. `SOURCE_DATE_EPOCH` is honored when `--generated-at`
is absent; otherwise the current UTC time is used. Input catalog `generated_at` is
**not** rewritten, so the publisher controls it.

The consumer side is a separate subcommand, `ocp plugin install <id>`; see
section 9.

### Publishing script

`scripts/publish-plugin-index.sh` wraps build and verify and writes the result
into `plugin-index/` (section 7):

- The key path is mandatory (`--key` or `OCP_PLUGIN_INDEX_KEY`); the script exits
  without it.
- Verification always uses the committed `crates/oc-market/plugin-index.pub`, so
  only a release key can publish.
- `--catalog`, `--min-app-version`, `--catalog-version`, `--generated-at` and
  `--mirror` pass through to `ocp plugin-index build`. `--dest` writes somewhere
  other than `plugin-index/`, for rehearsals.
- `OCP_BIN` selects the `ocp` binary (default `target/release/ocp`); build it with
  `cargo build --release -p oc-cli`.

### Key handling

- `keygen` writes `plugin-index.key` (PKCS#8 DER, mode 0600) and `plugin-index.pub`
  (64 hex characters + newline). The public key is safe to commit; the private key is
  not.
- Build key resolution: `--key FILE`, else `OCP_PLUGIN_INDEX_KEY` (path), else
  `OCP_PLUGIN_INDEX_KEY_PKCS8_HEX` (hex-encoded PKCS#8 bytes).
- Verify keys: `--public-key FILE` or `OCP_PLUGIN_INDEX_PUBKEY`. The file is
  either 32 raw bytes or a trust list: one 64-character hex key per line, with
  blank lines and `#` comments ignored. A `keygen` `.pub` file is a one-key list.
  A list holds at most eight distinct keys.

### Explicit remote trust

`crates/oc-market/plugin-index.pub` is the single release trust configuration,
shared by the GUI and the CLI. An empty list disables remote refresh: the app
starts with its bundled catalog and reports remote trust as unconfigured. The
retired development authority is rejected as a release key.

For local debugging, pass `--index URL --public-key FILE` to the CLI.
Debug GUI builds accept `OPENCOMPOSE_INDEX_BASE_URL` and
`OPENCOMPOSE_INDEX_PUBLIC_KEY` (hex); release GUI builds ignore those overrides.
Tests create throwaway authorities in process and never implicitly trust a fixture key.

Cached remote state is scoped to the trusted key list and the configured
origins. Caches from a different list or origin are left on disk and ignored,
and successful refreshes publish into the matching scope. A release that
changes the list therefore starts a new scope and refetches once.

### Determinism

Given the same catalog, generated timestamp, key, zstd level, and builder version,
the output bytes are identical: sorted keys, fixed two-space pretty printing, no
floating-point fields, no timestamps inside the zstd frame, and deterministic
compression. CI can diff the emitted files or compare hashes. The key itself is
random only at `keygen` time; production reuses one key.

### Rebuilding and verifying after a catalog edit

Editing the catalog is a normal source change; there is no generated step.

```sh
# 1. validate the committed catalog end to end (no network, throwaway key)
cargo test -p oc-cli bundled_catalog_builds_and_verifies

# 2. sign it into plugin-index/, then commit and push (section 7)
scripts/publish-plugin-index.sh --key ~/.config/opencompose/plugin-index-release/plugin-index.key
```

A catalog edit is not live until it is published: the app keeps using the last
signed index (or its bundled copy) until then.

Remote link checking is manual and deliberately outside `cargo test`, which never
touches the network:

```sh
python3 - <<'EOF'
import json, subprocess
catalog = json.load(open("crates/oc-gui/frontend/src/data/plugin-catalog.json"))
urls = {u for e in catalog["entries"] for i in e["installs"] for u in [i["url"]]}
urls |= {e["homepage"] for e in catalog["entries"] if e.get("homepage")}
for url in sorted(urls):
    code = subprocess.run(["curl", "-sS", "-o", "/dev/null", "-L", "-A", "Mozilla/5.0",
                           "-w", "%{http_code}", "--max-time", "40", url],
                          capture_output=True, text=True).stdout
    print(code, url)
EOF
```

A `000` means the origin refused or timed out (retry with a browser user agent);
`404` means the pinned asset is gone and the entry needs a new URL.

## 9. Installing a catalog entry

### The one-click rule

An entry installs in one call when the signed catalog answers all four of these:

1. the entry has a build for the host platform;
2. that build is a direct artifact (`direct` or `github_release`) that the
   installer can use: a `.zip`/`.tar.gz`/`.tar.xz`/`.7z` bundle, a macOS `.dmg`
   whose volume carries the declared bundles, an AppImage, or a vendor installer
   it can open;
3. the build declares `sha256` (and, when present, `bytes`);
4. for an executed install, the requested platform is the host platform.

Condition 3 is a hard rule, not a preference. A URL alone is an assertion that a
file exists, not an assertion about which bytes it contains, so an unpinned build
is refused instead of downloaded. Do not add a checksum you have not computed
from the exact artifact: see [Pinning an artifact](#pinning-an-artifact).

Two shapes are special:

- A build that names a `runtime` descriptor is a **managed separator runtime**:
  the app unpacks it into its own runtime directory and validates the descriptor
  (provider, layout, required files) before the archive is accepted. The resolver
  still requires the pinned sha256, and the mode is `extract`; `ocp plugin
  install` refuses it because the runtime lives inside the app.
- A build with `companion_content` installs the plugin and its verified factory
  data in one GUI, agent, or CLI operation. Both archives must pass their size
  and SHA-256 checks before either is published. The shared installer stages
  each destination, serializes publication, and rolls prior destinations back
  if a move fails. Separate directories do not share a filesystem atomic
  rename; this is error rollback, not a guarantee against process or machine
  failure. If rollback also fails, the error identifies retained backups for
  manual recovery.

Companion content currently supports the `surge-xt-factory` target for macOS
ARM64 and Intel VST3/CLAP ZIP builds with a TAR-GZ factory archive. Factory data
is installed under `~/Library/Application Support/Surge XT`; a custom plugin
directory changes only the plugin destination. Linux and Windows companion
builds are rejected until their storage and installation contracts are supported.
The installed check uses the scanned plugin format's selected descriptor and
content hash, so compatible identical factory data may be shared across plugin
versions. It does not use another format's first catalog descriptor.

For the Spleeter provider, a macOS or Linux build uses layout version 1 with
`bin/spleeter` as the launcher and `python/bin/python3` as its bundled
interpreter. A Windows x86-64 build uses layout version 2 with
`python/python.exe` as the launcher and `runner/separate.py` as the entry
script. The app runs that executable directly with `-I -B` and the script path.
Bundle the Python dependencies and model weights for offline use. Both layouts
require hashes for the launcher and the two-stem checkpoint, data, index and
`.probe` files under `models/2stems/`. Windows also requires hashes for the
entry script and `models/2stems/model.meta`.

Ship each platform as a pinned `standalone` build with a `tar-gz` archive
containing regular files and directories. Test the extracted package from a
directory with spaces on a clean host. During installation the runtime volume
needs room for both the temporary download and the expanded runtime. The runner
receives `separate -o <output-directory> <input-wav>` and must produce `vocals.wav`
and `accompaniment.wav` beneath the output directory.

Two modes come out of the resolver, and `plugin_search`, `plugin_pull` and
`ocp plugin install` all report the same one:

| mode | meaning |
| --- | --- |
| `extract` | download, verify, unpack into the plugin directory. Runs headlessly. |
| `installer` | download, verify, write the vendor installer, then open it. A person finishes it. |

`extract` covers zip, tar and 7z bundles, AppImages and macOS disk images. A disk
image is attached read-only without Finder or auto-open, its
`.vst3`/`.vst`/`.clap`/`.lv2` bundles at the volume root or one folder below are
copied into staging, and the volume is detached on every path. A vendor
installer inside the image is never run: an image that holds only a `.pkg` falls
back to the `installer` hand-off. A catalog entry whose image wraps an installer
sets `wrapped: true`, so the resolver reports the hand-off before a download.

### One policy, four surfaces

`crates/oc-market` owns the decision:

- `oc_market::plan` resolves an entry to a `Plan` (pinned artifact, format, mode)
  or a `Refusal`;
- `oc_market::verify_artifact` is the single size-and-sha256 rule;
- `oc_market::extract_bundles` / `extract_tar_bundles` / `extract_tar_xz_bundles` /
  `extract_seven_z_bundles` unpack a verified archive;
- `oc_market::extract_dmg_bundles` copies verified disk image bundles on macOS;
- `oc_market::index` holds the pinned index key and the manifest signature check.

The GUI Store, the agent tools (`plugin_search`, `plugin_pull`), and the CLI
(`ocp plugin install`) all call that crate. A build one of them installs is the
build the others install, and a refusal reads the same everywhere.

Install URLs may be `https://` anywhere, or plain `http://` on loopback
(`127.0.0.1`, `localhost`, `[::1]`). Loopback is the user's own machine and is
how the install path is rehearsed without the public internet; a remote install
source must still be TLS.

### Refusal vocabulary

`plugin_search` returns a `refusal` object with a stable `code`, the printed
`message`, and the `url` a person can open by hand. `plugin_pull` and
`ocp plugin install` return the same message as their error.

| code | message | what to do |
| --- | --- | --- |
| `unknown_id` | `Unknown plugin catalog id '<id>'` | Check the id with `plugin_search`. |
| `no_build` | `<name> has no build for <platform>` | Different platform, or the entry has no build there. |
| `no_direct_archive` | `<name> has no direct archive; open <url> manually` | The vendor only links a page; a person finishes the install. |
| `no_archive` | `<name> has no supported install archive for <format>` | The build is not an unpackable bundle (for example an AU or standalone zip). |
| `unpinned` | `<name> has no pinned sha256 in the signed index; refusing to install <url>` | Pin the artifact and republish the index. |
| `platform_mismatch` | `Can only install for the current platform, not <platform>` | Run the install on the target machine. |

Download failures use the verification vocabulary directly:
`Downloaded <actual> bytes, expected <expected>` for a size disagreement and
`Downloaded archive failed sha256 verification` for a hash disagreement. Neither
leaves a bundle behind.

### The manual path

An entry the catalog only links as a page installs by hand, and the Plugins
browser carries that path:

1. **Download from vendor** calls `plugin_manual_page(id)`. The command resolves
   the id against the active catalog, takes the `vendor_page` build the resolver
   would choose for this host, and opens it in the system browser. It returns the
   address it opened. The address must be `https`: every surface that opens a web
   page shares `commands::open_https_url`, which refuses another scheme, or a
   URL carrying whitespace, before a process starts. An entry that has a pinned
   artifact is refused here, because the Store has an install path for it.
2. The person finishes the vendor installer.
3. **Check again** calls `plugin_installed_check(id)`. It walks the plugin
   directories, the scan receipts and the managed runtimes, and reports
   `{installed, matched, scanned}`. It downloads nothing, writes nothing and
   launches no installer. When `installed` is true the row flips to installed and
   the app reloads the catalog.

Matching is one matcher (`plugin_store::installed_match`), shared by the Store
badge, the `installed` flag `plugin_search` reports and this check. It compares
the normalised entry name, its aliases, and the slug of a pinned `plugin_id` with
the normalised bundle name. A bundle name carrying a version suffix matches its
entry (`TAL-Vocoder-2` for `TAL Vocoder`); a key shorter than six characters must
match exactly, so another vendor's product cannot pass for this one.

`plugin_catalog` also carries a computed `installed` flag per entry, so the UI
never compares names itself: the badge, the check action and the agent tools all
read the same answer for the same walk of the plugin folders.

Three entries — Melda MFreeFXBundle, Kilohearts Essentials, Analog Obsession —
install as a set of per-plugin bundles, so no single bundle name identifies the
release and the check reports `installed: false` for them. Confirm one of those
with a plugin scan, or pin a vendor artifact when the publisher offers one.

### Agent tools

`plugin_search` never downloads. Each entry carries the install verdict, so
an agent can tell a one-click entry from a page it must hand to a person:

```json
{
  "id": "asb2m10.dexed",
  "name": "Dexed",
  "installable": true,
  "mode": "extract",
  "refusal": null,
  "installs": [{ "format": "vst3", "sha256": "4269…fc1", "bytes": 16939688 }]
}
```

```json
{
  "id": "u-he.zebralette",
  "name": "Zebralette",
  "installable": false,
  "mode": null,
  "refusal": {
    "code": "no_direct_archive",
    "message": "Zebralette has no direct archive; open https://u-he.com/products/zebralette/ manually",
    "url": "https://u-he.com/products/zebralette/"
  }
}
```

`plugin_pull` with `dry_run: true` returns the plan (`url`, `sha256`, `bytes`,
`target`, `mode`) and touches neither the network nor the disk. With
`dry_run: false` it installs and reports `installed`, `path` and `note`. An agent
should read `installable` before promising an install, and should pass the
`refusal.url` to the user instead of inventing a download step.

### CLI

`ocp plugin install <id>` is the one-command wrapper. `scripts/install-plugin.sh`
runs the same command after locating the binary, so a shell needs no arguments
beyond the id.

```sh
ocp plugin install asb2m10.dexed --dry-run
ocp plugin install asb2m10.dexed
scripts/install-plugin.sh asb2m10.dexed --into /tmp/plugins
```

- The catalog comes from the bundled JSON, from `--catalog FILE`, or from a
  signed index (`--index URL`, verified with `--public-key FILE` or the compiled
  key). An unsigned or tampered index is refused before anything is downloaded.
- `--into DIR` installs somewhere other than the host plugin directory.
- A build with mode `installer` needs `--open`; without it the command refuses and
  names the flag rather than silently downloading an installer nobody launches.
- `--json` prints the report for a script.

Remote CLI dry-run still fetches and verifies the index and checks any existing
signed high-water mark. It does not write or migrate cache state or advance that
mark. A normal remote request commits verified index state before downloading a
plugin artifact. Persistent monotonicity therefore covers committed catalogs,
while read-only planning leaves disk state unchanged.

An open GUI may continue displaying a last-good catalog after another client has
published a newer catalog in the same signing-authority and origin scope. Fresh-cache
shortcuts recheck that scope's authenticated watermark. GUI, agent and CLI installs
recheck the signed snapshot before downloading and again before publishing files or
opening a vendor installer. The publication check holds a shared catalog lock only
through local publication; network requests hold no catalog lock. A snapshot that has
become older is refused, while an exact replay remains valid. This also applies to
managed separator runtimes, companion content and instrument packs.

### TAR archive compatibility

TAR extraction accepts gzip and xz streams with regular files and directories
with valid header checksums, UTF-8 relative paths, and complete payload and
zero-padding blocks. The 7z reader counts bytes as they arrive under the same
entry and expansion budgets. Plugin bundles
and companion factory content also accept bounded GNU long-name records. Managed
runtimes and preset bundles reject those records; preset bundles additionally accept
only their documented record and asset filenames.

POSIX PAX extended headers are currently unsupported, including headers that carry
only timestamps, ownership, or other benign metadata. Such archives fail with an
explicit unsupported-metadata error. Symlinks, hard links, device entries, and GNU
long-link records are also refused. A vendor archive may therefore require a
supported archive layout or manual installation even when its plugin format is
supported.

Each consumer enforces compressed and expanded byte budgets, entry count,
per-entry and total payload limits. Paths are limited to 4,096 bytes and 64
components. The reader requires two zero end blocks, checks the decompressor
trailer, and rejects nonzero trailing data or an appended archive. Extraction happens in private
staging before publication; an archive-validation failure leaves installed content
in place.

### Rehearsing the install path

`oc_market::loopback::FixtureServer` is a tiny HTTP/1.1 server for tests. Serve a
fixture archive and a fixture catalog on `127.0.0.1`, then drive the real
downloader through `pull` or `plugin install`. No test reaches the public
internet.

```sh
CARGO_TARGET_DIR=target/pluginflow-verify \
  cargo test -p oc-market --lib
CARGO_TARGET_DIR=target/pluginflow-verify \
  cargo test -p oc-gui --lib -- plugin_store
CARGO_TARGET_DIR=target/pluginflow-verify \
  cargo test -p oc-cli --lib -- plugin_install
```

The tests cover: a pinned archive installs and unpacks; a sha256 mismatch is
refused and writes nothing; a size mismatch is refused and writes nothing; an
unsigned index is refused before any download; an unknown id is named; a page-only
entry is reported as `no_direct_archive` with its URL; an unpinned entry is
reported as `unpinned` and refused by name.

### Pinning an artifact

The installer cannot verify what the catalog does not pin, so a publisher run
starts with the audit: every direct artifact needs `sha256` and `bytes`.

```sh
url=https://example.com/plugin.zip
curl -fL --max-time 300 -o /tmp/plugin.zip "$url"
shasum -a 256 /tmp/plugin.zip      # sha256 in lower-case hex
wc -c < /tmp/plugin.zip            # byte size
```

Add both fields to that install's object in
`crates/oc-gui/frontend/src/data/plugin-catalog.json`, then rebuild and republish
the index (section 7). Pin the file you actually downloaded, from the URL in the
catalog: a hash computed from a different build, or copied from a release page,
is a false guarantee.

The builder serialises the catalog from typed structs, so it must declare every
field a client reads. `aliases` and `known_issues` are preserved; `runtime`,
`companion_content` and known packs use the same typed validation in the publisher
and clients. Anything the builder does not declare is dropped from the signed
catalog, which is how alias-based `installed` detection silently broke before
those fields existed.

Audit of the committed catalog, by host platform, counting each entry once:

| platform | one-click (`extract`) | installer hand-off | unpinned | page-only | no build |
| --- | --- | --- | --- | --- | --- |
| `macos-aarch64` | 7 | 8 | 1 | 22 | 5 |
| `macos-x86_64` | 7 | 8 | 1 | 22 | 5 |
| `windows-x86_64` | 7 | 10 | 0 | 23 | 3 |
| `linux-x86_64` | 8 | 4 | 0 | 15 | 16 |

A one-click build downloads and unpacks without a person. A hand-off build also
installs in one call, but ends by opening the vendor installer, and
`plugin_search` reports which one an entry is before an agent promises anything.

One macOS install stays unpinned because the vendor's only artifact is a single
installer of 974 MB (DISTRHO Cardinal); pinning an artifact that large would only
turn a clear refusal into a failed download. Surge XT is pinned to its
installerless plugin-only archives instead of the 397 MB disk image. Sixteen
entries have no Linux build at all. The remaining page-only entries either ship
installers our unwrap step cannot use (several `.pkg`/`.exe` files in one
archive), need an account, or expose only a JavaScript download flow; each entry
records the exact reason.

## 10. Implementation and tests

- `crates/oc-cli/src/plugin_index.rs` — canonical JSON, build/sign, verify,
  keygen and publisher command adapters.
- `crates/oc-market/src/catalog_wire.rs` — shared signed catalog types,
  descriptor validation, bounded forward-compatible packs and typed errors.
- `crates/oc-cli/src/lib.rs` — library target exposing the builder so the client
  rehearsal test uses the same code path as `ocp`.
- `crates/oc-cli/src/main.rs` — `plugin-index build|verify|keygen`.
- `crates/oc-gui/frontend/src-tauri/src/plugin_index.rs` — transport trait,
  pinned verification, anti-rollback, cache, refresh policy, status, tests.
- `crates/oc-gui/frontend/src-tauri/src/plugin_store.rs` — active-catalog
  store/search/install, shared catalog validation.
- `crates/oc-market/src/catalog.rs` — the resolver: candidate ranking, install
  mode, and the refusal vocabulary shared by every surface.
- `crates/oc-market/src/verify.rs` — the size-and-sha256 rule for downloads.
- `crates/oc-market/src/install.rs` — install-target selection and path-safe
  zip, tar and 7z extraction.
- `crates/oc-market/src/dmg.rs` — read-only disk image attach, bundle discovery
  and headless copy on macOS.
- `crates/oc-market/src/index.rs` — the trusted key list, the manifest
  signature check, and the test that verifies the committed `plugin-index/`.
- `crates/oc-market/src/loopback.rs` — the HTTP fixture server the install tests
  run against.
- `crates/oc-cli/src/plugin_install.rs` — `ocp plugin install`.
- `scripts/install-plugin.sh` — thin wrapper over that subcommand.
- `crates/oc-cli/tests/fixtures/plugin-catalog-60.json` — canonical sample.
- `plugin-index/` — the published signed index.
- `scripts/publish-plugin-index.sh` — signs and verifies the catalog into
  `plugin-index/`.
- `scripts/check-plugin-index-live.sh` — compares both live origins with
  `plugin-index/` byte for byte and verifies what they serve.

CLI tests (run with `CARGO_TARGET_DIR=... cargo test -p oc-cli`):

- `build_verify_round_trip`
- `bundled_catalog_builds_and_verifies` — builds and verifies the committed
  catalog with a throwaway key; fails on any malformed entry, missing license, or
  bad `plugin_id`
- `unknown_format_rejected`
- `missing_required_field_rejected`
- `bad_id_rejected`
- `non_http_url_rejected`
- `scanner_plugin_id_is_validated_when_present`
- `catalog_schema_accepts_older_and_rejects_newer`
- `tampered_catalog_rejected`
- `tampered_manifest_rejected`
- `tampered_signature_rejected`
- `unknown_fields_tolerated`
- `non_https_url_rejected`
- `duplicate_id_rejected`
- `sixty_entry_catalog_compresses_under_12kb`
- `build_is_deterministic`
- `keygen_writes_usable_keypair`
- `epoch_timestamp_conversion`

Client tests (run with `cargo test -p oc-gui`):

- `remote_refresh_writes_cache_and_serves_entries`
- `cached_catalog_loads_after_restart`
- `not_modified_keeps_cache_and_updates_checked_at`
- `tampered_signature_rejected_and_cache_kept`
- `tampered_manifest_rejected_and_cache_kept`
- `wrong_pinned_key_rejected`
- `unsigned_manifest_rejected`
- `catalog_sha256_mismatch_rejected`
- `oversized_decompression_rejected`
- `newer_min_app_version_refused_and_cache_kept`
- `newer_manifest_schema_refused`
- `bundled_fallback_when_cache_empty`
- `manual_refresh_floor_honoured`
- `startup_asks_for_nothing_and_opening_the_browser_asks_once`
- `a_second_open_inside_the_interval_makes_no_request`
- `a_failed_open_is_reported_in_the_status`
- `status_reports_staleness`
- `mirrors_are_used_when_primary_catalog_fails`
- `pinned_key_is_valid_hex`
- `selfhosted_rehearsal_serves_signed_index_over_plain_http`
- `selfhosted_rehearsal_serves_signed_index_under_a_path_prefix`
- `selfhosted_rehearsal_tampered_catalog_reports_catalog_hash`
- `selfhosted_rehearsal_tampered_manifest_reports_signature`
- `selfhosted_rehearsal_foreign_signature_reports_key_id`
- `selfhosted_rehearsal_missing_signature_keeps_last_good_cache`
- `selfhosted_rehearsal_404_reports_network`
- `rollback_rejects_older_generated_at_and_accepts_fresh_republish`
- `pull_installs_a_pinned_archive_from_a_loopback_server`
- `pull_refuses_a_hash_mismatch_and_installs_nothing`
- `pull_refuses_a_size_mismatch_and_installs_nothing`
- `search_reports_installability_for_pinned_page_only_and_unpinned_entries`
- `the_installed_matcher_finds_a_bundle_by_name_and_alias`
- `the_installed_matcher_accepts_a_version_suffix_and_a_scanner_id`
- `the_installed_check_reports_what_the_walk_found`
- `the_manual_page_is_the_https_page_of_a_page_only_entry`
- `the_store_response_recomputes_installed_from_the_walk`
- `only_https_addresses_reach_the_browser`

The frontend case is in `crates/oc-gui/frontend/src/__tests__/components/PluginStore.test.tsx`:
the vendor button and the check action appear only for page-only entries, the
button calls `plugin_manual_page`, and a successful check flips the row to
installed.

The `selfhosted_rehearsal_*` tests are end-to-end over real HTTP. Each one builds
a signed index with `oc_cli::plugin_index::build` (the same function `ocp` calls),
starts a minimal `TcpListener` on an ephemeral `127.0.0.1` port, serves the exact
manifest/signature/catalog bytes, and drives `IndexClient` through its real
`reqwest` transport. They cover the root and a `/plugin-index/` path prefix, three
negative cases that fail closed while keeping the last-good cache, and a 404 that
reports `network`. No test uses an external network; the existing tests either
inject a scripted `IndexFetch` or use the loopback server.

## 11. Not implemented here

- Vendor URL crawling; the live check confirms the index origins only.
- Signatures from more than one key on the same manifest; a manifest names one
  signer.
- JSON Schema publication; validation is in Rust and is the source of truth.
- Brotli/gzip output; zstd only.
