# Patch System: Compressed Format & Dispatch

This document describes the undo/redo patch system used in OpenCompose. It covers
the binary storage format (OCPC), the structural diff algorithm, patch dispatch,
and the scope/gesture merging rule.

---

## Architecture Overview

```
User edits project (GUI)
        │
        ▼
Tauri Command (e.g. set_tempo, add_note)
        │  capture old state (move, not clone)
        │  mutate inner.project in-place
        ▼
resync_with_undo(inner, old_project, UndoEdit { name, scopes, merge })
        │
        ▼
UndoStack::push(old, &new, edit)
        │
        ├─ gesture open?          → MERGE diff(merge_base, new)
        ├─ merge asked + scopes intersect the last entry?
        │                         → MERGE diff(merge_base, new)
        └─ otherwise              → NEW ENTRY: diff(old, new)
        ▼
UndoEntry { forward: OCPC, inverse: OCPC, name, scopes, timestamp }
```

Key files:
- `crates/oc-dsl/src/diff.rs` — diff, apply, hash, compress
- `crates/oc-gui/frontend/src-tauri/src/state.rs` — UndoStack, scopes, gesture tokens
- `crates/oc-gui/frontend/src-tauri/src/commands/shared.rs` — `resync_with_undo`
- `crates/oc-gui/frontend/src/stores/project.ts` — history list and jump command

---

## OCPC Binary Format

All patches are stored as OCPC (OpenCompose Patch Compressed) byte arrays.

### Layout

```
Offset  Size  Description
──────  ────  ───────────────────────────────────
0       4B    Magic: ASCII "OCPC"
4       1B    Version: 1
5       4B    Uncompressed payload size (u32 LE)
9       var   DEFLATE-compressed payload
```

Total header: 9 bytes.

### Encoding Pipeline

```
ProjectPatch (Rust struct)
    │
    ▼  serde_json::to_vec()
JSON bytes (uncompressed payload)
    │
    ▼  flate2::DeflateEncoder (compression level: default)
DEFLATE bytes
    │
    ▼  prepend 9-byte OCPC header
Final stored bytes (Vec<u8>)
```

### Decoding Pipeline

```
OCPC bytes
    │
    ▼  validate magic "OCPC", version == 1
    │  read uncompressed_size from header[5..9]
    ▼  flate2::DeflateDecoder
JSON bytes
    │
    ▼  serde_json::from_slice::<ProjectPatch>()
ProjectPatch (Rust struct)
```

### Implementation

```rust
// crates/oc-dsl/src/diff.rs

pub fn encode_patch(patch: &ProjectPatch) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(patch)?;
    // Header: OCPC + version + size
    // Payload: DEFLATE(json)
    encode_payload(&json)
}

pub fn decode_patch(data: &[u8]) -> Result<ProjectPatch, String> {
    let payload = decode_payload(data)?;  // validate header, decompress
    serde_json::from_slice(&payload)
}
```

### Notes

- Despite internal function names (`zstd_compress`/`zstd_decompress`), the actual
  algorithm is DEFLATE via the `flate2` crate. Names are aspirational.
- The uncompressed size field allows pre-allocating the output buffer during decode.
- Typical patch sizes: 200–2000 bytes compressed for single-track edits.

---

## Structural Diff Algorithm

The diff system operates at the **subtree level** — it detects which top-level
project subtrees changed and stores the full new value of each changed subtree.
It does NOT compute field-level deltas within a subtree.

### Hash-Based Change Detection

Before comparing content, both old and new projects are hashed:

```rust
pub struct ProjectHash {
    pub properties: u64,               // single hash for all properties
    pub presets: Vec<(String, u64)>,   // (name, hash) per preset
    pub patterns: Vec<(String, u64)>,  // (name, hash) per pattern
    pub tracks: Vec<(String, u64)>,    // (name, hash) per track
    pub buses: Vec<(String, u64)>,     // (name, hash) per bus
    pub master: u64,                   // single hash for master section
    pub sections: u64,                 // single hash for all sections
}
```

Each subtree is hashed by:
1. Serialize to JSON via `serde_json::to_string()`
2. Feed JSON string into `std::collections::hash_map::DefaultHasher` (SipHash-1-3)
3. Return `hasher.finish()` as `u64`

This means change detection is O(total project) for hashing, but O(changed subtrees)
for the actual diff generation.

### Diff Generation

```rust
pub fn diff(old: &Project, new: &Project) -> Option<ProjectPatch>
```

1. Compute `ProjectHash` for both old and new
2. For the project header and scalar subtrees (properties, master, sections): compare hashes directly
3. For named collections (presets, patterns, buses): compare (name, hash) pairs
   - Names in old but not in new → `Remove*`
   - Names in new with different hash (or not in old) → `Set*`
   - Order not produced by those membership edits → `ReorderPresets`, `ReorderPatterns` or `ReorderBuses`
4. For tracks (order-sensitive):
   - Detect pure reordering → `ReorderTracks`
   - Detect per-track content changes by hash → `SetTrack`
   - Detect removals → `RemoveTrack`
5. If no changes detected, return `None`
6. Generate a human-readable label from the change set

### Named Collection Diff (`diff_named_list`)

Generic over presets, patterns, and buses:

```rust
fn diff_named_list<T>(
    old_hashes: &[(String, u64)],
    new_hashes: &[(String, u64)],
    new_items: &[T],
    get_name: impl Fn(&T) -> &String,
    make_set: impl Fn(&T) -> PatchOp,
    make_remove: impl Fn(&str) -> PatchOp,
    make_reorder: impl Fn(Vec<String>) -> PatchOp,
    changes: &mut Vec<PatchOp>,
)
```

Logic:
- For each name in old not found in new → emit `Remove`
- For each name in new whose hash differs from old (or is absent) → emit `Set`
- Compare the resulting membership order with the requested order; emit one typed reorder only when needed. Inverse generation restores the old declaration order after inverse membership edits.

### Track Diff (`diff_tracks`)

Tracks are position-sensitive (index matters for rendering order):

1. Check if same set of names exist in both but in different order → emit `ReorderTracks`
2. For each old track name not in new → emit `RemoveTrack(old_index)`
3. For each new track whose hash differs from old → emit `SetTrack { index, track }`

---

## Patch Operations (PatchOp)

```rust
pub enum PatchOp {
    SetProperties(ProjectProperties),
    SetPreset(PresetDef),
    RemovePreset(String),
    SetPattern(PatternDef),
    RemovePattern(String),
    SetTrack { index: usize, track: Track },
    RemoveTrack(usize),
    ReorderTracks(Vec<String>),
    SetBus(Bus),
    RemoveBus(String),
    SetMaster(Option<Master>),
    SetSections(Vec<Section>),
}
```

Each operation stores the **complete new value** of the affected subtree.
This simplifies application but means a single mute toggle on a 200-note track
stores all 200 notes in the patch.

---

## Dispatch: `apply_patch`

```rust
pub fn apply_patch(project: &mut Project, patch: &ProjectPatch) -> Result<(), PatchApplyError>
```

Applies operations to a candidate project and publishes it only after all
operations succeed. `apply_patches` performs the same atomic operation for an
entire borrowed patch batch with one candidate clone. Typed errors report
invalid reorder permutations, track indices and audio-clip owner failures.
Named removals remain idempotent. A rejected patch or later batch operation
leaves the original project unchanged.

Dispatch on the candidate:

| PatchOp | Dispatch behavior |
|---------|-------------------|
| `SetProjectHeader(h)` | Replace the optional project name and declared DSL version |
| `SetProperties(p)` | `project.properties = p` |
| `SetPreset(p)` | Find by name → replace; or append if new |
| `RemovePreset(name)` | `project.presets.retain(\|p\| p.name != name)` |
| `SetPattern(p)` | Find by name → replace; or append if new |
| `RemovePattern(name)` | `project.patterns.retain(...)` |
| `SetTrack { index, track }` | Replace at index or append at exactly the current length; reject gaps |
| `RemoveTrack(index)` | Remove a valid index; reject an out-of-range index |
| `ReorderTracks(names)` | Apply an exact permutation; reject missing, unknown or duplicate names |
| `ReorderPresets`, `ReorderPatterns`, `ReorderBuses` | The same checked permutation for the named collection |
| `SetAudioClips(clips)` | Propagate the audio-pool owner result before publication |
| `SetBus(b)` | Find by name → replace; or append if new |
| `RemoveBus(name)` | `project.buses.retain(...)` |
| `SetMaster(m)` | `project.master = m` |
| `SetSections(s)` | `project.sections = s` |

Named items use **name-based lookup** (not index). This makes patches resilient
to insertions/removals of other items between patch creation and application.

Tracks use **index-based lookup** because track order is semantically meaningful
(affects rendering, display, and bus routing by position).

---

## Inverse Patch Generation

```rust
pub fn invert_patch(old: &Project, _new: &Project, patch: &ProjectPatch) -> ProjectPatch
```

For each forward `PatchOp`, produces the reverse operation using `old`:

| Forward | Inverse |
|---------|---------|
| `SetProperties(new_p)` | `SetProperties(old_properties)` |
| `SetPreset(new_p)` | `SetPreset(old_preset)` if existed, else `RemovePreset(name)` |
| `RemovePreset(name)` | `SetPreset(old_preset)` |
| `SetTrack { idx, _ }` | `SetTrack { idx, old_track }` if existed, else `RemoveTrack(idx)` |
| `RemoveTrack(idx)` | `SetTrack { idx, old_track }` |
| `ReorderTracks(_)` | `ReorderTracks(old_order)` |
| `SetBus(new_b)` | `SetBus(old_bus)` if existed, else `RemoveBus(name)` |
| `RemoveBus(name)` | `SetBus(old_bus)` |
| `SetMaster(_)` | `SetMaster(old_master)` |
| `SetSections(_)` | `SetSections(old_sections)` |

The `_new` parameter is unused (present for potential future use).

---

## Scopes and gestures

Merging is declared by the caller, never by a timer.

### Scopes

Every edit carries a closed set of `UndoScope` tags:

| Scope | Covers |
|---|---|
| `project` | playlist, clips, notes, patterns, tracks, DSL edits |
| `mixer_routing` | outputs, sends, buses, effect-chain membership |
| `mixer_levels` | volume, pan, mute/solo, send levels, channel overrides |
| `preset` | presets, patches, effect parameters |
| `automation` | automation lanes and parameter-write gestures |
| `plugin_state` | hosted plugin state and preset imports |
| `transport` | tempo, tempo map, time signature and meter map |

`UndoScopes` is a bitset serialized as an array of scope names.

### Parameters

```rust
const MAX_UNDO: usize = 256;  // maximum undo stack depth
```

### Mechanism

Two pushes merge only when:

1. a gesture is open (unconditional), or
2. the later edit sets `UndoEdit::merge()` **and** its scopes intersect the
   previous entry's scopes.

The merged entry keeps one base project and is recomputed as
`diff(merge_base, new)`; scopes are unioned and the name stays the one the
gesture started with. Returning to the base state drops the entry entirely.

```
begin_gesture(mixer_levels)
push(old_A, B) → NEW entry: diff(A, B)            merge_base = A
push(old_B, C) → MERGE: diff(A, C)                merge_base stays A
push(old_C, D) → MERGE: diff(A, D)                merge_base stays A
end_gesture()
push(old_D, E) → NEW entry (gesture ended): diff(D, E)
```

Two unrelated clicks without a gesture therefore stay two entries even when
they are milliseconds apart, and a two-second fader drag is one entry.

### State Reset

Merge state (`merge_base`, the open gesture) is cleared on:
- Undo
- Redo
- Jump to an undo level
- New project / open file

---

## Undo/Redo Execution

### Undo

```rust
pub fn undo(&mut self, project: &mut Project) -> Result<String, String>
```

1. Read the newest `UndoEntry` without removing it
2. Decode inverse patch: `decode_patch(entry.inverse)` → decompress → JSON → `ProjectPatch`
3. Apply the checked inverse; on success remove the entry, otherwise leave the project and both stacks unchanged
4. Push to redo stack with forward/inverse swapped:
   - `redo.forward = entry.inverse` (what was the inverse becomes the new forward)
   - `redo.inverse = entry.forward` (what was the forward becomes the new inverse)
5. Clear merge state
6. Return the name for UI feedback

### Redo

Mirror of undo:
1. Read the newest redo entry without removing it
2. Decode `entry.inverse` (which holds the original forward patch due to the swap)
3. Apply the checked forward patch, removing the entry only on success
4. Push back to undo stack with fields swapped back to original orientation
5. Clear merge state

### Jump to a level

`restore_undo_level(level, project)` decodes the selected inverse (backwards) or
forward (forwards) patches and applies the entire batch before moving history
entries. A failure in a later patch cannot leave an earlier edit applied. Level `0` is the state before the first entry; `history()` returns every
level with `{index, name, scopes, timestamp, applied}`. The frontend renders that
list and calls the command from the history panel.

---

## Edit names

The undo entry name is supplied by the caller (`UndoEdit::new(name, scope)`), for
example "mixer routing", "clip move" or "tempo". The patch label generated by
`diff.rs` is an internal diagnostic; the history shows the caller's name.

---

## Performance Characteristics

| Operation | Cost |
|-----------|------|
| Hash computation | O(n) — JSON serialize each subtree, SipHash the string |
| Diff generation | O(changed subtrees) after hashing |
| Patch storage | Only changed subtrees stored (compressed) |
| apply_patch | O(patch size) — iterate PatchOps, named lookups |
| encode/decode | JSON serialize + DEFLATE compress/decompress |

### Current bottleneck

`hash_subtree` serializes each subtree to JSON just to hash it. For a track with
hundreds of notes, this allocates a large string on every diff call. A structural
`Hash` derive on AST types would eliminate this allocation.

### Memory budget

Each `UndoEntry` stores two OCPC byte arrays (forward + inverse). Typical sizes:
- Single track mute toggle: ~500 bytes compressed (full track stored)
- Tempo change: ~100 bytes compressed
- Full DSL rewrite: ~5-20KB compressed

With `MAX_UNDO = 256` entries, worst case memory is ~5MB for the undo stack.

---

## Future Improvements

1. **Structural hashing:** Derive `Hash` on AST types instead of JSON-serialize-then-hash.
   Eliminates per-subtree String allocations during diff.

2. **Field-level deltas for tracks:** Instead of storing the entire 200-note track when
   only `mix.mute` changed, store a `SetTrackField { index, field: "mix.mute", value }`
   operation. Reduces patch size 100x for simple property changes.

3. **Replace DEFLATE with zstd:** Better compression ratio (~20% smaller) and faster
   decompression. Already named in the code, just not implemented.

4. **Replace JSON with bincode/postcard:** 2-5x smaller payloads before compression,
   faster serialize/deserialize. Requires versioning strategy for format migrations.

5. **Incremental hashing:** Cache per-subtree hashes and only recompute the one that
   was mutated. Currently, `diff()` re-hashes the entire project on both sides.
