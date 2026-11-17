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

/// Pair every forward operation with the inverse operation of the same key.
///
/// The result follows the forward patch's operation order. Reorders carry the
/// names both patches recorded, so a consumer can tell a real reorder from the
/// bookkeeping placement of a removed or added declaration. A forward operation
/// with no inverse partner still yields the side that is known, or is skipped
/// when no name or value can be recovered.
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
    // Removals describe in old declaration order even though the patch removes
    // them back-to-front; a later rename pairing compares them by position.
    for (_, position) in &removals {
        if let Some(track) = restored_by_position.get(position) {
            out.push(Mutation::Subtree(SubtreeChange {
                before: Some(Subtree::Track(track)),
                after: None,
            }));
        }
    }
    for op in &forward.changes {
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
            PatchOp::RemoveTrack(_) => {}
            PatchOp::ReorderTracks(names) => out.push(Mutation::Reordered {
                list: OrderedList::Tracks,
                order: OrderChange {
                    before: reorder_names(inverse, OrderedList::Tracks),
                    after: Some(names.as_slice()),
                },
            }),
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
            PatchOp::ReorderPresets(names) => out.push(Mutation::Reordered {
                list: OrderedList::Presets,
                order: OrderChange {
                    before: reorder_names(inverse, OrderedList::Presets),
                    after: Some(names.as_slice()),
                },
            }),
            PatchOp::ReorderPatterns(names) => out.push(Mutation::Reordered {
                list: OrderedList::Patterns,
                order: OrderChange {
                    before: reorder_names(inverse, OrderedList::Patterns),
                    after: Some(names.as_slice()),
                },
            }),
            PatchOp::ReorderBuses(names) => out.push(Mutation::Reordered {
                list: OrderedList::Buses,
                order: OrderChange {
                    before: reorder_names(inverse, OrderedList::Buses),
                    after: Some(names.as_slice()),
                },
            }),
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

fn reorder_names(patch: &ProjectPatch, list: OrderedList) -> Option<&[String]> {
    patch.changes.iter().find_map(|op| match (list, op) {
        (OrderedList::Tracks, PatchOp::ReorderTracks(names)) => Some(names.as_slice()),
        (OrderedList::Presets, PatchOp::ReorderPresets(names)) => Some(names.as_slice()),
        (OrderedList::Patterns, PatchOp::ReorderPatterns(names)) => Some(names.as_slice()),
        (OrderedList::Buses, PatchOp::ReorderBuses(names)) => Some(names.as_slice()),
        _ => None,
    })
}

fn find_inverse<'a>(
    inverse: &'a ProjectPatch,
    pick: impl Fn(&'a PatchOp) -> Option<Subtree<'a>>,
) -> Option<Subtree<'a>> {
    inverse.changes.iter().find_map(pick)
}
