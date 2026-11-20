# Sample marketplaces

OpenCompose searches other people's sample catalogues and downloads one file at
a time into your project. It keeps each file's licence and credit line beside
the audio. It hosts no catalogue of its own: every result comes from a market
that already publishes it.

The **Samples** panel lists matching files from your own sound folders first,
and the market results below them; see [Samples on this computer](#samples-on-this-computer).

The bottom panel, the agent tools and `ocp sample` use the same library
(`oc-sample`), the same licence rule and the same refusal codes. The panel and
the agent tools also share one multi-market search with paging, duplicate
removal and back-off; `ocp sample` searches one market at a time.

## Local provider plugins

Each market's search runs through a local provider plugin: a separate program
that the app starts for every request. Only debug builds load these plugins.
In a release build every market reports `no local sample-search plugin is
installed`. The signed plugin index does not list them.

To use them in a debug build, compile the adapter program:

```sh
cargo build -p oc-sample --features local-provider-runner --bin oc-sample-provider
```

Then add one JSON manifest per market to `~/.opencompose/sample-providers/`,
named after the market id. `OPENCOMPOSE_SAMPLE_PLUGIN_DIR` points at another
folder. For example, `wikimedia-commons.json`:

```json
{
  "protocol": 1,
  "executable": "/absolute/path/to/oc-sample-provider",
  "args": ["wikimedia-commons"],
  "configuration": { "timeout_seconds": 12 }
}
```

`protocol` must be `1` and `executable` must be an absolute path to an
existing file; otherwise the market reports that no plugin is installed. The
adapters use the market ids `openverse`, `wikimedia-commons`,
`internet-archive`, `freesound` and `jamendo`. Each manifest can name its own
program and configuration. Install only programs you trust: a plugin runs with
your user's permissions.

The app writes one JSON request to the program's standard input. The request
holds `protocol`, `configuration`, the `network` settings (proxy and timeout),
an optional `credential` and an `operation`, which is
`{"entrypoint":"search","query":{...}}` or `{"entrypoint":"resolve","id":"..."}`.
A search answers with a `SamplePage` and a resolve with a `SampleItem`. The
program writes one JSON object to standard output: `status: "ok"` with `value`,
or `status: "error"` with `message`. An error can also carry:

- `code`: only `unknown_id` and `no_download` are read as refusals; any other
  code is a failure;
- `kind`: one of the failure kinds listed under [Refusals](#refusals);
- `http_status` and `retry_after_secs`.

An error without a `kind`, or with a kind the app does not know, reads as
`other`. The wire types and the reference adapter are in
`crates/oc-sample/src/local_plugin.rs`. The app keeps each market's id and
capabilities in its own registry, not in the plugin.

The app always reaches markets through this process protocol, including in
builds with adapter test fixtures. The `test-providers` feature adds a fixture
runtime that recorded tests pass in explicitly; it does not install markets or
bypass manifests.

Save or remove a Freesound API key or a Jamendo client ID in
**Preferences > External Sources**. The page shows whether a credential comes
from the environment or from the operating system's credential store. The CLI
reads only the environment variables.

## The one rule

If a market has no programmatic search, or its terms forbid programmatic
download, OpenCompose does not scrape it. The market is recorded as page-only
with the reason, and every search reports it as unavailable:

```json
{ "id": "soundbible", "label": "SoundBible", "available": false,
  "reason": "no public search API; the site is browsed page by page and its terms do not allow programmatic download" }
```

An unavailable market is never left out of the list, and an empty result is
never invented for it.

## Markets

| Market | Licence | Key | Search | Download |
|---|---|---|---|---|
| Wikimedia Commons | Per file (`LicenseShortName`, `LicenseUrl`, `UsageTerms`) | none | yes | the original file |
| Openverse | Per result, from the source (`license`, `license_url`) | none | yes, but only when selected | a direct audio URL |
| Internet Archive | Per item (`metadata.licenseurl`) | none | yes | the original file, chosen from the item's file list |
| Freesound | Per sound (`Creative Commons 0`, `Attribution`, …) | `FREESOUND_API_KEY` or the credential store; an OAuth2 token for originals | yes | the original after you connect Freesound, otherwise the 128 kbps preview |
| Jamendo | Per track (`license_ccurl`) | `JAMENDO_CLIENT_ID` or the credential store | yes | the MP3 in `audiodownload`, only when the artist allows downloads |
| Looperman | Website terms | none | no | no |
| Splice | Website terms | none | no | no |
| SampleSwap | Website terms | none | no | no |
| SoundBible | Public-domain sound effects | none | no | no |
| Mixkit | Mixkit licence | none | no | no |
| Pixabay Audio | Pixabay Content License | none | no | no |
| Sample Focus | Website terms | none | no | no |
| Bedroom Producers Blog | Per pack | none | no | no |
| Cymatics | Per pack | none | no | no |

A search that names no market asks every available market except Openverse,
whose search often times out. Select its chip, or name it, to search it.

Why the last nine markets are page-only:

| Market | Reason |
|---|---|
| Looperman, SampleSwap | Website search only; no documented public search API. |
| Splice | No documented public catalogue API for third-party search. |
| SoundBible | No public search API, and its terms do not allow programmatic download. |
| Mixkit | No search API, and its download control runs only in the page's JavaScript. |
| Pixabay Audio | Its public API documents images and videos, not audio. |
| Sample Focus | Every search and sample page answers a plain client with a browser challenge (HTTP 403). No challenge is solved and no page is scraped. |
| Bedroom Producers Blog | Pages answer a plain client with HTTP 403. Its WordPress API lists the free packs, but each pack is one zip file, not a single sample. |
| Cymatics | Free packs are delivered behind an email form, and no single sample file is published. |

Each market declares one spec in `oc_sample::registry`:

| Field | Meaning |
|---|---|
| `id`, `label` | The stable id used in tool arguments, and the name on a chip. |
| `id_prefix` | The prefix of every id the market issues (`commons:`, `openverse:`, `ia:`, `freesound:`, `jamendo:`), so a pull can tell the market from the id alone. |
| `requires_key` | Whether a key is needed; `key_env` and `key_account` say where it comes from. |
| `kinds` | The kinds the catalogue can serve. |
| `license` | One sentence about the licences the market publishes. |
| `search`, `download` | Whether this build implements each half. |
| `download_referer` | A source-page header that a download host requires, if any. |
| `page_only` | Why a market is not wired, kept as data. |
| `default_search` | Whether a search that names no market includes it. |

### Wikimedia Commons

The MediaWiki search API: no key, and a licence on every file. The search sends
`filetype:audio` with the query text, the kind's words and each tag as a quoted
phrase. It drops MIDI files and video that the same search returns, and keeps
only `wav`, `flac`, `mp3`, `ogg`, `oga`, `aif` and `aiff`, the formats the
engine decodes. The index has no duration field, so duration and channel
bounds are checked on the returned page.

Commons is an archive, not a sample pack. A search for `snare drum` finds real
snare recordings with accurate metadata, but the recording quality is whatever
the uploader provided.

### Openverse

An aggregator over Freesound, Jamendo, Wikimedia and other sources. It needs no
key. Every result names its source and carries that source's licence, duration
(in milliseconds in the API), file size, sample rate, channels, direct audio
URL and `alt_files`. Licences vary per result, so one page can mix CC0, CC BY
and non-commercial files. The search sends only the text; every other filter is
checked on the returned page.

### Internet Archive

`advancedsearch.php` searches the index and returns each item's identifier,
title, creator and `licenseurl`. The query text is escaped, so characters such
as `:` and `(` are searched as text, not read as query syntax. The search does
not send tags. The pull reads `metadata/<id>` for the file list and picks one
audio file, preferring an original `wav`, then `flac`, `aiff`, `ogg` and `mp3`.
This keeps a search to one request instead of one per result.

Search results have no duration or channel count, so any duration or channel
filter removes every Internet Archive result. An item without a licence URL is
listed, and the pull asks you to accept its licence first.

### Freesound

A catalogue built for samples, with duration, channels, sample rate, tags and a
licence on every sound. It needs an API key, read from `FREESOUND_API_KEY` and
then from the operating system's credential store (service
`org.opencompose.samples`, account `freesound`). The server filters duration
and channels; the app checks them again on the page, along with kind and
licence.

Freesound serves original files only to an OAuth2 client.
**Freesound original quality** in **Preferences > External Sources** connects
your account: **Connect Freesound** asks for your Freesound application's
client ID and secret, listens on a random `127.0.0.1` port, and opens
`https://freesound.org/apiv2/oauth2/authorize/` with
`redirect_uri=http://127.0.0.1:<port>/callback`. It waits up to two minutes for
the browser, then exchanges the code at
`https://freesound.org/apiv2/oauth2/access_token/`. The token and the client
credentials are stored under the same service as `freesound-token`,
`freesound-client-id` and `freesound-client-secret`, so a later refresh needs
no browser. The token request uses the proxy set in **Preferences > Network**.

With a token, a pull downloads `/apiv2/sounds/<id>/download/` with
`Authorization: Bearer …`, and the receipt says `quality: "original"`. An HTTP
401 refreshes the token once and retries. If the original is refused for any
other reason, such as an application without download permission or a removed
file, the pull downloads the 128 kbps preview from the search instead, and the
receipt says `quality: "preview_mp3"`. Both paths keep the licence and credit
line. A GUI session without a credential store can set
`OPENCOMPOSE_FREESOUND_TOKEN`, `OPENCOMPOSE_FREESOUND_CLIENT_ID` and
`OPENCOMPOSE_FREESOUND_CLIENT_SECRET` instead.

### Jamendo

Creative Commons music behind a client ID, with no OAuth step. The key comes
from `JAMENDO_CLIENT_ID` or the credential store account `jamendo`. Every track
carries `license_ccurl`, its duration, the artist and a streaming MP3. The
stream is only a preview. The app downloads the file in `audiodownload`, and
only when `audiodownload_allowed` is true. A track whose artist does not allow
downloads can be previewed but not added; the panel shows **Preview only**.

## Searching

A search takes these fields:

| Field | Meaning |
|---|---|
| `text` | Words to match. |
| `kind` | `drum_one_shot`, `loop`, `vocal`, `fx`, `instrument_phrase` or `unknown`. Tools and `ocp` also accept `drum`, `instrument` and `any`. |
| `min_seconds`, `max_seconds` | Inclusive bounds on the duration. |
| `channels` | A channel count, usually 1 or 2. |
| `tags` | Extra terms a result must relate to. |
| `commercial_ok` | When `true`, keep only results whose licence allows commercial use; non-commercial and unclassified licences are dropped. Default `false`. |
| `providers` | The markets to ask. Without it, every available market in the default search. |
| `offset`, `limit` | Paging. `limit` applies per market and is clamped to 1 to 50; the default is 20. |
| `cursor` | The `cursor` of the previous result, to fetch the next page. |

Text and tags are cleaned up before any market sees them. Runs of whitespace
become one space, empty and repeated tags are dropped, the text is cut to 200
characters, and at most 8 tags of up to 64 characters each are kept. Text and
tags are always data, never query syntax: Internet Archive escapes query
characters in the text, Wikimedia Commons sends each tag as a quoted phrase
with its quotes escaped, and Freesound quotes any tag that is not one plain
word.

Every market checks kind, duration, channel count and licence on the page it
returns, so a filter the server applied is confirmed, and a filter it does not
support still applies. A result whose duration or channel count is unknown
fails that bound. A market's next offset counts what it returned before this
check, so a filtered-out result is not fetched again.

The markets are asked in parallel. One that is down does not delay the others
beyond its own deadline.

### One result

| Field | Notes |
|---|---|
| `provider`, `provider_name` | The market the item came from. |
| `id` | Scoped to the market and stable, so a pull needs nothing else. |
| `name`, `kind`, `tags` | The name, the kind the result appears to be, and its tags. |
| `duration_seconds`, `channels`, `sample_rate` | `null` when the source does not report a value. Values are never guessed. |
| `license` | Machine id, source name, URL, and the four terms that change what a project may do. |
| `attribution` | The credit line a project repeats. |
| `source_url` | The page a person opens. |
| `preview_url` | An audition URL, when the source has one. |
| `download_url` | The file the pull fetches, always from the market. |
| `sha256`, `bytes` | The declared checksum and size, checked before the file is saved. |
| `quality` | `original` or `preview_mp3`. |
| `license_ok` | Whether the licence is free to reuse. Otherwise `license_notice` holds the sentence a pull shows. |

In search results, the three URLs are shortened for display: the query string
is removed and long URLs are cut to 160 characters. A pull resolves its own
URL from the market and never reads these.

### The search result

A search returns `providers` (the availability of every market), `pages` (one
entry per market asked), `items`, `count`, `duplicates`, `commercial_ok`,
`cursor` and `next_offset`.

Items alternate between markets in the order they were asked, so the first
rows show each market's best matches. Two items whose landing pages have the
same address are one upload listed twice, for example by an aggregator and by
the site it indexes. The comparison ignores the scheme, the case of the host,
a trailing slash, the fragment, and tracking parameters such as `utm_*`. The
first market asked keeps the item, and `duplicates` counts the rest. Different
uploads with the same title are all kept.

Each market pages on its own, so the result carries an opaque `cursor`. Send it
back as `cursor` with the same query to get the next page of every market that
still has results. A market that has returned its last page is left out, and
`cursor` is `null` when no market has more. A cursor that is malformed, from
another format version or longer than 2048 characters is refused with
`invalid_cursor` before any market is asked. `offset` and `next_offset` still
work for older callers: every market starts at the same offset. A search that
sends a cursor returns `next_offset: null`.

### Page entries

Every entry in `pages` has these fields:

| Field | Meaning |
|---|---|
| `provider`, `provider_name` | The market. |
| `offset` | The offset the market was asked at; `null` when it was not asked. |
| `count`, `total`, `next_offset` | What the market returned, its reported total and its own next offset. |
| `error` | A sentence for people, or `null`. |
| `error_kind` | `needs_key`, `provider_unavailable`, `skipped`, `rate_limited`, `not_found`, `unavailable`, `timeout`, `network`, `parse` or `other`; `null` on success. A rejected key (HTTP 401 or 403) is `needs_key`, like a missing one. |
| `skipped`, `skip_reason` | `skipped` is `true` when the market was not asked, and `skip_reason` is `rate_limited` or `circuit_open`. |
| `retry_after_secs` | Seconds until the market is asked again, when known. |
| `http_status` | The status of a failed request, when there was one. |

### Back-off

A market that times out, cannot be reached or answers HTTP 5xx twice in a row
is skipped for ten minutes (`circuit_open`). A market that answers HTTP 429 is
skipped for as long as its `Retry-After` asks, at least one second and at most
ten minutes, or 60 seconds when it gives no `Retry-After` (`rate_limited`). The
app reads `Retry-After` only in seconds. A bad key, a missing record or an
unreadable answer says nothing about the market's health, so the market stays
searchable. The back-off state is shared by every search in the running app.

## Refusals

A market or the store refuses with a `SampleError` that has a stable `code`
and a sentence you can act on:

| Code | Cause |
|---|---|
| `key_required` | The market needs an API key and none is set. |
| `provider_unknown` | The market id is not one this build knows, or an id has no known prefix and no market was named. |
| `provider_unavailable` | The market is page-only, has no plugin installed, or has no direct download. |
| `invalid_kind` | The kind is not one of the accepted words. |
| `invalid_cursor` | The cursor is malformed, from another version or too long. |
| `unknown_id` | The id is not this market's, or the source no longer has the file. |
| `licence_notice` | The licence is not free to reuse (non-commercial, no derivatives, GFDL, a community licence or unknown). The message names the terms. Send `accept_licence: true` to download it anyway. |
| `no_download` | The source offers no file for this result. |
| `invalid_download` | The download did not answer with success, or exceeded the size limit. |
| `size_mismatch` | The file is not the declared size. |
| `checksum_mismatch` | The file does not match the declared SHA-256. |
| `invalid_audio` | The file is not an audio container the app recognizes, or its header is incomplete. |
| `project_unsaved` | The project has no file yet, so it has no media folder. |

A failure that is not a refusal carries a `kind`, so a caller can decide what
to do without reading the message:

| Kind | Cause |
|---|---|
| `rate_limited` | HTTP 429. `retry_after_secs` repeats the source's `Retry-After`, capped at one day. |
| `unauthorized` | HTTP 401 or 403: the key or token is missing, wrong or expired. |
| `not_found` | HTTP 404 or 410. |
| `unavailable` | HTTP 5xx. A `Retry-After` is repeated as for `rate_limited`. |
| `timeout` | The request did not finish in time. |
| `network` | The connection failed or broke. |
| `parse` | The answer was not the JSON the market documents. |
| `other` | Anything else. |

Each search request has a 15-second deadline and each download a 180-second
one, in the panel, the agent tools and `ocp` alike.

A search does not refuse an unavailable market. It lists the market in
`providers` with `available: false` and a `reason`, and lists a market that
failed in `pages` with its error. A pull refuses with the codes above and
returns the refusal as data, not as a transport error:

```json
{ "status": "refused", "code": "provider_unavailable", "message": "Mixkit is not available in this build: no search API; the catalogue is browsed in the page and the download flow is JavaScript-only." }
```

## The licence notice

The licence travels with the file. A licence that OpenCompose does not treat as
free to reuse is not refused: the pull asks once, you decide, and the file
keeps its licence and credit line in the project. Whether you use a free sample
in a production is your decision; the app's job is to show the terms.

| Licence | At the pull | What the project keeps |
|---|---|---|
| CC0 1.0, public domain | downloads | The source, in the sidecar. |
| CC BY, CC BY-SA | downloads | The author, the licence name and URL; `share_alike: true` for SA. |
| CC BY-NC, CC BY-NC-SA | asks | The non-commercial term and the credit line. |
| CC BY-ND | asks | The no-derivatives term. |
| GFDL | asks | The requirement to include the licence text. |
| A community licence (in existing receipts) | asks | The terms sentence and the credit line. |
| A Splice preview (in existing receipts) | asks | The preview terms, the pack name and the pack page. |
| A SampleSwap upload (in existing receipts) | asks | The public-domain dedication with the site's caveat, and the folder page. |
| Anything else | asks | The licence name as the source reported it. |

`sample_pull` takes `accept_licence` (`--accept-licence` in the CLI). Without
it, a licence that needs a notice returns
`{"status":"refused","code":"licence_notice"}` with the sentence you read. The
panel shows that sentence with **Download anyway** and **Cancel**. The receipt
records `licence_accepted: true` when you accepted.

The credit line is kept in three places, so it survives copying the folder:
the `.json` sidecar beside the audio, the receipt in the store's `index.json`,
and the `attribution` field of every tool result.

## The local store

Samples go into the project's media folder:

```text
<project>/media/samples/
  openverse-snare-hit-01-72a91f6b.mp3
  openverse-snare-hit-01-72a91f6b.json
  index.json
```

- The file name is `<market>-<slug>-<id hash>.<ext>`: the same for one id,
  readable, and free of characters a project path cannot hold. The extension
  follows the audio container the app detects in the file. If the name is
  taken, the pull adds part of the file's checksum and a number instead of
  replacing the existing file.
- The sidecar has the schema `opencompose.sample.v1`: the receipt, the licence
  and the credit line.
- `index.json` has the schema `opencompose.samples.v1`, with one receipt per
  id. Pulling the same id again replaces its receipt instead of adding a row.

A pull runs in this order, and a refusal or failure leaves the folder as it
was:

1. the licence notice (a licence that needs a notice requires
   `accept_licence`);
2. the download, at most 96 MiB;
3. the declared size, then the declared SHA-256;
4. the audio container check;
5. the audio, the sidecar and the updated index are written to a private
   staging folder, then put in place. Replacing `index.json` is the last step;
   if a step fails, the files this pull placed are removed again.

## The panel

**Samples** is a mode of the bottom panel, next to Clips, Piano Roll, Presets,
Mixer, Drum Kit, History and Inspector. It has no default shortcut; the command
**Show samples** (`view.toggleSamples`) opens it. The **Samples** tab of
the **Browse** overlay shows the same panel.

The bottom panel is short, so the panel spends its height on results: the
search, duration, commercial filter, kind chips and market chips share one
toolbar strip that wraps to a second line only when it must, every result is
a single line, and a wide panel puts two results on each line. A narrow panel
returns to one column and drops the duration and the commercial label before
it lets the toolbar or a row scroll sideways.

- A search field and a **Search** button. Enter also searches. One search
  asks your sound folders and the included markets.
- Kind chips: **All**, **One-shot**, **Loop**, **Vocal**, **FX** and
  **Instrument**.
- **Duration**: **Any duration**, **Under 2 s**, **2–10 s** or **Over 10 s**.
  The bounds are inclusive.
- **Commercial use OK** keeps only results whose licence allows commercial
  use.
- Market chips: every market the build knows, with an availability dot. The
  tooltip shows the licence sentence, or the reason the market is unavailable.
  Click a chip to include or exclude that market. Until you change the chips,
  every available market except Openverse is included.
- **External Sources** opens that Preferences page. When an included market
  needs a key it does not have, the panel also shows a key field; **Save key**
  stores the key in the operating system's credential store and reads the
  market list again.
- **On this computer**: matching files from your sound folders, listed first.
- **Online**: a notice line for each market that could not answer, for example
  that it needs an API key, is rate limited (with the wait), is temporarily
  skipped or could not be reached, then the market results.
- Each market result shows its name, market, kind, duration, credit line and
  a licence badge, green when the licence is free to reuse. Right-click a
  result to copy its credit, source link or download link.
- **Load more** fetches the next page of every market that has more, using the
  cursor. Rows already shown are not added again.

Changing the kind, duration, commercial filter or markets clears the results;
press **Search** again.

Clicking a result, or its play button, previews it. The panel stops the
previous preview and plays the file through the audition stream, never through
the transport, for at most 30 seconds. A preview download is limited to
48 MiB. A downloaded preview is kept for the session in `sample-previews/` in
the app's cache folder: at most 32 files and 256 MiB, removing the least
recently used first, and emptied when the app first opens it. Hearing a result
again needs no network, and **Add** reuses the file when the
download URL is the one previewed. The add still resolves the item and checks
the declared size and checksum.

**Add** downloads the file, writes the sidecar and receipt, and
places an `@audio_clip` on the selected track. With no track selected, it adds
an `audio/clip` track named `Samples` (`Samples 2` and so on if that name is
taken). The clip starts on the beat at or before the playhead and keeps the
sample's real length. If the project changes while the file downloads (you
edit it or open another project), the clip is not placed; the file stays in the
media folder.
A result with no downloadable file shows **Preview only** instead. A licence
that needs a notice opens the notice first.

When a command fails, the panel receives `{code, message}`, where `code` is a
refusal code (`licence_notice`, `key_required`, `invalid_cursor`, …) or
`null`. The panel shows its own sentence for the codes it knows and the
message otherwise.

### Samples on this computer

The local section searches the same sound folders that the first-run scan
counts samples in: the folders you added, the managed sound-pack folder, the
standard instrument folders, and the usual sample-library folders in your
Documents, Music and home folders. It reads `wav`, `aif`, `aiff`, `flac`,
`ogg` and `mp3` files, with the scan's limits of 20,000 entries and 8 folder
levels per folder. Hidden folders and symbolic links are skipped. The file list
is kept for 30 seconds, so several searches in a row walk the disk once.

- Every word of the search must appear in the file's path below its sound
  folder; case, `_`, `-` and folder separators do not matter. A file whose name
  holds every word comes before one that matches only through its folders, and
  shorter names come first.
- **Kind** is read from the path the same way a market result's name is read,
  so `Loops/snare roll.wav` is a loop.
- **Duration** reads the length from each file's header. A file whose header
  states no length fails a duration bound, as a market result does. At most
  400 headers are read per search.
- **Commercial use OK** and the market chips do not apply: these are your files.
- At most 50 files are listed. When more match, the panel says so; add words to
  narrow the search.

Each row shows the file name, its format, kind, the path below its sound folder
and a **Local file** badge; hover it for the full path. A preview plays the file
through the audition stream, like a market preview. **Add** copies
the file into `<project>/media/local-samples/` and places the clip as described
above. The copy keeps the file's name with unsafe characters replaced; adding
the same file again reuses its copy, and a different file with the same name
gets a number. If the clip cannot be placed, the copy is removed.

Preview and add accept only an audio file inside one of the searched sound
folders. They refuse with `not_found` (the file is gone), `outside_folders`,
`not_audio`, `too_large` (more than 512 MiB) or `project_unsaved`.

## Agent tools

`sample_search` takes `query`, `kind`, `min_seconds`, `max_seconds`,
`channels`, `tags`, `providers` (an array; without it, the default search),
`provider` (an older single-market form), `offset`, `limit`, `commercial_ok`
and `cursor`. It returns the search result described above, with `provider`,
`provider_name` and `license_ok` on every item.

`sample_pull` takes `id`, an optional `provider` (taken from the id prefix when
left out), an optional `dest` and `accept_licence`. It writes into
`<project>/media/samples` by default and returns `path`, the portable
`media/samples/...` path, the licence, the credit line, the SHA-256, the
`quality` and, for a preview, a warning. A Freesound pull uses the stored
OAuth2 token when there is one and returns `quality: "original"`; without a
token, or when the original is refused, it returns `quality: "preview_mp3"`.

Both tools are available in the AI panel: a search only reads catalogues, and a
pull writes one file into a folder the project already owns. A refused call
returns `{"status": "refused", ...}`. A network or disk failure returns an
error, because nothing changed and a retry may work.

## Command line

```sh
ocp sample providers
ocp sample search "snare drum" --kind drum --limit 5 --provider openverse
ocp sample search "vinyl loop" --min-seconds 1 --max-seconds 4 --channels 2
ocp sample search "pad" --commercial-ok
ocp sample pull "commons:File:Snare drum unmuffled.ogg"
ocp sample pull "ia:prelinger-snare" --provider internet-archive
FREESOUND_API_KEY=... ocp sample search "riser" --provider freesound --json
```

`ocp sample search` asks one market: the one in `--provider`, or Wikimedia
Commons. It pages with `--offset`; the last line prints the offset of the next
page. Each result shows its id, kind, duration, channels, sample rate, licence
and credit line, and says when its licence needs `--accept-licence`.
`--tag` adds a tag and can be repeated.

`ocp sample pull` downloads into `--dest`, by default `media/samples` under the
current folder. It prints the path, the kind, quality, size and file name, and
the credit line, with a warning for a preview. `ocp sample providers` lists
every market with its availability and reason. `--json` prints the raw page,
receipt or market list for scripts.

The CLI reads keys only from `FREESOUND_API_KEY` and `JAMENDO_CLIENT_ID`. It
prints the same refusal sentences as the tools and exits with a non-zero
status.

## Tests

The market parsers are tested against recorded answers in
`crates/oc-sample/tests/fixtures/`, through a mocked transport; no test reaches
the network.

The store tests cover the refusals that matter: a size mismatch, a checksum
mismatch and a licence that needs a notice each leave the folder empty, and a
successful pull writes the audio, the sidecar and the receipt with a checksum
that matches the bytes. The registry tests cover availability with and without
keys, the page-only reasons and the id prefixes. The panel tests render the
panel, search, show the market chips, page with **Load more** and add a result
through a mocked `invoke`. The local search tests cover word matching and
ranking, the kind and duration filters, nested folders, the path check that
preview and add apply, and the copy that a refused placement removes.
