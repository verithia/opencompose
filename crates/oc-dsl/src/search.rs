//! Case-insensitive name search over a parsed project.
//!
//! Search is a pure read: it never edits the project, and every path it
//! returns is accepted by [`crate::ops::AstPath::parse`] so a caller can pass
//! it straight to the same subtree operations the editing tools use.

use crate::ast::{Effect, Owner, Project};
use crate::ops::{AstPath, ItemRef, PathSegment, TrackRef};
use serde::{Deserialize, Serialize};

/// Longest query the matcher reads, in characters.
pub const MAX_QUERY_CHARS: usize = 200;
/// Default number of hits when the caller does not choose one.
pub const DEFAULT_LIMIT: usize = 20;
/// Most hits one search returns.
pub const MAX_LIMIT: usize = 50;

/// The kind of project object a hit names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Track,
    Preset,
    Pattern,
    Bus,
    Section,
    Marker,
    Effect,
    Phrase,
}

/// One matching project object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// An addressable subtree path, accepted by `AstPath::parse`.
    pub path: String,
    pub kind: SearchKind,
    pub name: String,
    /// The object whose list or chain contains this hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    /// A non-owner label such as a preset patch or section label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What a caller asks the matcher for.
#[derive(Debug, Clone)]
pub struct SearchRequest<'a> {
    pub query: &'a str,
    /// `None` searches every kind.
    pub kinds: Option<&'a [SearchKind]>,
    pub limit: usize,
}

impl<'a> SearchRequest<'a> {
    /// A request with the default limit and every kind.
    pub fn new(query: &'a str) -> Self {
        Self {
            query,
            kinds: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// Search a project by name and label tokens, best match first.
///
/// The result keeps project order between equal scores, so repeated searches
/// over the same project are stable.
pub fn search(project: &Project, request: &SearchRequest<'_>) -> Vec<SearchHit> {
    let query = request.query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return Vec::new();
    }
    let tokens = normalized_tokens(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let limit = request.limit.clamp(1, MAX_LIMIT);
    let mut candidates = Vec::new();
    collect(project, request.kinds, &mut candidates);
    let query = query.to_lowercase();
    let mut scored = Vec::new();
    for candidate in candidates {
        let Some(score) = candidate.score(&tokens, &query) else {
            continue;
        };
        scored.push((score, candidate.hit));
    }
    // The sort is stable, so equal scores stay in project order.
    scored.sort_by_key(|hit| std::cmp::Reverse(hit.0));
    scored.into_iter().take(limit).map(|(_, hit)| hit).collect()
}

struct Candidate {
    hit: SearchHit,
    /// The hit name in lowercase, first in `fields`.
    name_lower: String,
    /// Lowercase name and typed tokens the query is matched against.
    fields: Vec<String>,
}

impl Candidate {
    fn score(&self, tokens: &[String], query: &str) -> Option<u32> {
        let mut total = 0u32;
        for token in tokens {
            let mut best = 0u32;
            for (index, field) in self.fields.iter().enumerate() {
                best = best.max(field_score(field, token, index == 0));
            }
            if best == 0 {
                return None;
            }
            total += best;
        }
        total += whole_name_bonus(&self.name_lower, query);
        Some(total)
    }
}

fn field_score(field: &str, token: &str, is_name: bool) -> u32 {
    if field == token {
        return if is_name { 100 } else { 40 };
    }
    if field.starts_with(token) {
        return if is_name { 80 } else { 30 };
    }
    if field.split_whitespace().any(|word| word.starts_with(token)) {
        return if is_name { 60 } else { 24 };
    }
    if field.contains(token) {
        return if is_name { 40 } else { 16 };
    }
    0
}

fn whole_name_bonus(name: &str, query: &str) -> u32 {
    if name == query {
        30
    } else if name.starts_with(query) {
        20
    } else if name.contains(query) {
        10
    } else {
        0
    }
}

fn normalized_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|token| !token.is_empty())
        .collect()
}

fn collect(project: &Project, kinds: Option<&[SearchKind]>, out: &mut Vec<Candidate>) {
    for (index, track) in project.tracks.iter().enumerate() {
        let track_path = named_segment("track", &track.name, index);
        let owner = Owner::Track {
            name: track.name.clone(),
        };
        push(
            out,
            kinds,
            SearchKind::Track,
            &track_path,
            &track.name,
            None,
            Some(track.instrument.as_path()),
        );
        for (phrase_index, phrase) in track.phrases.iter().enumerate() {
            let path = format!(
                "{track_path}.phrase:{}",
                item_segment("phrase", &phrase.name, phrase_index)
            );
            push(
                out,
                kinds,
                SearchKind::Phrase,
                &path,
                &phrase.name,
                Some(owner.clone()),
                None,
            );
        }
        for (pattern_index, pattern) in track.patterns.iter().enumerate() {
            let path = format!(
                "{track_path}.pattern:{}",
                item_segment("pattern", &pattern.name, pattern_index)
            );
            push(
                out,
                kinds,
                SearchKind::Pattern,
                &path,
                &pattern.name,
                Some(owner.clone()),
                None,
            );
        }
        for (effect_index, effect) in track.effects.iter().enumerate() {
            push_effect(out, kinds, effect, &track_path, effect_index, owner.clone());
        }
    }
    for preset in &project.presets {
        let path = format!("preset:{}", preset.name);
        if single_named_segment(&path, &preset.name) {
            push(
                out,
                kinds,
                SearchKind::Preset,
                &path,
                &preset.name,
                None,
                preset_detail(preset),
            );
            let owner = Owner::Preset {
                name: preset.name.clone(),
            };
            for (effect_index, effect) in preset.effects.iter().enumerate() {
                push_effect(out, kinds, effect, &path, effect_index, owner.clone());
            }
        }
    }
    for (index, pattern) in project.patterns.iter().enumerate() {
        let path = format!("pattern:{}", item_segment("pattern", &pattern.name, index));
        push(
            out,
            kinds,
            SearchKind::Pattern,
            &path,
            &pattern.name,
            None,
            Some(pattern.instrument.as_path()),
        );
        let owner = Owner::Pattern {
            name: pattern.name.clone(),
        };
        for (effect_index, effect) in pattern.effects.iter().enumerate() {
            push_effect(out, kinds, effect, &path, effect_index, owner.clone());
        }
    }
    for bus in &project.buses {
        let path = format!("bus:{}", bus.name);
        if single_named_segment(&path, &bus.name) {
            push(out, kinds, SearchKind::Bus, &path, &bus.name, None, None);
            let owner = Owner::Bus {
                name: bus.name.clone(),
            };
            for (effect_index, effect) in bus.effects.iter().enumerate() {
                push_effect(out, kinds, effect, &path, effect_index, owner.clone());
            }
        }
    }
    if let Some(master) = &project.master {
        for (effect_index, effect) in master.effects.iter().enumerate() {
            push_effect(out, kinds, effect, "master", effect_index, Owner::Master);
        }
    }
    for (index, section) in project.sections.iter().enumerate() {
        let path = format!("section:{}", item_segment("section", &section.name, index));
        push(
            out,
            kinds,
            SearchKind::Section,
            &path,
            &section.name,
            None,
            section.label.clone(),
        );
    }
    for (index, marker) in project.markers.iter().enumerate() {
        let path = format!("marker:{}", item_segment("marker", &marker.name, index));
        push(
            out,
            kinds,
            SearchKind::Marker,
            &path,
            &marker.name,
            None,
            Some(format!("{}:{}", marker.position.bar, marker.position.beat)),
        );
    }
}

fn preset_detail(preset: &crate::ast::PresetDef) -> Option<String> {
    preset
        .patch
        .clone()
        .or_else(|| preset.plugin.clone())
        .or_else(|| preset.source.clone())
}

fn push(
    out: &mut Vec<Candidate>,
    kinds: Option<&[SearchKind]>,
    kind: SearchKind,
    path: &str,
    name: &str,
    owner: Option<Owner>,
    detail: Option<String>,
) {
    if kinds.is_some_and(|kinds| !kinds.contains(&kind)) {
        return;
    }
    // A quoted name with a path separator cannot be addressed by `AstPath`;
    // omit the hit rather than return a path that parses to another object.
    let Some(path) = addressable(path.to_owned()) else {
        return;
    };
    let name_lower = name.to_lowercase();
    let mut fields = vec![name_lower.clone()];
    if let Some(owner_name) = owner.as_ref().and_then(Owner::name) {
        fields.push(owner_name.to_lowercase());
    }
    if let Some(detail) = &detail {
        fields.push(detail.to_lowercase());
    }
    out.push(Candidate {
        hit: SearchHit {
            path,
            kind,
            name: name.to_owned(),
            owner,
            detail,
        },
        name_lower,
        fields,
    });
}

fn push_effect(
    out: &mut Vec<Candidate>,
    kinds: Option<&[SearchKind]>,
    effect: &Effect,
    owner_path: &str,
    index: usize,
    owner: Owner,
) {
    let token = effect.effect_type.token();
    let path = format!("{owner_path}.effect:{index}");
    push(
        out,
        kinds,
        SearchKind::Effect,
        &path,
        &token,
        Some(owner),
        None,
    );
}

fn addressable(path: String) -> Option<String> {
    AstPath::parse(&path).ok().map(|_| path)
}

/// A track path prefers the name and falls back to the index when the name
/// cannot be a path segment, such as a quoted name containing a dot.
fn named_segment(prefix: &str, name: &str, index: usize) -> String {
    let named = format!("{prefix}:{name}");
    if single_named_segment(&named, name) {
        named
    } else {
        format!("{prefix}:{index}")
    }
}

fn item_segment(prefix: &str, name: &str, index: usize) -> String {
    if single_named_segment(&format!("{prefix}:{name}"), name) {
        name.to_owned()
    } else {
        index.to_string()
    }
}

/// A name is safe only when parsing preserves its identity as one name,
/// rather than interpreting a number as an index or a dot as a child path.
fn single_named_segment(path: &str, name: &str) -> bool {
    let Ok(path) = AstPath::parse(path) else {
        return false;
    };
    matches!(path.segments.as_slice(),
        [PathSegment::Track(TrackRef::Name(parsed))]
        | [PathSegment::Phrase(ItemRef::Name(parsed))]
        | [PathSegment::Pattern(ItemRef::Name(parsed))]
        | [PathSegment::Section(ItemRef::Name(parsed))]
        | [PathSegment::Marker(ItemRef::Name(parsed))]
        | [PathSegment::Preset(parsed)]
        | [PathSegment::Bus(parsed)] if parsed == name
    )
}
