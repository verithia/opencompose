# Sample marketplaces

The DAW is a **client** of other people's sample catalogues. It searches
several external markets, pulls one file at a time into the project, and keeps
the licence and credit beside the audio. It hosts no catalogue of its own and
maintains none: every result comes from a provider that already published it.

The same resolver, store and licence notice serve the agent tools, the bottom
panel and the `ocp sample` command, so a search from the panel and a search from
a tool answer with the same vocabulary.

## Local provider plugins

Sample search adapters are local debugging plugins. Release builds do not load
local sample providers. The signed plugin index does not list these adapters.

For a debug build, compile the optional adapter executable:

```sh
cargo build -p oc-sample --features local-provider-runner --bin oc-sample-provider
```

Add one JSON manifest per provider to `~/.opencompose/sample-providers/`.
`OPENCOMPOSE_SAMPLE_PLUGIN_DIR` can select a separate directory. For example,
`wikimedia-commons.json` contains:

```json
{
  "protocol": 1,
  "executable": "/absolute/path/to/oc-sample-provider",
  "args": ["wikimedia-commons"],
  "configuration": { "timeout_seconds": 12 }
}
```

The documented API adapters use the provider IDs `openverse`,
`wikimedia-commons`, `internet-archive`, `freesound`, and `jamendo`.
Each manifest can select its own executable and configuration. Only install
executables you trust. A plugin runs with the local user's permissions.

The host starts the executable for each request and sends a single JSON object
on stdin. The request contains `protocol`, `configuration`, the current
`network` settings, an optional `credential`, and an `operation`. The two
entry points are `{"entrypoint":"search","query":{...}}` and
`{"entrypoint":"resolve","id":"..."}`. Search returns a normalized
`SamplePage`, and resolve returns a normalized `SampleItem`. The executable
writes one JSON object on stdout, with `status: "ok"` and `value`, or
`status: "error"` and `message`. The Rust wire types and reference adapter
are in `crates/oc-sample/src/local_plugin.rs`. The host keeps the provider ID
and capability metadata in its registry.

The application always resolves providers through this local process protocol,
including builds that enable adapter test fixtures. `test-providers` exposes an
explicit fixture runtime for recorded adapter tests; it does not install
providers or bypass manifests. Availability listings and search/open operations
use the same runtime and credentials. Tests may supply an explicit plugin
directory without reading or changing the user's process configuration.

In the desktop app, use **Preferences → External Sources** to save or remove a
Freesound API token or Jamendo client ID. The app reports whether a credential
comes from the environment or the OS credential store. Freesound OAuth for
original downloads is on the same page. The CLI reads the credential
environment variables.

## The one rule

If a market has no programmatic search, or its terms forbid programmatic
download, this build does **not** scrape it. The provider is recorded as
page-only with the reason, and every search result reports it as unavailable:

```json
{ "id": "soundbible", "label": "SoundBible", "available": false,
  "reason": "no public search API; the site is browsed page by page and its terms do not allow programmatic download" }
```

A provider that is unavailable is never silently dropped, and an empty result
is never invented for it.

## Providers

| Provider | Licence | Key | Search | Download |
|---|---|---|---|---|
| Openverse | Per result, aggregated from the source (`license`, `license_url`) | none | yes | yes, direct audio URL |
| Wikimedia Commons | Per file (`LicenseShortName`, `LicenseUrl`, `UsageTerms`) | none | yes | yes, original file |
| Internet Archive | Per item (`metadata.licenseurl`) | none | yes | yes, file list read on pull |
| Freesound | Per sound label (`Creative Commons 0`, `Attribution`, …) | `FREESOUND_API_KEY` or keyring `org.opencompose.samples`/`freesound`; OAuth2 token for originals | yes | original after the DAW connects Freesound, preview otherwise |
| Jamendo | Per track (`license_ccurl`) | `JAMENDO_CLIENT_ID` or keyring `jamendo` | yes | transcoded MP3 (`preview_mp3`) |
| Looperman | Website terms apply | — | no | no |
| Splice | Website terms apply | — | no | no |
| SampleSwap | Website terms apply | — | no | no |
| SoundBible | Public-domain sound effects | none | no | no |
| Mixkit | Mixkit licence | none | no | no |
| Pixabay Audio | Pixabay Content License | — | no | no |

Providers without a supported search API appear as website-only entries.

| Site | Why not |
|---|---|
| Sample Focus | Cloudflare answers a browser challenge to the search page and to every sample page: HTTP 403 to a plain client. Recorded as page-only; no challenge is solved and no page is scraped. |
| Bedroom Producers Blog | A page request answers HTTP 403 to a plain client. The WordPress API does answer and lists the free packs, but each pack is one zip file, not a single sample this build can pull. |
| Cymatics | Free packs are delivered behind an email gate, and no per-sample file is published. |

Each provider declares one spec in `oc_sample::registry`:

| Field | Meaning |
|---|---|
| `id`, `label` | Stable id used in a tool argument and the name a chip shows. |
| `id_prefix` | Prefix every id this provider issues carries, so a pull infers the market from the id alone. |
| `requires_key` | Whether a key is needed; `key_env` and `key_account` name where it comes from. |
| `kinds` | The kinds the catalogue can plausibly serve. |
| `license` | One sentence about the licences the source publishes. |
| `search`, `download` | Whether this build implements each half. |
| `download_referer` | Optional source-page header required by a download host. |
| `page_only` | Why a market is not wired, kept as data instead of a comment. |

### Openverse

An aggregator over Freesound, Jamendo, Wikimedia and others, and the widest
market reachable without a key. It answers anonymous requests. Every result
names its source `provider`, and carries that source's licence, duration
(milliseconds in the API), file size, sample rate, channels, direct audio URL
and `alt_files`. The licence travels per result, so one page can mix CC0, CC BY
and non-commercial files; a non-free licence needs one acceptance at the pull.

### Wikimedia Commons

The MediaWiki search API, no key, a licence on every file. Search asks for
`filetype:audio` plus the query, kind and tag words, and drops MIDI sequences
and video that the same search returns under `filetype:audio`: the engine can
decode `wav`, `flac`, `mp3`, `ogg`, `oga`, `aif` and `aiff` and nothing else.
The index has no duration field, so duration and channel bounds are applied to
the returned page.

Commons is an archive, not a sample pack. A search for `snare drum` finds real
snare recordings and the metadata is honest, but the recording quality is
whatever an uploader contributed.

### Internet Archive

`advancedsearch.php` searches the index and returns each item's identifier,
title, creator and `licenseurl`; `metadata/<id>` returns the file list. The
search therefore names the item while the pull reads the file list and picks
one audio file, which keeps one search to one request instead of one request
per result. The pull prefers an original `wav`, then `flac`, `aiff`, `ogg` and
`mp3`, and records the declared length and size. An item without a licence URL
is visible, and the pull asks for acceptance before it downloads.

### Freesound

The catalogue built for samples: duration, channels, sample rate, tags and a
licence on every sound. It needs an API key, read from `FREESOUND_API_KEY`
first and then the OS credential store under the service
`org.opencompose.samples`, account `freesound`.

Freesound serves a sound's original file only to an OAuth2 client. The DAW
therefore runs an authorization-code flow: **Connect Freesound** in the Samples
panel asks for the application's client id and secret, binds a listener on an
ephemeral `127.0.0.1` port, opens
`https://freesound.org/apiv2/oauth2/authorize/` with
`redirect_uri=http://127.0.0.1:<port>/callback`, and exchanges the returned
code at `https://freesound.org/apiv2/oauth2/access_token/`. The token set and
the client credentials are stored in the same keyring service under
`freesound-token`, `freesound-client-id` and `freesound-client-secret`, so a
later refresh needs no browser. The token request follows the
Preferences ▸ Network proxy.

With a token, a pull rewrites the URL to `/apiv2/sounds/<id>/download/` and
sends `Authorization: Bearer …`; the receipt then says `quality: "original"`.
A 401 refreshes the token once and retries. Any other refusal of the original
— a client without the download scope, a removed file — falls back to the
128 kbps preview the search returned, so the pull still lands a legal clip and
the receipt says `quality: "preview_mp3"`. The licence and credit line apply on
both paths. A headless GUI session can set `OPENCOMPOSE_FREESOUND_TOKEN`,
`OPENCOMPOSE_FREESOUND_CLIENT_ID` and `OPENCOMPOSE_FREESOUND_CLIENT_SECRET`
instead of using the keyring.

### Jamendo

Creative Commons music behind a client id, with no OAuth step. The API takes
`client_id` as a plain query parameter, so it is wired: the key comes from
`JAMENDO_CLIENT_ID` or the keyring account `jamendo`. Every track carries
`license_ccurl`, its duration, artist and a streaming MP3. Jamendo exposes a streaming preview separately from the artist-controlled
download. A track can be previewed even when the artist disables downloads.
The app imports only the file in `audiodownload` when
`audiodownload_allowed` is true.

### SoundBible, Mixkit, Pixabay, Sample Focus, Bedroom Producers Blog and Cymatics

SoundBible publishes public-domain effects and Mixkit publishes free effects
and music, but neither exposes a search API; Mixkit's download control is
JavaScript-only. Pixabay's public API documents images and videos, not audio,
so its key does not open a documented audio endpoint. Sample Focus answers
every page with a Cloudflare browser challenge, Bedroom Producers Blog serves
free packs as one zip per article, and Cymatics delivers free packs behind an
email gate. All six are recorded with their reason, reported in the
availability map, and never scraped.

## Results

A search asks for text, a kind, a duration range, a channel count and tags:

| Field | Meaning |
|---|---|
| `text` | Words to match. |
| `kind` | `drum_one_shot`, `loop`, `vocal`, `fx`, `instrument_phrase` or `unknown`. |
| `min_seconds`, `max_seconds` | Inclusive bounds on the sample's duration. |
| `channels` | Channel count, usually 1 or 2. |
| `tags` | Extra terms a result must relate to. |
| `providers` | Markets to search; omitted means every available one. |
| `offset`, `limit` | Paging. `limit` is per market and clamped to 1..50. |

One result carries what a project needs to use and credit the file:

| Field | Notes |
|---|---|
| `provider`, `provider_name` | The market the item came from. |
| `id` | Provider-scoped and stable, so `pull` needs nothing else. |
| `name`, `kind` | Human name and the kind the result looks like. |
| `duration_seconds`, `channels`, `sample_rate` | `null` when the source does not report the value; the field is never guessed. |
| `license` | Machine id, source name, URL, and the four terms that change what a project may do. |
| `attribution` | The one-line credit a project repeats. |
| `source_url` | The page a person opens. |
| `preview_url` | An audition URL when the source has one. |
| `download_url` | The file the pull fetches; always from the provider. |
| `sha256`, `bytes` | Declared checksum and size, verified before the file lands. |
| `quality` | `original` or `preview_mp3`. |
| `license_ok` | Whether the licence is free to reuse; anything else also carries `license_notice`. |

A page reports `offset`, `limit`, `total` when the source knows it, and
`next_offset` when another page exists. Cross-market duplicate titles are kept:
two markets may publish the same recording under different licences and
credits, and only the licence decides whether one may enter a project.

## Refusals

A provider and the store refuse with `SampleError`, which
has a stable `code` and a sentence a person can act on:

| Code | Cause |
|---|---|
| `key_required` | The market needs an API key and none is configured. |
| `provider_unknown` | The market id is not one this build knows, or an id has no known prefix and no provider was passed. |
| `provider_unavailable` | The market is page-only, not wired, or has no direct download. |
| `unknown_id` | The id is not this provider's, or the source no longer has the file. |
| `licence_notice` | The licence is not free to reuse (non-commercial, no-derivatives, GFDL, a community licence or unknown); the message names the terms. Send `accept_licence: true` to pull it anyway. |
| `size_mismatch` | The body is not the declared size. |
| `checksum_mismatch` | The body is not the declared sha256. |
| `no_download` | The source offers no file for this result. |
| `project_unsaved` | The project has no file, so a relative media folder has no root. |

A search does not refuse an unavailable market: it reports it in `providers`
with `available: false` and a `reason`, and records a failing market in `pages`
with its error. A pull refuses with the codes above and returns the refusal as
data, not as a transport error:

```json
{ "status": "refused", "code": "provider_unavailable", "message": "Mixkit is not available in this build: no search API; the catalogue is browsed in the page and the download flow is JavaScript-only." }
```

## The licence notice

The licence travels with the file, and a licence the DAW cannot classify is
**not** a refusal: the pull asks once, the user decides, and the file keeps its
licence and credit in the project. Using a free-of-charge sample in a production
is the user's call; the DAW's job is to say what the terms are.

| Licence | At the pull | What the project carries |
|---|---|---|
| CC0 1.0, public domain | downloads | The source is still recorded in the sidecar. |
| CC BY, CC BY-SA | downloads | The author, the licence name and URL; `share_alike: true` on SA. |
| CC BY-NC, CC BY-NC-SA | asks | The non-commercial term is named; the credit line is kept. |
| CC BY-ND | asks | The no-derivatives term is named. |
| GFDL | asks | The licence text requirement is named. |
| A community licence (existing receipts) | asks | The terms sentence plus the credit line. |
| Splice preview in an existing receipt | asks | The preview terms, the pack name and the pack page. |
| SampleSwap community upload in an existing receipt | asks | The public-domain dedication with the site's caveat, and the folder page. |
| Anything else | asks | The name as the source reported it. |

`sample_pull` takes `accept_licence` (CLI `--accept-licence`); without it a
non-free licence returns `{"status":"refused","code":"licence_notice"}` with the
sentence the user reads. The panel shows that sentence and a **Download anyway**
button. A receipt records `licence_accepted: true` when the user confirmed.

The credit line travels in three places, so it survives a folder copy: the
`.json` sidecar beside the audio, the store's `index.json` receipt, and the
`attribution` field of every tool result.

## The local store

Samples live in the project's media folder:

```text
<project>/media/samples/
  openverse-snare-hit-01-72a91f6b.mp3
  openverse-snare-hit-01-72a91f6b.json
  index.json
```

* The file name is `<provider>-<slug>-<id hash>.<ext>`: stable for one id,
  readable, and free of characters a project path cannot carry.
* The sidecar is `opencompose.sample.v1`: the receipt plus the licence and the
  credit line.
* `index.json` is `opencompose.samples.v1`: one receipt per id. Pulling the same
  id again replaces its receipt instead of adding a second row.

A pull runs a fixed order, and a refusal leaves the folder untouched:

1. the licence notice (a non-free licence needs `accept_licence`),
2. the download (bounded at 96 MiB),
3. the declared size, then the declared sha256,
4. write `<file>.part`, then rename it into place,
5. write the sidecar, then the index, and remove the audio again if either
   document cannot be written.

## The panel

The bottom panel has a **Samples** mode beside Clips, Piano Roll, Presets,
Mixer, Drum Kit and Detail. It has no default keyboard shortcut; the command
palette entry `view.toggleSamples` opens it.

* A search field and a Search button; Enter submits.
* Kind chips: All, One-shot, Loop, Vocal, FX.
* Provider chips: every market the build knows, with an availability dot and
  the reason in a tooltip. Pressing a chip includes or excludes that market.
  The first search without a choice searches every available market.
* A keyed market that is included without its key shows one inline key field;
  saving writes the key to the OS keyring and re-reads the availability map.
  There is no settings panel for sample keys.
* When Freesound is included, a second row shows its OAuth2 state and a
  **Connect Freesound** control. Connecting asks for the Freesound
  application's client id and secret once, opens the browser, and stores the
  token set; a connected row offers **Disconnect** instead.
* A result list with name, provider, kind, duration, licence badge and credit
  line. A refused licence is shown in red and its Add button is disabled.
* Selecting a result previews it: the panel stops the previous audition and
  plays the new file through the audition stream, never the transport.
* **Add to project** downloads the file, writes the sidecar and receipt, and
  places an `@audio_clip` at the playhead on the selected track, or on a new
  `audio/clip` track named `Samples` when no track is selected.

## Agent tools

`sample_search` takes `query`, `kind`, `min_seconds`, `max_seconds`, `channels`,
`tags`, `providers` (an array; omitted means every available market),
`provider` (a deprecated single-market alias), `offset` and `limit`. It returns
`providers` (the availability map), `pages` (what each searched market
answered, or why it failed) and `items` with `provider`, `provider_name` and
`license_ok` on every result.

`sample_pull` takes `id`, an optional `provider` (inferred from the id prefix
when omitted) and an optional `dest`. It writes into `<project>/media/samples`
by default and returns `path`, the portable `media/samples/...` path, the
licence, the credit line, the sha256, the `quality` and, for a preview, a
warning. A Freesound pull uses the stored OAuth2 token when there is one, so
the same call returns `quality: "original"`; without a token, or after a
refused original, it returns `quality: "preview_mp3"`. Both tools are enabled in
the panel: a search reads a catalogue, and a pull writes one file into a folder
the project already owns.

A refused call returns `{"status": "refused", ...}`; a network or disk failure
returns an error, because it changed nothing and is worth retrying.

## Command line

```sh
ocp sample providers
ocp sample search "snare drum" --kind drum --limit 5 --provider openverse
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2
ocp sample pull "commons:File:Snare drum unmuffled.ogg"
ocp sample pull "ia:prelinger-snare" --provider internet-archive
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound --json
```

`search` prints one block per result with its kind, duration, channels, sample
rate, licence, credit line and, when the licence is refused, the reason.
`pull` prints the absolute path, the credit line and the quality. `providers`
lists every market with its availability and reason. `--json` prints the raw
page, receipt or provider list for a script.

The command shares the library's refusal vocabulary: a missing key prints the
same sentence the tool returns, and the process exits non-zero.

## Tests

Provider parsers use offline fixtures in `crates/oc-sample/tests/fixtures/`.
The transport is mocked for tests.

The store tests cover the refusals that matter: a size mismatch, a checksum
mismatch and an unsupported licence each leave the directory empty, and a
successful pull writes the audio, the sidecar and the receipt with a checksum
that matches the bytes. The registry tests cover availability with and without
keys, the page-only reasons and the id prefixes. The panel test renders the
panel, searches, shows the provider map and adds a result through a mocked
`invoke`.
