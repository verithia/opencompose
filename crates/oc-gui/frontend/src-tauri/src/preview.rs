//! Preview an edit as a document.
//!
//! A preview holds two documents: the base the session had when the proposal
//! was made, and the proposal itself. One of them is the live document and the
//! other waits in memory, so a side swap installs an AST that is already parsed
//! and records no undo entry. Accept records the base-to-proposal change as one
//! entry; decline puts the base back and drops the proposal.
//!
//! While a preview is open the document is read-only. [`READ_ONLY`] is the
//! sentence every refused mutation returns, and the panel shows the same text.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use oc_dsl::ast::{self, Project};

/// The refusal every mutation returns while a preview holds the document. The
/// panel shows the same sentence, so a hidden control and a refused call say
/// the same thing.
pub const READ_ONLY: &str = "Finish the preview before editing.";

/// Which document the session is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSide {
    Base,
    Proposal,
}

impl PreviewSide {
    /// Read the wire spelling the panel sends.
    pub fn parse(side: &str) -> Result<Self, String> {
        match side {
            "base" => Ok(Self::Base),
            "proposal" => Ok(Self::Proposal),
            other => Err(format!("unknown preview side: {other}")),
        }
    }
}

/// One changed pattern, as a span of bars inside it.
///
/// `start_bar` and `end_bar` are 1-based and inclusive. A pattern whose hits
/// changed gets its whole written length, because a hit carries no bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewNoteSpan {
    /// Global pattern name, or the pattern's own name for a track-inline one.
    pub pattern: String,
    /// Track whose inline pattern changed; absent for a global pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    pub start_bar: u32,
    pub end_bar: u32,
}

/// One changed clip, identified by the index it holds on each side.
///
/// A clip has no name of its own: its identity is the kind, the pattern or
/// media it names, and where it sits. A clip that moved therefore reads as one
/// removed placement and one added one, and both indices are highlighted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewClipRef {
    /// Track the clip sits on.
    pub track: String,
    /// `pattern`, `audio` or `automation`.
    pub kind: String,
    /// Pattern name, media name or automation parameter.
    pub name: String,
    /// Index among the track's pattern clips or automation lanes, or among the
    /// project's audio clips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_index: Option<usize>,
}

/// One changed effect, identified by its owner and its position in the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewEffectRef {
    /// `track:<name>`, `bus:<name>`, `preset:<name>` or `master`.
    pub target: String,
    pub index: usize,
}

/// Every subtree the proposal changes, by identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewHighlight {
    /// Track names whose subtree differs.
    pub tracks: Vec<String>,
    /// Global pattern names whose subtree differs.
    pub patterns: Vec<String>,
    /// Track-inline patterns whose subtree differs, as `track:name/pattern:name`.
    pub inline_patterns: Vec<String>,
    /// Preset names whose subtree differs.
    pub presets: Vec<String>,
    /// Clips whose placement or content differs.
    pub clips: Vec<PreviewClipRef>,
    /// Effects whose type, parameters, bypass or sidechain differ.
    pub effects: Vec<PreviewEffectRef>,
    /// Changed bars per pattern, from the notes and hits the proposal moves.
    pub notes: Vec<PreviewNoteSpan>,
}

/// What the panel needs to name the side and paint the changed subtrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewState {
    pub side: PreviewSide,
    /// The subtree path the proposal replaces, as `extract_subtree` spells it.
    pub target_path: String,
    pub highlight: PreviewHighlight,
}

/// The two documents a preview holds.
#[derive(Debug)]
pub struct PreviewSession {
    pub base_project: Project,
    pub base_dsl: String,
    pub proposal_project: Project,
    /// The whole proposed document, exactly as Accept will write it.
    pub proposal_dsl: String,
    /// The subtree text the caller proposed, kept so a repeated bring-up of the
    /// same proposal is recognisable before it becomes a document.
    pub subtree_dsl: String,
    /// The side the live document currently shows.
    pub side: PreviewSide,
    pub target_path: String,
    pub highlight: PreviewHighlight,
    /// The dirty flag the base document carried into the preview.
    pub base_dirty: bool,
}

impl PreviewSession {
    /// The read-only view the panel reads.
    pub fn view(&self) -> PreviewState {
        PreviewState {
            side: self.side,
            target_path: self.target_path.clone(),
            highlight: self.highlight.clone(),
        }
    }

    /// The document the session is showing, as its exact DSL text.
    pub fn dsl(&self) -> &str {
        self.dsl_for(self.side)
    }

    /// One side's DSL text, whether or not it is the side on screen.
    pub fn dsl_for(&self, side: PreviewSide) -> &str {
        match side {
            PreviewSide::Base => &self.base_dsl,
            PreviewSide::Proposal => &self.proposal_dsl,
        }
    }

    /// One side's document, whether or not it is the side on screen.
    pub fn project_for(&self, side: PreviewSide) -> &Project {
        match side {
            PreviewSide::Base => &self.base_project,
            PreviewSide::Proposal => &self.proposal_project,
        }
    }
}

/// Compare two documents and return the subtrees that differ.
///
/// Identity is the name a user sees — track, pattern, clip and preset name, and
/// an effect's position in its chain — so a proposal that rewrites one subtree
/// highlights that subtree and nothing else.
pub fn highlight(base: &Project, proposal: &Project) -> PreviewHighlight {
    let mut out = PreviewHighlight::default();

    for track in &proposal.tracks {
        match find_track(&base.tracks, &track.name) {
            Some(old) if old == track => {}
            _ => out.tracks.push(track.name.clone()),
        }
    }
    for track in &base.tracks {
        if find_track(&proposal.tracks, &track.name).is_none() {
            out.tracks.push(track.name.clone());
        }
    }

    for preset in &proposal.presets {
        match base.presets.iter().find(|old| old.name == preset.name) {
            Some(old) if old == preset => {}
            _ => out.presets.push(preset.name.clone()),
        }
    }
    for preset in &base.presets {
        if !proposal.presets.iter().any(|new| new.name == preset.name) {
            out.presets.push(preset.name.clone());
        }
    }

    for pattern in &proposal.patterns {
        match base.patterns.iter().find(|old| old.name == pattern.name) {
            Some(old) if old == pattern => {}
            _ => out.patterns.push(pattern.name.clone()),
        }
    }
    for pattern in &base.patterns {
        if !proposal.patterns.iter().any(|new| new.name == pattern.name) {
            out.patterns.push(pattern.name.clone());
        }
    }

    for track in &proposal.tracks {
        let Some(old) = find_track(&base.tracks, &track.name) else {
            continue;
        };
        for pattern in &track.inline_patterns {
            match old
                .inline_patterns
                .iter()
                .find(|existing| existing.name == pattern.name)
            {
                Some(existing) if existing == pattern => {}
                _ => out
                    .inline_patterns
                    .push(format!("track:{}/pattern:{}", track.name, pattern.name)),
            }
        }
        for pattern in &old.inline_patterns {
            if !track
                .inline_patterns
                .iter()
                .any(|existing| existing.name == pattern.name)
            {
                out.inline_patterns
                    .push(format!("track:{}/pattern:{}", track.name, pattern.name));
            }
        }
    }

    let per_bar = beats_per_bar(proposal);

    // Effects, one chain at a time, so an index names the same slot on both
    // sides unless the proposal inserted or removed a device.
    for (target, new) in effect_chains(proposal) {
        let old = effect_chains(base)
            .into_iter()
            .find(|(name, _)| *name == target)
            .map(|(_, chain)| chain)
            .unwrap_or_default();
        for index in 0..old.len().max(new.len()) {
            match (old.get(index), new.get(index)) {
                (Some(before), Some(after)) if before == after => {}
                (None, None) => {}
                _ => out.effects.push(PreviewEffectRef {
                    target: target.clone(),
                    index,
                }),
            }
        }
    }

    out.clips = changed_clips(base, proposal);
    out.notes = changed_notes(base, proposal, per_bar);

    out.tracks.sort();
    out.tracks.dedup();
    out.patterns.sort();
    out.patterns.dedup();
    out.inline_patterns.sort();
    out.inline_patterns.dedup();
    out.presets.sort();
    out.presets.dedup();
    out.effects
        .sort_by(|a, b| (&a.target, a.index).cmp(&(&b.target, b.index)));
    out.effects.dedup();
    out.clips.sort_by(|a, b| {
        (&a.track, &a.kind, &a.name, a.proposal_index).cmp(&(
            &b.track,
            &b.kind,
            &b.name,
            b.proposal_index,
        ))
    });
    out.clips.dedup();
    out.notes
        .sort_by(|a, b| (&a.pattern, a.start_bar).cmp(&(&b.pattern, b.start_bar)));
    out
}

fn find_track<'a>(tracks: &'a [ast::Track], name: &str) -> Option<&'a ast::Track> {
    tracks.iter().find(|track| track.name == name)
}

fn beats_per_bar(project: &Project) -> f64 {
    let signature = &project.properties.time_signature;
    let numerator = f64::from(signature.numerator.max(1));
    let denominator = f64::from(signature.denominator.max(1));
    (numerator * 4.0 / denominator).max(1.0)
}

/// The bar a pattern-local beat lands in, 1-based.
fn bar_of(beat: f64, per_bar: f64) -> u32 {
    if !beat.is_finite() || beat <= 0.0 {
        return 1;
    }
    (beat / per_bar).floor() as u32 + 1
}

/// Merge 1-based bars into inclusive spans.
fn merge_bars(bars: BTreeSet<u32>) -> Vec<(u32, u32)> {
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for bar in bars {
        match spans.last_mut() {
            Some((_, end)) if *end + 1 == bar => *end = bar,
            Some((_, end)) if *end >= bar => {}
            _ => spans.push((bar, bar)),
        }
    }
    spans
}

/// Every effect chain in a document, keyed the way `extract_subtree` spells it.
fn effect_chains(project: &Project) -> Vec<(String, Vec<ast::Effect>)> {
    let mut chains = Vec::new();
    for track in &project.tracks {
        chains.push((format!("track:{}", track.name), track.effects.clone()));
    }
    for bus in &project.buses {
        chains.push((format!("bus:{}", bus.name), bus.effects.clone()));
    }
    for preset in &project.presets {
        chains.push((format!("preset:{}", preset.name), preset.effects.clone()));
    }
    if let Some(master) = &project.master {
        chains.push(("master".to_string(), master.effects.clone()));
    }
    chains
}

/// One clip's identity inside one track's pool: kind, name and placement.
fn clip_key(payload: &ast::ClipPayload) -> (String, String, u32, u32, u32) {
    match payload {
        ast::ClipPayload::Pattern(clip) => (
            "pattern".to_string(),
            clip.pattern.clone(),
            clip.time_range.start.bar,
            clip.time_range.start.beat,
            clip.time_range.start.subdivision,
        ),
        ast::ClipPayload::Audio(clip) => (
            "audio".to_string(),
            clip.name.clone(),
            clip.time_range.start.bar,
            clip.time_range.start.beat,
            clip.time_range.start.subdivision,
        ),
        ast::ClipPayload::Automation(lane) => (
            "automation".to_string(),
            lane.param.clone(),
            lane.time_range
                .map(|range| range.start.bar)
                .unwrap_or_default(),
            0,
            0,
        ),
    }
}

/// Changed clips, carrying the index each side stores them at.
fn changed_clips(base: &Project, proposal: &Project) -> Vec<PreviewClipRef> {
    let mut out = Vec::new();
    let mut tracks: Vec<&ast::Track> = proposal.tracks.iter().collect();
    for track in &base.tracks {
        if !proposal.tracks.iter().any(|new| new.name == track.name) {
            tracks.push(track);
        }
    }
    for track in tracks {
        let old = find_track(&base.tracks, &track.name);
        let new = find_track(&proposal.tracks, &track.name);
        let mut keys: Vec<(String, String, u32, u32, u32)> = Vec::new();
        if let Some(old) = old {
            keys.extend(old.clips.iter().map(|clip| clip_key(&clip.payload)));
        }
        if let Some(new) = new {
            for clip in new.clips.iter() {
                let key = clip_key(&clip.payload);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        let index_of = |track: Option<&ast::Track>, key: &(String, String, u32, u32, u32)| {
            track.and_then(|track| {
                track
                    .clips
                    .iter()
                    .position(|clip| &clip_key(&clip.payload) == key)
            })
        };
        for key in keys {
            let in_base = index_of(old, &key);
            let in_proposal = index_of(new, &key);
            let changed = match (old, new) {
                (Some(old), Some(new)) => {
                    let before = in_base.and_then(|index| old.clips.iter().nth(index));
                    let after = in_proposal.and_then(|index| new.clips.iter().nth(index));
                    before != after
                }
                _ => true,
            };
            if !changed {
                continue;
            }
            out.push(PreviewClipRef {
                track: track.name.clone(),
                kind: key.0,
                name: key.1,
                base_index: in_base,
                proposal_index: in_proposal,
            });
        }
    }
    out
}

/// The bars whose notes changed, per pattern.
fn changed_notes(base: &Project, proposal: &Project, per_bar: f64) -> Vec<PreviewNoteSpan> {
    let mut out = Vec::new();
    let mut names: Vec<&str> = proposal
        .patterns
        .iter()
        .map(|pattern| pattern.name.as_str())
        .collect();
    for pattern in &base.patterns {
        if !proposal.patterns.iter().any(|new| new.name == pattern.name) {
            names.push(pattern.name.as_str());
        }
    }
    for name in names {
        let old = base.patterns.iter().find(|pattern| pattern.name == name);
        let new = proposal
            .patterns
            .iter()
            .find(|pattern| pattern.name == name);
        let (Some(old), Some(new)) = (old, new) else {
            continue;
        };
        if old == new {
            continue;
        }
        out.extend(pattern_spans(old, new, name, None, per_bar));
    }

    // Inline patterns are reachable from the piano roll by name too, so they
    // highlight with their track attached.
    for track in &proposal.tracks {
        let Some(old_track) = find_track(&base.tracks, &track.name) else {
            continue;
        };
        for pattern in &track.inline_patterns {
            let Some(old) = old_track
                .inline_patterns
                .iter()
                .find(|existing| existing.name == pattern.name)
            else {
                continue;
            };
            if old == pattern {
                continue;
            }
            out.extend(pattern_spans(
                old,
                pattern,
                &pattern.name,
                Some(track.name.clone()),
                per_bar,
            ));
        }
    }
    out
}

fn pattern_spans(
    old: &ast::PatternDef,
    new: &ast::PatternDef,
    name: &str,
    track: Option<String>,
    per_bar: f64,
) -> Vec<PreviewNoteSpan> {
    let mut bars = BTreeSet::new();
    let length = old.events.len().max(new.events.len());
    for index in 0..length {
        match (old.events.get(index), new.events.get(index)) {
            (Some(before), Some(after)) if before == after => {}
            (Some(before), Some(after)) => {
                bars.insert(bar_of(before.start_beat, per_bar));
                bars.insert(bar_of(after.start_beat, per_bar));
            }
            (Some(before), None) => {
                bars.insert(bar_of(before.start_beat, per_bar));
            }
            (None, Some(after)) => {
                bars.insert(bar_of(after.start_beat, per_bar));
            }
            (None, None) => {}
        }
    }
    // A hit has no bar of its own, so a changed hit set marks the whole written
    // pattern instead of guessing where it moved.
    if old.hits != new.hits {
        let quarters = new.content_quarters().max(old.content_quarters());
        let pattern_bars = ((quarters / per_bar).ceil() as u32).max(1);
        bars.insert(1);
        bars.insert(pattern_bars);
    }
    merge_bars(bars)
        .into_iter()
        .map(|(start_bar, end_bar)| PreviewNoteSpan {
            pattern: name.to_string(),
            track: track.clone(),
            start_bar,
            end_bar,
        })
        .collect()
}
