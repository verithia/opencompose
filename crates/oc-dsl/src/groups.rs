//! Clip groups: membership, identifier allocation and pruning.
//!
//! A group names its members by [`crate::ast::Clip::id`], so membership
//! survives a move. Removing a clip removes it from every group, and a group
//! that drops below [`MIN_GROUP_MEMBERS`] is removed rather than left as a stub.

use std::collections::HashSet;

use crate::ast::{Clip, ClipGroup, MIN_GROUP_MEMBERS, Project};

/// Every clip identifier the project currently uses.
pub fn clip_ids(project: &Project) -> HashSet<&str> {
    project
        .tracks
        .iter()
        .flat_map(|track| track.pattern_clips())
        .filter_map(|clip| clip.id.as_deref())
        .collect()
}

/// The clip a group names, with its track and index inside that track.
pub fn clip_by_id<'a>(project: &'a Project, id: &str) -> Option<(usize, usize, &'a Clip)> {
    project
        .tracks
        .iter()
        .enumerate()
        .find_map(|(track_index, track)| {
            track
                .pattern_clips()
                .enumerate()
                .find(|(_, clip)| clip.id.as_deref() == Some(id))
                .map(|(clip_index, clip)| (track_index, clip_index, clip))
        })
}

/// The group a clip identifier belongs to. A clip belongs to at most one.
pub fn group_of<'a>(project: &'a Project, clip_id: &str) -> Option<&'a ClipGroup> {
    project
        .groups
        .iter()
        .find(|group| group.members.iter().any(|member| member == clip_id))
}

/// Prune members whose clip is gone and drop groups left below the minimum
/// size. An identifier exists exactly while a group names its clip, so an
/// unreferenced one is cleared here too: a project with no groups writes the
/// same bytes it wrote before it ever had one.
pub fn reconcile(project: &mut Project) {
    let ids: HashSet<String> = clip_ids(project).into_iter().map(str::to_string).collect();
    project.groups.retain_mut(|group| {
        let mut seen = HashSet::new();
        group
            .members
            .retain(|member| ids.contains(member) && seen.insert(member.clone()));
        group.members.len() >= MIN_GROUP_MEMBERS
    });
    let referenced: HashSet<&str> = project
        .groups
        .iter()
        .flat_map(|group| group.members.iter().map(String::as_str))
        .collect();
    for clip in project
        .tracks
        .iter_mut()
        .flat_map(|track| track.pattern_clips_mut())
    {
        if clip
            .id
            .as_deref()
            .is_some_and(|id| !referenced.contains(id))
        {
            clip.id = None;
        }
    }
}

/// The next free `c1`, `c2`, … clip identifier.
pub fn next_clip_id(project: &Project) -> String {
    let used = clip_ids(project);
    let mut index = 1_u32;
    loop {
        let candidate = format!("c{index}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        index += 1;
    }
}

/// The next free `g1`, `g2`, … group identifier.
pub fn next_group_id(project: &Project) -> String {
    let mut index = 1_u32;
    loop {
        let candidate = format!("g{index}");
        if !project.groups.iter().any(|group| group.id == candidate) {
            return candidate;
        }
        index += 1;
    }
}

/// Give every named clip without an identifier the next free one.
fn assign_ids(project: &mut Project, wanted: &[(usize, usize)]) -> Vec<String> {
    let mut assigned = Vec::with_capacity(wanted.len());
    for &(track_index, clip_index) in wanted {
        let existing = project
            .tracks
            .get(track_index)
            .and_then(|track| track.pattern_clip(clip_index))
            .and_then(|clip| clip.id.clone());
        if let Some(id) = existing {
            assigned.push(id);
            continue;
        }
        let id = next_clip_id(project);
        if let Some(clip) = project
            .tracks
            .get_mut(track_index)
            .and_then(|track| track.pattern_clip_mut(clip_index))
        {
            clip.id = Some(id.clone());
        }
        assigned.push(id);
    }
    assigned
}

/// Group the clips at `wanted` (track, index) into one new group.
///
/// A clip that already belongs to a group leaves it first; a source group left
/// with fewer than two members is removed. Clips without an identifier get the
/// next free one. Returns the identifier of the new group.
pub fn group_clips(project: &mut Project, wanted: &[(usize, usize)]) -> Result<String, String> {
    if wanted.len() < MIN_GROUP_MEMBERS {
        return Err(format!(
            "a group needs at least {MIN_GROUP_MEMBERS} clips; {} was selected",
            wanted.len()
        ));
    }
    for &(track_index, clip_index) in wanted {
        let present = project
            .tracks
            .get(track_index)
            .is_some_and(|track| clip_index < track.pattern_clip_count());
        if !present {
            return Err(format!(
                "clip {clip_index} on track {track_index} is not in the project"
            ));
        }
    }
    let members = assign_ids(project, wanted);
    for group in &mut project.groups {
        group.members.retain(|member| !members.contains(member));
    }
    let id = next_group_id(project);
    project.groups.push(ClipGroup {
        id: id.clone(),
        members,
    });
    reconcile(project);
    Ok(id)
}

/// Remove the group one of `ids` belongs to. Returns the removed group's id.
pub fn ungroup_clips(project: &mut Project, ids: &[String]) -> Result<String, String> {
    let group_id = ids
        .iter()
        .find_map(|id| group_of(project, id).map(|group| group.id.clone()))
        .ok_or_else(|| "none of the selected clips belongs to a group".to_string())?;
    project.groups.retain(|group| group.id != group_id);
    reconcile(project);
    Ok(group_id)
}
