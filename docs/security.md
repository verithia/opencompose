# Credential and secret handling

OpenCompose stores AI provider credentials in the operating system's credential
store, never in a project file and never in plaintext on disk. This page states
what is stored, what that protects against, and where the boundary ends.

## What is stored, and where

| Secret | Store | Account / path |
|---|---|---|
| API key (per provider) | macOS Keychain, Windows Credential Manager, Linux Secret Service | service `org.opencompose.ai`, account `<provider id>:<sha256(endpoint)>` |
| OAuth token set (access + refresh) | same, separate service | service `org.opencompose.ai.oauth`, account `<provider id>:<auth provider>` |
| Agent loopback bearer token | `~/.opencompose/agent.json` (0600) | regenerated on every launch |

The provider configuration in `~/.opencompose/ai-providers.yaml` (0600, inside a
0700 directory) contains endpoints, model IDs, capability flags and an optional
OAuth host allowlist. It contains no key or token material.

Credentials are written as bytes, not as a password string, because the Windows
Credential Manager blob is limited to 2560 bytes and a UTF-16 password halves
that. Payloads that do not fit are compressed, and if still too large they are
split across numbered entries behind an explicit manifest. Replacing a sharded
token writes the new generation before switching the manifest, so an interrupted
refresh keeps the previous token readable.

Secrets held in memory are overwritten when dropped, and `Debug` output is
redacted. HTTP clients still copy header values into their own buffers; that is
outside our control.

## Threat model

The store protects against:

- another local user reading your credentials;
- offline disk theft when the login password / disk encryption is unknown;
- casual inspection of config files, backups and sync folders;
- accidentally committing provider configuration.

It does not protect against:

- **code running inside the DAW process.** VST3/CLAP plugins are loaded with
  `dlopen` and run with the application's identity, so they are trusted. A
  malicious or compromised plugin, dependency or injected library can read
  credentials through the OS store or straight from process memory, and can send
  them anywhere. The host allowlist only constrains requests OpenCompose itself
  makes; a plugin can open its own socket.
- same-user malware, infostealers or a debugger attached to the process;
- another application on Windows or Linux: Credential Manager/DPAPI and the
  unlocked Secret Service session are readable by any process running as you.
  GNOME's Secret Service has no per-application ACL (KWallet prompts).
- secrets while they are in use in memory.

## Platform behaviour

- **macOS.** The item is a legacy Keychain generic password. Development and
  unsigned builds are ad-hoc signed, so the Keychain records the binary's
  cdhash as the trusted application. Every rebuild looks like a different app
  and may prompt again; "Always Allow" is per item and does not survive a new
  cdhash. A release update is also a new binary, so a prompt is possible after
  updating.
- **Windows.** Credentials are per-user, not per-application: any process in
  your session can read them without a prompt. File modes from the Unix build
  do not apply; the protection for `~/.opencompose` is the profile directory
  ACL.
- **Linux.** A desktop Secret Service (GNOME Keyring, KWallet) is required.
  There is no file fallback: on a headless machine, over SSH or in a container
  without a session bus, credential reads and writes fail instead of touching
  disk.

## Fail-closed behaviour

OpenCompose never writes a credential to a plaintext file, and never silently
falls back to an in-memory or file-backed store. If the OS credential store is
locked or unavailable, the operation reports an error and the secret is not
saved. The configuration file can still be edited and saved as long as no
credential needs to be deleted. Removing a provider deletes
both its API-key and OAuth entries, and the save is refused if that deletion
fails, so the UI cannot report success while the secret is retained.

OAuth tokens are bound to an allowlist of exact endpoint hosts. A provider entry
may add explicit per-host exceptions (`oauth_allowed_hosts`), but there is no
blanket switch that disables the check, and hosts are compared as whole names,
never by prefix.

## Not covered by this design

- Signed/notarized macOS releases and the data-protection Keychain with an
  access group (removes the rebuild prompt and gives a stable app identity).
- A credential broker that keeps tokens out of plugin-reachable memory.
- Hardening against malicious plugins, supply-chain compromise or same-user
  malware. Those need code signing, hardened runtime where plugin loading
  allows it, dependency review and, for the plugin case, a broker.
