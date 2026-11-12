//! Hairpin expansion: `cresc` and `dim` become gain ramps on the owning track.
//!
//! A mark names no levels of its own. It swells from the dynamic in force at
//! its position to the next written dynamic in the same phrase, and to the
//! phrase end when nothing later is written. Notes between the two marks take
//! the start level; the ramp itself is a post-insert gain clip, so the level
//! moves continuously through them instead of stepping note by note.
//!
//! The ramp is linear in decibels and is written as points at every note onset,
//! because a gain clip interpolates linearly in amplitude between its points.
//! Levels use the sampler's velocity-to-amplitude law, so the ramp reaches the
//! level the target dynamic really sounds at.

use crate::ast::{
    AutomationPoint, BarBeat, GainAutomation, GainInterpolation, HairpinKind, HairpinMark,
    NoteEvent, ParamValue, PatternDef, Phrase, Project, ProjectProperties, TimeRange, Track,
};
use crate::timing::ProjectClock;
use std::borrow::Cow;

/// Velocity of `pp`: the start of a `cresc` with no dynamic before it.
const PP_VELOCITY: u8 = 33;
/// Velocity of `f`: the target of a `cresc` with no dynamic after it.
const F_VELOCITY: u8 = 96;
/// Subdivision ticks per quarter note when a position is off the notated grid.
const FINE_PPQ: u32 = 960;
/// A looped placement repeats at most this many times, matching the engine.
const MAX_OCCURRENCES: usize = 1024;

/// One hairpin's span and the dynamics it moves between, in absolute quarter
/// notes. A sampler reads this to crossfade velocity layers with the ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerRamp {
    pub start: f64,
    pub end: f64,
    pub start_velocity: u8,
    pub target_velocity: u8,
}

/// One hairpin's expansion, before it is merged into the track.
pub struct HairpinExpansion {
    /// Index into `phrase.hairpins`.
    pub mark: usize,
    /// The post-insert gain clip, in project coordinates.
    pub clip: GainAutomation,
    /// Event indices that take the start level, with the velocity they take.
    pub levels: Vec<(usize, u8)>,
    /// The same span as the clip, with the levels it moves between.
    pub ramp: LayerRamp,
}

impl HairpinExpansion {
    /// CC11 values across the mark, at most one per sixteenth note.
    ///
    /// The points are in project quarter notes. A caller emits them only for a
    /// source that reads CC11; a built-in track uses [`Self::clip`] instead.
    pub fn expression_points(&self) -> Vec<(f64, u8)> {
        let span = self.ramp.end - self.ramp.start;
        if !span.is_finite() || span <= 0.0 {
            return Vec::new();
        }
        let parts = ((span / 0.25).ceil() as usize).clamp(1, 4096);
        (0..=parts)
            .map(|index| {
                let fraction = index as f64 / parts as f64;
                (
                    self.ramp.start + span * fraction,
                    expression_velocity(
                        self.ramp.start_velocity,
                        self.ramp.target_velocity,
                        fraction,
                    ),
                )
            })
            .collect()
    }
}

/// Whether any phrase uses dynamics hairpins.
pub fn has_hairpins(project: &Project) -> bool {
    project
        .tracks
        .iter()
        .any(|track| track_has_hairpins(project, track))
}

/// Whether a phrase hairpin on this track should drive CC11 rather than a gain
/// clip.
///
/// A built-in synth has no CC11 receiver, so its hairpin stays a post-insert
/// gain ramp. A SoundFont or hosted instrument owns a dynamics law; sending it
/// CC11 is the only way the instrument itself follows the mark, and CC11 is the
/// General MIDI expression control every one of those sources understands.
/// Pattern marks keep their gain clip: a pattern may name its own preset, and
/// this classification is per track.
pub fn track_uses_expression_cc11(project: &Project, track: &Track) -> bool {
    let Some(preset) = track
        .preset
        .as_deref()
        .and_then(|name| project.presets.iter().find(|preset| preset.name == name))
    else {
        return false;
    };
    match preset.source.as_deref() {
        Some("soundfont" | "clap" | "vst3" | "vst2" | "au" | "lv2") => true,
        Some(_) => false,
        None => preset
            .plugin
            .as_deref()
            .is_some_and(|plugin| !plugin.starts_with("builtin.")),
    }
}

/// Velocity to use for a hairpin's fraction from `start` to `target`.
///
/// The gain ramp is linear in decibels; inverting the sampler's square
/// velocity law gives the CC11 value that produces the same level.
pub fn expression_velocity(start: u8, target: u8, fraction: f64) -> u8 {
    let fraction = fraction.clamp(0.0, 1.0);
    let rise = 20.0 * (level(target) / level(start)).log10();
    let amplitude = level(start) * 10f64.powf(rise * fraction / 20.0);
    (127.0 * amplitude.sqrt()).round().clamp(1.0, 127.0) as u8
}

/// True when a track carries a phrase mark or a placed pattern mark.
pub fn track_has_hairpins(project: &Project, track: &Track) -> bool {
    track
        .phrases
        .iter()
        .any(|phrase| !phrase.hairpins.is_empty())
        || track
            .inline_patterns
            .iter()
            .any(|pattern| !pattern.hairpins.is_empty())
        || track.pattern_clips().any(|clip| {
            project
                .patterns
                .iter()
                .any(|pattern| pattern.name == clip.pattern && !pattern.hairpins.is_empty())
        })
}

/// Copy the project with every hairpin expanded into track gain clips.
///
/// Fails when a mark has no note after it, when two marks overlap, or when a
/// generated clip collides with gain automation the track already has. A
/// pattern mark expands once per placement, because the mark is relative to the
/// pattern's content rather than to the timeline.
///
/// The notes keep the dynamics they were written with: the start level reaches
/// the interpreter through [`HairpinExpansion::levels`], not through a rewritten
/// AST, so expanding the same project twice is the same as expanding it once.
pub fn expand(project: &Project) -> Result<Cow<'_, Project>, String> {
    if !has_hairpins(project) {
        return Ok(Cow::Borrowed(project));
    }
    let clock = ProjectClock::new(&project.properties)?;
    let mut expanded = project.clone();
    for track in &mut expanded.tracks {
        let expression = track_uses_expression_cc11(project, track);
        let mut generated = Vec::new();
        for phrase in &track.phrases {
            for expansion in expand_phrase_with(phrase, &clock)? {
                if !expression {
                    generated.push(expansion.clip);
                }
            }
        }
        generated.extend(pattern_placement_clips(track, project, &clock)?);
        track.gain_automation = merge_clips(&track.name, &track.gain_automation, generated)?;
    }
    Ok(Cow::Owned(expanded))
}

/// Merge generated clips into the track's own, refusing two clips on one span.
///
/// A gain clip is unity outside its range, so two clips that overlap would
/// fight over the same samples instead of summing.
pub fn merge_clips(
    track: &str,
    existing: &[GainAutomation],
    generated: Vec<GainAutomation>,
) -> Result<Vec<GainAutomation>, String> {
    // Re-expanding an already expanded project must not double the ramp.
    let clips: Vec<GainAutomation> = existing
        .iter()
        .filter(|clip| !generated.contains(clip))
        .chain(generated.iter())
        .cloned()
        .collect();
    let mut clips = clips;
    clips.sort_by_key(|clip| {
        (
            clip.time_range.start.bar,
            clip.time_range.start.beat,
            clip.time_range.start.subdivision,
        )
    });
    let start = |clip: &GainAutomation| {
        (
            clip.time_range.start.bar,
            clip.time_range.start.beat,
            clip.time_range.start.subdivision,
        )
    };
    let end = |clip: &GainAutomation| {
        (
            clip.time_range.end.bar,
            clip.time_range.end.beat,
            clip.time_range.end.subdivision,
        )
    };
    if let Some(pair) = clips
        .windows(2)
        .find(|pair| start(&pair[1]) < end(&pair[0]))
    {
        return Err(format!(
            "track '{track}' has two gain clips over the same span: {}:{} - {}:{} and {}:{} - {}:{}; \
             a hairpin expands to a clip of its own, so leave room before an existing gain ramp",
            pair[0].time_range.start.bar,
            pair[0].time_range.start.beat,
            pair[0].time_range.end.bar,
            pair[0].time_range.end.beat,
            pair[1].time_range.start.bar,
            pair[1].time_range.start.beat,
            pair[1].time_range.end.bar,
            pair[1].time_range.end.beat,
        ));
    }
    Ok(clips)
}

/// Every hairpin ramp on one track, in absolute quarter notes, sorted by start.
///
/// Pattern marks join the phrase marks here: a SoundFont lane crossfades its
/// velocity layers from this table, and a pattern placement is a span too.
pub fn track_layer_ramps(
    project: &Project,
    track: &Track,
    clock: &ProjectClock,
) -> Result<Vec<LayerRamp>, String> {
    if !track_has_hairpins(project, track) {
        return Ok(Vec::new());
    }
    let mut ramps = Vec::new();
    for phrase in &track.phrases {
        for expansion in expand_phrase_with(phrase, clock)? {
            ramps.push(expansion.ramp);
        }
    }
    for placement in pattern_placements(track, project, clock)? {
        for expansion in expand_pattern_with(
            placement.pattern,
            clock,
            placement.anchor,
            placement.cut,
            placement.skip_before,
        )? {
            ramps.push(expansion.ramp);
        }
    }
    ramps.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(ramps)
}

/// Expand every hairpin of one phrase.
pub fn expand_phrase(
    phrase: &Phrase,
    properties: &ProjectProperties,
) -> Result<Vec<HairpinExpansion>, String> {
    expand_phrase_with(phrase, &ProjectClock::new(properties)?)
}

/// The same expansion for a caller that already holds the project clock.
pub fn expand_phrase_with(
    phrase: &Phrase,
    clock: &ProjectClock,
) -> Result<Vec<HairpinExpansion>, String> {
    if phrase.hairpins.is_empty() {
        return Ok(Vec::new());
    }
    let phrase_start = phrase
        .time_range
        .as_ref()
        .map(|range| clock.quarter_notes_at(&range.start))
        .transpose()?
        .unwrap_or(0.0);
    let content_end = phrase_end_beats(phrase, clock, phrase_start)?;
    expand_marks(
        &phrase.hairpins,
        &phrase.events,
        phrase_start,
        content_end,
        None,
        None,
        clock,
    )
}

/// Expand a pattern's hairpins for one placement.
///
/// `anchor` is the absolute quarter note of the pattern's content beat 0. `cut`
/// stops a ramp at the placement's end, so a clip shorter than the swell does
/// not leak past it. `skip_before` drops marks hidden by a slip, matching the
/// rule for pedal marks in a hidden head.
pub fn expand_pattern_with(
    pattern: &PatternDef,
    clock: &ProjectClock,
    anchor: f64,
    cut: Option<f64>,
    skip_before: Option<f64>,
) -> Result<Vec<HairpinExpansion>, String> {
    if pattern.hairpins.is_empty() {
        return Ok(Vec::new());
    }
    expand_marks(
        &pattern.hairpins,
        &pattern.events,
        anchor,
        pattern_end_beats(&pattern.events),
        cut,
        skip_before,
        clock,
    )
}

/// The gain clips a track's placed patterns generate, one per mark occurrence.
pub fn pattern_placement_clips(
    track: &Track,
    project: &Project,
    clock: &ProjectClock,
) -> Result<Vec<GainAutomation>, String> {
    let mut clips = Vec::new();
    for placement in pattern_placements(track, project, clock)? {
        for expansion in expand_pattern_with(
            placement.pattern,
            clock,
            placement.anchor,
            placement.cut,
            placement.skip_before,
        )? {
            clips.push(expansion.clip);
        }
    }
    Ok(clips)
}

/// One placement of a pattern on a track, as the mark expansion sees it.
struct PatternPlacement<'a> {
    pattern: &'a PatternDef,
    anchor: f64,
    cut: Option<f64>,
    skip_before: Option<f64>,
}

/// Every clip and inline pattern on a track, with the anchor each occurrence
/// contributes. A loop yields one occurrence per repetition so a swell restarts
/// at the seam instead of overlapping the next one.
fn pattern_placements<'a>(
    track: &'a Track,
    project: &'a Project,
    clock: &ProjectClock,
) -> Result<Vec<PatternPlacement<'a>>, String> {
    let mut placements = Vec::new();
    for clip in track.pattern_clips() {
        let Some(pattern) = project
            .patterns
            .iter()
            .find(|pattern| pattern.name == clip.pattern)
        else {
            continue;
        };
        if pattern.hairpins.is_empty() {
            continue;
        }
        let clip_start = clock.quarter_notes_at(&clip.time_range.start)?;
        let clip_end = clock.quarter_notes_at(&clip.time_range.end)?;
        let offset = clip.offset_quarters;
        let first_anchor = clip_start - offset;
        if clip.loop_quarters > 0.0 {
            let mut anchor = first_anchor;
            let mut count = 0;
            // A repetition occupies [clip_start + k*loop, + loop); the content-0
            // anchor sits `offset` quarters before that seam.
            while anchor + offset < clip_end - 1e-9 && count < MAX_OCCURRENCES {
                placements.push(PatternPlacement {
                    pattern,
                    anchor,
                    cut: Some((anchor + offset + clip.loop_quarters).min(clip_end)),
                    skip_before: Some(offset),
                });
                anchor += clip.loop_quarters;
                count += 1;
            }
        } else {
            placements.push(PatternPlacement {
                pattern,
                anchor: first_anchor,
                cut: Some(clip_end),
                skip_before: Some(offset),
            });
        }
    }
    for pattern in &track.inline_patterns {
        if pattern.hairpins.is_empty() {
            continue;
        }
        placements.push(PatternPlacement {
            pattern,
            anchor: 0.0,
            cut: None,
            skip_before: None,
        });
    }
    Ok(placements)
}

/// The shared expansion: marks are relative to content beat 0 at `anchor`.
#[allow(clippy::too_many_arguments)]
fn expand_marks(
    hairpins: &[HairpinMark],
    events: &[NoteEvent],
    anchor: f64,
    content_end: f64,
    cut: Option<f64>,
    skip_before: Option<f64>,
    clock: &ProjectClock,
) -> Result<Vec<HairpinExpansion>, String> {
    if hairpins.is_empty() {
        return Ok(Vec::new());
    }
    let cut_beat = cut.map(|cut| cut - anchor);
    let mut order: Vec<usize> = (0..hairpins.len())
        .filter(|index| skip_before.is_none_or(|skip| hairpins[*index].beat >= skip - 1e-9))
        .collect();
    order.sort_by(|a, b| hairpins[*a].beat.total_cmp(&hairpins[*b].beat));

    let mut expansions = Vec::with_capacity(order.len());
    let mut previous_end: Option<f64> = None;
    for index in order {
        let mark = hairpins[index];
        if let Some(cut_beat) = cut_beat
            && cut_beat <= mark.beat + 1e-9
        {
            continue;
        }
        if let Some(previous) = previous_end
            && mark.beat < previous - 1e-9
        {
            return Err(format!(
                "{} at beat {} starts before the previous hairpin ends (beat {previous}); \
                 hairpins in one phrase or pattern cannot overlap",
                mark.kind.keyword(),
                mark.beat,
            ));
        }
        let (start_level, target_level, mut end) = levels_and_span(events, &mark, content_end)?;
        if let Some(cut_beat) = cut_beat {
            end = end.min(cut_beat);
        }
        if start_level == target_level {
            return Err(format!(
                "{} at beat {} would not move: the dynamic in force and the one it swells to are the same level",
                mark.kind.keyword(),
                mark.beat,
            ));
        }
        previous_end = Some(end);
        let clip = ramp(clock, events, &mark, start_level, target_level, end, anchor)?;
        let levels = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                !is_rest(event)
                    && event.start_beat >= mark.beat - 1e-9
                    && event.start_beat < end - 1e-9
                    && written_velocity(event).is_none()
            })
            .map(|(index, _)| (index, start_level))
            .collect();
        expansions.push(HairpinExpansion {
            mark: index,
            clip,
            levels,
            ramp: LayerRamp {
                start: anchor + mark.beat,
                end: anchor + end,
                start_velocity: start_level,
                target_velocity: target_level,
            },
        });
    }
    Ok(expansions)
}

/// The level the mark starts from, the level it swells to, and the beat it ends
/// on: the next written dynamic, or the end of the content.
fn levels_and_span(
    events: &[NoteEvent],
    mark: &HairpinMark,
    content_end: f64,
) -> Result<(u8, u8, f64), String> {
    let sounding = || {
        events
            .iter()
            .filter(|event| !is_rest(event) && !event.muted)
    };
    let in_force = sounding()
        .filter(|event| event.start_beat <= mark.beat + 1e-9)
        .filter_map(written_velocity)
        .next_back();
    let after = || sounding().filter(|event| event.start_beat > mark.beat + 1e-9);
    if after().next().is_none() {
        return Err(format!(
            "{} at beat {} has no following notes: write the notes it swells into, or drop the mark",
            mark.kind.keyword(),
            mark.beat,
        ));
    }
    let next_mark = after()
        .filter_map(|event| written_velocity(event).map(|velocity| (event.start_beat, velocity)))
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((end, _)) = next_mark
        && end <= mark.beat + 1e-9
    {
        return Err(format!(
            "{} at beat {} ends at beat {end}, before it starts; move the mark earlier",
            mark.kind.keyword(),
            mark.beat,
        ));
    }
    let (default_start, default_target) = match mark.kind {
        HairpinKind::Crescendo => (PP_VELOCITY, F_VELOCITY),
        HairpinKind::Diminuendo => (F_VELOCITY, PP_VELOCITY),
    };
    let (end, target) = match next_mark {
        Some((end, velocity)) => (end, velocity),
        None => (content_end, default_target),
    };
    Ok((in_force.unwrap_or(default_start), target, end))
}

#[allow(clippy::too_many_arguments)]
fn ramp(
    clock: &ProjectClock,
    events: &[NoteEvent],
    mark: &HairpinMark,
    start_level: u8,
    target_level: u8,
    end: f64,
    anchor: f64,
) -> Result<GainAutomation, String> {
    let rise = 20.0 * (level(target_level) / level(start_level)).log10();
    let mut onsets: Vec<f64> = vec![mark.beat];
    onsets.extend(
        events
            .iter()
            .filter(|event| !is_rest(event))
            .map(|event| event.start_beat)
            .filter(|beat| *beat > mark.beat + 1e-9 && *beat < end - 1e-9),
    );
    onsets.sort_by(f64::total_cmp);
    onsets.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // The end position leaves one tick of room: a point may not sit on the clip's
    // range end, and a gain clip holds its last value, so the ramp reaches the
    // target there and holds it for a tick.
    let quarters: Vec<f64> = onsets
        .iter()
        .map(|beat| anchor + beat)
        .chain(std::iter::once(anchor + end))
        .collect();
    let (positions, ppq) = notate(clock, &quarters)?;
    let end_position = fine_position(clock, anchor + end - 1.0 / f64::from(ppq), ppq)?;
    let gain = |fraction: f64| {
        ParamValue::Number((10f64.powf(rise * fraction / 20.0) * 1e6).round() / 1e6)
    };

    let points = onsets
        .iter()
        .zip(positions.iter())
        .map(|(beat, position)| AutomationPoint {
            position: *position,
            value: gain((beat - mark.beat) / (end - mark.beat)),
            curve: crate::ast::Curve::Linear,
            tension: None,
        })
        .chain(std::iter::once(AutomationPoint {
            position: end_position,
            value: gain(1.0),
            curve: crate::ast::Curve::Linear,
            tension: None,
        }))
        .collect();
    Ok(GainAutomation {
        time_range: TimeRange {
            start: positions[0],
            end: *positions.last().expect("a ramp has an end"),
        },
        interpolation: GainInterpolation::Linear,
        ppq,
        points,
    })
}

/// Velocity to amplitude, matching the sampler's square law.
fn level(velocity: u8) -> f64 {
    let normalized = f64::from(velocity.min(127)) / 127.0;
    normalized * normalized
}

fn written_velocity(event: &NoteEvent) -> Option<u8> {
    event
        .velocity
        .or_else(|| event.dynamics.map(|dynamics| dynamics.to_velocity()))
}

pub(crate) fn is_rest(event: &NoteEvent) -> bool {
    event.modifiers.iter().any(|(key, _)| key == "rest")
}

/// The last beat the phrase covers: its written range, else its last note end.
fn phrase_end_beats(
    phrase: &Phrase,
    clock: &ProjectClock,
    phrase_start: f64,
) -> Result<f64, String> {
    if let Some(range) = &phrase.time_range {
        return Ok(clock.quarter_notes_at(&range.end)? - phrase_start);
    }
    Ok(pattern_end_beats(&phrase.events))
}

/// The last beat a pattern's notes cover; a hit has no note-off to swell into.
fn pattern_end_beats(events: &[NoteEvent]) -> f64 {
    events
        .iter()
        .filter(|event| !is_rest(event))
        .map(|event| event.start_beat + event.duration.to_beats())
        .fold(0.0, f64::max)
}

/// Clip positions and the subdivision they need. A span whose every position
/// falls on the notated grid keeps `ppq=4`, so the emitted line stays readable.
fn notate(clock: &ProjectClock, quarters: &[f64]) -> Result<(Vec<BarBeat>, u32), String> {
    let notated: Option<Vec<BarBeat>> = quarters
        .iter()
        .map(|quarter| notated_position(clock, *quarter))
        .collect();
    match notated {
        Some(positions) => Ok((positions, 4)),
        None => {
            let mut positions = Vec::with_capacity(quarters.len());
            for quarter in quarters {
                positions.push(fine_position(clock, *quarter, FINE_PPQ)?);
            }
            Ok((positions, FINE_PPQ))
        }
    }
}

/// The notated position when the beat lands exactly on the 16th-note grid.
fn notated_position(clock: &ProjectClock, quarter: f64) -> Option<BarBeat> {
    let position = clock.bar_beat_at_quarter(quarter).ok()?;
    let exact = clock.quarter_notes_at(&position).ok()?;
    ((exact - quarter).abs() < 1e-9).then_some(position)
}

/// Beat position plus fine subdivision ticks, for positions off the notated grid.
///
/// The anchor is a whole quarter note: an explicit PPQ counts quarter-note
/// ticks, so a notated 16th in the subdivision field would mean a different
/// offset than the one it names.
fn fine_position(clock: &ProjectClock, quarter: f64, ppq: u32) -> Result<BarBeat, String> {
    let anchor_quarter = quarter.floor();
    let anchor = clock.bar_beat_at_quarter(anchor_quarter)?;
    let sub = ((quarter - anchor_quarter) * f64::from(ppq)).round();
    if anchor.subdivision != 0 || !(0.0..f64::from(ppq)).contains(&sub) {
        return Err("hairpin position does not fit one subdivision tick".into());
    }
    Ok(BarBeat::new(anchor.bar, anchor.beat, sub as u32))
}
