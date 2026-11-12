//! Shared source layout for streaming and offline rendering.
//!
//! Each placed or inline pattern owns its instrument state and source inserts.
//! Explicit shared plugin outputs coalesce compatible placements on one owner.
//! Sources sum into the hosting track before its mixer inserts. Preset defaults
//! injected by the editor inherit the host preset, while authored presets and
//! known pattern instruments override the host. Empty tracks retain a source
//! for live input, and performance command streams remain atomic.

mod processing;

#[cfg(feature = "plugin-host")]
pub(crate) use processing::align;
pub use processing::effect_chain;

use std::borrow::Cow;

use oc_dsl::ast::{Effect, PatternDef, Project, Track};

/// Lane layout for one render.
#[derive(Debug)]
pub struct LanePlan<'a> {
    /// Source tracks with only source-owned inserts in `effects`. Parent track
    /// inserts remain on the input project and run after the lanes are summed.
    pub project: Cow<'a, Project>,
    /// Lane index to the hosting track index in the input project.
    pub targets: Vec<usize>,
    /// Whether a hosting track has multiple independent sources.
    pub split: bool,
}

impl LanePlan<'_> {
    /// Project used to build the lane plan.
    pub fn tracks(&self) -> &Project {
        &self.project
    }
}

/// Plan independent source instances using the same rules for every renderer.
/// A placement stays independent even when another placement uses the same
/// pattern and preset. Explicit shared output routing opts compatible placements
/// on the same owner into one source so the shared audio is mixed only once.
pub fn plan(project: &Project) -> Result<LanePlan<'_>, String> {
    validate(project)?;
    let mut tracks = Vec::new();
    let mut targets = Vec::new();
    let mut split = false;
    for (target, track) in project.tracks.iter().enumerate() {
        let start = tracks.len();
        let mut base = track.clone();
        base.effects.clear();
        if track.performance.is_some() {
            tracks.push(base);
            targets.push(target);
            continue;
        }
        base.clips.set_pattern_clips(std::iter::empty());
        base.clips.set_audio_clips(std::iter::empty());
        base.inline_patterns.clear();
        for clip in track.pattern_clips() {
            let pattern = project
                .patterns
                .iter()
                .find(|pattern| pattern.name == clip.pattern)
                .ok_or_else(|| {
                    format!(
                        "Unknown pattern '{}' on track '{}'",
                        clip.pattern, track.name
                    )
                })?;
            let mut lane = pattern_lane(&base, track, pattern);
            lane.clips.set_pattern_clips(std::iter::once(clip.clone()));
            push_source(project, &mut tracks, start, lane)?;
        }
        for pattern in &track.inline_patterns {
            let mut lane = pattern_lane(&base, track, pattern);
            lane.inline_patterns.push(pattern.clone());
            push_source(project, &mut tracks, start, lane)?;
        }
        if !track.phrases.is_empty()
            || !track.patterns.is_empty()
            || (track.pattern_clip_count() == 0 && track.inline_patterns.is_empty())
        {
            push_source(project, &mut tracks, start, base)?;
        }
        targets.extend(std::iter::repeat_n(target, tracks.len() - start));
        split |= tracks.len() - start > 1;
    }
    let project = if tracks == project.tracks {
        Cow::Borrowed(project)
    } else {
        let mut source_project = project.clone();
        source_project.tracks = tracks;
        Cow::Owned(source_project)
    };
    Ok(LanePlan {
        project,
        targets,
        split,
    })
}

/// Whether authored routing requests a shared plugin output instance. This
/// predicate also owns the grouping decision used by the offline plugin host.
pub fn shared_output(project: &Project, track: &Track) -> bool {
    track.output_port.is_some()
        || track.output_pair.is_some()
        || track
            .engine_params
            .as_ref()
            .is_some_and(|params| params.iter().any(|(key, _)| key == "plugin_output_pair"))
        || track
            .preset
            .as_deref()
            .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
            .is_some_and(|preset| {
                preset
                    .params
                    .iter()
                    .any(|(key, _)| key == "plugin_output_pair")
            })
}

fn push_source(
    project: &Project,
    tracks: &mut Vec<Track>,
    start: usize,
    candidate: Track,
) -> Result<(), String> {
    if !shared_output(project, &candidate) {
        tracks.push(candidate);
        return Ok(());
    }
    let existing = tracks[start..]
        .iter()
        .position(|lane| {
            lane.preset == candidate.preset
                && lane.output_port == candidate.output_port
                && lane.output_pair == candidate.output_pair
                && shared_output(project, lane)
        })
        .map(|index| start + index);
    let Some(index) = existing else {
        tracks.push(candidate);
        return Ok(());
    };
    let existing = &mut tracks[index];
    if existing.effects != candidate.effects
        || existing.instrument != candidate.instrument
        || existing.engine != candidate.engine
        || existing.engine_params != candidate.engine_params
        || existing.channel != candidate.channel
        || existing.performance != candidate.performance
    {
        return Err(format!(
            "Unsupported source chain on track '{}': placements sharing preset '{}' and its selected plugin output have different source inserts, instruments or engine settings. Use separate presets for independent instances, or make the source settings identical and move common inserts to the parent track.",
            candidate.name,
            candidate.preset.as_deref().unwrap_or("<none>")
        ));
    }
    for clip in candidate.pattern_clips() {
        existing.push_pattern_clip(clip.clone());
    }
    existing.inline_patterns.extend(candidate.inline_patterns);
    existing.phrases.extend(candidate.phrases);
    existing.patterns.extend(candidate.patterns);
    Ok(())
}

fn pattern_lane(base: &Track, host: &Track, pattern: &PatternDef) -> Track {
    let mut lane = base.clone();
    lane.phrases.clear();
    lane.patterns.clear();
    lane.effects = pattern.effects.clone();
    lane.preset = authored_preset(pattern.preset.as_deref())
        .map(str::to_owned)
        .or_else(|| host.preset.clone());
    let category = pattern.instrument.category.trim();
    if !category.is_empty()
        && !category.eq_ignore_ascii_case("unknown")
        && !category.eq_ignore_ascii_case("audio")
    {
        lane.instrument = pattern.instrument.clone();
    }
    lane
}

/// Ordered source inserts. Parent mixer inserts are deliberately absent.
pub fn effects(project: &Project, lane: &Track) -> Vec<Effect> {
    let mut effects = lane
        .preset
        .as_deref()
        .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
        .map(|preset| preset.effects.clone())
        .unwrap_or_default();
    effects.extend(lane.effects.clone());
    effects
}

/// Refuse missing authored presets before allocating source audio. Injected
/// factory defaults are inherited host choices and are not authored references.
pub fn validate(project: &Project) -> Result<(), String> {
    for pattern in project.patterns.iter().chain(
        project
            .tracks
            .iter()
            .flat_map(|track| &track.inline_patterns),
    ) {
        if let Some(preset) = authored_preset(pattern.preset.as_deref())
            && !project
                .presets
                .iter()
                .any(|declared| declared.name == preset)
        {
            return Err(format!(
                "Pattern '{}' references unknown preset '{}'; no instrument can play its clips",
                pattern.name, preset
            ));
        }
    }
    Ok(())
}

fn authored_preset(preset: Option<&str>) -> Option<&str> {
    preset.filter(|name| !oc_dsl::instruments::is_injected(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(source: &str) -> Project {
        oc_dsl::parse(source).expect("the lane fixture parses")
    }

    const TWO_PRESETS: &str = r#"@project
  tempo: 120
@preset bright
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 880}
@preset dark
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220}
@pattern lead | preset: bright
  events:
    @0 C4 w mf
@pattern bass | preset: dark
  events:
    @0 C2 w mf
@track shared | preset: bright, instrument: synth/lead_square
  clip: lead [1:1 - 2:1]
  clip: bass [2:1 - 3:1]
"#;

    #[test]
    fn a_pattern_preset_becomes_its_own_lane() {
        let project = project(TWO_PRESETS);
        let plan = plan(&project).expect("the lanes plan");
        assert!(plan.split);
        assert_eq!(plan.targets, vec![0, 0]);
        assert_eq!(plan.tracks().tracks.len(), 2);
        assert_eq!(
            plan.tracks().tracks[0].preset.as_deref(),
            Some("bright"),
            "the track's own preset keeps the clips that do not override it"
        );
        assert_eq!(
            plan.tracks().tracks[1].preset.as_deref(),
            Some("dark"),
            "the overriding clip gets the pattern's preset"
        );
        assert_eq!(plan.tracks().tracks[0].pattern_clip_count(), 1);
        assert_eq!(plan.tracks().tracks[1].pattern_clip_count(), 1);
        assert_eq!(
            plan.tracks().tracks[1].pattern_clip(0).unwrap().pattern,
            "bass"
        );
    }

    #[test]
    fn a_project_without_pattern_presets_is_borrowed_unchanged() {
        let project = project(
            r#"@project
@preset one
  source: builtin
@pattern riff
  events:
    @0 C4 w mf
@track keys | preset: one, instrument: synth/lead_square
  clip: riff [1:1 - 2:1]
"#,
        );
        let plan = plan(&project).expect("the lanes plan");
        assert!(!plan.split);
        assert_eq!(plan.targets, vec![0]);
        assert!(matches!(plan.project, Cow::Borrowed(_)));
    }

    #[test]
    fn a_pattern_preset_equal_to_the_track_preset_does_not_split() {
        let project = project(
            r#"@project
@preset one
  source: builtin
@pattern riff | preset: one
  events:
    @0 C4 w mf
@track keys | preset: one, instrument: synth/lead_square
  clip: riff [1:1 - 2:1]
"#,
        );
        let plan = plan(&project).expect("the lanes plan");
        assert!(!plan.split, "an equal preset is not an override");
        assert_eq!(plan.tracks().tracks.len(), 1);
    }

    #[test]
    fn an_unknown_pattern_preset_names_the_pattern_and_the_preset() {
        let project = project(
            r#"@project
@preset one
  source: builtin
@pattern riff | preset: missing_patch
  events:
    @0 C4 w mf
@track keys | preset: one, instrument: synth/lead_square
  clip: riff [1:1 - 2:1]
"#,
        );
        let error = plan(&project).expect_err("an unknown preset must fail");
        assert!(error.contains("riff"), "{error}");
        assert!(error.contains("missing_patch"), "{error}");
    }

    #[test]
    fn an_injected_factory_default_does_not_override_the_track() {
        let project = project(
            r#"@project
@preset oc_default_lead
  source: builtin
@preset oc_default_pad
  source: builtin
@pattern riff | preset: oc_default_pad
  events:
    @0 C4 w mf
@track keys | preset: oc_default_lead, instrument: synth/lead_square
  clip: riff [1:1 - 2:1]
"#,
        );
        let plan = plan(&project).expect("the lanes plan");
        assert!(
            !plan.split,
            "a preset the editor injected must not become an override"
        );
    }

    #[test]
    fn inline_pattern_presets_split_the_same_way() {
        // An inline pattern is not reachable through the text grammar yet, so
        // this builds the AST the editor can hand the renderer.
        let mut project = project(
            r#"@project
@preset bright
  source: builtin
@preset dark
  source: builtin
@track keys | preset: bright, instrument: synth/lead_square
  @phrase line [1:1 - 2:1]
    C4 q mf
"#,
        );
        project.tracks[0].inline_patterns.push(PatternDef {
            name: "stab".into(),
            instrument: oc_dsl::ast::InstrumentPath::new("synth", "lead_square"),
            preset: Some("dark".into()),
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: Vec::new(),
            events: Vec::new(),
            pedals: Vec::new(),
            hairpins: Vec::new(),
            hits: Vec::new(),
            color: None,
        });
        let plan = plan(&project).expect("the lanes plan");
        assert!(plan.split);
        assert_eq!(plan.targets, vec![0, 0]);
        assert_eq!(plan.tracks().tracks[0].inline_patterns.len(), 1);
        assert!(plan.tracks().tracks[1].inline_patterns.is_empty());
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    fn fixture() -> Project {
        oc_dsl::parse("@project\n@preset host\n  source: builtin\n@preset oc_default_pad\n  source: builtin\n@pattern riff\n  events:\n    @0 C4 q mf\n@track keys | preset: host, instrument: piano/acoustic_grand\n  clip: riff [1:1 - 2:1]\n  clip: riff [1:2 - 2:2]\n").unwrap()
    }

    #[test]
    fn repeated_same_preset_placements_keep_independent_sources_and_host_instrument() {
        let project = fixture();
        let plan = plan(&project).unwrap();
        assert_eq!(plan.targets, vec![0, 0]);
        for lane in &plan.tracks().tracks {
            assert_eq!(lane.pattern_clip_count(), 1);
            assert_eq!(lane.instrument, project.tracks[0].instrument);
            assert_eq!(lane.preset.as_deref(), Some("host"));
        }
        assert_ne!(
            plan.tracks().tracks[0].pattern_clip(0).unwrap().time_range,
            plan.tracks().tracks[1].pattern_clip(0).unwrap().time_range
        );
    }

    #[test]
    fn explicit_instrument_overrides_but_injected_preset_inherits_host() {
        let mut project = fixture();
        project.patterns[0].instrument = oc_dsl::ast::InstrumentPath::new("strings", "violin");
        project.patterns[0].preset = Some("oc_default_pad".into());
        let plan = plan(&project).unwrap();
        for lane in &plan.tracks().tracks {
            assert_eq!(lane.instrument, project.patterns[0].instrument);
            assert_eq!(lane.preset.as_deref(), Some("host"));
        }
    }

    #[test]
    fn source_inserts_keep_preset_pattern_and_parent_ownership_distinct() {
        let mut project = fixture();
        let inserts = oc_dsl::parse("@project\n@track fx | instrument: synth/lead\n  effects:\n    - gain: factor=0.5\n    - gain: factor=0.25\n    - gain: factor=0.125\n").unwrap().tracks.remove(0).effects;
        project.presets[0].effects = vec![inserts[0].clone()];
        project.patterns[0].effects = vec![inserts[1].clone()];
        project.tracks[0].effects = vec![inserts[2].clone()];
        let plan = plan(&project).unwrap();
        for lane in &plan.tracks().tracks {
            assert_eq!(lane.effects, vec![inserts[1].clone()]);
            assert_eq!(effects(&project, lane), inserts[..2]);
        }
        assert_eq!(project.tracks[0].effects, vec![inserts[2].clone()]);
    }

    #[test]
    fn empty_sources_and_performance_streams_remain_atomic() {
        let mut project = fixture();
        project.tracks[0].performance =
            Some(oc_dsl::performance::PerformanceTimeline { events: Vec::new() });
        let performance_plan = plan(&project).unwrap();
        assert_eq!(performance_plan.targets, vec![0]);
        assert_eq!(
            performance_plan.tracks().tracks[0].performance,
            project.tracks[0].performance
        );
        assert_eq!(performance_plan.tracks().tracks[0].pattern_clip_count(), 2);
        project.tracks[0].performance = None;
        project.tracks[0]
            .clips
            .set_pattern_clips(std::iter::empty());
        let plan = plan(&project).unwrap();
        assert_eq!(plan.targets, vec![0]);
        assert_eq!(plan.tracks().tracks[0].preset.as_deref(), Some("host"));
    }
}

#[cfg(test)]
mod shared_output_tests {
    use super::*;

    #[test]
    fn shared_owner_merges_compatible_clips_inline_patterns_and_track_notes() {
        let mut project = oc_dsl::parse("@project\n@preset host\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    @0 C4 q mf\n@track keys | preset: host, instrument: piano/acoustic_grand\n  clip: riff [1:1 - 2:1]\n  clip: riff [1:2 - 2:2]\n  @phrase held [1:1 - 2:1]\n    E4 h mf\n").unwrap();
        project.tracks[0].output_pair = Some(0);
        project.tracks[0]
            .inline_patterns
            .push(project.patterns[0].clone());
        let shared = plan(&project).unwrap();
        assert_eq!(shared.targets, vec![0]);
        let lane = &shared.tracks().tracks[0];
        assert_eq!(lane.pattern_clip_count(), 2);
        assert_eq!(lane.inline_patterns.len(), 1);
        assert_eq!(lane.phrases, project.tracks[0].phrases);
    }
}
