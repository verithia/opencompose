//! Pair the forward and inverse halves of an edit into typed subtree changes.
//!
//! A [`ProjectPatch`] names the new value of every subtree it writes. The
//! inverse patch names the old value of the same key. Pairing the two gives a
//! consumer both sides of one edit without holding the old and new projects,
//! which is what the undo stack stores.
//!
//! Tracks are keyed by name but removed by index. A removed track's old
//! subtree therefore comes from the inverse `SetTrack`; the order of those
//! inverse additions follows the old declaration order, while forward removals
//! name old indices, so the two are paired after sorting removals by index.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Bus, ClipGroup, Marker, Master, PatternDef, PresetDef, ProjectProperties, Section, Track,
};

use super::{OrderedAudioClip, PatchOp, ProjectHeader, ProjectPatch};

/// One side of a subtree edit: an addition, a removal, or both old and new
/// values of the same key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side<T> {
    Added(T),
    Removed(T),
    Changed(T, T),
}

impl<T> Side<T> {
    /// The value before the edit, when this side carries one.
    pub fn before(&self) -> Option<&T> {
        match self {
            Self::Removed(value) | Self::Changed(value, _) => Some(value),
            Self::Added(_) => None,
        }
    }

    /// The value after the edit, when this side carries one.
    pub fn after(&self) -> Option<&T> {
        match self {
            Self::Added(value) | Self::Changed(_, value) => Some(value),
            Self::Removed(_) => None,
        }
    }
}

macro_rules! subtree_kinds {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        /// One typed subtree edit, with a variant per subtree kind.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub enum SubtreeChange<'a> {
            $($variant(Side<&'a $ty>)),*
        }

        #[derive(Debug, Clone, Copy)]
        enum Node<'a> {
            $($variant(&'a $ty)),*
        }

        fn pair<'a>(
            before: Option<Node<'a>>,
            after: Option<Node<'a>>,
        ) -> Option<SubtreeChange<'a>> {
            match (before, after) {
                $(
                    (None, Some(Node::$variant(after))) => {
                        Some(SubtreeChange::$variant(Side::Added(after)))
                    }
                    (Some(Node::$variant(before)), None) => {
                        Some(SubtreeChange::$variant(Side::Removed(before)))
                    }
                    (Some(Node::$variant(before)), Some(Node::$variant(after))) => {
                        Some(SubtreeChange::$variant(Side::Changed(before, after)))
                    }
                )*
                _ => None,
            }
        }
    };
}

subtree_kinds! {
    Header(ProjectHeader),
    Properties(ProjectProperties),
    Preset(PresetDef),
    Pattern(PatternDef),
    Track(Track),
    Bus(Bus),
    Master(Option<Master>),
    Sections([Section]),
    Markers([Marker]),
    Groups([ClipGroup]),
    AudioClips([OrderedAudioClip]),
}

/// A list whose declaration order changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedList {
    Tracks,
    Presets,
    Patterns,
    Buses,
}

/// The declaration order each side of a reorder recorded.
///
/// A patch writes a reorder only when membership edits do not reproduce the
/// list on their own, so a side that is absent keeps the order implied by the
/// other side's membership edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderChange<'a> {
    /// The old declaration order, when the inverse patch recorded one.
    pub before: Option<&'a [String]>,
    /// The new declaration order, when the forward patch recorded one.
    pub after: Option<&'a [String]>,
}

/// One fact about an edit, borrowed from the two patches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mutation<'a> {
    /// One subtree with the value on each side.
    Subtree(SubtreeChange<'a>),
    /// A declaration list changed order, with the order each side recorded.
    Reordered {
        list: OrderedList,
        order: OrderChange<'a>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key<'a> {
    Header,
    Properties,
    Preset(&'a str),
    Pattern(&'a str),
    Track(&'a str),
    Bus(&'a str),
    Master,
    Sections,
    Markers,
    Groups,
    AudioClips,
}

fn key<'a>(op: &'a PatchOp) -> Option<Key<'a>> {
    match op {
        PatchOp::SetProjectHeader(_) => Some(Key::Header),
        PatchOp::SetProperties(_) => Some(Key::Properties),
        PatchOp::SetPreset(preset) => Some(Key::Preset(&preset.name)),
        PatchOp::RemovePreset(name) => Some(Key::Preset(name)),
        PatchOp::SetPattern(pattern) => Some(Key::Pattern(&pattern.name)),
        PatchOp::RemovePattern(name) => Some(Key::Pattern(name)),
        PatchOp::SetTrack { track, .. } => Some(Key::Track(&track.name)),
        PatchOp::RemoveTrack(_) => None,
        PatchOp::SetBus(bus) => Some(Key::Bus(&bus.name)),
        PatchOp::RemoveBus(name) => Some(Key::Bus(name)),
        PatchOp::SetMaster(_) => Some(Key::Master),
        PatchOp::SetSections(_) => Some(Key::Sections),
        PatchOp::SetMarkers(_) => Some(Key::Markers),
        PatchOp::SetGroups(_) => Some(Key::Groups),
        PatchOp::SetAudioClips(_) => Some(Key::AudioClips),
        PatchOp::ReorderTracks(_)
        | PatchOp::ReorderPresets(_)
        | PatchOp::ReorderPatterns(_)
        | PatchOp::ReorderBuses(_) => None,
    }
}

fn node<'a>(op: &'a PatchOp) -> Option<Node<'a>> {
    match op {
        PatchOp::SetProjectHeader(header) => Some(Node::Header(header)),
        PatchOp::SetProperties(properties) => Some(Node::Properties(properties)),
        PatchOp::SetPreset(preset) => Some(Node::Preset(preset)),
        PatchOp::SetPattern(pattern) => Some(Node::Pattern(pattern)),
        PatchOp::SetTrack { track, .. } => Some(Node::Track(track)),
        PatchOp::SetBus(bus) => Some(Node::Bus(bus)),
        PatchOp::SetMaster(master) => Some(Node::Master(master)),
        PatchOp::SetSections(sections) => Some(Node::Sections(sections)),
        PatchOp::SetMarkers(markers) => Some(Node::Markers(markers)),
        PatchOp::SetGroups(groups) => Some(Node::Groups(groups)),
        PatchOp::SetAudioClips(clips) => Some(Node::AudioClips(clips)),
        _ => None,
    }
}

/// Pair every forward operation with the inverse operation of the same key.
///
/// Removed tracks come first in old declaration order, then the remaining
/// forward operations in patch order, then any reorder that only the inverse
/// recorded. A forward write with no inverse partner is an addition; a forward
/// removal with no inverse partner is skipped because its value is unknown.
pub fn mutations<'a>(forward: &'a ProjectPatch, inverse: &'a ProjectPatch) -> Vec<Mutation<'a>> {
    let forward_track_names: HashSet<&str> = forward
        .changes
        .iter()
        .filter_map(|op| match op {
            PatchOp::SetTrack { track, .. } => Some(track.name.as_str()),
            _ => None,
        })
        .collect();
    let restored_tracks: Vec<&'a Track> = inverse
        .changes
        .iter()
        .filter_map(|op| match op {
            PatchOp::SetTrack { track, .. } => Some(track),
            _ => None,
        })
        .filter(|track| !forward_track_names.contains(track.name.as_str()))
        .collect();

    // Forward removals carry old indices in descending order; sorting them
    // restores the old declaration order that the inverse additions follow.
    let mut removals: Vec<(usize, usize)> = forward
        .changes
        .iter()
        .enumerate()
        .filter_map(|(position, op)| match op {
            PatchOp::RemoveTrack(index) => Some((*index, position)),
            _ => None,
        })
        .collect();
    removals.sort_by_key(|(index, _)| *index);
    let mut restored_by_position: HashMap<usize, &'a Track> = HashMap::new();
    for ((_, position), track) in removals.iter().zip(restored_tracks.iter()) {
        restored_by_position.insert(*position, *track);
    }

    let mut inverse_by_key: HashMap<Key<'a>, Node<'a>> = HashMap::new();
    for op in &inverse.changes {
        if let (Some(key), Some(node)) = (key(op), node(op)) {
            inverse_by_key.entry(key).or_insert(node);
        }
    }

    let mut out = Vec::with_capacity(forward.changes.len());
    // Removals describe in old declaration order even though the patch removes
    // them back-to-front; a later rename pairing compares them by position.
    for (_, position) in &removals {
        if let Some(track) = restored_by_position.get(position) {
            out.push(Mutation::Subtree(SubtreeChange::Track(Side::Removed(
                track,
            ))));
        }
    }
    for op in &forward.changes {
        match op {
            PatchOp::RemoveTrack(_) => {}
            PatchOp::ReorderTracks(names) => out.push(reordered(
                OrderedList::Tracks,
                inverse,
                Some(names.as_slice()),
            )),
            PatchOp::ReorderPresets(names) => out.push(reordered(
                OrderedList::Presets,
                inverse,
                Some(names.as_slice()),
            )),
            PatchOp::ReorderPatterns(names) => out.push(reordered(
                OrderedList::Patterns,
                inverse,
                Some(names.as_slice()),
            )),
            PatchOp::ReorderBuses(names) => out.push(reordered(
                OrderedList::Buses,
                inverse,
                Some(names.as_slice()),
            )),
            _ => {
                let Some(op_key) = key(op) else {
                    continue;
                };
                let before = inverse_by_key.get(&op_key).copied();
                if let Some(change) = pair(before, node(op)) {
                    out.push(Mutation::Subtree(change));
                }
            }
        }
    }
    // A membership edit can need an inverse reorder without a forward one when
    // the added declaration landed at the list's head. The old order is still
    // the reference a rename pairs against.
    for list in [
        OrderedList::Tracks,
        OrderedList::Presets,
        OrderedList::Patterns,
        OrderedList::Buses,
    ] {
        if reorder_names(forward, list).is_none()
            && let Some(before) = reorder_names(inverse, list)
        {
            out.push(Mutation::Reordered {
                list,
                order: OrderChange {
                    before: Some(before),
                    after: None,
                },
            });
        }
    }
    out
}

fn reordered<'a>(
    list: OrderedList,
    inverse: &'a ProjectPatch,
    after: Option<&'a [String]>,
) -> Mutation<'a> {
    Mutation::Reordered {
        list,
        order: OrderChange {
            before: reorder_names(inverse, list),
            after,
        },
    }
}

fn reorder_names(patch: &ProjectPatch, list: OrderedList) -> Option<&[String]> {
    patch.changes.iter().find_map(|op| match (list, op) {
        (OrderedList::Tracks, PatchOp::ReorderTracks(names)) => Some(names.as_slice()),
        (OrderedList::Presets, PatchOp::ReorderPresets(names)) => Some(names.as_slice()),
        (OrderedList::Patterns, PatchOp::ReorderPatterns(names)) => Some(names.as_slice()),
        (OrderedList::Buses, PatchOp::ReorderBuses(names)) => Some(names.as_slice()),
        _ => None,
    })
}
