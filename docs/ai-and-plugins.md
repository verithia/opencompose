# AI assistant and instrument setup

## AI settings

1. Open the **AI** tab and press **Settings**.
2. Select a provider. In the endpoint field, enter its base URL or the full
   request URL. A base URL such as `https://host/v1` gets the protocol path (for
   example `/chat/completions`) appended.
3. Enter the model ID and the key, then press **Save settings / key**.
4. Press **Test Connection** before you start chatting.

Model names are editable. A built-in entry does not mean your account has access
to that model.

Each provider speaks one protocol, chosen under **Provider protocol**: OpenAI
Chat Completions, OpenAI Responses, Anthropic Messages, Gemini Interactions or
Gemini GenerateContent. The bundled catalog sets the protocol for its own
providers.

Keys are stored in the macOS Keychain, the Windows Credential Manager or the
Linux Secret Service. They never go into YAML, project files or browser storage.
On Linux, a Secret Service (for example GNOME Keyring or a compatible service)
must be running and unlocked. If the store is unavailable, the app reports the
failure and does not fall back to an insecure location. Settings that are not
secret live in `~/.opencompose/ai-providers.yaml`.

The panel reads the active provider's key when it loads and whenever you switch
provider, and remembers the result for the life of the process. The first time a
build reads its own keychain item, macOS asks you to allow it; answer
**Always Allow** so that later reads are silent. The question comes before you
send anything and never during a turn: a turn only reads the remembered result,
and that result can be a refusal. If you dismissed the prompt, the turn fails
with a configuration error that names the fix, and it does not appear to hang.
**Read key again** in Settings asks the OS once more.

After you rebuild the app, macOS usually does not trust the new build yet,
because it ties the keychain item to the binary that created it. Save the key
again in Settings, or press **Read key again** and choose **Always Allow**. The
dialog never displays a stored key. It reports whether the key is readable and,
when it is not, shows the store's own message.

A stored key belongs to one provider ID and one exact endpoint
(`<provider-id>:sha256(<endpoint>)`). Changing the endpoint does not forward the
existing credential to the new server. Saving deletes the old entry, and the
status line then reports no saved key for the new endpoint. If you still need
the old credential, select the original endpoint and save before you remove it.

- A blank key field keeps the stored credential.
- A key you type stays in memory for that provider until you save or remove it,
  so closing the dialog does not lose it. It is never written to another
  provider.
- **Remove saved key** deletes the key.
- Removing a provider you added deletes its key when the settings are saved.

An endpoint on a host from the bundled catalog always authenticates. A request to
one with no saved key is refused, with the provider named, before it is sent. A
loopback server or your own gateway may run without a key; its authentication is
its own business.

**Add provider** can start from a catalog preset. A preset fills in the name,
protocol and base URL for providers such as OpenRouter, Google AI Studio,
DashScope, SiliconFlow, Moonshot, Zhipu, Groq and xAI. Its model list starts
empty, so **Fetch models** reads your account's list, and you can add any model
ID by hand. A provider cannot be created until at least one model is enabled.

For a headless run, set `OPENCOMPOSE_AI_KEY`. It takes precedence over the store,
so no process opens the keychain. `/ai/complete` resolves credentials the same
way.

Remote endpoints require HTTPS. HTTP is allowed only on loopback (`localhost`,
`127.0.0.1` or `::1`). The app rejects URLs that contain userinfo, a query string
or a fragment, and does not follow HTTP redirects. A request has a 15 s connect
timeout, a 300 s idle timeout and a 600 s total limit, and you can cancel it. The
request text is limited to 2 MiB and the response to 16 MiB. A failed request is
never retried automatically, because a retry can be billed. Provider error bodies
are not shown, because they can contain credentials.

Turn on **Native tools** when the endpoint and model support native function
calling. With it off, the panel uses a complete JSON text-tool request. Turn on
**Streaming** for server-sent events, and turn it off for servers that do not
support them.

**Project instructions**, in the same dialog, is a short note for the open
project, such as "always print stems at -14 LUFS" or "this song is in D minor, do
not modulate". The note goes into the system prompt of every request for that
project and never into a request for another project. It is limited to 4,000
characters. [Saved conversations](ai-sessions.md) describes where the note is
stored and how it counts against the prompt budget.

Sending a message sends the selected project context and the recent conversation
to the configured provider, and tools can retrieve more of the project. Use a
provider you trust with that project, and with any audio or image file you
attach: the app reads an attachment from disk and sends it inline when it builds
the request.

### OAuth providers

This build ships no OAuth sign-in, because it bundles no client registration.
Use an API-key provider.

When the settings load, a saved provider entry whose protocol or sign-in this
build does not ship is skipped, with a warning in the log. The other providers
load unchanged, and the next save writes the file without that entry.

### Available tools and edit policy

The panel offers every catalog tool its policy enables, plus the panel-local
`edit_subtree`, `plan_update` and `ask_user`. [Drawing from an
agent](manual/agents.md) lists what the panel leaves out. Some of the tools a
turn uses most:

- `list_skills()` and `read_skill(id)` read the bundled reference notes.
- `plugin_search(...)` searches the bundled free-plugin catalog. It is read-only
  and never downloads or installs anything.
- `project_context(path)`, `tracks_list()` and `presets_list()` inspect the open
  project.
- `project_validate(path, dsl)` checks a replacement without changing anything.
- `edit_subtree(path, new_dsl)` proposes one complete replacement. Under **Ask**,
  inspect the diff and choose **Approve** or **Reject**. An approved replacement
  is one undoable transaction. A session with nobody to ask (a headless agent)
  applies the validated replacement itself, as the same single undoable
  transaction.

The model gets no shell, no filesystem access, no arbitrary IPC and no
unrestricted network access. The one install path is `plugin_pull`. It defaults
to `dry_run: true`, installs only with `dry_run: false`, and takes its URLs from
the verified catalog. From the panel it installs only native plugin archives
(VST3, VST2, CLAP, LV2) and managed separator runtimes that the signed index pins
by checksum and size; it refuses manual DMG, PKG and EXE installers.

Edits follow the conversation's permission mode. **Ask** (`require_approval`)
asks before every change, **Review** (`autoreview`) records each diff and then
applies it, and **Auto** (`write`) applies directly. A destructive call, such as
removing a track, asks in all three modes unless you chose **Always allow** for
it in this conversation. See [Agent feedback loop](agent-loop.md).

If the project changes while a proposal is waiting, the proposal is refused;
ask for a new one so newer work is not overwritten. A turn waits on one approval
at a time. A turn has at most 64 model rounds and 32 tool calls per round, and
reaching the round cap commits the edits that landed.

To check the setup:

1. Ask "List tracks, then read the bass track and suggest a change."
2. Check the tool results and the diff. Reject the first proposal, then approve
   a new one, then press Undo.
3. Cancel a request and confirm that no edit appears afterwards.
4. Restart the app and press **Test Connection** to confirm the credential
   persists.

## Attachment input

The paperclip in the composer attaches one file at a time: an audio file (`wav`,
`mp3`, `aif`, `aiff`, `aac`, `ogg`, `flac`, `m4a`, `opus`) or an image (`png`,
`jpg`, `jpeg`, `webp`). The attachment descriptor holds the path, size and
content hash. When a request is built, the app reads the file and sends it
inline. The limits are 12 MiB per file, four files per message and 20 MB of
base64 per request. An audio file between 12 MiB and 512 MiB is attached as a
local reference: the request carries its path and size, no audio, and the model
can pass the path to the analysis tools.

The model's declared inputs decide what is accepted, and the refusal happens
before the request is sent:

- Audio follows the model's audio input, which comes from the catalog entry's
  `audio_in` flag. The protocol also limits the formats: the OpenAI protocols
  take `wav` and `mp3`, the Gemini protocols take every listed format, and
  Anthropic Messages takes no audio.
- Images follow `input.image` in the merged model limits. The bundled catalog
  declares no image input, so enable it per model under **Edit limits**. A model
  with image input off refuses the image, and the message names the model and the
  inputs it does declare (`text`, or `text, audio`).

The panel checks these rules before sending, and the request builder enforces
them again. The headless `/ai/complete` endpoint therefore refuses an image in
the same way and names the same model. A refused attachment stays in the
composer, so after you switch to a model that accepts images, you can send it
without picking the file again.

A saved conversation keeps each attachment descriptor. When you reopen it, the
chip appears again, and a later turn sends the file again, hashed anew at send
time. If the file no longer exists, the chip reads **file is missing**, that
attachment is skipped and the rest of the message is sent, with one text line in
the request naming the missing path. A saved attachment that the current model
cannot take is handled the same way: the chip stays, and the request carries one
text line naming the file in place of its bytes. Only an attachment on the
current turn can make a request fail, so a conversation that switches to a
text-only model keeps working. An exported conversation names its attachments
but never includes their contents.

## Model prices and cost

The panel shows an estimated cost for the tokens a session uses. Prices are per
million tokens, in the currency the catalogue publishes, and come from the
models.dev dataset. The bundled snapshot is
`crates/oc-gui/frontend/src-tauri/ai-prices.json`, with one record per model the
bundled catalogue declares.

A model without a published price has no record, and the panel shows no cost for
it. It does not show `$0.00`, because an unknown price is not a price of zero. A
local server such as Ollama is the usual case, since it does not bill per token.

The cost comes from the usage the provider reports:

- fresh input tokens at the input price;
- cached input tokens at the cached-input price, or at the input price when the
  catalogue publishes no cached price;
- output tokens, including reasoning tokens, at the output price.

A model the catalogue prices at zero is free and costs zero.

The session total adds up the turns that had a price. If the session also used
an unpriced model, the total shows the priced part and is marked **partial**.

Under **Edit limits** you can set your own price for a model. It overrides the
catalogue for that provider and model, and the dialog still shows the catalogue
price for comparison.

Refresh the snapshot when the dataset changes:

```sh
python3 scripts/refresh-ai-prices.py            # fetch, rewrite the snapshot
python3 scripts/refresh-ai-prices.py --check    # fail when the file is stale
```

The script keeps only the provider and model pairs the bundled catalogue
declares, so a model the app cannot request never gets a price. Commit the
rewritten `ai-prices.json` with the prices the script reports, and do not edit
prices by hand.

## Load an instrument plugin

1. Install the instrument with its vendor installer into the standard plugin
   folder for its format. [Plugins](manual/plugins.md) lists the hosted formats
   and the folders that are searched.
2. Open **Presets** (`P`) and press **+**. Press **Rescan plugins** if the
   instrument is new, then open the **Plugins** category or search by name.
   Select the instrument to create a project preset.
3. Select that preset. The plugin section loads its parameters and shows
   **Open editor** when the host can open the plugin's editor.
   **Reload parameters** retries the load, and failures appear in the panel.
   Native VST3 editor hosting is qualified on macOS only; Windows and Linux
   editor support is not claimed.
4. Choose a sound inside the plugin. Under **Advanced instrument settings →
   Saved plugin settings**, press **Embed** to store the state in the project, or
   **Save** to write a state sidecar file. Then save the `.ocp` file. Sample
   content from a library stays external and must be installed and licensed on
   the machine.
5. Assign the preset to a track with the track's preset selector, or press the
   preset's **Add as new track** button (`+↗`). Add notes and listen in context.

## Kontakt / Tokyo Scoring Strings Free

1. In Native Access, install and activate Kontakt Player and the string library,
   including its sample content.
2. Scan, select **Kontakt 8 (VST3)**, press **Open editor**, and load the violin
   or cello patch you want in Kontakt.
3. Store the state with **Embed** or **Save** as described above.

A bare Kontakt instance is not a loaded string instrument.

Start with a separate named preset for each instrument or patch. Route a track to
it and keep the track's MIDI channel consistent with the loaded instrument. Set
dynamics (commonly CC1) and expression (commonly CC11) as the library expects;
note velocity alone is not enough. Check articulations and keyswitches against
the patch's documentation, because overlapping notes alone do not guarantee a
realistic legato. Captured state restores the selection without reopening the
editor for every render. It does not redistribute samples, bypass activation or
make every patch equally realistic.

The **Strings** category in the chooser lists General MIDI SoundFont programs.
For a Kontakt library, use **Plugins → Kontakt**.

## Verification scope

Automated fixtures cover the transport's authentication, redaction and redirect
policy, streamed tool arguments, bounded tool execution and confirmation-only
edits. Browser fixtures exercise the settings and proposal acceptance without a
paid API. The native macOS credential store has a round-trip smoke test with a
temporary account. Live provider and model compatibility needs your key. The
Windows and Linux credential and plugin paths need testing on those platforms
before a release is certified.

Implementation references: [keyring](https://docs.rs/keyring/3.6.3/keyring/),
[reqwest redirect policy](https://docs.rs/reqwest/0.12.28/reqwest/redirect/struct.Policy.html).

## Bundled reference skills

The app bundles short skills, each tagged with a category such as
arrangement, mixing, production or workflow. Before it proposes an edit, the
model can call `list_skills` and `read_skill` to read the relevant notes. You do
not install anything or run a retrieval service. You can add your own skills as
`~/.opencompose/ai/skills/<id>.md`; a user skill cannot reuse a bundled id. A
skill grants no training rights and no tools, and edits still follow the
conversation's permission mode. The bundled notes are in
`crates/oc-gui/frontend/src-tauri/src/ai/skills/builtin/`.

To try it, ask: "List your bundled skills, read the unmask-kick-and-bass skill,
and inspect my bass track before suggesting any change." The tool log should
show the skill list and a single skill read. Under **Ask**, the project does not
change until you approve an edit.
