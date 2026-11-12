# oc-build-parity

Feature pins that keep the `ocp` and GUI builds on one artifact set. The crate
has no runtime code.

## What it owns

The GUI's Tauri dependencies enable extra features on crates that the CLI shares
with it: `log/kv_unstable` from `tauri-plugin-log`, `objc2/exception` from the
AppKit bindings, `syn/fold` from `tauri-build`. Cargo keys artifacts by resolved
features, so without a pin, alternating `ocp` and the GUI builds recompiles
seventy-odd units because everything above `log` changed. Both front-ends depend
on this crate, so both resolutions match.

## Key types

None. The crate is a manifest: its `Cargo.toml` carries the pins and its
`lib.rs` is a doc comment.

## Depends on

Nothing inside the workspace. It depends on the crates it pins, at the versions
and features the GUI needs.

## Rules

- A pin is a build-time statement, not a place to add code. If you need a
  function here, it belongs in the crate that owns the behaviour.
- Add a pin only when a real feature mismatch exists, and say in the commit
  message which pair of builds it fixes.
- Removing a pin is a performance change for the shared target directory, not a
  cleanup; check the rebuild count before and after.

## API

[`api/oc_build_parity/`](../api/oc_build_parity/index.html).
