//! Capture subjects from the same pest nodes used to construct their AST owners.
use super::{Pair, Rule};
use crate::source_map::{
    ByteSpan, ProjectField, ReferenceKind, SourceLimits, SourceMap, SourceMapError, SourceOwner,
    SourcePart, SourceSubject,
};

#[derive(Default)]
struct Fields {
    named: bool,
    bus_routes: usize,
}

pub(super) struct Recorder<'a> {
    source: &'a str,
    limits: SourceLimits,
    map: SourceMap,
    error: Option<SourceMapError>,
}
impl<'a> Recorder<'a> {
    pub(super) fn new(source: &'a str, limits: SourceLimits) -> Result<Self, SourceMapError> {
        Ok(Self {
            source,
            limits,
            map: SourceMap::new(source, limits)?,
            error: None,
        })
    }
    fn insert(&mut self, owner: SourceOwner, part: SourcePart, pair: &Pair<'_>) {
        if self.error.is_some() {
            return;
        }
        let span = pair.as_span();
        let result = ByteSpan::checked(self.source, span.start(), span.end()).and_then(|span| {
            self.map.insert(
                SourceSubject { owner, part },
                span,
                self.limits.max_map_entries,
            )
        });
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
    pub(super) fn version(&mut self, pair: &Pair<'_>) {
        if let Some(value) = pair.clone().into_inner().next() {
            self.insert(SourceOwner::Project, SourcePart::Version, &value);
        }
    }
    pub(super) fn project_header(&mut self, pair: &Pair<'_>) {
        self.insert(SourceOwner::Project, SourcePart::Whole, pair);
        for child in pair.clone().into_inner() {
            match child.as_rule() {
                Rule::string_literal => self.insert(SourceOwner::Project, SourcePart::Name, &child),
                Rule::project_property => {
                    if let Some(property) = child.into_inner().next() {
                        let field = match property.as_rule() {
                            Rule::tempo_prop => ProjectField::Tempo,
                            Rule::time_sig_prop => ProjectField::TimeSignature,
                            Rule::key_prop => ProjectField::Key,
                            Rule::swing_prop => ProjectField::Swing,
                            Rule::solo_mode_prop => ProjectField::SoloMode,
                            Rule::mpe_prop => ProjectField::Mpe,
                            Rule::export_prop => ProjectField::Export,
                            Rule::live_resample_prop => ProjectField::LiveResample,
                            Rule::automation_normalization_prop => {
                                ProjectField::AutomationNormalization
                            }
                            Rule::picture_clock_prop => ProjectField::PictureClock,
                            Rule::tempo_map_prop => ProjectField::TempoMap,
                            Rule::time_sig_map_prop => ProjectField::TimeSignatureMap,
                            Rule::key_map_prop => ProjectField::KeyMap,
                            _ => continue,
                        };
                        self.insert(
                            SourceOwner::Project,
                            SourcePart::ProjectField(field),
                            &property,
                        );
                    }
                }
                _ => {}
            }
        }
    }
    pub(super) fn block(&mut self, owner: SourceOwner, pair: &Pair<'_>) {
        self.insert(owner, SourcePart::Whole, pair);
        self.fields(owner, pair.clone(), &mut Fields::default());
    }
    fn reference(&mut self, owner: SourceOwner, kind: ReferenceKind, pair: Pair<'_>) {
        if let Some(value) = pair.into_inner().next_back() {
            self.insert(
                owner,
                SourcePart::Reference {
                    kind,
                    occurrence: 0,
                },
                &value,
            );
        }
    }
    fn fields(&mut self, owner: SourceOwner, pair: Pair<'_>, state: &mut Fields) {
        for child in pair.into_inner() {
            match child.as_rule() {
                Rule::identifier | Rule::string_literal if !state.named => {
                    self.insert(owner, SourcePart::Name, &child);
                    state.named = true;
                }
                Rule::track_header
                | Rule::bus_header
                | Rule::track_inline_props
                | Rule::track_inline_prop
                | Rule::pattern_inline_props
                | Rule::pattern_inline_prop
                | Rule::preset_content
                | Rule::bus_content
                | Rule::control_source_props
                | Rule::control_source_prop
                | Rule::vocal_tune_selector
                | Rule::vocal_harmony_selector => self.fields(owner, child, state),
                Rule::instrument_prop => {
                    if let Some(value) = child.into_inner().next() {
                        self.insert(owner, SourcePart::Instrument, &value);
                    }
                }
                Rule::preset_ref_prop => self.reference(owner, ReferenceKind::Preset, child),
                Rule::preset_extends_prop => {
                    self.reference(owner, ReferenceKind::ParentPreset, child)
                }
                Rule::audio_clip_track_prop => self.reference(owner, ReferenceKind::Track, child),
                Rule::vocal_tune_reference_prop => {
                    self.reference(owner, ReferenceKind::VocalReference, child)
                }
                Rule::vocal_tune_content => {
                    // Only this top-level reference is in scope; note targets remain unmapped.
                    if let Some(field) = child.into_inner().next()
                        && field.as_rule() == Rule::vocal_tune_reference_prop
                    {
                        self.reference(owner, ReferenceKind::VocalReference, field);
                    }
                }
                Rule::vocal_tune_selector_prop => {
                    let mut fields = child.into_inner();
                    let key = fields.next();
                    let value = fields.next();
                    if let (Some(key), Some(value)) = (key, value) {
                        let kind = match key.as_str() {
                            "clip" => ReferenceKind::Clip,
                            "track" => ReferenceKind::Track,
                            _ => continue,
                        };
                        self.insert(
                            owner,
                            SourcePart::Reference {
                                kind,
                                occurrence: 0,
                            },
                            &value,
                        );
                    }
                }
                Rule::control_from_prop => {
                    if let Some(tap) = child.into_inner().next()
                        && let Some(value) = tap.into_inner().next()
                    {
                        self.insert(
                            owner,
                            SourcePart::Reference {
                                kind: ReferenceKind::ControlTap,
                                occurrence: 0,
                            },
                            &value,
                        );
                    }
                }
                Rule::routes_line | Rule::group_members_prop => {
                    let kind = if child.as_rule() == Rule::routes_line {
                        ReferenceKind::BusRoute
                    } else {
                        ReferenceKind::GroupMember
                    };
                    if let Some(list) = child.into_inner().next() {
                        for (index, value) in list.into_inner().enumerate() {
                            let occurrence = if kind == ReferenceKind::BusRoute {
                                let index = state.bus_routes;
                                state.bus_routes += 1;
                                index
                            } else {
                                index
                            };
                            self.insert(owner, SourcePart::Reference { kind, occurrence }, &value);
                        }
                    }
                }
                Rule::time_range => self.insert(owner, SourcePart::TimeRange, &child),
                _ => {}
            }
        }
    }
    pub(super) fn relocate_audio(&mut self, track: usize) {
        if self.error.is_none()
            && let Err(error) = self.map.relocate_audio(track, self.limits.max_map_entries)
        {
            self.error = Some(error);
        }
    }
    pub(super) fn finish(mut self) -> Result<SourceMap, SourceMapError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.map.finish();
        Ok(self.map)
    }
}

pub(super) fn check_tree(pair: Pair<'_>, limits: SourceLimits) -> Result<(), SourceMapError> {
    fn visit(
        pair: Pair<'_>,
        depth: usize,
        remaining: &mut usize,
        limits: SourceLimits,
    ) -> Result<(), SourceMapError> {
        if depth > limits.max_parse_depth {
            return Err(SourceMapError::Budget("parse depth"));
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or(SourceMapError::Budget("parse nodes"))?;
        for child in pair.into_inner() {
            visit(child, depth + 1, remaining, limits)?;
        }
        Ok(())
    }
    let mut remaining = limits.max_parse_nodes;
    visit(pair, 0, &mut remaining, limits)
}
