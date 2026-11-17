//! Typed descriptions of edits, derived from paired patch subtrees.
//!
//! The GUI and the agent panel localize and format these facts. Nothing here
//! writes English: names, numbers and DSL identifiers are the parameters a
//! template needs. Each fact names one field or one contained item, so a
//! consumer can show a precise sentence instead of a caller-authored label.
//!
//! A description needs only the forward and inverse patches. Positions are
//! quarter-note beats from the project start; turning them into bars is the
//! frontend's job because it holds the current meter.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

pub use crate::ast::Owner;
use crate::ast::{
    AudioClip, AutomationLane, BarBeat, Bus, Clip, ClipGroup, Effect, GainAutomation, KeyMode,
    KeySignature, Marker, Master, MixState, NoteEvent, PatternDef, Phrase, PresetDef,
    ProjectProperties, Section, TimeRange, TimeSignature, Track,
};
use crate::diff::{Mutation, OrderChange, OrderedList, Side, SubtreeChange};
use crate::timing::ProjectClock;

/// Largest number of facts one change set carries.
pub const MAX_CHANGES: usize = 8;

/// Largest number of characters one name or text fact carries.
///
/// The parser does not bound declaration names, so a description clips them
/// before it reaches a UI or a saved conversation.
pub const MAX_FACT_TEXT_CHARS: usize = 200;

/// A span on the quarter-note beat axis shared by project, clips and notes.
///
/// The frontend converts beats to bars with the meter in force, so this type
/// never stores a bar number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatSpan {
    pub start: f64,
    pub end: f64,
}

/// A key signature parameter, spelled as the DSL identifier.
///
/// `root` keeps the note letter and accidental spelling (`C`, `F#`, `Bb`);
/// `mode` is the DSL's `major` or `minor` keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyToken {
    pub root: String,
    pub mode: KeyMode,
}

/// One mix parameter that changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MixParam {
    Volume {
        from: f64,
        to: f64,
    },
    Pan {
        from: f64,
        to: f64,
    },
    StereoBalance {
        from: bool,
        to: bool,
    },
    Mute {
        from: bool,
        to: bool,
    },
    Solo {
        from: bool,
        to: bool,
    },
    PolarityInverted {
        from: bool,
        to: bool,
    },
    Output {
        from: Option<String>,
        to: Option<String>,
    },
    /// The send list changed; the rows carry their own typed values.
    Sends,
    /// Presentation or solo-behaviour overrides changed.
    Overrides,
    LatencyCompensation {
        from: crate::ast::LatencyCompensation,
        to: crate::ast::LatencyCompensation,
    },
    LatencyOffset {
        from: i32,
        to: i32,
    },
    /// The control-source gain ride changed.
    ControlGain,
    SmartDisable {
        from: bool,
        to: bool,
    },
    /// A bus routing target list changed.
    Routes,
}

/// One typed fact about an edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    // Project header and properties.
    ProjectRenamed {
        from: Option<String>,
        to: Option<String>,
    },
    ProjectVersion {
        from: Option<String>,
        to: Option<String>,
    },
    Tempo {
        from: f64,
        to: f64,
    },
    TimeSignature {
        from: TimeSignature,
        to: TimeSignature,
    },
    Key {
        from: Option<KeyToken>,
        to: Option<KeyToken>,
    },
    Swing {
        from: Option<f64>,
        to: Option<f64>,
    },
    TempoMap,
    TimeSignatureMap,
    KeyMap,
    ExportSettings,
    ControlLinks,
    /// Properties changed without a more specific fact.
    ProjectSettings,

    // Tracks.
    TrackAdded {
        track: String,
    },
    TrackRemoved {
        track: String,
    },
    TrackRenamed {
        from: String,
        to: String,
    },
    TracksReordered,
    TrackInstrument {
        track: String,
    },
    TrackMix {
        track: String,
        params: Vec<MixParam>,
    },
    TrackPerformance {
        track: String,
    },
    TrackRouting {
        track: String,
    },
    /// A track field changed without a more specific fact.
    TrackChanged {
        track: String,
    },

    // Effect chains.
    EffectAdded {
        owner: Owner,
        effect: String,
    },
    EffectRemoved {
        owner: Owner,
        effect: String,
    },
    EffectChanged {
        owner: Owner,
        effect: String,
    },
    EffectBypassed {
        owner: Owner,
        effect: String,
        bypassed: bool,
    },
    EffectsReordered {
        owner: Owner,
    },

    // Track content.
    NotesEdited {
        track: String,
        span: Option<BeatSpan>,
        added: u32,
        removed: u32,
        changed: u32,
    },
    ClipsEdited {
        track: String,
        span: Option<BeatSpan>,
    },
    AutomationEdited {
        owner: Owner,
        span: Option<BeatSpan>,
    },

    // Named declarations.
    PresetAdded {
        preset: String,
    },
    PresetRemoved {
        preset: String,
    },
    PresetRenamed {
        from: String,
        to: String,
    },
    PresetChanged {
        preset: String,
    },
    PatternAdded {
        pattern: String,
    },
    PatternRemoved {
        pattern: String,
    },
    PatternRenamed {
        from: String,
        to: String,
    },
    PatternChanged {
        pattern: String,
        span: Option<BeatSpan>,
    },
    BusAdded {
        bus: String,
    },
    BusRemoved {
        bus: String,
    },
    BusRenamed {
        from: String,
        to: String,
    },
    BusMix {
        bus: String,
        params: Vec<MixParam>,
    },
    /// A bus changed without a more specific fact.
    BusChanged {
        bus: String,
    },
    MasterChanged,
    PresetsReordered,
    PatternsReordered,
    BusesReordered,

    // Arrangement.
    SectionsEdited {
        span: Option<BeatSpan>,
    },
    MarkersEdited {
        added: u32,
        removed: u32,
        changed: u32,
    },
    GroupsEdited,
    AudioClipsEdited {
        track: Option<String>,
        span: Option<BeatSpan>,
    },
}

/// A bounded list of edit facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub changes: Vec<Change>,
    /// Facts dropped because the list is bounded by [`MAX_CHANGES`].
    pub more: u32,
}

impl ChangeSet {
    /// True when the edit produced no fact at all.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.more == 0
    }
}

/// Describe the edit between a forward and inverse patch.
///
/// `properties` is the current meter, so bar-beat spans use the real meter
/// instead of the default. A subtree change with both sides known pairs one
/// declaration key; renames are a removal plus an addition, never a same-key
/// pair with different names.
pub fn describe(
    forward: &crate::diff::ProjectPatch,
    inverse: &crate::diff::ProjectPatch,
    properties: &ProjectProperties,
) -> ChangeSet {
    let mutations = crate::diff::mutations(forward, inverse);
    let clock = clock_from_properties(properties);
    let context = Context { clock };
    describe_in(&context, &mutations)
}

/// What every fact in one description shares: the project's current meter,
/// which places both sides of an edit on the same bar grid.
struct Context {
    clock: ProjectClock,
}

fn default_clock() -> ProjectClock {
    ProjectClock::new(&ProjectProperties::default())
        .expect("default project properties form a valid clock")
}

fn clock_from_properties(properties: &ProjectProperties) -> ProjectClock {
    ProjectClock::new(properties).unwrap_or_else(|_| default_clock())
}

fn quarter_at(clock: &ProjectClock, position: &BarBeat) -> Option<f64> {
    clock.quarter_notes_at(position).ok()
}

fn range_quarters(clock: &ProjectClock, range: &TimeRange) -> Option<(f64, f64)> {
    Some((
        quarter_at(clock, &range.start)?,
        quarter_at(clock, &range.end)?,
    ))
}

fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[derive(Default)]
struct SpanAccumulator {
    min: Option<f64>,
    max: Option<f64>,
}

impl SpanAccumulator {
    fn add(&mut self, start: f64, end: f64) {
        if !start.is_finite() || !end.is_finite() {
            return;
        }
        let (low, high) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        self.min = Some(self.min.map_or(low, |current| current.min(low)));
        self.max = Some(self.max.map_or(high, |current| current.max(high)));
    }

    fn add_range(&mut self, range: Option<(f64, f64)>) {
        if let Some((start, end)) = range {
            self.add(start, end);
        }
    }

    fn finish(self) -> Option<BeatSpan> {
        Some(BeatSpan {
            start: self.min?,
            end: self.max?,
        })
    }
}

fn describe_in(context: &Context, mutations: &[Mutation]) -> ChangeSet {
    let mut out = Vec::new();
    let mut consumed = vec![false; mutations.len()];
    let tracks = pair_renames::<Track>(
        mutations,
        reorder_of(mutations, OrderedList::Tracks),
        &mut consumed,
    );
    let presets = pair_renames::<PresetDef>(
        mutations,
        reorder_of(mutations, OrderedList::Presets),
        &mut consumed,
    );
    let patterns = pair_renames::<PatternDef>(
        mutations,
        reorder_of(mutations, OrderedList::Patterns),
        &mut consumed,
    );
    let buses = pair_renames::<Bus>(
        mutations,
        reorder_of(mutations, OrderedList::Buses),
        &mut consumed,
    );
    let renames = Renames {
        tracks: tracks
            .iter()
            .map(|(before, after)| (before.name.as_str(), after.name.as_str()))
            .collect(),
        presets: presets
            .iter()
            .map(|(before, after)| (before.name.as_str(), after.name.as_str()))
            .collect(),
        patterns: patterns
            .iter()
            .map(|(before, after)| (before.name.as_str(), after.name.as_str()))
            .collect(),
        buses: buses
            .iter()
            .map(|(before, after)| (before.name.as_str(), after.name.as_str()))
            .collect(),
    };

    for (before, after) in &tracks {
        out.push(before.rename(after));
    }
    for (before, after) in &presets {
        out.push(before.rename(after));
    }
    for (before, after) in &patterns {
        out.push(before.rename(after));
    }
    for (before, after) in &buses {
        out.push(before.rename(after));
    }

    for (index, mutation) in mutations.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if let Mutation::Reordered { list, order } = mutation {
            if common_order_changed(order) {
                out.push(match list {
                    OrderedList::Tracks => Change::TracksReordered,
                    OrderedList::Presets => Change::PresetsReordered,
                    OrderedList::Patterns => Change::PatternsReordered,
                    OrderedList::Buses => Change::BusesReordered,
                });
            }
            continue;
        }
        describe_mutation(context, mutation, &renames, &mut out);
    }

    let more = out.len().saturating_sub(MAX_CHANGES);
    out.truncate(MAX_CHANGES);
    for change in &mut out {
        change.clip(MAX_FACT_TEXT_CHARS);
    }
    ChangeSet {
        changes: out,
        more: count_u32(more),
    }
}

fn clip_text(text: &mut String, max: usize) {
    if text.chars().count() <= max {
        return;
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(3)).collect();
    clipped.push_str("...");
    *text = clipped;
}

fn clip_option(text: &mut Option<String>, max: usize) {
    if let Some(value) = text {
        clip_text(value, max);
    }
}

fn clip_owner(owner: &mut Owner, max: usize) {
    match owner {
        Owner::Track { name }
        | Owner::Bus { name }
        | Owner::Preset { name }
        | Owner::Pattern { name } => clip_text(name, max),
        Owner::Master => {}
    }
}

fn clip_mix_param(param: &mut MixParam, max: usize) {
    if let MixParam::Output { from, to } = param {
        clip_option(from, max);
        clip_option(to, max);
    }
}

impl Change {
    fn clip(&mut self, max: usize) {
        match self {
            Change::ProjectRenamed { from, to } | Change::ProjectVersion { from, to } => {
                clip_option(from, max);
                clip_option(to, max);
            }
            Change::Key { from, to } => {
                if let Some(token) = from {
                    clip_text(&mut token.root, max);
                }
                if let Some(token) = to {
                    clip_text(&mut token.root, max);
                }
            }
            Change::TrackAdded { track }
            | Change::TrackRemoved { track }
            | Change::TrackInstrument { track }
            | Change::TrackPerformance { track }
            | Change::TrackRouting { track }
            | Change::TrackChanged { track }
            | Change::NotesEdited { track, .. }
            | Change::ClipsEdited { track, .. } => clip_text(track, max),
            Change::TrackRenamed { from, to }
            | Change::PresetRenamed { from, to }
            | Change::PatternRenamed { from, to }
            | Change::BusRenamed { from, to } => {
                clip_text(from, max);
                clip_text(to, max);
            }
            Change::TrackMix { track, params } | Change::BusMix { bus: track, params } => {
                clip_text(track, max);
                for param in params {
                    clip_mix_param(param, max);
                }
            }
            Change::EffectAdded { owner, effect }
            | Change::EffectRemoved { owner, effect }
            | Change::EffectChanged { owner, effect }
            | Change::EffectBypassed { owner, effect, .. } => {
                clip_owner(owner, max);
                clip_text(effect, max);
            }
            Change::EffectsReordered { owner } | Change::AutomationEdited { owner, .. } => {
                clip_owner(owner, max)
            }
            Change::PresetAdded { preset }
            | Change::PresetRemoved { preset }
            | Change::PresetChanged { preset } => clip_text(preset, max),
            Change::PatternAdded { pattern }
            | Change::PatternRemoved { pattern }
            | Change::PatternChanged { pattern, .. } => clip_text(pattern, max),
            Change::BusAdded { bus } | Change::BusRemoved { bus } | Change::BusChanged { bus } => {
                clip_text(bus, max)
            }
            Change::AudioClipsEdited { track, .. } => clip_option(track, max),
            Change::Tempo { .. }
            | Change::TimeSignature { .. }
            | Change::Swing { .. }
            | Change::TempoMap
            | Change::TimeSignatureMap
            | Change::KeyMap
            | Change::ExportSettings
            | Change::ControlLinks
            | Change::ProjectSettings
            | Change::TracksReordered
            | Change::PresetsReordered
            | Change::PatternsReordered
            | Change::BusesReordered
            | Change::MasterChanged
            | Change::SectionsEdited { .. }
            | Change::MarkersEdited { .. }
            | Change::GroupsEdited => {}
        }
    }
}

/// A named declaration whose removal and addition can pair as a rename.
trait Declaration<'a>: Clone + PartialEq {
    fn name(&self) -> &str;
    fn renamed(&self, name: &str) -> Self;
    fn same_except_name(&self, after: &Self) -> bool {
        self.name() != after.name() && self.renamed(after.name()) == *after
    }
    fn rename(&self, after: &Self) -> Change;
    fn removed(mutation: &Mutation<'a>) -> Option<&'a Self>;
    fn added(mutation: &Mutation<'a>) -> Option<&'a Self>;
}

macro_rules! declaration {
    ($ty:ty, $variant:ident, $rename:ident) => {
        declaration!($ty, $variant, $rename, |value: &$ty, name: &str| {
            let mut renamed = value.clone();
            renamed.name = name.to_owned();
            renamed
        });
    };
    ($ty:ty, $variant:ident, $rename:ident, $renamed:expr) => {
        impl<'a> Declaration<'a> for $ty {
            fn name(&self) -> &str {
                &self.name
            }

            fn renamed(&self, name: &str) -> Self {
                ($renamed)(self, name)
            }

            fn rename(&self, after: &Self) -> Change {
                Change::$rename {
                    from: self.name.clone(),
                    to: after.name.clone(),
                }
            }

            fn removed(mutation: &Mutation<'a>) -> Option<&'a Self> {
                match mutation {
                    Mutation::Subtree(SubtreeChange::$variant(Side::Removed(value))) => Some(value),
                    _ => None,
                }
            }

            fn added(mutation: &Mutation<'a>) -> Option<&'a Self> {
                match mutation {
                    Mutation::Subtree(SubtreeChange::$variant(Side::Added(value))) => Some(value),
                    _ => None,
                }
            }
        }
    };
}

declaration!(PresetDef, Preset, PresetRenamed);
declaration!(PatternDef, Pattern, PatternRenamed);
declaration!(Bus, Bus, BusRenamed);
// A track's audio clips name the track they sit on, so a rename carries them.
declaration!(Track, Track, TrackRenamed, |track: &Track, name: &str| {
    let mut renamed = track.clone();
    for clip in renamed.clips.audio_clips_mut() {
        if clip.track == track.name {
            clip.track = name.to_owned();
        }
    }
    renamed.name = name.to_owned();
    renamed
});

type DeclarationGap<'a> = (Option<&'a str>, Option<&'a str>);

/// Rebuild the old and new declaration orders from whichever reorder a patch
/// recorded, so membership edits have a position for every changed name.
fn reconstructed_orders<'a, T: Declaration<'a>>(
    order: Option<OrderChange<'a>>,
    removals: &[(usize, &'a T)],
    additions: &[(usize, &'a T)],
) -> (Option<Vec<&'a str>>, Option<Vec<&'a str>>) {
    let mut before: Option<Vec<&'a str>> = order
        .and_then(|order| order.before)
        .map(|names| names.iter().map(String::as_str).collect());
    let mut after: Option<Vec<&'a str>> = order
        .and_then(|order| order.after)
        .map(|names| names.iter().map(String::as_str).collect());
    if before.is_none()
        && let Some(after) = &after
    {
        let added: HashSet<&str> = additions.iter().map(|(_, item)| item.name()).collect();
        let mut rebuilt: Vec<&str> = after
            .iter()
            .copied()
            .filter(|item| !added.contains(item))
            .collect();
        rebuilt.extend(removals.iter().map(|(_, item)| item.name()));
        before = Some(rebuilt);
    }
    if after.is_none()
        && let Some(before) = &before
    {
        let removed: HashSet<&str> = removals.iter().map(|(_, item)| item.name()).collect();
        let mut rebuilt: Vec<&str> = before
            .iter()
            .copied()
            .filter(|item| !removed.contains(item))
            .collect();
        rebuilt.extend(additions.iter().map(|(_, item)| item.name()));
        after = Some(rebuilt);
    }
    (before, after)
}

/// Map every changed name to the surviving declarations on either side of it.
///
/// A rename keeps its place between the same neighbours; a removal and an
/// unrelated addition elsewhere in the list must not pair even when their
/// content matches.
fn gap_keys<'a>(
    names: &[&'a str],
    survivors: &HashSet<&'a str>,
) -> HashMap<&'a str, DeclarationGap<'a>> {
    let mut next = vec![None; names.len()];
    let mut next_survivor = None;
    for index in (0..names.len()).rev() {
        next[index] = next_survivor;
        if survivors.contains(names[index]) {
            next_survivor = Some(names[index]);
        }
    }
    let mut previous = None;
    let mut gaps = HashMap::new();
    for (index, name) in names.iter().enumerate() {
        if survivors.contains(*name) {
            previous = Some(*name);
        } else {
            gaps.insert(*name, (previous, next[index]));
        }
    }
    gaps
}

fn gap_maps<'a>(
    before: &Option<Vec<&'a str>>,
    after: &Option<Vec<&'a str>>,
) -> (
    HashMap<&'a str, DeclarationGap<'a>>,
    HashMap<&'a str, DeclarationGap<'a>>,
) {
    let (Some(before), Some(after)) = (before, after) else {
        return (HashMap::new(), HashMap::new());
    };
    let after_names: HashSet<&str> = after.iter().copied().collect();
    let before_names: HashSet<&str> = before.iter().copied().collect();
    let survivors: HashSet<&str> = before
        .iter()
        .copied()
        .filter(|name| after_names.contains(name))
        .collect();
    debug_assert_eq!(
        survivors.len(),
        after
            .iter()
            .filter(|name| before_names.contains(*name))
            .count(),
    );
    (gap_keys(before, &survivors), gap_keys(after, &survivors))
}

/// Pair renames within the gap between the same surviving declarations, by
/// their rank in that gap.
///
/// The reorder each patch recorded supplies absolute positions; when neither
/// side recorded one, membership edits place removals at the old list's end and
/// additions at the new list's end, and the shared rank is the same position.
fn pair_renames<'a, T: Declaration<'a>>(
    mutations: &[Mutation<'a>],
    order: Option<OrderChange<'a>>,
    consumed: &mut [bool],
) -> Vec<(&'a T, &'a T)> {
    let removals: Vec<(usize, &'a T)> = mutations
        .iter()
        .enumerate()
        .filter_map(|(index, mutation)| T::removed(mutation).map(|item| (index, item)))
        .collect();
    let additions: Vec<(usize, &'a T)> = mutations
        .iter()
        .enumerate()
        .filter_map(|(index, mutation)| T::added(mutation).map(|item| (index, item)))
        .collect();

    let (before, after) = reconstructed_orders(order, &removals, &additions);
    let mut removals = removals;
    if let Some(before) = &before {
        let old_index: HashMap<&str, usize> = before
            .iter()
            .enumerate()
            .map(|(index, name)| (*name, index))
            .collect();
        removals.sort_by_key(|(_, item)| old_index.get(item.name()).copied().unwrap_or(usize::MAX));
    }
    let (removal_gaps, addition_gaps) = gap_maps(&before, &after);

    let mut additions_by_gap: HashMap<DeclarationGap<'_>, Vec<(usize, &'a T)>> = HashMap::new();
    for (index, item) in &additions {
        let gap = addition_gaps
            .get(item.name())
            .copied()
            .unwrap_or((None, None));
        additions_by_gap
            .entry(gap)
            .or_default()
            .push((*index, *item));
    }
    let mut cursors: HashMap<DeclarationGap<'_>, usize> = HashMap::new();
    let mut renames = Vec::new();
    for (removal_index, before_item) in &removals {
        let before_item = *before_item;
        let gap = removal_gaps
            .get(before_item.name())
            .copied()
            .unwrap_or((None, None));
        let rank = {
            let cursor = cursors.entry(gap).or_insert(0);
            let rank = *cursor;
            *cursor += 1;
            rank
        };
        let Some((addition_index, after_item)) = additions_by_gap
            .get(&gap)
            .and_then(|list| list.get(rank))
            .copied()
        else {
            continue;
        };
        if before_item.same_except_name(after_item) {
            consumed[*removal_index] = true;
            consumed[addition_index] = true;
            renames.push((before_item, after_item));
        }
    }
    renames
}

fn reorder_of<'b>(mutations: &[Mutation<'b>], list: OrderedList) -> Option<OrderChange<'b>> {
    mutations.iter().find_map(|mutation| match mutation {
        Mutation::Reordered { list: found, order } if *found == list => Some(*order),
        _ => None,
    })
}

/// Whether two recorded orders disagree about the declarations both contain.
fn common_order_changed(order: &OrderChange<'_>) -> bool {
    let (Some(before), Some(after)) = (order.before, order.after) else {
        return false;
    };
    let after_names: HashSet<&str> = after.iter().map(String::as_str).collect();
    let before_names: HashSet<&str> = before.iter().map(String::as_str).collect();
    let old_common: Vec<&str> = before
        .iter()
        .map(String::as_str)
        .filter(|item| after_names.contains(item))
        .collect();
    let new_common: Vec<&str> = after
        .iter()
        .map(String::as_str)
        .filter(|item| before_names.contains(item))
        .collect();
    old_common != new_common
}

/// The declarations that were renamed in one edit, by kind.
#[derive(Default)]
struct Renames<'a> {
    tracks: BTreeMap<&'a str, &'a str>,
    buses: BTreeMap<&'a str, &'a str>,
    presets: BTreeMap<&'a str, &'a str>,
    patterns: BTreeMap<&'a str, &'a str>,
}

impl Renames<'_> {
    fn is_empty(&self) -> bool {
        self.tracks.is_empty()
            && self.buses.is_empty()
            && self.presets.is_empty()
            && self.patterns.is_empty()
    }
}

fn mapped<'a>(names: &BTreeMap<&'a str, &'a str>, name: &str) -> Option<String> {
    names.get(name).map(|name| (*name).to_owned())
}

fn rename_in(names: &BTreeMap<&str, &str>, value: &mut String) {
    if let Some(name) = mapped(names, value) {
        *value = name;
    }
}

fn rename_optional(names: &BTreeMap<&str, &str>, value: &mut Option<String>) {
    if let Some(name) = value.as_deref().and_then(|name| mapped(names, name)) {
        *value = Some(name);
    }
}

fn renamed_track_or_bus(renames: &Renames<'_>, name: &str) -> Option<String> {
    mapped(&renames.tracks, name).or_else(|| mapped(&renames.buses, name))
}

fn apply_effect_references(effect: &mut Effect, renames: &Renames<'_>) {
    if let Some(source) = &mut effect.sidechain_source
        && let Some(name) = renamed_track_or_bus(renames, source)
    {
        *source = name;
    }
}

fn apply_input_route_references(route: &mut crate::ast::InputRoute, renames: &Renames<'_>) {
    if let Some(name) = renamed_track_or_bus(renames, &route.source) {
        route.source = name;
    }
}

fn apply_pattern_references(pattern: &PatternDef, renames: &Renames<'_>) -> PatternDef {
    let mut next = pattern.clone();
    rename_optional(&renames.presets, &mut next.preset);
    for effect in &mut next.effects {
        apply_effect_references(effect, renames);
    }
    next
}

fn apply_preset_references(preset: &PresetDef, renames: &Renames<'_>) -> PresetDef {
    let mut next = preset.clone();
    rename_optional(&renames.presets, &mut next.extends);
    for effect in &mut next.effects {
        apply_effect_references(effect, renames);
    }
    next
}

fn apply_track_references(track: &Track, renames: &Renames<'_>) -> Track {
    let mut next = track.clone();
    rename_optional(&renames.presets, &mut next.preset);
    rename_optional(&renames.buses, &mut next.mix.output);
    for send in &mut next.mix.sends {
        rename_in(&renames.buses, &mut send.target);
        if let Some(source) = &mut send.source
            && let Some(name) = mapped(&renames.tracks, &source.owner)
                .or_else(|| mapped(&renames.presets, &source.owner))
        {
            source.owner = name;
        }
    }
    for effect in &mut next.effects {
        apply_effect_references(effect, renames);
    }
    for clip in next.clips.pattern_clips_mut() {
        rename_in(&renames.patterns, &mut clip.pattern);
    }
    for clip in next.clips.audio_clips_mut() {
        rename_in(&renames.tracks, &mut clip.track);
    }
    for pattern in &mut next.inline_patterns {
        *pattern = apply_pattern_references(pattern, renames);
    }
    for route in &mut next.input_routes {
        apply_input_route_references(route, renames);
    }
    next
}

fn apply_bus_references(bus: &Bus, renames: &Renames<'_>) -> Bus {
    let mut next = bus.clone();
    for route in &mut next.routes {
        rename_in(&renames.buses, route);
    }
    for effect in &mut next.effects {
        apply_effect_references(effect, renames);
    }
    for route in &mut next.input_routes {
        apply_input_route_references(route, renames);
    }
    next
}

fn apply_master_references(master: &Option<Master>, renames: &Renames<'_>) -> Option<Master> {
    let mut next = master.clone();
    if let Some(master) = &mut next {
        for effect in &mut master.effects {
            apply_effect_references(effect, renames);
        }
        for route in &mut master.input_routes {
            apply_input_route_references(route, renames);
        }
    }
    next
}

fn audio_clip_follows_rename(before: &AudioClip, after: &AudioClip, renames: &Renames<'_>) -> bool {
    let Some(track) = mapped(&renames.tracks, &before.track) else {
        return false;
    };
    if track != after.track {
        return false;
    }
    let mut renamed = before.clone();
    renamed.track = track;
    renamed == *after
}

/// Whether the only difference between the two sides is a reference rewritten
/// by a rename that is already its own fact.
fn reference_only(change: &SubtreeChange<'_>, renames: &Renames<'_>) -> bool {
    if renames.is_empty() {
        return false;
    }
    match *change {
        SubtreeChange::Track(Side::Changed(before, after)) => {
            apply_track_references(before, renames) == *after
        }
        SubtreeChange::Preset(Side::Changed(before, after)) => {
            apply_preset_references(before, renames) == *after
        }
        SubtreeChange::Pattern(Side::Changed(before, after)) => {
            apply_pattern_references(before, renames) == *after
        }
        SubtreeChange::Bus(Side::Changed(before, after)) => {
            apply_bus_references(before, renames) == *after
        }
        SubtreeChange::Master(Side::Changed(before, after)) => {
            apply_master_references(before, renames) == *after
        }
        _ => false,
    }
}

fn describe_mutation(
    context: &Context,
    mutation: &Mutation,
    renames: &Renames<'_>,
    out: &mut Vec<Change>,
) {
    let change = match mutation {
        Mutation::Reordered { .. } => return,
        Mutation::Subtree(change) => *change,
    };
    if reference_only(&change, renames) {
        return;
    }
    match change {
        SubtreeChange::Header(side) => {
            describe_header(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Properties(side) => {
            describe_properties(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Preset(side) => {
            describe_preset(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Pattern(side) => {
            describe_pattern(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Track(side) => {
            describe_track(context, side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Bus(side) => {
            describe_bus(context, side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Master(side) => {
            describe_master(context, side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Sections(side) => {
            describe_sections(context, side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Markers(side) => {
            describe_markers(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::Groups(side) => {
            describe_groups(side.before().copied(), side.after().copied(), out)
        }
        SubtreeChange::AudioClips(side) => describe_audio_clips(
            context,
            side.before().copied(),
            side.after().copied(),
            renames,
            out,
        ),
    }
}

fn describe_header(
    before: Option<&crate::diff::ProjectHeader>,
    after: Option<&crate::diff::ProjectHeader>,
    out: &mut Vec<Change>,
) {
    let before_name = before.and_then(|header| header.name.clone());
    let after_name = after.and_then(|header| header.name.clone());
    if before_name != after_name {
        out.push(Change::ProjectRenamed {
            from: before_name,
            to: after_name,
        });
    }
    let before_version = before.and_then(|header| header.ocp_version.clone());
    let after_version = after.and_then(|header| header.ocp_version.clone());
    if before_version != after_version {
        out.push(Change::ProjectVersion {
            from: before_version,
            to: after_version,
        });
    }
}

fn describe_properties(
    before: Option<&ProjectProperties>,
    after: Option<&ProjectProperties>,
    out: &mut Vec<Change>,
) {
    let (Some(before), Some(after)) = (before, after) else {
        out.push(Change::ProjectSettings);
        return;
    };
    if before.tempo != after.tempo {
        out.push(Change::Tempo {
            from: before.tempo,
            to: after.tempo,
        });
    }
    if before.time_signature != after.time_signature {
        out.push(Change::TimeSignature {
            from: before.time_signature,
            to: after.time_signature,
        });
    }
    if before.key != after.key {
        out.push(Change::Key {
            from: before.key.as_ref().map(key_token),
            to: after.key.as_ref().map(key_token),
        });
    }
    if before.swing != after.swing {
        out.push(Change::Swing {
            from: before.swing,
            to: after.swing,
        });
    }
    if before.tempo_map != after.tempo_map {
        out.push(Change::TempoMap);
    }
    if before.time_sig_map != after.time_sig_map {
        out.push(Change::TimeSignatureMap);
    }
    if before.key_map != after.key_map {
        out.push(Change::KeyMap);
    }
    if before.export != after.export {
        out.push(Change::ExportSettings);
    }
    if before.control_links != after.control_links {
        out.push(Change::ControlLinks);
    }
    let other_changed = before.swing_subdivision != after.swing_subdivision
        || before.solo_mode != after.solo_mode
        || before.picture_clock != after.picture_clock
        || before.vocal_tunes != after.vocal_tunes
        || before.vocal_harmonies != after.vocal_harmonies
        || before.mpe != after.mpe
        || before.live_resample != after.live_resample
        || before.control_sources != after.control_sources
        || before.automation_normalization != after.automation_normalization;
    if other_changed {
        out.push(Change::ProjectSettings);
    }
}

fn key_token(key: &KeySignature) -> KeyToken {
    KeyToken {
        root: format!("{}{}", key.root.as_str(), key.accidental.as_str()),
        mode: key.mode,
    }
}

fn describe_preset(before: Option<&PresetDef>, after: Option<&PresetDef>, out: &mut Vec<Change>) {
    match (before, after) {
        (None, Some(preset)) => out.push(Change::PresetAdded {
            preset: preset.name.clone(),
        }),
        (Some(preset), None) => out.push(Change::PresetRemoved {
            preset: preset.name.clone(),
        }),
        (Some(before), Some(after)) if before != after => {
            out.push(Change::PresetChanged {
                preset: after.name.clone(),
            });
        }
        _ => {}
    }
}

fn describe_pattern(
    before: Option<&PatternDef>,
    after: Option<&PatternDef>,
    out: &mut Vec<Change>,
) {
    match (before, after) {
        (None, Some(pattern)) => out.push(Change::PatternAdded {
            pattern: pattern.name.clone(),
        }),
        (Some(pattern), None) => out.push(Change::PatternRemoved {
            pattern: pattern.name.clone(),
        }),
        (Some(before), Some(after)) if before != after => {
            let content_changed = before.events != after.events || before.hits != after.hits;
            let span = content_changed.then(|| BeatSpan {
                start: 0.0,
                end: before.content_quarters().max(after.content_quarters()),
            });
            out.push(Change::PatternChanged {
                pattern: after.name.clone(),
                span,
            });
        }
        _ => {}
    }
}

fn describe_track(
    context: &Context,
    before: Option<&Track>,
    after: Option<&Track>,
    out: &mut Vec<Change>,
) {
    let (Some(before), Some(after)) = (before, after) else {
        match (before, after) {
            (None, Some(track)) => out.push(Change::TrackAdded {
                track: track.name.clone(),
            }),
            (Some(track), None) => out.push(Change::TrackRemoved {
                track: track.name.clone(),
            }),
            _ => {}
        }
        return;
    };
    if before == after {
        return;
    }
    let start = out.len();
    if before.instrument != after.instrument
        || before.preset != after.preset
        || before.engine != after.engine
        || before.engine_params != after.engine_params
    {
        out.push(Change::TrackInstrument {
            track: after.name.clone(),
        });
    }

    let mix = mix_parameters(&before.mix, &after.mix);
    if !mix.is_empty() {
        out.push(Change::TrackMix {
            track: after.name.clone(),
            params: mix,
        });
    }

    describe_effects(
        Owner::Track {
            name: after.name.clone(),
        },
        &before.effects,
        &after.effects,
        out,
    );

    let before_notes = collect_notes(before, &context.clock);
    let after_notes = collect_notes(after, &context.clock);
    let notes_fact = notes_edited(&after.name, &before_notes, &after_notes);
    let notes_changed = notes_fact.is_some();
    if let Some(fact) = notes_fact {
        out.push(fact);
    }

    describe_clips(context, before, after, out);
    describe_track_automation(context, before, after, out);

    if before.performance != after.performance {
        out.push(Change::TrackPerformance {
            track: after.name.clone(),
        });
    }

    if before.channel != after.channel
        || before.output_port != after.output_port
        || before.output_pair != after.output_pair
        || before.input_routes != after.input_routes
    {
        out.push(Change::TrackRouting {
            track: after.name.clone(),
        });
    }

    let phrase_ranges_changed = before
        .phrases
        .iter()
        .map(|phrase| phrase.time_range)
        .ne(after.phrases.iter().map(|phrase| phrase.time_range));
    let unhandled = before.patterns != after.patterns
        || phrase_containers_differ(&before.phrases, &after.phrases)
        || (phrase_ranges_changed && !notes_changed)
        || inline_containers_differ(&before.inline_patterns, &after.inline_patterns);

    if out.len() == start || unhandled {
        out.push(Change::TrackChanged {
            track: after.name.clone(),
        });
    }
}

fn mix_parameters(before: &MixState, after: &MixState) -> Vec<MixParam> {
    let mut params = Vec::new();
    if before.output != after.output {
        params.push(MixParam::Output {
            from: before.output.clone(),
            to: after.output.clone(),
        });
    }
    if before.volume_db != after.volume_db {
        params.push(MixParam::Volume {
            from: before.volume_db,
            to: after.volume_db,
        });
    }
    if before.pan != after.pan {
        params.push(MixParam::Pan {
            from: before.pan,
            to: after.pan,
        });
    }
    if before.stereo_balance != after.stereo_balance {
        params.push(MixParam::StereoBalance {
            from: before.stereo_balance,
            to: after.stereo_balance,
        });
    }
    if before.sends != after.sends {
        params.push(MixParam::Sends);
    }
    if before.mute != after.mute {
        params.push(MixParam::Mute {
            from: before.mute,
            to: after.mute,
        });
    }
    if before.solo != after.solo {
        params.push(MixParam::Solo {
            from: before.solo,
            to: after.solo,
        });
    }
    if before.polarity_inverted != after.polarity_inverted {
        params.push(MixParam::PolarityInverted {
            from: before.polarity_inverted,
            to: after.polarity_inverted,
        });
    }
    if before.overrides != after.overrides {
        params.push(MixParam::Overrides);
    }
    if before.latency_compensation != after.latency_compensation {
        params.push(MixParam::LatencyCompensation {
            from: before.latency_compensation,
            to: after.latency_compensation,
        });
    }
    if before.latency_offset != after.latency_offset {
        params.push(MixParam::LatencyOffset {
            from: before.latency_offset,
            to: after.latency_offset,
        });
    }
    if before.control_gain != after.control_gain {
        params.push(MixParam::ControlGain);
    }
    if before.smart_disable != after.smart_disable {
        params.push(MixParam::SmartDisable {
            from: before.smart_disable,
            to: after.smart_disable,
        });
    }
    params
}

fn describe_effects(owner: Owner, before: &[Effect], after: &[Effect], out: &mut Vec<Change>) {
    if before == after {
        return;
    }
    let before_tokens: Vec<String> = before
        .iter()
        .map(|effect| effect.effect_type.token())
        .collect();
    let after_tokens: Vec<String> = after
        .iter()
        .map(|effect| effect.effect_type.token())
        .collect();
    let mut used_after = vec![false; after.len()];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut unmatched_before = Vec::new();
    for (before_index, token) in before_tokens.iter().enumerate() {
        if let Some(after_index) = after_tokens
            .iter()
            .enumerate()
            .find(|(index, candidate)| !used_after[*index] && *candidate == token)
            .map(|(index, _)| index)
        {
            used_after[after_index] = true;
            pairs.push((before_index, after_index));
        } else {
            unmatched_before.push(before_index);
        }
    }
    for before_index in unmatched_before {
        out.push(Change::EffectRemoved {
            owner: owner.clone(),
            effect: before_tokens[before_index].clone(),
        });
    }
    for (index, token) in after_tokens.iter().enumerate() {
        if !used_after[index] {
            out.push(Change::EffectAdded {
                owner: owner.clone(),
                effect: token.clone(),
            });
        }
    }
    for (before_index, after_index) in pairs {
        let previous = &before[before_index];
        let current = &after[after_index];
        if previous == current {
            continue;
        }
        let token = after_tokens[after_index].clone();
        if previous.bypass != current.bypass
            && previous.params == current.params
            && previous.effect_type == current.effect_type
            && previous.sidechain_source == current.sidechain_source
        {
            out.push(Change::EffectBypassed {
                owner: owner.clone(),
                effect: token,
                bypassed: current.bypass,
            });
        } else {
            out.push(Change::EffectChanged {
                owner: owner.clone(),
                effect: token,
            });
        }
    }
    let reordered = before.len() == after.len()
        && !used_after.contains(&false)
        && before_tokens.iter().ne(after_tokens.iter());
    if reordered {
        out.push(Change::EffectsReordered { owner });
    }
}

#[derive(Clone, Copy)]
struct NoteRecord<'t> {
    start: f64,
    end: f64,
    note: &'t NoteEvent,
}

fn collect_notes<'t>(track: &'t Track, clock: &ProjectClock) -> Vec<NoteRecord<'t>> {
    let mut records = Vec::new();
    for phrase in &track.phrases {
        let base = phrase
            .time_range
            .as_ref()
            .and_then(|range| quarter_at(clock, &range.start))
            .unwrap_or(0.0);
        for event in &phrase.events {
            let start = base + event.start_beat;
            records.push(NoteRecord {
                start,
                end: start + event.duration.to_beats(),
                note: event,
            });
        }
    }
    for pattern in &track.inline_patterns {
        for event in &pattern.events {
            let start = event.start_beat;
            records.push(NoteRecord {
                start,
                end: start + event.duration.to_beats(),
                note: event,
            });
        }
    }
    records
}

fn note_order(note: &NoteEvent) -> (i16, u64, u8, bool, bool) {
    let velocity = note
        .velocity
        .unwrap_or_else(|| note.dynamics.map(|value| value.to_velocity()).unwrap_or(0));
    (
        note.pitch.semitone_number(),
        note.duration.to_beats().to_bits(),
        velocity,
        note.muted,
        note.grace,
    )
}

fn notes_edited(
    track: &str,
    before: &[NoteRecord<'_>],
    after: &[NoteRecord<'_>],
) -> Option<Change> {
    let mut before_groups: BTreeMap<u64, Vec<NoteRecord<'_>>> = BTreeMap::new();
    let mut after_groups: BTreeMap<u64, Vec<NoteRecord<'_>>> = BTreeMap::new();
    for record in before {
        before_groups
            .entry(record.start.to_bits())
            .or_default()
            .push(*record);
    }
    for record in after {
        after_groups
            .entry(record.start.to_bits())
            .or_default()
            .push(*record);
    }
    let starts: BTreeSet<u64> = before_groups
        .keys()
        .chain(after_groups.keys())
        .copied()
        .collect();

    let mut added = 0usize;
    let mut removed = 0usize;
    let mut changed = 0usize;
    let mut span = SpanAccumulator::default();
    for start in starts {
        let mut before_notes = before_groups.remove(&start).unwrap_or_default();
        let mut after_notes = after_groups.remove(&start).unwrap_or_default();
        before_notes.sort_by_key(|record| note_order(record.note));
        after_notes.sort_by_key(|record| note_order(record.note));
        let paired = before_notes.len().min(after_notes.len());
        for index in 0..paired {
            let previous = before_notes[index];
            let current = after_notes[index];
            if previous.note != current.note {
                changed += 1;
                span.add(previous.start, previous.end);
                span.add(current.start, current.end);
            }
        }
        for record in &before_notes[paired..] {
            removed += 1;
            span.add(record.start, record.end);
        }
        for record in &after_notes[paired..] {
            added += 1;
            span.add(record.start, record.end);
        }
    }

    if added + removed + changed == 0 {
        return None;
    }
    Some(Change::NotesEdited {
        track: track.to_string(),
        span: span.finish(),
        added: count_u32(added),
        removed: count_u32(removed),
        changed: count_u32(changed),
    })
}

fn describe_clips(context: &Context, before: &Track, after: &Track, out: &mut Vec<Change>) {
    let before_clips: Vec<&Clip> = before.pattern_clips().collect();
    let after_clips: Vec<&Clip> = after.pattern_clips().collect();
    if before_clips == after_clips {
        return;
    }
    let mut span = SpanAccumulator::default();
    let count = before_clips.len().max(after_clips.len());
    for index in 0..count {
        let previous = before_clips.get(index);
        let current = after_clips.get(index);
        if previous == current {
            continue;
        }
        if let Some(clip) = previous {
            span.add_range(range_quarters(&context.clock, &clip.time_range));
        }
        if let Some(clip) = current {
            span.add_range(range_quarters(&context.clock, &clip.time_range));
        }
    }
    out.push(Change::ClipsEdited {
        track: after.name.clone(),
        span: span.finish(),
    });
}

fn describe_track_automation(
    context: &Context,
    before: &Track,
    after: &Track,
    out: &mut Vec<Change>,
) {
    let before_lanes: Vec<&AutomationLane> = before.automation().collect();
    let after_lanes: Vec<&AutomationLane> = after.automation().collect();
    if before_lanes == after_lanes && before.gain_automation == after.gain_automation {
        return;
    }
    let span = automation_span(
        &before_lanes,
        &after_lanes,
        &before.gain_automation,
        &after.gain_automation,
        &context.clock,
    );
    out.push(Change::AutomationEdited {
        owner: Owner::Track {
            name: after.name.clone(),
        },
        span,
    });
}

fn automation_span(
    before_lanes: &[&AutomationLane],
    after_lanes: &[&AutomationLane],
    before_gain: &[GainAutomation],
    after_gain: &[GainAutomation],
    clock: &ProjectClock,
) -> Option<BeatSpan> {
    let mut span = SpanAccumulator::default();
    let count = before_lanes.len().max(after_lanes.len());
    for index in 0..count {
        let previous = before_lanes.get(index);
        let current = after_lanes.get(index);
        if previous == current {
            continue;
        }
        if let Some(lane) = previous {
            span.add_range(lane_extent(lane, clock));
        }
        if let Some(lane) = current {
            span.add_range(lane_extent(lane, clock));
        }
    }
    let count = before_gain.len().max(after_gain.len());
    for index in 0..count {
        let previous = before_gain.get(index);
        let current = after_gain.get(index);
        if previous == current {
            continue;
        }
        if let Some(clip) = previous {
            span.add_range(range_quarters(clock, &clip.time_range));
        }
        if let Some(clip) = current {
            span.add_range(range_quarters(clock, &clip.time_range));
        }
    }
    span.finish()
}

fn lane_extent(lane: &AutomationLane, clock: &ProjectClock) -> Option<(f64, f64)> {
    if let Some(range) = &lane.time_range {
        return range_quarters(clock, range);
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for point in &lane.points {
        let quarter = if lane.ppq == 0 {
            quarter_at(clock, &point.position)
        } else {
            clock.quarter_notes_at_ppq(&point.position, lane.ppq).ok()
        };
        let quarter = quarter?;
        min = min.min(quarter);
        max = max.max(quarter);
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

fn describe_bus(
    context: &Context,
    before: Option<&Bus>,
    after: Option<&Bus>,
    out: &mut Vec<Change>,
) {
    let (Some(before), Some(after)) = (before, after) else {
        match (before, after) {
            (None, Some(bus)) => out.push(Change::BusAdded {
                bus: bus.name.clone(),
            }),
            (Some(bus), None) => out.push(Change::BusRemoved {
                bus: bus.name.clone(),
            }),
            _ => {}
        }
        return;
    };
    if before == after {
        return;
    }
    let start = out.len();
    let mut mix = Vec::new();
    if before.routes != after.routes {
        mix.push(MixParam::Routes);
    }
    if before.volume_db != after.volume_db {
        mix.push(MixParam::Volume {
            from: before.volume_db,
            to: after.volume_db,
        });
    }
    if before.mute != after.mute {
        mix.push(MixParam::Mute {
            from: before.mute,
            to: after.mute,
        });
    }
    if before.solo != after.solo {
        mix.push(MixParam::Solo {
            from: before.solo,
            to: after.solo,
        });
    }
    if !mix.is_empty() {
        out.push(Change::BusMix {
            bus: after.name.clone(),
            params: mix,
        });
    }

    describe_effects(
        Owner::Bus {
            name: after.name.clone(),
        },
        &before.effects,
        &after.effects,
        out,
    );

    if before.automation != after.automation || before.gain_automation != after.gain_automation {
        let before_lanes: Vec<&AutomationLane> = before.automation.iter().collect();
        let after_lanes: Vec<&AutomationLane> = after.automation.iter().collect();
        let span = automation_span(
            &before_lanes,
            &after_lanes,
            &before.gain_automation,
            &after.gain_automation,
            &context.clock,
        );
        out.push(Change::AutomationEdited {
            owner: Owner::Bus {
                name: after.name.clone(),
            },
            span,
        });
    }

    let unhandled = before.input_routes != after.input_routes;
    if out.len() == start || unhandled {
        out.push(Change::BusChanged {
            bus: after.name.clone(),
        });
    }
}

fn describe_master(
    context: &Context,
    before: Option<&Option<Master>>,
    after: Option<&Option<Master>>,
    out: &mut Vec<Change>,
) {
    let before = before.and_then(|master| master.as_ref());
    let after = after.and_then(|master| master.as_ref());
    let (Some(before), Some(after)) = (before, after) else {
        if before.is_some() != after.is_some() {
            out.push(Change::MasterChanged);
        }
        return;
    };
    if before == after {
        return;
    }
    let start = out.len();

    describe_effects(Owner::Master, &before.effects, &after.effects, out);

    if before.automation != after.automation || before.gain_automation != after.gain_automation {
        let before_lanes: Vec<&AutomationLane> = before.automation.iter().collect();
        let after_lanes: Vec<&AutomationLane> = after.automation.iter().collect();
        let span = automation_span(
            &before_lanes,
            &after_lanes,
            &before.gain_automation,
            &after.gain_automation,
            &context.clock,
        );
        out.push(Change::AutomationEdited {
            owner: Owner::Master,
            span,
        });
    }

    let unhandled = before.loudness_target != after.loudness_target
        || before.volume_db != after.volume_db
        || before.input_routes != after.input_routes;
    if out.len() == start || unhandled {
        out.push(Change::MasterChanged);
    }
}

fn describe_sections(
    context: &Context,
    before: Option<&[Section]>,
    after: Option<&[Section]>,
    out: &mut Vec<Change>,
) {
    let before = before.unwrap_or(&[]);
    let after = after.unwrap_or(&[]);
    if before == after {
        return;
    }
    let mut span = SpanAccumulator::default();
    for section in unmatched_sections(before, after) {
        span.add_range(range_quarters(&context.clock, &section.time_range));
    }
    for section in unmatched_sections(after, before) {
        span.add_range(range_quarters(&context.clock, &section.time_range));
    }
    out.push(Change::SectionsEdited {
        span: span.finish(),
    });
}

fn unmatched_sections<'s>(items: &'s [Section], other: &[Section]) -> Vec<&'s Section> {
    let mut used = vec![false; other.len()];
    let mut unmatched = Vec::new();
    for item in items {
        if let Some(index) = other
            .iter()
            .enumerate()
            .position(|(index, candidate)| !used[index] && candidate == item)
        {
            used[index] = true;
        } else {
            unmatched.push(item);
        }
    }
    unmatched
}

fn describe_markers(before: Option<&[Marker]>, after: Option<&[Marker]>, out: &mut Vec<Change>) {
    let before = before.unwrap_or(&[]);
    let after = after.unwrap_or(&[]);
    if before == after {
        return;
    }
    let mut used_after = vec![false; after.len()];
    let mut removed_markers = Vec::new();
    for marker in before {
        if let Some(index) = after
            .iter()
            .enumerate()
            .position(|(index, candidate)| !used_after[index] && candidate == marker)
        {
            used_after[index] = true;
        } else {
            removed_markers.push(marker);
        }
    }
    let added_candidates: Vec<&Marker> = after
        .iter()
        .enumerate()
        .filter(|(index, _)| !used_after[*index])
        .map(|(_, marker)| marker)
        .collect();
    let mut used_candidates = vec![false; added_candidates.len()];
    let mut removed = 0usize;
    let mut changed = 0usize;
    for marker in removed_markers {
        if let Some(index) = added_candidates
            .iter()
            .enumerate()
            .position(|(index, candidate)| !used_candidates[index] && candidate.name == marker.name)
        {
            used_candidates[index] = true;
            changed += 1;
        } else {
            removed += 1;
        }
    }
    let added = used_candidates.iter().filter(|used| !**used).count();
    if added + removed + changed > 0 {
        out.push(Change::MarkersEdited {
            added: count_u32(added),
            removed: count_u32(removed),
            changed: count_u32(changed),
        });
    }
}

fn describe_groups(
    before: Option<&[ClipGroup]>,
    after: Option<&[ClipGroup]>,
    out: &mut Vec<Change>,
) {
    if before.unwrap_or(&[]) != after.unwrap_or(&[]) {
        out.push(Change::GroupsEdited);
    }
}

fn describe_audio_clips(
    context: &Context,
    before: Option<&[crate::diff::OrderedAudioClip]>,
    after: Option<&[crate::diff::OrderedAudioClip]>,
    renames: &Renames<'_>,
    out: &mut Vec<Change>,
) {
    let before = before.unwrap_or(&[]);
    let after = after.unwrap_or(&[]);
    let before_map: BTreeMap<usize, &AudioClip> = before
        .iter()
        .map(|entry| (entry.order, &entry.clip))
        .collect();
    let after_map: BTreeMap<usize, &AudioClip> = after
        .iter()
        .map(|entry| (entry.order, &entry.clip))
        .collect();
    let orders: BTreeSet<usize> = before_map.keys().chain(after_map.keys()).copied().collect();
    for order in orders {
        let previous = before_map.get(&order).copied();
        let current = after_map.get(&order).copied();
        if previous == current {
            continue;
        }
        // A renamed track rewrites every clip that points at it. The rename is
        // already one fact, so the clip rows that only follow it add nothing.
        if let (Some(previous), Some(current)) = (previous, current)
            && audio_clip_follows_rename(previous, current, renames)
        {
            continue;
        }
        let mut span = SpanAccumulator::default();
        if let Some(clip) = previous {
            span.add_range(range_quarters(&context.clock, &clip.time_range));
        }
        if let Some(clip) = current {
            span.add_range(range_quarters(&context.clock, &clip.time_range));
        }
        out.push(Change::AudioClipsEdited {
            track: current.or(previous).map(|clip| clip.track.clone()),
            span: span.finish(),
        });
    }
}

fn phrase_containers_differ(before: &[Phrase], after: &[Phrase]) -> bool {
    if before.len() != after.len() {
        return true;
    }
    before.iter().zip(after).any(|(before, after)| {
        let mut before = before.clone();
        before.events.clear();
        before.time_range = None;
        let mut after = after.clone();
        after.events.clear();
        after.time_range = None;
        before != after
    })
}

fn inline_containers_differ(before: &[PatternDef], after: &[PatternDef]) -> bool {
    if before.len() != after.len() {
        return true;
    }
    before.iter().zip(after).any(|(before, after)| {
        let mut before = before.clone();
        before.events.clear();
        let mut after = after.clone();
        after.events.clear();
        before != after
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_identical_renames_describe_each_rename() {
        const COUNT: usize = 5;
        let mut old = String::from("@project\n");
        let mut new = String::from("@project\n");
        for index in 0..COUNT {
            old.push_str(&format!("@track old{index}\n"));
            new.push_str(&format!("@track new{index}\n"));
        }
        let old = crate::parse(&old).expect("old fixture parses");
        let new = crate::parse(&new).expect("new fixture parses");
        let forward = crate::diff::diff(&old, &new)
            .expect("projects serialize")
            .expect("renames are an edit");
        let inverse = crate::diff::invert_patch(&old, &new, &forward);
        let set = describe(&forward, &inverse, &new.properties);
        assert_eq!(set.changes.len(), COUNT);
        assert!(
            set.changes
                .iter()
                .all(|change| matches!(change, Change::TrackRenamed { .. }))
        );
    }
}
