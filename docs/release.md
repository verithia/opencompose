# Release checklist

This is the release procedure for the desktop app. It is a checklist: every
step is either a command you can run or an item the maintainer must supply.

Two builds are possible from the same tree:

| Build | Needs a certificate | Produces | Who can run it |
|---|---|---|---|
| Unsigned | no | `.app`, `.dmg` | anyone, offline |
| Signed + notarised | yes | `.app`, `.dmg` with a stapled notarisation ticket | maintainer with an Apple Developer account |

The unsigned build needs no certificate. Sections 4 and 5 are maintainer-only
and require an Apple Developer account.

Both builds use the engine's own SoundFont player, so neither build needs a
SoundFont library on the target machine. The desktop bundle also carries one
General MIDI bank, `FluidR3Mono_GM.sf3`, so a new install can play the 128 GM
programs without a download. The bank is MIT-licensed and ships with its
license text under the app's `soundfonts` resource directory.

The bundle also carries `THIRD_PARTY_LICENSES.txt` at the root of its resource
directory: the notices of every shipped crate, npm package and bundled asset.
`scripts/third-party-licenses.py` generates it and CI checks that it matches
`Cargo.lock` and the frontend `package-lock.json`. Regenerate it before tagging
a release if a dependency changed.

---

## 1. Version

One version, **0.1.0**, lives in these files and nowhere else:

- `crates/oc-gui/frontend/package.json` — source of truth
- `crates/oc-gui/frontend/package-lock.json` — root `version` and `packages[""].version`
- `crates/oc-gui/frontend/src-tauri/tauri.conf.json` — `version`
- `Cargo.toml` — `[workspace.package].version` (inherited by the Rust crates)
- `Cargo.lock` — every `oc-*` workspace entry

Checklist:

- [ ] Bump `package.json` with
      `npm version <x.y.z> --no-git-tag-version`; it updates `package.json`
      and `package-lock.json`. It does **not** touch the Rust side.
- [ ] Set the same version in `tauri.conf.json`,
      and `Cargo.toml [workspace.package]`.
- [ ] Refresh `Cargo.lock` with any cargo command, e.g.
      `cargo check -p oc-gui`.
- [ ] Run `npm run version:check`. It compares every manifest, prints the ones
      that agree, lists every one that disagrees and exits non-zero.
      `npm run build` and `npm run tauri build` run it automatically, so a
      stale version cannot produce a bundle.
- [ ] `npm run smoke:macos` re-runs the same gate against the built
      `Info.plist` (`CFBundleShortVersionString` and `CFBundleVersion`).

Recorded: the gate reports 24 manifests at `0.1.0` without a bundle, 26 with
the built bundle. Temporarily setting only `package.json` to `0.1.1` made it
exit 1 and name all 24 stale files, so drift cannot slip through.

## 2. Unsigned bundle (repeatable, offline)

Run from the repository root:

```bash
cd crates/oc-gui/frontend
npm ci
npm run tauri build                              # shipping set (§2)
```

`npm run tauri build` is the Tauri CLI from `devDependencies`. It runs
`build.beforeBuildCommand` (`npm run build`: version gate, `tsc -b`, Vite) and
then compiles and bundles. `scripts/build-gui.sh` wraps the same command and
prints the artefact paths and sizes:

```bash
./scripts/build-gui.sh              # release .app + .dmg
./scripts/build-gui.sh --bundles app   # .app only, faster
```

`bundle.targets` is `all`, which on macOS means `.app` + `.dmg`. Nothing is
signed: `bundle.macOS` has no `signingIdentity`, and when neither that setting
nor `APPLE_SIGNING_IDENTITY` is present Tauri skips signing and notarisation
instead of failing. `hardenedRuntime` and `entitlements.plist` are recorded in
the config but only take effect when a signing identity exists.

Output paths (Tauri follows `CARGO_TARGET_DIR` when it is set):

```
<cargo target>/release/bundle/macos/OpenCompose.app
<cargo target>/release/bundle/dmg/OpenCompose_0.1.0_aarch64.dmg
```

Without `CARGO_TARGET_DIR` the target dir is the workspace `target/` (Tauri
reads `cargo metadata`), not `src-tauri/target`.

### Feature set: the engine's own SoundFont player

SoundFont tracks are rendered by `crates/oc-engine/src/sf2/`, our own bank
reader and sample player. It needs no library, so the shipping configuration is
the plain one: nothing probes pkg-config, the binary has no SoundFont library
load command, and a SoundFont project renders on a machine that never had one
installed. This is the default for every package, so a bare
`npm run tauri build` is a shippable bundle.

The player is the only SoundFont engine: there is no cargo feature,
environment variable or CLI flag that selects a different backend, and a
project file never records one.

### Bundled GM bank and size budget

The Tauri resources carry `FluidR3Mono_GM.sf3` (23,712,790 bytes) and its MIT
license. The app resolves it through `BaseDirectory::Resource`, so deb and
AppImage layouts work even when the resource directory is not beside the
executable. Discovery also looks for it beside the executable and in the
repository resource directory for development builds.

The release profile strips symbols. The universal binary otherwise carries
about 11 MB of symbol table that no runtime path reads, and the packaging
budget is 100 MB for both the installed app and the distributable artifact.
Measure both after every dependency or resource change; the DMG is compressed
and the `.app` is not.

Universal build reference:

| Artefact | Bytes | Display |
|---|---:|---|
| `OpenCompose.app` (installed) | 94,351,106 | 90.0 MiB |
| `OpenCompose_0.1.0_universal.dmg` | 49,852,725 | 47.5 MiB |
| `Contents/MacOS/oc-gui` | 69,682,736 | 66.5 MiB |
| `Contents/Resources/soundfonts/FluidR3Mono_GM.sf3` | 23,712,790 | 22.6 MiB |

The app is 94.35 MB by the decimal measure a download page usually prints, so
it stays under the 100 MB budget with the GM bank included.

The macOS bundle declares `LSMinimumSystemVersion` 11.0, matching the 11.0
minimum the universal binary records. Older systems cannot launch it.

Build artefacts stay out of git: `src-tauri/.gitignore` ignores `/target/`,
the root `.gitignore` ignores `crates/oc-gui/frontend/dist/` and the generated
`gen/` directory. When `CARGO_TARGET_DIR` points outside the repo there is
nothing to ignore.

## 3. Three-platform packaging CI

`.github/workflows/package.yml` builds the desktop bundles on macOS, Linux and
Windows without creating a release. Run it from **Actions → Package → Run
workflow**, or push a `v*` tag. Each job uploads one artifact:

| Runner | Target | Artifact |
|---|---|---|
| `macos-15` | universal (`aarch64` + `x86_64`) | `OpenCompose-macOS-universal` with the `.dmg` |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `OpenCompose-Linux-x86_64` with the `.deb` and `.AppImage` |
| `windows-2022` | `x86_64-pc-windows-msvc` | `OpenCompose-Windows-x86_64` with the `.msi` and `-setup.exe` |

Every artifact also carries `SHA256SUMS.txt`. The macOS job sets `CXX=clang++`
so the x86_64 half of the universal app links the C++ exception barrier. The
workflow uses the crate defaults, so MP3 export is compiled in; it installs no
SoundFont library. The bundle includes the MIT-licensed
`FluidR3Mono_GM.sf3`, and a size gate fails the job when an app or installer
exceeds 100 MiB. It does not create a GitHub release, sign the macOS bundle,
or publish the plugin index. Release publishing and index deployment stay
separate manual actions.

## 4. Smoke test the bundle

```bash
cd crates/oc-gui/frontend
npm run smoke:macos                          # default bundle path
npm run smoke:macos -- /path/to/OpenCompose.app
```

The script launches the bundle's `CFBundleExecutable` directly (no `open -a`,
so no Gatekeeper prompt), waits 10 s for startup, holds it alive for another
8 s, quits it with `SIGTERM`, escalates to `SIGKILL` if needed, and fails if the
process died or if a new `OpenCompose*` crash report appeared in
`~/Library/Logs/DiagnosticReports`. It also runs the `Info.plist` version gate
and prints the code-signing status.

Recorded evidence (unsigned bundle above):

```
smoke: alive after 10s (pid 66893); holding for 8s
smoke: quitting
smoke: log (last 20 lines):
  | [opencompose-agent] http://127.0.0.1:38642
smoke: PASS - bundle launched, stayed alive 18s, quit, no crash report
```

`pgrep -fl "OpenCompose.app/Contents/MacOS/oc-gui"` is empty afterwards, and
`~/Library/Logs/DiagnosticReports` has no `OpenCompose*` report. The log line is
the agent HTTP listener the app starts on launch; the release log level is
`Warn`, so a quiet log is normal.

For a manual check, the same sequence is:

```bash
./OpenCompose.app/Contents/MacOS/oc-gui &   # window appears
sleep 10 && kill -0 $! && kill -TERM $!          # still alive, then quit
ls -lt ~/Library/Logs/DiagnosticReports | head   # no new OpenCompose report
```

## 5. Signed and notarised release (maintainer only)

This path needs a Developer ID certificate and notary credentials in the
keychain. Without them, `codesign` finds no identity and `notarytool` cannot
authenticate. Everything below is the maintainer's job on a
machine signed into the Apple Developer account.

### Prerequisites

- [ ] Apple Developer Program membership; **Developer ID Application**
      certificate (not "Apple Distribution", not "Mac App Development")
      installed in the login keychain. Check:
      `security find-identity -v -p codesigning`
      → `Developer ID Application: OpenCompose <TEAMID>`.
- [ ] Team ID from the membership page (10 characters).
- [ ] An app-specific password for the Apple ID, or an App Store Connect API
      key (`.p8` + key ID + issuer ID).
- [ ] Notary credentials stored once in the keychain:
      ```bash
      xcrun notarytool store-credentials opencompose-notary \
        --apple-id "<apple-id>" --team-id "<TEAMID>" --password "<app-password>"
      ```
      For an App Store Connect API key instead:
      `xcrun notarytool store-credentials opencompose-notary --key "<AuthKey>.p8" --key-id "<KEYID>" --issuer "<ISSUER>"`.
      The profile name (`opencompose-notary`) is what later `notarytool`
      calls use.
- [ ] Tauri runs on the same machine and has the Rust target installed.
- [ ] The entitlement file has been reviewed (see below).

### Entitlements and hardened runtime

`crates/oc-gui/frontend/src-tauri/entitlements.plist` is passed to `codesign`
through `bundle.macOS.entitlements`, and `bundle.macOS.hardenedRuntime` is
`true`. Current contents:

- `com.apple.security.cs.disable-library-validation` — required. The app
  `dlopen`s CLAP/VST3 bundles signed by other teams; hardened runtime library
  validation refuses those without this entitlement.
- `com.apple.security.device.audio-input` — enabled. Takes and monitoring open
  a capture stream, and `src-tauri/Info.plist` merges the matching
  `NSMicrophoneUsageDescription`. Without both, macOS denies input or kills the
  process on first capture.
- No App Sandbox entitlements. The app is not sandboxed; it must read and write
  arbitrary project/plugin paths.

### Build and sign

**Option A — Tauri does the whole flow** (recommended; inferred from the CLI and
its generated DMG script):

```bash
cd crates/oc-gui/frontend
export APPLE_SIGNING_IDENTITY="Developer ID Application: OpenCompose (TEAMID)"
export APPLE_ID="<apple-id>"
export APPLE_PASSWORD="<app-password>"      # app-specific password
export APPLE_TEAM_ID="<TEAMID>"
npm run tauri build                              # shipping set (§2)
```

Tauri signs the app with the hardened runtime and the entitlements, notarises
the app bundle, staples the ticket, then signs and notarises the DMG with the
same identity. `APPLE_SIGNING_IDENTITY` alone signs without notarising. To
authenticate with an App Store Connect API key instead of the app-specific
password, set `APPLE_API_KEY`, `APPLE_API_ISSUER` and `APPLE_API_KEY_PATH`.

The DMG helper Tauri leaves at `<target>/release/bundle/dmg/bundle_dmg.sh`
records exactly what that last step runs: `codesign -s <identity> <dmg>`,
`xcrun notarytool submit <dmg> --keychain-profile <profile> --wait`, then
`xcrun stapler staple <dmg>`.

**Option B — sign and notarise by hand with a keychain profile.** Use this if
Option A stops before notarising, or if policy requires the stored profile:

```bash
cd crates/oc-gui/frontend
APP="$CARGO_TARGET_DIR/release/bundle/macos/OpenCompose.app"
DMG="$CARGO_TARGET_DIR/release/bundle/dmg/OpenCompose_0.1.0_aarch64.dmg"
IDENTITY="Developer ID Application: OpenCompose (TEAMID)"
ENTITLEMENTS="src-tauri/entitlements.plist"

# 1. Sign the app: hardened runtime, timestamp, entitlements.
codesign --force --deep --options runtime --timestamp \
  --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"

# 2. Notarise the app (notarytool needs an archive), then staple it.
ditto -c -k --keepParent "$APP" /tmp/OpenCompose.zip
xcrun notarytool submit /tmp/OpenCompose.zip \
  --keychain-profile opencompose-notary --wait
xcrun stapler staple "$APP"

# 3. Build the distribution DMG from the stapled app, then sign the DMG.
hdiutil create -volname "OpenCompose" -srcfolder "$APP" -ov -format UDZO "$DMG"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

# 4. Notarise the DMG and staple it.
xcrun notarytool submit "$DMG" --keychain-profile opencompose-notary --wait
xcrun stapler staple "$DMG"
```

Notes:

- `--deep` is a one-shot convenience for this bundle (it has no nested
  `Frameworks/`). Tauri's own signer walks nested binaries itself; Apple
  recommends the same. Never re-sign the CLAP/VST3 bundles the app loads at
  runtime — their vendors signed them.
- `--options runtime` is the hardened runtime flag; without it notarisation is
  rejected. `--timestamp` is also mandatory for notarisation.
- Use the `.app` that Tauri built as input. Do not rebuild it between steps 1
  and 3, or the signature and the stapled ticket are lost.
- Notarisation is the only step that needs network. It takes minutes.
- `ditto` and `hdiutil` ship with macOS; no extra tooling.

### Verify

```bash
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d --entitlements :- "$APP"
codesign -dv "$APP"                       # Authority=Developer ID Application...
spctl -a -vv -t exec "$APP"               # accepted, source=Notarized Developer ID
spctl -a -vv -t open --context context:primary-signature "$DMG"
xcrun stapler validate "$APP"
xcrun stapler validate "$DMG"
```

Expected: `spctl` says `accepted` with `source=Notarized Developer ID`; the
stapler prints `The validate action worked!`. An unsigned bundle fails
`spctl` with `rejected` and opens only through
System Settings → Privacy & Security → Open Anyway (or right-click → Open).

## 6. Update path

The updater plugin is **not** in the tree, so there is no automatic update
channel. The chosen flow is a manual download with an explicit version check;
it needs no paid service and no update-signing key of its own.

Operator procedure:

- [ ] Publish the notarised DMG at a stable URL, e.g.
      `https://<host>/releases/OpenCompose_<version>_aarch64.dmg`.
- [ ] Publish `SHA256SUMS` next to it:
      `shasum -a 256 OpenCompose_<version>_aarch64.dmg > SHA256SUMS`.
- [ ] Publish `latest.json` with the current version, download URL and hash
      (this is release metadata for humans and scripts, not a Tauri updater
      manifest — see the note below):

      ```json
      {
        "version": "0.1.0",
        "released": "2026-10-24",
        "download": "https://<host>/releases/OpenCompose_0.1.0_aarch64.dmg",
        "sha256": "<sha256 of the dmg>"
      }
      ```

- [ ] Users compare with the running version (app menu → About OpenCompose, or
      the version in the exported diagnostics archive), download, and verify
      `shasum -a 256 -c SHA256SUMS` before installing.
- [ ] Every release updates `latest.json`, `SHA256SUMS` and the DMG in one go.

Trade-off: users must download and install the new version themselves, but the
project avoids running an update server and holding a second signing key.
Adopting `tauri-plugin-updater` later means a minisign key pair, signed
artefacts and a manifest with a `signature` field — a code change, not a new
release procedure.

`latest.json` above deliberately has no `signature`: Tauri's updater requires a
minisign signature per platform, so do not point the plugin at this file until
that key exists and the artefacts are signed with it.

## 7. What a release must not do

- Do not commit `dist/`, `gen/`, `target/`, `.app`, `.dmg` or `latest.json`.
- Do not ship a bundle whose `npm run version:check` or `npm run smoke:macos`
  failed.
- Do not add a SoundFont library link to the bundle: the engine's player needs
  no dylib, and an unbundled link would abort at launch on a machine without
  it.
- Never remove `com.apple.security.cs.disable-library-validation` from the
  signed app: third-party plugins stop loading without it.
- Do not push build artefacts into the source tree to make them easier to
  find; publish them from the release page.
