# AI assistant and instrument setup

## AI settings

Open **AI → Settings**. Select a provider, enter the full OpenAI-compatible
chat-completions endpoint, model ID and key, then **Save settings / key**.
Use **Test Connection** before chatting. Model names are editable; a built-in
entry does not guarantee your account has access to that model.

Keys live in macOS Keychain, Windows Credential Manager or Linux Secret Service,
not YAML, project files or browser storage. Linux needs a running/unlocked Secret
Service (e.g. GNOME Keyring or compatible service). Failure is visible; no insecure
fallback. Non-secret settings live in `~/.opencompose/ai-providers.yaml`.

The panel reads the active provider's key when it loads and whenever you switch
provider, and keeps the answer for the life of the process. macOS asks you to
allow the read the first time a build touches its own keychain item; answer
**Always Allow** so later reads are silent. The question is asked before you send
anything, never during a turn: a turn only reads the remembered verdict, and that
verdict can be a refusal. A dismissed prompt therefore fails the turn with a
configuration error that names the fix instead of leaving it looking hung, and
**Read key again** in Settings asks the OS once more.

A build the store does not trust yet is the normal result of rebuilding the app,
because macOS ties the keychain item to the binary that created it. Save the key
again in Settings, or press **Read key again** and choose **Always Allow**. The
dialog never displays a stored key; it reports whether the key is readable, and
shows the store's own words when it is not.

Stored keys are scoped to provider ID + exact endpoint
(`<provider-id>:sha256(<endpoint>)`). Changing the endpoint does not forward an
existing credential to a new server: saving deletes the old entry, and the status
line then reports no saved key for the new endpoint. Select the original endpoint
and save before removing its old credential. A blank key field preserves the
stored credential; a key you type stays in memory for that provider until you
save or remove it, so closing the dialog does not lose it, and it is never
written to another provider. **Remove saved key** deletes it explicitly, and
removing a provider you added deletes its key with it.

An endpoint on a host from the bundled catalog always authenticates, so a request
to one with no saved key is refused by name before it is sent. A loopback server
or your own gateway may be keyless: its authentication is its own.

**Add provider** can start from a catalog preset. A preset fills the name,
protocol and base URL for providers such as OpenRouter, Google AI Studio,
DashScope, SiliconFlow, Moonshot, Zhipu, Groq and xAI; its model list is left
empty so **Fetch models** reads the account's own list, and any model id can be
added by hand. A preset with no model cannot be created until at least one model
is enabled.

For a headless run, set `OPENCOMPOSE_AI_KEY` instead. It wins over the store, so
no process opens the keychain; `/ai/complete` resolves credentials the same way.

Remote endpoints require HTTPS. HTTP is allowed only on loopback, including
localhost/127.0.0.1/::1. URLs containing userinfo, query strings or fragments and
HTTP redirects are rejected. Requests have a 15s connection/120s total timeout,
2MiB request/response limits and cancellation. No automatic billable retries.
Provider error bodies are not echoed because they can contain credentials.

Enable **Native tools** if the endpoint/model supports OpenAI function tools.
Otherwise a complete JSON text-tool request is supported. Enable **Streaming**
for OpenAI-compatible SSE; turn it off for servers without SSE support.
This is not an Anthropic Messages or Gemini-native protocol adapter.

**Project instructions** in the same dialog is a short note for the open project:
"always print stems at -14 LUFS", "this song is in D minor, do not modulate". The
note rides in the system prompt of every request for that project and never in
another project's request; it is capped at 4,000 characters. See
`docs/ai-sessions.md` for where it is stored and how it is counted against the
prompt budget.

Sending a message sends the selected project context and recent conversation to
the configured provider. Tools may retrieve further project subtrees. Use only
a provider you trust with that project, and with any audio or image file you
attach: an attachment is read from disk and sent inline while the request is
built.

### OAuth providers

No OAuth sign-in ships in this build because no client registration is
bundled. Use an API-key provider instead.

A saved provider entry whose protocol or sign-in this build does not ship is
skipped when the settings load, with a warning in the log. The other providers
load unchanged, and the next save writes the file without that entry.

### Available tools and edit policy

- `list_skills()`, `read_skill(id)` consult the bundled reference catalog.
- `plugin_search(...)` searches the bundled free-plugin catalog read-only; it
  never downloads, installs, opens URLs, or touches the network.
- `read_context(path)`, `list_tracks()`, `list_presets()` inspect the open project.
- `validate(path, dsl)` checks a replacement without changing anything.
- `edit_subtree(path, new_dsl)` proposes one complete replacement. Inspect the
  diff and choose **Accept** or **Reject**. Accept is one undoable transaction.
  A session with no user to ask (a headless agent) applies the validated
  replacement itself, as the same one undoable transaction.

No model-controlled shell, filesystem, arbitrary IPC, plugin installation or
unrestricted network tools. Plugin installation exists only on the external
agent API's `plugin_pull`, which requires `dry_run: false` and uses catalog URLs. Edits follow the conversation's permission mode: `require_approval` asks for
every change, `autoreview` records each diff before applying it, and `write`
applies directly; a destructive call asks under all three. See [Agent feedback
loop](agent-loop.md). A changed project invalidates an old proposal; regenerate
rather than overwriting newer work. At most sixty-four
model rounds and thirty-two calls per round; the round cap commits what landed. One
pending edit per turn. Audio preview/tool contracts remain outside this release.

Smoke test: ask “List tracks, then read the bass track and suggest a change.”
Check the tool results and diff, reject once, then accept a new proposal and Undo.
Cancel a request and confirm no edit appears afterward. Restart the DAW and use
Test Connection to check credential persistence.

## Attachment input

The composer's paperclip attaches one file at a time: an audio file (`wav`,
`mp3`, `aif`, `aiff`, `aac`, `ogg`, `flac`, `m4a`, `opus`) or an image (`png`,
`jpg`, `jpeg`, `webp`). The descriptor carries the path, size and content hash;
a request reads the file and sends it inline, up to 12 MiB per file, four files
per message and 20 MB of base64 per request.

The model decides what it accepts, and the refusal happens **before** the
request:

- Audio is gated on the catalog entry's `audio_in` flag.
- Image input is gated on the merged model limits' `input.image`. The bundled
  catalog declares no image input, so enable it per model under **Edit limits**;
  a model with `image` off refuses the image and names itself plus the
  modalities it does declare (`text`, or `text, audio`).

The panel checks the gate before sending, and the request builder enforces the
same rule, so the headless `/ai/complete` endpoint refuses an image the same way
and names the same model. A refusal leaves the attachment in the composer, so
switching to a model that accepts images sends it without re-picking the file.

A saved conversation keeps each attachment descriptor. Reopening it paints the
chip again and a later turn replays the file, re-hashed at send. If the file no
longer exists, the chip reads **file is missing**, that attachment is skipped,
and the rest of the message is sent; one text line in the request names the
missing path. A descriptor the current model cannot take degrades the same way:
the saved chip stays, and the request carries one text line naming the file
instead of its bytes. Only the turn's own attachment can refuse a request, so a
conversation that switches to a text-only model keeps running. Export names
attachments but never inlines them.

## Model prices and cost

The panel shows an estimated cost of what a session spends on tokens. Prices are
per million tokens, in the currency the catalogue publishes them in, and come
from the models.dev dataset. The bundled snapshot is
`crates/oc-gui/frontend/src-tauri/ai-prices.json`; it carries one record per
model the bundled catalogue declares.

A model without a published price has no record. The panel then shows no cost for
it rather than `$0.00`, because zero is a claim and an unknown price is not a
claim of zero. A local server such as Ollama is the usual case: it is not billed
per token.

Cost is computed from the usage the provider reports: fresh input tokens at the
input price, cached input tokens at the cached-input price (the input price when
the catalogue publishes none), and output tokens, including reasoning tokens, at
the output price. A model priced at zero in the catalogue is free and costs
zero.

The session total adds the turns that had a price. When a session also used an
unpriced model, the total states the priced part and marks itself **partial**.

**Edit limits** on a model lets you set your own price. Your price wins over the
catalogue for that provider and model, and the dialog still shows the catalogue
price as the baseline.

Refresh the snapshot after the dataset changes:

```sh
python3 scripts/refresh-ai-prices.py            # fetch, rewrite the snapshot
python3 scripts/refresh-ai-prices.py --check    # fail when the file is stale
```

The script keeps only the provider and model pairs the bundled catalogue
declares, so a model it cannot request is never priced. Commit the rewritten
`ai-prices.json` with the prices it reports; do not hand-edit prices.

## Load an instrument plugin

1. Install a compatible **CLAP or VST3** plugin using its vendor installer in
   the normal OS plugin directory. AU-only instruments are not supported here.
2. Open **Presets** (`P`), press **+**, then refresh/rescan and choose **Plugins**
   or search by name. Select the instrument to create a project preset.
3. Select that preset. The CLAP/VST3 section loads its parameters and shows
   **Open GUI** when the host supports its editor. **Reload** retries loading;
   failures appear in the panel. VST3 native editor hosting is currently macOS-
   qualified, not a claim of Windows/Linux editor support.
4. Choose a sound inside the plugin. Under **Rendering Profile → Plugin state**,
   use **Embed** (self-contained state) or **Save** (state sidecar). Save the OCP.
   Library sample content remains external and must be installed/licensed.
5. Assign the preset to a track using its preset selector, or use the preset's
   **+T** action to add a track. Add notes and audition in context.

## Kontakt / Tokyo Scoring Strings Free

Install/activate Kontakt Player and the string library in Native Access, including
its sample content. Scan and select **Kontakt 8 (VST3)**, then **Open GUI** and load
the desired violin/cello patch in Kontakt. Capture **Embed** or **Save** state as
above. A bare Kontakt instance is not a loaded string instrument.

Use a separate named preset for each instrument/patch initially. Route a track to
it and keep its MIDI channel consistent with the loaded instrument. Set dynamics
(commonly CC1) and expression (commonly CC11) according to the library, not only
note velocity. Verify articulations/keyswitches against the patch's documentation;
note overlap alone does not guarantee realistic legato. Captured state restores
the selection without reopening the editor for every render, but it does not
redistribute samples, bypass activation, or make every patch equally realistic.

The chooser's **Strings** category lists General MIDI soundfont programs, not
Kontakt libraries. Use **Plugins → Kontakt** for the sample library workflow.

## Verification scope

Automated fixtures cover transport authentication/redaction/redirect policy,
streamed tool arguments, bounded tool execution and confirmation-only edits.
Browser fixtures exercise settings and proposal acceptance without a paid API.
The native macOS credential store has a temporary-account round-trip smoke.
Live provider/model compatibility needs your key; Windows/Linux credential and
plugin paths require testing on those platforms before a release certification.

Implementation references: [keyring](https://docs.rs/keyring/3.6.3/keyring/),
[reqwest redirect policy](https://docs.rs/reqwest/0.12.28/reqwest/redirect/struct.Policy.html).

## Bundled reference skills

The app includes 31 compact skills in four groups: composition, production/sound
design, DAW workflows, and style applications. The AI can call `list_skills` and
`read_skill` to consult relevant guidance before proposing an edit. No separate
skill installation or RAG service is needed. Reference guidance does not grant
training rights or new tools; edits still require confirmation. Details:
the skill catalog at `crates/oc-gui/frontend/src/ai/skills/`.

Try: “List your bundled skills, read the low-end roles skill, and inspect my bass
track before suggesting any change.” The tool log should show discovery and a
single skill read. No project change occurs until an edit proposal is accepted.
