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

/// A subtree carried by one side of a paired edit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Subtree<'a> {
    /// Authored project name and declared version.
    Header(&'a ProjectHeader),
    /// Musical project properties.
    Properties(&'a ProjectProperties),
    /// A named preset declaration.
    Preset(&'a PresetDef),
    /// A named pattern declaration.
    Pattern(&'a PatternDef),
    /// A track and everything it owns.
    Track(&'a Track),
    /// A named bus.
    Bus(&'a Bus),
    /// The optional master section.
    Master(&'a Option<Master>),
    /// The arrangement sections.
    Sections(&'a [Section]),
    /// Timeline markers.
    Markers(&'a [Marker]),
    /// Clip groups.
    Groups(&'a [ClipGroup]),
    /// Project audio clips in their global order.
    AudioClips(&'a [OrderedAudioClip]),
}

/// The subtree kind a [`Subtree`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtreeKind {
    Header,
    Properties,
    Preset,
    Pattern,
    Track,
    Bus,
    Master,
    Sections,
    Markers,
    Groups,
    AudioClips,
}

impl<'a> Subtree<'a> {
    /// The kind of subtree this value borrows.
    pub fn kind(&self) -> SubtreeKind {
        match self {
            Self::Header(_) => SubtreeKind::Header,
            Self::Properties(_) => SubtreeKind::Properties,
            Self::Preset(_) => SubtreeKind::Preset,
            Self::Pattern(_) => SubtreeKind::Pattern,
            Self::Track(_) => SubtreeKind::Track,
            Self::Bus(_) => SubtreeKind::Bus,
            Self::Master(_) => SubtreeKind::Master,
            Self::Sections(_) => SubtreeKind::Sections,
            Self::Markers(_) => SubtreeKind::Markers,
            Self::Groups(_) => SubtreeKind::Groups,
            Self::AudioClips(_) => SubtreeKind::AudioClips,
        }
    }

    /// The header this subtree borrows, if it is a header.
    pub fn as_header(&self) -> Option<&'a ProjectHeader> {
        match self {
            Self::Header(value) => Some(value),
            _ => None,
        }
    }

    /// The properties this subtree borrows, if it is properties.
    pub fn as_properties(&self) -> Option<&'a ProjectProperties> {
        match self {
            Self::Properties(value) => Some(value),
            _ => None,
        }
    }

    /// The preset this subtree borrows, if it is a preset.
    pub fn as_preset(&self) -> Option<&'a PresetDef> {
        match self {
            Self::Preset(value) => Some(value),
            _ => None,
        }
    }

    /// The pattern this subtree borrows, if it is a pattern.
    pub fn as_pattern(&self) -> Option<&'a PatternDef> {
        match self {
            Self::Pattern(value) => Some(value),
            _ => None,
        }
    }

    /// The track this subtree borrows, if it is a track.
    pub fn as_track(&self) -> Option<&'a Track> {
        match self {
            Self::Track(value) => Some(value),
            _ => None,
        }
    }

    /// The bus this subtree borrows, if it is a bus.
    pub fn as_bus(&self) -> Option<&'a Bus> {
        match self {
            Self::Bus(value) => Some(value),
            _ => None,
        }
    }

    /// The master option this subtree borrows, if it is the master section.
    pub fn as_master(&self) -> Option<&'a Option<Master>> {
        match self {
            Self::Master(value) => Some(value),
            _ => None,
        }
    }

    /// The sections this subtree borrows, if it is the section list.
    pub fn as_sections(&self) -> Option<&'a [Section]> {
        match self {
            Self::Sections(value) => Some(value),
            _ => None,
        }
    }

    /// The markers this subtree borrows, if it is the marker list.
    pub fn as_markers(&self) -> Option<&'a [Marker]> {
        match self {
            Self::Markers(value) => Some(value),
            _ => None,
        }
    }

    /// The groups this subtree borrows, if it is the group list.
    pub fn as_groups(&self) -> Option<&'a [ClipGroup]> {
        match self {
            Self::Groups(value) => Some(value),
            _ => None,
        }
    }

    /// The audio clips this subtree borrows, if it is the audio clip list.
    pub fn as_audio_clips(&self) -> Option<&'a [OrderedAudioClip]> {
        match self {
            Self::AudioClips(value) => Some(value),
            _ => None,
        }
    }
}

/// Two sides of one subtree edit; either side is absent for an addition or a
/// removal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubtreeChange<'a> {
    /// The subtree before the edit, when the old patch carried it.
    pub before: Option<Subtree<'a>>,
    /// The subtree after the edit, when the new patch carried it.
    pub after: Option<Subtree<'a>>,
}

/// A list whose declaration order changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedList {
    Tracks,
    Presets,
    Patterns,
    Buses,
}

/// One fact about an edit, borrowed from the two patches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mutation<'a> {
    /// One subtree with the value on each side.
    Subtree(SubtreeChange<'a>),
    /// A declaration list changed order.
    Reordered(OrderedList),
}

/// Pair every forward operation with the inverse operation of the same key.
///
/// The result follows the forward patch's operation order. Inverse-only
/// reorders from membership bookkeeping produce no entry; only a forward
/// reorder records an [`OrderedList`]. A forward operation with no inverse
/// partner still yields the side that is known, or is skipped when no name or
/// value can be recovered.
pub fn mutations<'a>(forward: &'a ProjectPatch, inverse: &'a ProjectPatch) -> Vec<Mutation<'a>> {
    let forward_track_names: HashSet<&str> = forward
        .changes
        .iter()
        .filter_map(|op| match op {
            PatchOp::SetTrack { track, .. } => Some(track.name.as_str()),
            _ => None,
        })
        .collect();
    let inverse_tracks: Vec<&'a Track> = inverse
        .changes
        .iter()
        .filter_map(|op| match op {
            PatchOp::SetTrack { track, .. } => Some(track),
            _ => None,
        })
        .collect();
    // An inverse SetTrack whose name no forward SetTrack mentions restores a
    // forward-removed track. Inverse tracks arrive in old declaration order.
    let restored_tracks: Vec<&'a Track> = inverse_tracks
        .into_iter()
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

    let mut out = Vec::with_capacity(forward.changes.len());
    for (position, op) in forward.changes.iter().enumerate() {
        match op {
            PatchOp::SetProjectHeader(header) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetProjectHeader(old) => Some(Subtree::Header(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Header(header)),
                }));
            }
            PatchOp::SetProperties(properties) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetProperties(old) => Some(Subtree::Properties(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Properties(properties)),
                }));
            }
            PatchOp::SetPreset(preset) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetPreset(old) if old.name == preset.name => {
                        Some(Subtree::Preset(old))
                    }
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Preset(preset)),
                }));
            }
            PatchOp::RemovePreset(name) => {
                if let Some(before) = find_inverse(inverse, |op| match op {
                    PatchOp::SetPreset(old) if old.name == *name => Some(Subtree::Preset(old)),
                    _ => None,
                }) {
                    out.push(Mutation::Subtree(SubtreeChange {
                        before: Some(before),
                        after: None,
                    }));
                }
            }
            PatchOp::SetPattern(pattern) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetPattern(old) if old.name == pattern.name => {
                        Some(Subtree::Pattern(old))
                    }
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Pattern(pattern)),
                }));
            }
            PatchOp::RemovePattern(name) => {
                if let Some(before) = find_inverse(inverse, |op| match op {
                    PatchOp::SetPattern(old) if old.name == *name => Some(Subtree::Pattern(old)),
                    _ => None,
                }) {
                    out.push(Mutation::Subtree(SubtreeChange {
                        before: Some(before),
                        after: None,
                    }));
                }
            }
            PatchOp::SetTrack { track, .. } => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetTrack { track: old, .. } if old.name == track.name => {
                        Some(Subtree::Track(old))
                    }
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Track(track)),
                }));
            }
            PatchOp::RemoveTrack(_) => {
                if let Some(track) = restored_by_position.get(&position) {
                    out.push(Mutation::Subtree(SubtreeChange {
                        before: Some(Subtree::Track(track)),
                        after: None,
                    }));
                }
            }
            PatchOp::ReorderTracks(_) => out.push(Mutation::Reordered(OrderedList::Tracks)),
            PatchOp::SetBus(bus) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetBus(old) if old.name == bus.name => Some(Subtree::Bus(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Bus(bus)),
                }));
            }
            PatchOp::RemoveBus(name) => {
                if let Some(before) = find_inverse(inverse, |op| match op {
                    PatchOp::SetBus(old) if old.name == *name => Some(Subtree::Bus(old)),
                    _ => None,
                }) {
                    out.push(Mutation::Subtree(SubtreeChange {
                        before: Some(before),
                        after: None,
                    }));
                }
            }
            PatchOp::SetMaster(master) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetMaster(old) => Some(Subtree::Master(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Master(master)),
                }));
            }
            PatchOp::SetSections(sections) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetSections(old) => Some(Subtree::Sections(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Sections(sections)),
                }));
            }
            PatchOp::SetMarkers(markers) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetMarkers(old) => Some(Subtree::Markers(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Markers(markers)),
                }));
            }
            PatchOp::SetGroups(groups) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetGroups(old) => Some(Subtree::Groups(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::Groups(groups)),
                }));
            }
            PatchOp::SetAudioClips(clips) => {
                let before = find_inverse(inverse, |op| match op {
                    PatchOp::SetAudioClips(old) => Some(Subtree::AudioClips(old)),
                    _ => None,
                });
                out.push(Mutation::Subtree(SubtreeChange {
                    before,
                    after: Some(Subtree::AudioClips(clips)),
                }));
            }
            PatchOp::ReorderPresets(_) => out.push(Mutation::Reordered(OrderedList::Presets)),
            PatchOp::ReorderPatterns(_) => out.push(Mutation::Reordered(OrderedList::Patterns)),
            PatchOp::ReorderBuses(_) => out.push(Mutation::Reordered(OrderedList::Buses)),
        }
    }
    out
}

fn find_inverse<'a>(
    inverse: &'a ProjectPatch,
    pick: impl Fn(&'a PatchOp) -> Option<Subtree<'a>>,
) -> Option<Subtree<'a>> {
    inverse.changes.iter().find_map(pick)
}
