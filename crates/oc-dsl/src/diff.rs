//! Subtree diff for the OpenCompose AST.
//!
//! The project is a tree:
//! ```text
//! Project
//! ├── header     (name and declared OCP version)
//! ├── properties (leaf)
//! ├── presets[]   (each a leaf-like subtree)
//! ├── patterns[]  (each contains events[], hits[])
//! ├── tracks[]    (each contains phrases[], clips[], effects[], mix)
//! ├── buses[]     (each contains effects[])
//! ├── master      (contains effects[])
//! ├── sections[]  (leaves)
//! ├── markers[]   (leaves)
//! └── groups[]    (leaves)
//! ```
//!
//! The semantic model is a DAG (patterns → clips, presets → tracks, buses → sends),
//! but the diff operates purely on the parse tree structure.
//!
//! ## Strategy
//!
//! Each subtree node computes a hash of its content. When comparing two project states:
//! 1. Compare root hashes — if equal, nothing changed.
//! 2. If different, descend: compare each child's hash to identify which subtrees changed.
//! 3. For changed subtrees, store the minimal replacement.
//!
//! This gives us O(n) where n is the number of changed subtrees, not the total project size.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::ast::*;

// ============================================================================
// Tree Hash — compute structural hashes for subtree comparison
// ============================================================================

/// Compute a hash for a serializable subtree. Uses the serde JSON representation
/// as the canonical form for hashing, which is order-stable for structs.
fn hash_subtree<T: Serialize>(node: &T) -> Result<u64, serde_json::Error> {
    let mut hasher = DefaultHasher::new();
    // Use JSON as canonical form (deterministic for structs, order-preserving for vecs)
    serde_json::to_string(node)?.hash(&mut hasher);
    Ok(hasher.finish())
}

/// Structural hash of the entire project, broken into top-level subtree hashes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectHash {
    /// Header hash is absent in older serialized structural hashes.
    #[serde(default)]
    pub header: u64,
    pub properties: u64,
    pub presets: Vec<(String, u64)>,  // (name, hash)
    pub patterns: Vec<(String, u64)>, // (name, hash)
    pub tracks: Vec<(String, u64)>,   // (name, hash)
    pub buses: Vec<(String, u64)>,    // (name, hash)
    pub master: u64,
    pub sections: u64,
    pub markers: u64,
    pub groups: u64,
    /// Audio clips, because they live in track pools and are not part of a
    /// track's JSON shape.
    pub audio_clips: u64,
}

impl ProjectHash {
    /// Compute hashes for all subtrees of a project.
    pub fn compute(project: &Project) -> Result<Self, serde_json::Error> {
        // Keep this exhaustive: adding a Project root field requires an
        // explicit hash/diff ownership decision instead of silently ignoring it.
        let Project {
            name,
            ocp_version,
            properties,
            presets,
            patterns,
            tracks,
            buses,
            master,
            sections,
            markers,
            groups,
        } = project;
        Ok(Self {
            header: hash_subtree(&(name, ocp_version))?,
            properties: hash_subtree(properties)?,
            presets: presets
                .iter()
                .map(|p| hash_subtree(p).map(|hash| (p.name.clone(), hash)))
                .collect::<Result<_, _>>()?,
            patterns: patterns
                .iter()
                .map(|p| hash_subtree(p).map(|hash| (p.name.clone(), hash)))
                .collect::<Result<_, _>>()?,
            tracks: tracks
                .iter()
                .map(|t| hash_subtree(t).map(|hash| (t.name.clone(), hash)))
                .collect::<Result<_, _>>()?,
            buses: buses
                .iter()
                .map(|b| hash_subtree(b).map(|hash| (b.name.clone(), hash)))
                .collect::<Result<_, _>>()?,
            master: hash_subtree(master)?,
            sections: hash_subtree(sections)?,
            markers: hash_subtree(markers)?,
            groups: hash_subtree(groups)?,
            audio_clips: hash_subtree(&project.ordered_audio_clips())?,
        })
    }
}

// ============================================================================
// Diff — describes what changed between two project states
// ============================================================================

/// A patch describing the difference between two project states.
/// Apply the patch to the old state to get the new state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPatch {
    /// Human-readable description of the change (for undo UI)
    pub label: String,
    /// Individual changes — only the modified subtrees
    pub changes: Vec<PatchOp>,
}

/// Authored project header, kept separate from musical properties. Explicit
/// `None` values clear either field when the patch is applied or inverted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectHeader {
    pub name: Option<String>,
    pub ocp_version: Option<String>,
}

impl From<&Project> for ProjectHeader {
    fn from(project: &Project) -> Self {
        Self {
            name: project.name.clone(),
            ocp_version: project.ocp_version.clone(),
        }
    }
}

/// A single change operation in a patch.
// Undo diffs are short-lived; boxing the largest payload would allocate per edit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
/// One reversible project edit.
pub enum PatchOp {
    /// Properties subtree was replaced
    SetProperties(ProjectProperties),
    /// A preset was added or modified
    SetPreset(PresetDef),
    /// A preset was removed (by name)
    RemovePreset(String),
    /// A pattern was added or modified
    SetPattern(PatternDef),
    /// A pattern was removed (by name)
    RemovePattern(String),
    /// A track was added or modified (identified by index in the new state)
    SetTrack { index: usize, track: Track },
    /// A track was removed (by index in the old state)
    RemoveTrack(usize),
    /// Track ordering changed (new order as list of old indices, or names)
    ReorderTracks(Vec<String>),
    /// A bus was added or modified
    SetBus(Bus),
    /// A bus was removed (by name)
    RemoveBus(String),
    /// Master section was replaced
    SetMaster(Option<Master>),
    /// Sections were replaced
    SetSections(Vec<Section>),
    /// Markers were replaced
    SetMarkers(Vec<Marker>),
    /// Clip groups were replaced
    SetGroups(Vec<ClipGroup>),
    /// Audio clips were replaced, with the global order each one carries.
    SetAudioClips(Vec<OrderedAudioClip>),
    /// Name and declared version changed. Append new variants so existing
    /// serialized operations retain their representation and variant order.
    SetProjectHeader(ProjectHeader),
    /// Declaration order after preset membership/content changes.
    ReorderPresets(Vec<String>),
    /// Declaration order after pattern membership/content changes.
    ReorderPatterns(Vec<String>),
    /// Declaration order after bus membership/content changes.
    ReorderBuses(Vec<String>),
}

/// One audio clip in a patch, with its position in the `@audio_clip` sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderedAudioClip {
    /// Global audio order the clip keeps.
    pub order: usize,
    /// The clip itself.
    pub clip: AudioClip,
}

/// Compute the diff (patch) between two project states.
///
/// Returns `None` if the projects are identical. A subtree that cannot
/// serialize is an error, not an empty change.
pub fn diff(old: &Project, new: &Project) -> Result<Option<ProjectPatch>, serde_json::Error> {
    if old == new {
        return Ok(None);
    }
    let old_hash = ProjectHash::compute(old)?;
    let new_hash = ProjectHash::compute(new)?;

    let mut changes = Vec::new();

    if old_hash.header != new_hash.header {
        changes.push(PatchOp::SetProjectHeader(ProjectHeader::from(new)));
    }

    // Properties
    if old_hash.properties != new_hash.properties {
        changes.push(PatchOp::SetProperties(new.properties.clone()));
    }

    // Presets: detect additions, modifications, removals
    diff_named_list(
        &old_hash.presets,
        &new_hash.presets,
        &new.presets,
        |p| &p.name,
        |p| PatchOp::SetPreset(p.clone()),
        |name| PatchOp::RemovePreset(name.to_string()),
        PatchOp::ReorderPresets,
        &mut changes,
    );

    // Patterns
    diff_named_list(
        &old_hash.patterns,
        &new_hash.patterns,
        &new.patterns,
        |p| &p.name,
        |p| PatchOp::SetPattern(p.clone()),
        |name| PatchOp::RemovePattern(name.to_string()),
        PatchOp::ReorderPatterns,
        &mut changes,
    );

    // Tracks — order-sensitive (index matters)
    diff_tracks(old, new, &mut changes, |name| {
        let lookup = |hash: &ProjectHash| {
            hash.tracks
                .iter()
                .find(|(track, _)| track == name)
                .map(|(_, hash)| *hash)
        };
        lookup(&old_hash) != lookup(&new_hash)
    });

    // Buses
    diff_named_list(
        &old_hash.buses,
        &new_hash.buses,
        &new.buses,
        |b| &b.name,
        |b| PatchOp::SetBus(b.clone()),
        |name| PatchOp::RemoveBus(name.to_string()),
        PatchOp::ReorderBuses,
        &mut changes,
    );

    // Master
    if old_hash.master != new_hash.master {
        changes.push(PatchOp::SetMaster(new.master.clone()));
    }

    // Sections
    if old_hash.sections != new_hash.sections {
        changes.push(PatchOp::SetSections(new.sections.clone()));
    }

    // Markers
    if old_hash.markers != new_hash.markers {
        changes.push(PatchOp::SetMarkers(new.markers.clone()));
    }

    // Clip groups
    if old_hash.groups != new_hash.groups {
        changes.push(PatchOp::SetGroups(new.groups.clone()));
    }

    // Audio clips. They are applied last so the tracks they name exist first.
    if old_hash.audio_clips != new_hash.audio_clips {
        changes.push(PatchOp::SetAudioClips(ordered_audio_clips(new)));
    }

    if changes.is_empty() {
        // Different ASTs with equal subtree fingerprints would be a hash
        // collision; catch it in tests instead of dropping the edit.
        debug_assert_eq!(old, new, "project change detection lost an edit");
        Ok(None)
    } else {
        let label = generate_label(&changes);
        Ok(Some(ProjectPatch { label, changes }))
    }
}

/// A named declaration list whose reorder operation failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchList {
    Presets,
    Patterns,
    Buses,
    Tracks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Why a reorder operation cannot be applied.
pub enum ReorderError {
    LengthMismatch { expected: usize, actual: usize },
    DuplicateName(String),
    UnknownName(String),
}

/// Checked patch application failure. The destination project is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchApplyError {
    InvalidReorder {
        list: PatchList,
        reason: ReorderError,
    },
    InvalidAudioClips {
        cause: String,
    },
    InvalidSetTrackIndex {
        index: usize,
        track_count: usize,
    },
    InvalidRemoveTrackIndex {
        index: usize,
        track_count: usize,
    },
}

impl std::fmt::Display for PatchApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAudioClips { cause } => write!(f, "Cannot replace audio clips: {cause}"),
            Self::InvalidSetTrackIndex { index, track_count } => write!(
                f,
                "Cannot set track at index {index}: project has {track_count} tracks; append only at index {track_count}"
            ),
            Self::InvalidRemoveTrackIndex { index, track_count } => write!(
                f,
                "Cannot remove track at index {index}: project has {track_count} tracks"
            ),
            Self::InvalidReorder { list, reason } => {
                let list = match list {
                    PatchList::Presets => "presets",
                    PatchList::Patterns => "patterns",
                    PatchList::Buses => "buses",
                    PatchList::Tracks => "tracks",
                };
                match reason {
                    ReorderError::LengthMismatch { expected, actual } => write!(
                        f,
                        "Cannot reorder {list}: expected {expected} names, got {actual}"
                    ),
                    ReorderError::DuplicateName(name) => {
                        write!(f, "Cannot reorder {list}: name '{name}' is duplicated")
                    }
                    ReorderError::UnknownName(name) => {
                        write!(f, "Cannot reorder {list}: no declaration named '{name}'")
                    }
                }
            }
        }
    }
}
impl std::error::Error for PatchApplyError {}

/// Apply one patch atomically. Validation failures publish none of its edits.
/// Named preset/pattern/bus removals remain idempotent when a name is absent.
/// A track set replaces an existing index or appends exactly at the current
/// length; a removal must name an existing index. No full-project validation
/// is imposed on legitimate intermediate edits.
pub fn apply_patch(project: &mut Project, patch: &ProjectPatch) -> Result<(), PatchApplyError> {
    apply_patches(project, std::iter::once(patch))
}

/// Apply a history jump atomically, cloning the project once for the whole
/// batch. A failed operation in any patch leaves the destination unchanged.
pub fn apply_patches<'a>(
    project: &mut Project,
    patches: impl IntoIterator<Item = &'a ProjectPatch>,
) -> Result<(), PatchApplyError> {
    let mut patches = patches.into_iter().peekable();
    if patches.peek().is_none() {
        return Ok(());
    }
    let mut candidate = project.clone();
    for patch in patches {
        apply_patch_in_place(&mut candidate, patch)?;
    }
    *project = candidate;
    Ok(())
}

fn apply_patch_in_place(
    project: &mut Project,
    patch: &ProjectPatch,
) -> Result<(), PatchApplyError> {
    for op in &patch.changes {
        match op {
            PatchOp::ReorderPresets(names) => {
                reorder_named_list(&mut project.presets, names, PatchList::Presets, |item| {
                    &item.name
                })?;
            }
            PatchOp::ReorderPatterns(names) => {
                reorder_named_list(&mut project.patterns, names, PatchList::Patterns, |item| {
                    &item.name
                })?;
            }
            PatchOp::ReorderBuses(names) => {
                reorder_named_list(&mut project.buses, names, PatchList::Buses, |item| {
                    &item.name
                })?;
            }
            PatchOp::SetProjectHeader(header) => {
                project.name = header.name.clone();
                project.ocp_version = header.ocp_version.clone();
            }
            PatchOp::SetProperties(props) => {
                project.properties = props.clone();
            }
            PatchOp::SetPreset(preset) => {
                if let Some(existing) = project.presets.iter_mut().find(|p| p.name == preset.name) {
                    *existing = preset.clone();
                } else {
                    project.presets.push(preset.clone());
                }
            }
            PatchOp::RemovePreset(name) => {
                project.presets.retain(|p| p.name != *name);
            }
            PatchOp::SetPattern(pattern) => {
                if let Some(existing) = project.patterns.iter_mut().find(|p| p.name == pattern.name)
                {
                    *existing = pattern.clone();
                } else {
                    project.patterns.push(pattern.clone());
                }
            }
            PatchOp::RemovePattern(name) => {
                project.patterns.retain(|p| p.name != *name);
            }
            PatchOp::SetTrack { index, track } => {
                if *index > project.tracks.len() {
                    return Err(PatchApplyError::InvalidSetTrackIndex {
                        index: *index,
                        track_count: project.tracks.len(),
                    });
                }
                if let Some(existing) = project.tracks.get_mut(*index) {
                    let mut replacement = track.clone();
                    // Track wire payloads carry notes and automation; audio
                    // clips are only serialized by SetAudioClips.
                    if replacement.name == existing.name
                        && replacement.audio_clips().next().is_none()
                    {
                        for entry in existing.clips.iter() {
                            if let Some(clip) = entry.as_audio() {
                                replacement.clips.push_audio_at(entry.order, clip.clone());
                            }
                        }
                    }
                    *existing = replacement;
                } else {
                    project.tracks.push(track.clone());
                }
            }
            PatchOp::RemoveTrack(index) => {
                if *index >= project.tracks.len() {
                    return Err(PatchApplyError::InvalidRemoveTrackIndex {
                        index: *index,
                        track_count: project.tracks.len(),
                    });
                }
                project.tracks.remove(*index);
            }
            PatchOp::ReorderTracks(names) => {
                reorder_named_list(&mut project.tracks, names, PatchList::Tracks, |item| {
                    &item.name
                })?;
            }
            PatchOp::SetBus(bus) => {
                if let Some(existing) = project.buses.iter_mut().find(|b| b.name == bus.name) {
                    *existing = bus.clone();
                } else {
                    project.buses.push(bus.clone());
                }
            }
            PatchOp::RemoveBus(name) => {
                project.buses.retain(|b| b.name != *name);
            }
            PatchOp::SetMaster(master) => {
                project.master = master.clone();
            }
            PatchOp::SetSections(sections) => {
                project.sections = sections.clone();
            }
            PatchOp::SetMarkers(markers) => {
                project.markers = markers.clone();
            }
            PatchOp::SetGroups(groups) => {
                project.groups = groups.clone();
            }
            PatchOp::SetAudioClips(clips) => {
                let ordered = clips
                    .iter()
                    .map(|clip| (clip.order, clip.clip.clone()))
                    .collect::<Vec<_>>();
                project
                    .set_ordered_audio_clips(ordered)
                    .map_err(|cause| PatchApplyError::InvalidAudioClips { cause })?;
            }
        }
    }
    Ok(())
}

/// The audio clips of a project as patch payload.
fn ordered_audio_clips(project: &Project) -> Vec<OrderedAudioClip> {
    project
        .ordered_audio_clips()
        .into_iter()
        .map(|(order, clip)| OrderedAudioClip {
            order,
            clip: clip.clone(),
        })
        .collect()
}

/// Compute the reverse of a patch (for undo).
pub fn invert_patch(old: &Project, new: &Project, patch: &ProjectPatch) -> ProjectPatch {
    let mut inverse_changes = Vec::new();
    let mut tracks_inverted = false;
    let mut presets_touched = false;
    let mut patterns_touched = false;
    let mut buses_touched = false;

    for op in &patch.changes {
        match op {
            PatchOp::ReorderPresets(_) => presets_touched = true,
            PatchOp::ReorderPatterns(_) => patterns_touched = true,
            PatchOp::ReorderBuses(_) => buses_touched = true,
            PatchOp::SetProjectHeader(_) => {
                inverse_changes.push(PatchOp::SetProjectHeader(ProjectHeader::from(old)));
            }
            PatchOp::SetProperties(_) => {
                inverse_changes.push(PatchOp::SetProperties(old.properties.clone()));
            }
            PatchOp::SetPreset(preset) => {
                presets_touched = true;
                if let Some(old_preset) = old.presets.iter().find(|p| p.name == preset.name) {
                    inverse_changes.push(PatchOp::SetPreset(old_preset.clone()));
                } else {
                    inverse_changes.push(PatchOp::RemovePreset(preset.name.clone()));
                }
            }
            PatchOp::RemovePreset(name) => {
                presets_touched = true;
                if let Some(old_preset) = old.presets.iter().find(|p| p.name == *name) {
                    inverse_changes.push(PatchOp::SetPreset(old_preset.clone()));
                }
            }
            PatchOp::SetPattern(pattern) => {
                patterns_touched = true;
                if let Some(old_pattern) = old.patterns.iter().find(|p| p.name == pattern.name) {
                    inverse_changes.push(PatchOp::SetPattern(old_pattern.clone()));
                } else {
                    inverse_changes.push(PatchOp::RemovePattern(pattern.name.clone()));
                }
            }
            PatchOp::RemovePattern(name) => {
                patterns_touched = true;
                if let Some(old_pattern) = old.patterns.iter().find(|p| p.name == *name) {
                    inverse_changes.push(PatchOp::SetPattern(old_pattern.clone()));
                }
            }
            PatchOp::SetTrack { .. } | PatchOp::RemoveTrack(_) | PatchOp::ReorderTracks(_) => {
                if !tracks_inverted {
                    let modified: std::collections::HashSet<_> = patch
                        .changes
                        .iter()
                        .filter_map(|op| match op {
                            PatchOp::SetTrack { track, .. } => Some(track.name.as_str()),
                            _ => None,
                        })
                        .collect();
                    diff_tracks(new, old, &mut inverse_changes, |name| {
                        modified.contains(name)
                    });
                    tracks_inverted = true;
                }
            }
            PatchOp::SetBus(bus) => {
                buses_touched = true;
                if let Some(old_bus) = old.buses.iter().find(|b| b.name == bus.name) {
                    inverse_changes.push(PatchOp::SetBus(old_bus.clone()));
                } else {
                    inverse_changes.push(PatchOp::RemoveBus(bus.name.clone()));
                }
            }
            PatchOp::RemoveBus(name) => {
                buses_touched = true;
                if let Some(old_bus) = old.buses.iter().find(|b| b.name == *name) {
                    inverse_changes.push(PatchOp::SetBus(old_bus.clone()));
                }
            }
            PatchOp::SetMaster(_) => {
                inverse_changes.push(PatchOp::SetMaster(old.master.clone()));
            }
            PatchOp::SetSections(_) => {
                inverse_changes.push(PatchOp::SetSections(old.sections.clone()));
            }
            PatchOp::SetMarkers(_) => {
                inverse_changes.push(PatchOp::SetMarkers(old.markers.clone()));
            }
            PatchOp::SetGroups(_) => {
                inverse_changes.push(PatchOp::SetGroups(old.groups.clone()));
            }
            PatchOp::SetAudioClips(_) => {
                inverse_changes.push(PatchOp::SetAudioClips(ordered_audio_clips(old)));
            }
        }
    }

    if presets_touched {
        append_inverse_named_reorder(
            old.presets.iter().map(|item| item.name.clone()).collect(),
            new.presets.iter().map(|item| item.name.clone()).collect(),
            |op| match op {
                PatchOp::SetPreset(item) => Some((true, item.name.as_str())),
                PatchOp::RemovePreset(name) => Some((false, name.as_str())),
                _ => None,
            },
            PatchOp::ReorderPresets,
            &mut inverse_changes,
        );
    }
    if patterns_touched {
        append_inverse_named_reorder(
            old.patterns.iter().map(|item| item.name.clone()).collect(),
            new.patterns.iter().map(|item| item.name.clone()).collect(),
            |op| match op {
                PatchOp::SetPattern(item) => Some((true, item.name.as_str())),
                PatchOp::RemovePattern(name) => Some((false, name.as_str())),
                _ => None,
            },
            PatchOp::ReorderPatterns,
            &mut inverse_changes,
        );
    }
    if buses_touched {
        append_inverse_named_reorder(
            old.buses.iter().map(|item| item.name.clone()).collect(),
            new.buses.iter().map(|item| item.name.clone()).collect(),
            |op| match op {
                PatchOp::SetBus(item) => Some((true, item.name.as_str())),
                PatchOp::RemoveBus(name) => Some((false, name.as_str())),
                _ => None,
            },
            PatchOp::ReorderBuses,
            &mut inverse_changes,
        );
    }

    ProjectPatch {
        label: format!("Undo: {}", patch.label),
        changes: inverse_changes,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Validate the entire order before swapping; never discard list contents.
fn reorder_named_list<T>(
    items: &mut [T],
    names: &[String],
    list: PatchList,
    name: impl Fn(&T) -> &String,
) -> Result<(), PatchApplyError> {
    let error = |reason| PatchApplyError::InvalidReorder { list, reason };
    if items.len() != names.len() {
        return Err(error(ReorderError::LengthMismatch {
            expected: items.len(),
            actual: names.len(),
        }));
    }
    for (index, item) in items.iter().enumerate() {
        if items[..index]
            .iter()
            .any(|previous| name(previous) == name(item))
        {
            return Err(error(ReorderError::DuplicateName(name(item).clone())));
        }
    }
    for (index, entry) in names.iter().enumerate() {
        if names[..index].contains(entry) {
            return Err(error(ReorderError::DuplicateName(entry.clone())));
        }
        if !items.iter().any(|item| name(item) == entry) {
            return Err(error(ReorderError::UnknownName(entry.clone())));
        }
    }
    for (index, entry) in names.iter().enumerate() {
        let offset = items[index..]
            .iter()
            .position(|item| name(item) == entry)
            .expect("validated exact declaration permutation");
        items.swap(index, index + offset);
    }
    Ok(())
}

/// Inverse membership operations may append an old declaration in a new
/// position even when the forward patch needed no reorder. Project the actual
/// inverse edits, then restore the old order after every membership update.
fn append_inverse_named_reorder(
    old_names: Vec<String>,
    mut names: Vec<String>,
    edit: fn(&PatchOp) -> Option<(bool, &str)>,
    make_reorder: fn(Vec<String>) -> PatchOp,
    changes: &mut Vec<PatchOp>,
) {
    for op in changes.iter() {
        if let Some((set, name)) = edit(op) {
            if set {
                if !names.iter().any(|existing| existing == name) {
                    names.push(name.to_string());
                }
            } else {
                names.retain(|existing| existing != name);
            }
        }
    }
    if names != old_names {
        changes.push(make_reorder(old_names));
    }
}

/// Diff a list of named items (presets, patterns, buses).
// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
fn diff_named_list<T>(
    old_hashes: &[(String, u64)],
    new_hashes: &[(String, u64)],
    new_items: &[T],
    get_name: impl Fn(&T) -> &String,
    make_set: impl Fn(&T) -> PatchOp,
    make_remove: impl Fn(&str) -> PatchOp,
    make_reorder: impl Fn(Vec<String>) -> PatchOp,
    changes: &mut Vec<PatchOp>,
) {
    // Detect removals: items in old but not in new
    for (name, _) in old_hashes {
        if !new_hashes.iter().any(|(n, _)| n == name) {
            changes.push(make_remove(name));
        }
    }

    // Detect additions and modifications
    for (name, new_h) in new_hashes {
        let old_h = old_hashes.iter().find(|(n, _)| n == name).map(|(_, h)| *h);
        if old_h != Some(*new_h) {
            // Changed or new — store the full new item
            if let Some(item) = new_items.iter().find(|i| get_name(i) == name) {
                changes.push(make_set(item));
            }
        }
    }
    // Existing items keep their relative order; newly named items append in
    // new-declaration order. Request an explicit reorder only if necessary.
    let projected: Vec<&str> = old_hashes
        .iter()
        .filter(|(name, _)| new_hashes.iter().any(|(new, _)| new == name))
        .map(|(name, _)| name.as_str())
        .chain(
            new_hashes
                .iter()
                .filter(|(name, _)| !old_hashes.iter().any(|(old, _)| old == name))
                .map(|(name, _)| name.as_str()),
        )
        .collect();
    if !projected
        .iter()
        .copied()
        .eq(new_hashes.iter().map(|(name, _)| name.as_str()))
    {
        changes.push(make_reorder(
            new_hashes.iter().map(|(name, _)| name.clone()).collect(),
        ));
    }
}

/// Remove indices back-to-front, then add, reorder and update in that order.
fn diff_tracks(
    old: &Project,
    new: &Project,
    changes: &mut Vec<PatchOp>,
    modified: impl Fn(&str) -> bool,
) {
    let new_names: Vec<_> = new.tracks.iter().map(|track| track.name.as_str()).collect();
    let old_names: std::collections::HashSet<_> =
        old.tracks.iter().map(|track| track.name.as_str()).collect();
    let mut order: Vec<_> = old.tracks.iter().map(|track| track.name.as_str()).collect();
    for index in (0..order.len()).rev() {
        if !new_names.contains(&order[index]) {
            changes.push(PatchOp::RemoveTrack(index));
            order.remove(index);
        }
    }
    for track in &new.tracks {
        if !old_names.contains(track.name.as_str()) {
            changes.push(PatchOp::SetTrack {
                index: order.len(),
                track: track.clone(),
            });
            order.push(&track.name);
        }
    }
    if order != new_names {
        changes.push(PatchOp::ReorderTracks(
            new_names.into_iter().map(str::to_owned).collect(),
        ));
    }
    for (index, track) in new.tracks.iter().enumerate() {
        if old_names.contains(track.name.as_str()) && modified(&track.name) {
            changes.push(PatchOp::SetTrack {
                index,
                track: track.clone(),
            });
        }
    }
}

fn generate_label(changes: &[PatchOp]) -> String {
    if changes.len() == 1 {
        match &changes[0] {
            PatchOp::ReorderPresets(_) => "Reordered presets".to_string(),
            PatchOp::ReorderPatterns(_) => "Reordered patterns".to_string(),
            PatchOp::ReorderBuses(_) => "Reordered buses".to_string(),
            PatchOp::SetProjectHeader(_) => "Changed project header".to_string(),
            PatchOp::SetProperties(_) => "Changed project properties".to_string(),
            PatchOp::SetPreset(p) => format!("Modified preset '{}'", p.name),
            PatchOp::RemovePreset(n) => format!("Removed preset '{}'", n),
            PatchOp::SetPattern(p) => format!("Modified pattern '{}'", p.name),
            PatchOp::RemovePattern(n) => format!("Removed pattern '{}'", n),
            PatchOp::SetTrack { track, .. } => format!("Modified track '{}'", track.name),
            PatchOp::RemoveTrack(_) => "Removed track".to_string(),
            PatchOp::ReorderTracks(_) => "Reordered tracks".to_string(),
            PatchOp::SetBus(b) => format!("Modified bus '{}'", b.name),
            PatchOp::RemoveBus(n) => format!("Removed bus '{}'", n),
            PatchOp::SetMaster(_) => "Changed master settings".to_string(),
            PatchOp::SetSections(_) => "Changed arrangement sections".to_string(),
            PatchOp::SetMarkers(_) => "Changed timeline markers".to_string(),
            PatchOp::SetGroups(_) => "Changed clip groups".to_string(),
            PatchOp::SetAudioClips(_) => "Changed audio clips".to_string(),
        }
    } else {
        let kinds: Vec<&str> = changes
            .iter()
            .map(|c| match c {
                PatchOp::ReorderPresets(_) => "preset order",
                PatchOp::ReorderPatterns(_) => "pattern order",
                PatchOp::ReorderBuses(_) => "bus order",
                PatchOp::SetProjectHeader(_) => "project header",
                PatchOp::SetProperties(_) => "properties",
                PatchOp::SetPreset(_) | PatchOp::RemovePreset(_) => "preset",
                PatchOp::SetPattern(_) | PatchOp::RemovePattern(_) => "pattern",
                PatchOp::SetTrack { .. } | PatchOp::RemoveTrack(_) | PatchOp::ReorderTracks(_) => {
                    "track"
                }
                PatchOp::SetBus(_) | PatchOp::RemoveBus(_) => "bus",
                PatchOp::SetMaster(_) => "master",
                PatchOp::SetSections(_) => "sections",
                PatchOp::SetMarkers(_) => "marker",
                PatchOp::SetGroups(_) => "group",
                PatchOp::SetAudioClips(_) => "audio clip",
            })
            .collect();
        let unique: std::collections::HashSet<&str> = kinds.iter().copied().collect();
        if unique.len() == 1 {
            format!("{} {} changes", changes.len(), kinds[0])
        } else {
            format!("{} changes", changes.len())
        }
    }
}

// ============================================================================
// Compressed format (.ocpc)
// ============================================================================

/// Compressed project format for efficient storage and transmission.
///
/// Format: magic bytes + version + raw DEFLATE-compressed JSON payload.
///
/// ```text
/// [4 bytes] Magic: "OCPC"
/// [1 byte]  Version: 1
/// [4 bytes] Uncompressed size (u32 LE)
/// [...]     raw DEFLATE-compressed JSON(Project)
/// ```
///
/// Size comparison for a typical project:
/// - .ocp text:  ~5-20 KB
/// - .ocpc:      ~1-4 KB (5-10x smaller due to eliminating text redundancy + compression)
///
/// The compressed format is ideal for:
/// - Undo stack entries (patches stored as compressed PatchOp lists)
/// - Autosave snapshots
/// - Network transmission to AI model
/// - Clipboard operations
pub mod compressed;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn performance_edits_survive_compressed_undo_redo_and_project_reopen() {
        let original = crate::parse("@project\n@track t | instrument: piano/acoustic_grand\n  performance: { @0.125 ch2 note_on 60 90; @0.125 ch2 note_off 60 37; @1.234567891 ch2 bend -1234; }\n").unwrap();
        let mut edited = original.clone();
        let events = &mut edited.tracks[0].performance.as_mut().unwrap().events;
        events[1].message = crate::performance::ChannelCommand::NoteOff {
            note: 60,
            velocity: 73,
        };
        events[2].quarter = 1.345678912;
        let forward = super::diff(&original, &edited)
            .expect("projects serialize")
            .expect("performance edit must produce a patch");
        let inverse = super::invert_patch(&original, &edited, &forward);
        let forward =
            super::compressed::decode_patch(&super::compressed::encode_patch(&forward).unwrap())
                .unwrap();
        let inverse =
            super::compressed::decode_patch(&super::compressed::encode_patch(&inverse).unwrap())
                .unwrap();
        let mut current = original.clone();
        super::apply_patch(&mut current, &forward).unwrap();
        assert_eq!(crate::serialize(&current), crate::serialize(&edited));
        super::apply_patch(&mut current, &inverse).unwrap();
        assert_eq!(crate::serialize(&current), crate::serialize(&original));
        super::apply_patch(&mut current, &forward).unwrap();
        let reopened = crate::parse(&crate::serialize(&current)).unwrap();
        assert_eq!(reopened.tracks[0].performance, edited.tracks[0].performance);
    }

    use super::*;

    #[test]
    fn change_detection_reports_a_serialize_failure() {
        struct Failing;
        impl serde::Serialize for Failing {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(<S::Error as serde::ser::Error>::custom(
                    "serialization failed",
                ))
            }
        }
        let error = hash_subtree(&Failing)
            .expect_err("a failing serializer must not hash as an empty subtree");
        assert!(error.to_string().contains("serialization failed"));
    }

    fn make_project() -> Project {
        Project {
            name: Some("Test".to_string()),
            ocp_version: Some("0.2.0".to_string()),
            properties: ProjectProperties::default(),
            presets: vec![],
            patterns: vec![],
            tracks: vec![],
            buses: vec![],
            master: None,
            sections: vec![],
            markers: Vec::new(),
            groups: Vec::new(),
        }
    }

    fn make_track(name: &str) -> Track {
        Track {
            name: name.to_string(),
            instrument: InstrumentPath::new("piano", "grand"),
            engine: None,
            preset: None,
            mix: MixState::default(),
            effects: vec![],
            phrases: vec![],
            patterns: vec![],
            inline_patterns: vec![],
            clips: ClipPool::new(),
            gain_automation: vec![],
            engine_params: None,
            performance: None,
            channel: None,
            output_port: None,
            output_pair: None,
            input_routes: vec![],
        }
    }

    fn assert_patch_replays_and_inverts(old: &Project, new: &Project) -> ProjectPatch {
        let patch = diff(old, new)
            .expect("projects serialize")
            .expect("edit must produce a patch");
        let inverse = invert_patch(old, new, &patch);
        let reopen = |patch: &ProjectPatch| {
            let json = serde_json::to_string(patch).unwrap();
            let json_patch: ProjectPatch = serde_json::from_str(&json).unwrap();
            let bytes = compressed::encode_patch(&json_patch).unwrap();
            assert_eq!(
                &bytes[..5],
                b"OCPC\x01",
                "compressed format version stays unchanged"
            );
            compressed::decode_patch(&bytes).unwrap()
        };
        let mut state = old.clone();
        apply_patch(&mut state, &reopen(&patch)).unwrap();
        assert_eq!(&state, new);
        apply_patch(&mut state, &reopen(&inverse)).unwrap();
        assert_eq!(&state, old);
        apply_patch(&mut state, &reopen(&patch)).unwrap();
        assert_eq!(&state, new);
        patch
    }

    #[test]
    fn project_header_name_only_edit_hashes_applies_and_inverts() {
        let old = make_project();
        let mut new = old.clone();
        new.name = Some("A new project title".into());
        let before = ProjectHash::compute(&old).unwrap();
        let after = ProjectHash::compute(&new).unwrap();
        assert_ne!(before.header, after.header);
        assert_eq!(before.properties, after.properties);
        let patch = assert_patch_replays_and_inverts(&old, &new);
        assert!(
            matches!(patch.changes.as_slice(), [PatchOp::SetProjectHeader(header)]
            if header.name == new.name && header.ocp_version == old.ocp_version)
        );
        assert!(patch.label.contains("header"));
    }

    #[test]
    fn project_header_version_only_edit_does_not_change_project_name() {
        let old = make_project();
        let mut new = old.clone();
        new.ocp_version = Some("0.1.0".into());
        let patch = assert_patch_replays_and_inverts(&old, &new);
        assert!(
            matches!(patch.changes.as_slice(), [PatchOp::SetProjectHeader(header)]
            if header.name == old.name && header.ocp_version == new.ocp_version)
        );
        assert_ne!(
            ProjectHash::compute(&old).unwrap().header,
            ProjectHash::compute(&new).unwrap().header
        );
    }

    #[test]
    fn project_header_explicit_none_clears_either_or_both_fields_through_wire() {
        let old = make_project();
        for (name, ocp_version) in [
            (None, old.ocp_version.clone()),
            (old.name.clone(), None),
            (None, None),
        ] {
            let mut new = old.clone();
            new.name = name;
            new.ocp_version = ocp_version;
            let patch = assert_patch_replays_and_inverts(&old, &new);
            let wire = serde_json::to_value(&patch).unwrap();
            let header = &wire["changes"][0]["SetProjectHeader"];
            assert_eq!(header["name"], serde_json::to_value(&new.name).unwrap());
            assert_eq!(
                header["ocp_version"],
                serde_json::to_value(&new.ocp_version).unwrap()
            );
        }
        let mut absent = old.clone();
        absent.name = None;
        absent.ocp_version = None;
        assert!(diff(&absent, &absent.clone()).unwrap().is_none());
    }

    #[test]
    fn project_header_equal_content_is_a_noop_and_mixed_edits_replay_whole_state() {
        let mut old = make_project();
        old.tracks.push(make_track("Piano"));
        assert!(diff(&old, &old.clone()).unwrap().is_none());
        let mut new = old.clone();
        new.name = Some("Mixed edit".into());
        new.ocp_version = None;
        new.properties.tempo = 137.0;
        new.tracks[0].mix.volume_db = -5.0;
        let patch = assert_patch_replays_and_inverts(&old, &new);
        assert_eq!(patch.changes.len(), 3);
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::SetProjectHeader(_)))
        );
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::SetProperties(_)))
        );
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::SetTrack { .. }))
        );
        assert_eq!(patch.label, "3 changes");
    }

    #[test]
    fn project_header_addition_preserves_legacy_patch_wire_and_hash_decode() {
        let legacy_wire = r#"{"label":"legacy cleanup","changes":[{"SetMaster":null},{"RemovePreset":"obsolete"}]}"#;
        let legacy: ProjectPatch = serde_json::from_str(legacy_wire).unwrap();
        let encoded = compressed::encode_patch(&legacy).unwrap();
        assert_eq!(&encoded[..5], b"OCPC\x01");
        let reopened = compressed::decode_patch(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(&reopened).unwrap(),
            serde_json::from_str::<serde_json::Value>(legacy_wire).unwrap()
        );
        let mut project = make_project();
        let old_header = ProjectHeader::from(&project);
        apply_patch(&mut project, &reopened).unwrap();
        assert_eq!(ProjectHeader::from(&project), old_header);
        let mut wire = serde_json::to_value(ProjectHash::compute(&project).unwrap()).unwrap();
        wire.as_object_mut().unwrap().remove("header");
        let legacy_hash: ProjectHash = serde_json::from_value(wire).unwrap();
        assert_eq!(legacy_hash.header, 0);
        assert_eq!(
            legacy_hash.properties,
            ProjectHash::compute(&project).unwrap().properties
        );
    }

    fn named_order_project() -> Project {
        let mut project = make_project();
        for (index, name) in ["A", "B", "C"].into_iter().enumerate() {
            project.presets.push(PresetDef {
                name: name.into(),
                source: Some("soundfont".into()),
                plugin: None,
                program: Some(index as u8),
                patch: None,
                file: None,
                root_note: None,
                params: Vec::new(),
                effects: Vec::new(),
                extends: None,
            });
            project.patterns.push(PatternDef {
                name: name.into(),
                instrument: InstrumentPath::new("piano", "grand"),
                preset: None,
                engine: None,
                transpose: None,
                swing: None,
                bars: Some(1.0),
                effects: Vec::new(),
                events: Vec::new(),
                pedals: Vec::new(),
                hairpins: Vec::new(),
                hits: Vec::new(),
                color: Some(name.into()),
            });
            project.buses.push(Bus {
                name: name.into(),
                routes: Vec::new(),
                effects: Vec::new(),
                gain_automation: Vec::new(),
                automation: Vec::new(),
                volume_db: -(index as f64),
                mute: false,
                solo: false,
                input_routes: Vec::new(),
            });
            project.tracks.push(make_track(name));
        }
        project
    }

    fn named_reorder_op(list: PatchList, names: Vec<String>) -> PatchOp {
        match list {
            PatchList::Presets => PatchOp::ReorderPresets(names),
            PatchList::Patterns => PatchOp::ReorderPatterns(names),
            PatchList::Buses => PatchOp::ReorderBuses(names),
            PatchList::Tracks => PatchOp::ReorderTracks(names),
        }
    }

    #[test]
    fn named_list_order_only_changes_emit_one_typed_reorder_and_roundtrip() {
        let old = named_order_project();
        for list in [PatchList::Presets, PatchList::Patterns, PatchList::Buses] {
            let mut new = old.clone();
            match list {
                PatchList::Presets => new.presets.rotate_right(1),
                PatchList::Patterns => new.patterns.rotate_right(1),
                PatchList::Buses => new.buses.rotate_right(1),
                PatchList::Tracks => unreachable!(),
            }
            let patch = assert_patch_replays_and_inverts(&old, &new);
            assert_eq!(patch.changes.len(), 1);
            let expected = named_reorder_op(list, vec!["C".into(), "A".into(), "B".into()]);
            assert_eq!(
                serde_json::to_value(&patch.changes[0]).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert!(patch.label.starts_with("Reordered"));
        }
    }

    #[test]
    fn named_list_order_mixed_add_remove_rename_and_content_changes_roundtrip() {
        let old = named_order_project();
        let mut new = old.clone();
        new.presets[0].name = "renamed-A".into();
        new.presets[2].program = Some(99);
        let mut preset = old.presets[1].clone();
        preset.name = "D".into();
        new.presets = vec![preset, new.presets[2].clone(), new.presets[0].clone()];
        new.patterns[0].name = "renamed-A".into();
        new.patterns[2].transpose = Some(2);
        let mut pattern = old.patterns[1].clone();
        pattern.name = "D".into();
        new.patterns = vec![pattern, new.patterns[2].clone(), new.patterns[0].clone()];
        new.buses[0].name = "renamed-A".into();
        new.buses[2].volume_db = -11.0;
        let mut bus = old.buses[1].clone();
        bus.name = "D".into();
        new.buses = vec![bus, new.buses[2].clone(), new.buses[0].clone()];
        let patch = assert_patch_replays_and_inverts(&old, &new);
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderPresets(_)))
        );
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderPatterns(_)))
        );
        assert!(
            patch
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderBuses(_)))
        );
    }

    #[test]
    fn named_list_inverse_restores_removed_declaration_position_without_forward_reorder() {
        let old = named_order_project();
        let mut new = old.clone();
        new.presets.remove(0);
        new.patterns.remove(0);
        new.buses.remove(0);
        let patch = assert_patch_replays_and_inverts(&old, &new);
        assert_eq!(patch.changes.len(), 3);
        let inverse = invert_patch(&old, &new, &patch);
        assert!(
            inverse
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderPresets(_)))
        );
        assert!(
            inverse
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderPatterns(_)))
        );
        assert!(
            inverse
                .changes
                .iter()
                .any(|op| matches!(op, PatchOp::ReorderBuses(_)))
        );
    }

    #[test]
    fn named_list_invalid_reorders_are_typed_and_never_drop_items() {
        for list in [
            PatchList::Presets,
            PatchList::Patterns,
            PatchList::Buses,
            PatchList::Tracks,
        ] {
            for (names, reason) in [
                (
                    vec!["A", "B"],
                    ReorderError::LengthMismatch {
                        expected: 3,
                        actual: 2,
                    },
                ),
                (
                    vec!["A", "B", "C", "D"],
                    ReorderError::LengthMismatch {
                        expected: 3,
                        actual: 4,
                    },
                ),
                (vec!["A", "A", "C"], ReorderError::DuplicateName("A".into())),
                (
                    vec!["A", "missing", "C"],
                    ReorderError::UnknownName("missing".into()),
                ),
            ] {
                let original = named_order_project();
                let mut project = original.clone();
                let patch = ProjectPatch {
                    label: "invalid order".into(),
                    changes: vec![named_reorder_op(
                        list,
                        names.into_iter().map(String::from).collect(),
                    )],
                };
                assert_eq!(
                    apply_patch(&mut project, &patch),
                    Err(PatchApplyError::InvalidReorder { list, reason })
                );
                assert_eq!(project, original);
            }
        }
    }

    #[test]
    fn named_list_reorder_rejects_ambiguous_source_names_and_accepts_empty_lists() {
        let mut duplicate = named_order_project();
        duplicate.presets[1].name = "A".into();
        let original = duplicate.clone();
        let patch = ProjectPatch {
            label: "ambiguous".into(),
            changes: vec![PatchOp::ReorderPresets(vec![
                "A".into(),
                "B".into(),
                "C".into(),
            ])],
        };
        assert_eq!(
            apply_patch(&mut duplicate, &patch),
            Err(PatchApplyError::InvalidReorder {
                list: PatchList::Presets,
                reason: ReorderError::DuplicateName("A".into())
            })
        );
        assert_eq!(duplicate, original);
        for list in [
            PatchList::Presets,
            PatchList::Patterns,
            PatchList::Buses,
            PatchList::Tracks,
        ] {
            let mut empty = make_project();
            let original = empty.clone();
            apply_patch(
                &mut empty,
                &ProjectPatch {
                    label: "empty".into(),
                    changes: vec![named_reorder_op(list, Vec::new())],
                },
            )
            .unwrap();
            assert_eq!(empty, original);
        }
    }

    #[test]
    fn named_list_late_invalid_operation_rolls_back_whole_patch_and_batch() {
        let original = named_order_project();
        let header = ProjectHeader {
            name: Some("must not publish".into()),
            ocp_version: None,
        };
        let valid = ProjectPatch {
            label: "valid first".into(),
            changes: vec![
                PatchOp::SetProjectHeader(header),
                PatchOp::ReorderPresets(vec!["C".into(), "A".into(), "B".into()]),
            ],
        };
        let invalid = ProjectPatch {
            label: "invalid later".into(),
            changes: vec![PatchOp::ReorderTracks(vec![
                "A".into(),
                "missing".into(),
                "C".into(),
            ])],
        };
        let mut project = original.clone();
        let mut combined = valid.clone();
        combined.changes.extend(invalid.changes.clone());
        assert!(apply_patch(&mut project, &combined).is_err());
        assert_eq!(project, original);
        assert!(apply_patches(&mut project, [&valid, &invalid]).is_err());
        assert_eq!(project, original);
        apply_patches(&mut project, std::iter::empty()).unwrap();
        assert_eq!(project, original);
    }

    #[test]
    fn checked_patch_rejects_track_index_gaps_and_late_invalid_removals_atomically() {
        let original = named_order_project();
        for (invalid, expected) in [
            (
                PatchOp::SetTrack {
                    index: 5,
                    track: make_track("gap"),
                },
                PatchApplyError::InvalidSetTrackIndex {
                    index: 5,
                    track_count: 4,
                },
            ),
            (
                PatchOp::RemoveTrack(4),
                PatchApplyError::InvalidRemoveTrackIndex {
                    index: 4,
                    track_count: 4,
                },
            ),
            (
                PatchOp::RemoveTrack(usize::MAX),
                PatchApplyError::InvalidRemoveTrackIndex {
                    index: usize::MAX,
                    track_count: 4,
                },
            ),
        ] {
            let first = ProjectPatch {
                label: "append valid track".into(),
                changes: vec![PatchOp::SetTrack {
                    index: 3,
                    track: make_track("D"),
                }],
            };
            let second = ProjectPatch {
                label: "invalid track index".into(),
                changes: vec![invalid],
            };
            let mut project = original.clone();
            let mut combined = first.clone();
            combined.changes.extend(second.changes.clone());
            assert_eq!(apply_patch(&mut project, &combined), Err(expected.clone()));
            assert_eq!(project, original);
            assert_eq!(
                apply_patches(&mut project, [&first, &second]),
                Err(expected)
            );
            assert_eq!(project, original);
        }
    }

    #[test]
    fn checked_patch_preserves_exact_audio_clip_owner_error_and_rolls_back_partial_owner_work() {
        let original = crate::parse("@project\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n@track tape | instrument: audio/clip\n").unwrap();
        let valid_clip = original.audio_clips()[0].clone();
        let mut invalid_clip = valid_clip.clone();
        invalid_clip.track = "missing-track".into();
        let mut failing_owner = original.clone();
        let expected = failing_owner
            .set_ordered_audio_clips([(0, valid_clip.clone()), (1, invalid_clip.clone())])
            .unwrap_err();
        let mut properties = original.properties.clone();
        properties.tempo = 180.0;
        let first = ProjectPatch {
            label: "valid edit".into(),
            changes: vec![PatchOp::SetProperties(properties)],
        };
        let second = ProjectPatch {
            label: "invalid audio ownership".into(),
            changes: vec![PatchOp::SetAudioClips(vec![
                OrderedAudioClip {
                    order: 0,
                    clip: valid_clip,
                },
                OrderedAudioClip {
                    order: 1,
                    clip: invalid_clip,
                },
            ])],
        };
        let mut project = original.clone();
        let mut combined = first.clone();
        combined.changes.extend(second.changes.clone());
        assert_eq!(
            apply_patch(&mut project, &combined),
            Err(PatchApplyError::InvalidAudioClips {
                cause: expected.clone()
            })
        );
        assert_eq!(project, original);
        assert_eq!(
            apply_patches(&mut project, [&first, &second]),
            Err(PatchApplyError::InvalidAudioClips { cause: expected })
        );
        assert_eq!(project, original);
        assert_eq!(project.audio_clips().len(), 1);
    }

    #[test]
    fn checked_patch_keeps_contiguous_track_append_and_named_removals_idempotent() {
        let original = named_order_project();
        let mut project = original.clone();
        let patch = ProjectPatch {
            label: "valid intermediate edits".into(),
            changes: vec![
                PatchOp::SetTrack {
                    index: 3,
                    track: make_track("D"),
                },
                PatchOp::RemoveTrack(3),
                PatchOp::RemovePreset("absent".into()),
                PatchOp::RemovePattern("absent".into()),
                PatchOp::RemoveBus("absent".into()),
            ],
        };
        apply_patch(&mut project, &patch).unwrap();
        assert_eq!(project, original);
    }

    #[test]
    fn named_list_history_batch_replays_and_inverts_in_one_atomic_request() {
        let original = named_order_project();
        let mut middle = original.clone();
        middle.presets.rotate_left(1);
        let first = diff(&original, &middle).unwrap().unwrap();
        let mut final_state = middle.clone();
        final_state.patterns.reverse();
        final_state.buses.remove(0);
        let second = diff(&middle, &final_state).unwrap().unwrap();
        let mut state = original.clone();
        apply_patches(&mut state, [&first, &second]).unwrap();
        assert_eq!(state, final_state);
        let undo_second = invert_patch(&middle, &final_state, &second);
        let undo_first = invert_patch(&original, &middle, &first);
        apply_patches(&mut state, [&undo_second, &undo_first]).unwrap();
        assert_eq!(state, original);
    }

    #[test]
    fn named_list_reorders_preserve_valid_legacy_track_patch_wire() {
        let wire = r#"{"label":"Reordered tracks","changes":[{"ReorderTracks":["C","A","B"]}]}"#;
        let patch: ProjectPatch = serde_json::from_str(wire).unwrap();
        let encoded = compressed::encode_patch(&patch).unwrap();
        assert_eq!(&encoded[..5], b"OCPC\x01");
        let patch = compressed::decode_patch(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(&patch).unwrap(),
            serde_json::from_str::<serde_json::Value>(wire).unwrap()
        );
        let mut project = named_order_project();
        apply_patch(&mut project, &patch).unwrap();
        assert_eq!(
            project
                .tracks
                .iter()
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>(),
            vec!["C", "A", "B"]
        );
        assert!(diff(&project, &project.clone()).unwrap().is_none());
    }

    #[test]
    fn test_identical_projects_produce_no_diff() {
        let p = make_project();
        assert!(diff(&p, &p).unwrap().is_none());
    }

    #[test]
    fn test_tempo_change_produces_properties_diff() {
        let old = make_project();
        let mut new = old.clone();
        new.properties.tempo = 140.0;

        let patch = diff(&old, &new).unwrap().unwrap();
        assert_eq!(patch.changes.len(), 1);
        assert!(matches!(&patch.changes[0], PatchOp::SetProperties(p) if p.tempo == 140.0));
        assert!(patch.label.contains("properties"));
    }

    #[test]
    fn test_track_addition() {
        let old = make_project();
        let mut new = old.clone();
        new.tracks.push(make_track("Piano"));

        let patch = diff(&old, &new).unwrap().unwrap();
        assert!(
            patch
                .changes
                .iter()
                .any(|c| matches!(c, PatchOp::SetTrack { track, .. } if track.name == "Piano"))
        );
    }

    #[test]
    fn test_track_removal() {
        let mut old = make_project();
        old.tracks.push(make_track("Piano"));
        let new = make_project();

        let patch = diff(&old, &new).unwrap().unwrap();
        assert!(
            patch
                .changes
                .iter()
                .any(|c| matches!(c, PatchOp::RemoveTrack(0)))
        );
    }

    #[test]
    fn test_track_modification() {
        let mut old = make_project();
        old.tracks.push(make_track("Piano"));
        let mut new = old.clone();
        new.tracks[0].mix.volume_db = -6.0;

        let patch = diff(&old, &new).unwrap().unwrap();
        assert!(patch.changes.iter().any(
            |c| matches!(c, PatchOp::SetTrack { index: 0, track } if track.mix.volume_db == -6.0)
        ));
    }

    #[test]
    fn test_apply_patch_roundtrip() {
        let old = make_project();
        let mut new = old.clone();
        new.properties.tempo = 160.0;
        new.tracks.push(make_track("Drums"));

        let patch = diff(&old, &new).unwrap().unwrap();
        let mut reconstructed = old.clone();
        apply_patch(&mut reconstructed, &patch).unwrap();

        assert_eq!(reconstructed.properties.tempo, 160.0);
        assert_eq!(reconstructed.tracks.len(), 1);
        assert_eq!(reconstructed.tracks[0].name, "Drums");
    }

    #[test]
    fn test_invert_patch_restores_old_state() {
        let old = make_project();
        let mut new = old.clone();
        new.properties.tempo = 180.0;
        new.tracks.push(make_track("Bass"));

        let patch = diff(&old, &new).unwrap().unwrap();
        let inverse = invert_patch(&old, &new, &patch);

        let mut state = new.clone();
        apply_patch(&mut state, &inverse).unwrap();

        assert_eq!(state.properties.tempo, old.properties.tempo);
        assert_eq!(state.tracks.len(), 0);
    }

    #[test]
    fn mixed_track_edits_restore_order_and_audio_after_compressed_undo() {
        let mut old = crate::parse(
            "@project\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n@track tape | instrument: audio/clip\n",
        )
        .unwrap();
        old.tracks.extend([
            make_track("Drop A"),
            make_track("Keep"),
            make_track("Drop B"),
        ]);
        let mut new = old.clone();
        new.tracks.retain(|track| !track.name.starts_with("Drop "));
        new.tracks.swap(0, 1);
        new.tracks[1].mix.volume_db = -3.0;
        new.tracks.insert(0, make_track("New"));
        let forward = diff(&old, &new).unwrap().unwrap();
        let inverse = invert_patch(&old, &new, &forward);
        let reopen =
            |patch| compressed::decode_patch(&compressed::encode_patch(patch).unwrap()).unwrap();

        let mut state = old.clone();
        apply_patch(&mut state, &reopen(&forward)).unwrap();
        assert_eq!(state, new);
        apply_patch(&mut state, &reopen(&inverse)).unwrap();
        assert_eq!(state, old);
    }

    /// Audio clips live in track pools, so they need their own patch op to
    /// survive a compressed undo round trip.
    #[test]
    fn audio_clip_edits_survive_a_compressed_undo_round_trip() {
        let mut old = crate::parse(
            "@project\n  tempo: 120\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n@track tape | instrument: audio/clip\n",
        )
        .unwrap();
        old.tracks.push(make_track("Keys"));
        let mut new = old.clone();
        new.audio_clip_mut(0).unwrap().gain_db = -6.0;

        let patch = diff(&old, &new)
            .expect("projects serialize")
            .expect("a gain edit is a change");
        assert!(
            patch
                .changes
                .iter()
                .any(|change| matches!(change, PatchOp::SetAudioClips(_))),
            "the patch must carry the clip"
        );
        let encoded = compressed::encode_project(&new).unwrap();
        assert!(compressed::decode_project(&encoded).is_ok());

        let mut state = old.clone();
        apply_patch(&mut state, &patch).unwrap();
        assert_eq!(state, new);

        let inverse = invert_patch(&old, &new, &patch);
        apply_patch(&mut state, &inverse).unwrap();
        assert_eq!(state, old);

        // The textual order of the `@audio_clip` blocks survives both ways.
        assert_eq!(crate::serialize(&state), crate::serialize(&old));
    }

    #[test]
    fn test_compressed_roundtrip() {
        let mut p = make_project();
        p.tracks.push(make_track("Piano"));
        p.tracks.push(make_track("Drums"));
        p.properties.tempo = 145.0;

        let encoded = compressed::encode_project(&p).unwrap();
        let decoded = compressed::decode_project(&encoded).unwrap();

        assert_eq!(p, decoded);
        // Compressed should be smaller than JSON
        let json_size = serde_json::to_vec(&p).unwrap().len();
        assert!(
            encoded.len() < json_size,
            "Compressed {} >= JSON {}",
            encoded.len(),
            json_size
        );
    }

    #[test]
    fn test_patch_compression() {
        let old = make_project();
        let mut new = old.clone();
        new.tracks.push(make_track("Piano"));

        let patch = diff(&old, &new).unwrap().unwrap();
        let encoded = compressed::encode_patch(&patch).unwrap();
        let decoded = compressed::decode_patch(&encoded).unwrap();

        assert_eq!(patch.label, decoded.label);
        assert_eq!(patch.changes.len(), decoded.changes.len());
    }

    #[test]
    fn test_label_generation() {
        let old = make_project();
        let mut new = old.clone();
        new.properties.tempo = 120.0; // same as default, hash won't change
        new.properties.tempo = 200.0;

        let patch = diff(&old, &new).unwrap().unwrap();
        assert!(!patch.label.is_empty());
    }
}
