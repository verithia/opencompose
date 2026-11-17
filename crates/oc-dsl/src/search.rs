//! Case-insensitive name search over a parsed project.
//!
//! Search is a pure read: it never edits the project, and every path it
//! returns is accepted by [`crate::ops::AstPath::parse`] so a caller can pass
//! it straight to the same subtree operations the editing tools use.

use crate::ast::{Effect, EffectType, Project};
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
    collect(project, &mut candidates);
    let mut scored = Vec::new();
    for candidate in candidates {
        if request
            .kinds
            .is_some_and(|kinds| !kinds.contains(&candidate.hit.kind))
        {
            continue;
        }
        let Some(score) = candidate.score(&tokens, query) else {
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
    fields: Vec<String>,
}

impl Candidate {
    fn score(&self, tokens: &[String], query: &str) -> Option<u32> {
        let name = self.hit.name.to_lowercase();
        let query = query.to_lowercase();
        let mut total = 0u32;
        for token in tokens {
            let mut best = 0u32;
            for (index, field) in self.fields.iter().enumerate() {
                best = best.max(field_score(&field.to_lowercase(), token, index == 0));
            }
            if best == 0 {
                return None;
            }
            total += best;
        }
        total += whole_name_bonus(&name, &query);
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

fn collect(project: &Project, out: &mut Vec<Candidate>) {
    for (index, track) in project.tracks.iter().enumerate() {
        let track_path = named_segment("track", &track.name, index);
        push(
            out,
            SearchKind::Track,
            &track_path,
            &track.name,
            Some(track.instrument.as_path()),
        );
        for (phrase_index, phrase) in track.phrases.iter().enumerate() {
            let path = format!(
                "{track_path}.phrase:{}",
                item_segment(&phrase.name, phrase_index)
            );
            push(
                out,
                SearchKind::Phrase,
                &path,
                &phrase.name,
                Some(format!("on {}", track.name)),
            );
        }
        for (pattern_index, pattern) in track.patterns.iter().enumerate() {
            let path = format!(
                "{track_path}.pattern:{}",
                item_segment(&pattern.name, pattern_index)
            );
            push(
                out,
                SearchKind::Pattern,
                &path,
                &pattern.name,
                Some(format!("on {}", track.name)),
            );
        }
        for effect in &track.effects {
            push_effect(out, effect, &format!("{track_path}.effects"), &track.name);
        }
    }
    for preset in &project.presets {
        let path = format!("preset:{}", preset.name);
        if single_named_segment(&path, &preset.name) {
            push(
                out,
                SearchKind::Preset,
                &path,
                &preset.name,
                preset_detail(preset),
            );
            for effect in &preset.effects {
                push_effect(out, effect, &path, &preset.name);
            }
        }
    }
    for (index, pattern) in project.patterns.iter().enumerate() {
        let path = format!("pattern:{}", item_segment(&pattern.name, index));
        push(
            out,
            SearchKind::Pattern,
            &path,
            &pattern.name,
            Some(pattern.instrument.as_path()),
        );
        for effect in &pattern.effects {
            push_effect(out, effect, &path, &pattern.name);
        }
    }
    for bus in &project.buses {
        let path = format!("bus:{}", bus.name);
        if single_named_segment(&path, &bus.name) {
            push(out, SearchKind::Bus, &path, &bus.name, None);
            for effect in &bus.effects {
                push_effect(out, effect, &path, &bus.name);
            }
        }
    }
    if let Some(master) = &project.master {
        for effect in &master.effects {
            push_effect(out, effect, "master", "master");
        }
    }
    for section in &project.sections {
        push(
            out,
            SearchKind::Section,
            "sections",
            &section.name,
            section.label.clone(),
        );
    }
    for marker in &project.markers {
        push(
            out,
            SearchKind::Marker,
            "sections",
            &marker.name,
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
    kind: SearchKind,
    path: &str,
    name: &str,
    detail: Option<String>,
) {
    // A quoted name with a path separator cannot be addressed by `AstPath`;
    // omit the hit rather than return a path that parses to another object.
    let Some(path) = addressable(path.to_owned()) else {
        return;
    };
    let mut fields = vec![name.to_owned()];
    if let Some(detail) = &detail {
        fields.push(detail.clone());
    }
    out.push(Candidate {
        hit: SearchHit {
            path,
            kind,
            name: name.to_owned(),
            detail,
        },
        fields,
    });
}

fn push_effect(out: &mut Vec<Candidate>, effect: &Effect, path: &str, owner: &str) {
    let token = match &effect.effect_type {
        EffectType::Plugin(id) => id.clone(),
        builtin => builtin.lane_name().to_owned(),
    };
    push(
        out,
        SearchKind::Effect,
        path,
        &token,
        Some(format!("on {owner}")),
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

fn item_segment(name: &str, index: usize) -> String {
    if single_named_segment(&format!("phrase:{name}"), name) {
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
        | [PathSegment::Preset(parsed)]
        | [PathSegment::Bus(parsed)] if parsed == name
    )
}
