//! Section arrangement: map written positions onto the repeated rendered timeline.
//! Semantics and limits are documented in `docs/arrangement.md`.

use std::collections::HashSet;

use crate::ast::*;
use crate::timing::ProjectClock;

#[derive(Debug, Clone, Copy)]
struct Play {
    start: f64,
    end: f64,
    rendered_start: f64,
    /// Index into the project's sections and the zero-based play of it, so a
    /// refusal can name the play it is about.
    section: usize,
    repeat: u32,
    /// True when the rendered timeline does not continue the written timeline
    /// after this play: a repeat seam, or a removed gap.
    cut_at_end: bool,
}

impl Play {
    /// True when this play replays the written quarter `quarter`.
    fn holds(&self, quarter: f64) -> bool {
        quarter >= self.start - 1e-9 && quarter < self.end - 1e-9
    }

    /// Rendered quarter at which this play replays written `quarter`.
    fn rendered(&self, quarter: f64) -> f64 {
        self.rendered_start + quarter - self.start
    }
}

/// One rendered instance of the play containing a container's start.
#[derive(Debug, Clone, Copy)]
pub struct Shift {
    /// Rendered start minus written start.
    pub delta: f64,
    /// Written position at which the play stops.
    pub written_end: f64,
    /// True when content is cut at [`Shift::written_end`].
    pub cut_at_end: bool,
}

impl Shift {
    /// Rendered position at which the play stops.
    pub fn rendered_end(&self) -> f64 {
        self.delta + self.written_end
    }
}

/// Resolved section layout. Empty when the project declares no sections.
#[derive(Debug, Clone)]
pub struct Arrangement {
    plays: Vec<Play>,
    rendered_end: f64,
}

impl Arrangement {
    /// Expands a project's sections and repeats into the rendered timeline.
    pub fn from_project(project: &Project) -> Result<Self, String> {
        let clock = ProjectClock::new(&project.properties)?;
        let mut plays = Vec::new();
        let Some(first) = project.sections.first() else {
            return Ok(Self {
                plays,
                rendered_end: 0.0,
            });
        };
        let anchor = clock.quarter_notes_at(&first.time_range.start)?;
        let mut cursor = anchor;
        let mut previous_end = 0.0_f64;
        for (section_index, section) in project.sections.iter().enumerate() {
            let start = clock.quarter_notes_at(&section.time_range.start)?;
            let end = clock.quarter_notes_at(&section.time_range.end)?;
            if end <= start {
                return Err(format!(
                    "section {:?} must end after its start",
                    section.name
                ));
            }
            if start < previous_end - 1e-9 {
                return Err(format!(
                    "section {:?} overlaps or precedes the previous section",
                    section.name
                ));
            }
            let repeat = section.repeat.unwrap_or(1);
            if repeat == 0 {
                return Err(format!(
                    "section {:?} repeat must be positive",
                    section.name
                ));
            }
            for repeat_index in 0..repeat {
                plays.push(Play {
                    start,
                    end,
                    rendered_start: cursor,
                    section: section_index,
                    repeat: repeat_index,
                    cut_at_end: false,
                });
                cursor += end - start;
            }
            previous_end = end;
        }
        for index in 0..plays.len() {
            // A cut exists where the next play restarts or skips written time.
            plays[index].cut_at_end = plays
                .get(index + 1)
                .is_some_and(|next| (next.start - plays[index].end).abs() > 1e-9);
        }
        Ok(Self {
            plays,
            rendered_end: cursor,
        })
    }

    /// Whether the plan schedules no clips.
    pub fn is_empty(&self) -> bool {
        self.plays.is_empty()
    }

    /// True when mapping every container start is a no-op, so callers can keep
    /// the authored AST (and its sections) unchanged.
    pub fn is_identity(&self, project: &Project, clock: &ProjectClock) -> Result<bool, String> {
        if self.plays.is_empty() {
            return Ok(true);
        }
        if self.plays[0].start != 0.0 {
            return Ok(false);
        }
        let mut cursor = 0.0;
        for play in &self.plays {
            if play.start != play.rendered_start || play.start != cursor {
                return Ok(false);
            }
            cursor = play.end;
        }
        Ok(max_content_start(project, clock)? < self.rendered_end + 1e-9)
    }

    /// Rendered plays containing `quarter`, with their written end and whether
    /// the rendered timeline cuts there.
    pub fn shifts(&self, quarter: f64) -> Vec<Shift> {
        self.plays
            .iter()
            .filter(|play| play.holds(quarter))
            .map(|play| Shift {
                delta: play.rendered_start - play.start,
                written_end: play.end,
                cut_at_end: play.cut_at_end,
            })
            .collect()
    }

    /// The rendered end in quarter notes, after repeats and removed gaps.
    pub fn rendered_end(&self) -> f64 {
        self.rendered_end
    }

    fn rendered_sections(
        &self,
        project: &Project,
        grid: &RenderedGrid,
    ) -> Result<Vec<Section>, String> {
        let mut sections = Vec::new();
        let mut index = 0;
        for section in &project.sections {
            let repeat = section.repeat.unwrap_or(1);
            for _ in 0..repeat {
                let play = self.plays[index];
                sections.push(Section {
                    name: section.name.clone(),
                    time_range: TimeRange {
                        start: grid.position_at(play.rendered_start)?,
                        end: grid.position_at(play.rendered_start + play.end - play.start)?,
                    },
                    repeat: None,
                    label: section.label.clone(),
                    flags: section.flags,
                });
                index += 1;
            }
        }
        Ok(sections)
    }
}

fn max_content_start(project: &Project, clock: &ProjectClock) -> Result<f64, String> {
    let mut maximum = 0.0_f64;
    let mut observe = |position: f64| {
        if position > maximum {
            maximum = position;
        }
    };
    for track in &project.tracks {
        for phrase in &track.phrases {
            observe(match &phrase.time_range {
                Some(range) => clock.quarter_notes_at(&range.start)?,
                None => 0.0,
            });
        }
        for clip in track.pattern_clips() {
            observe(clock.quarter_notes_at(&clip.time_range.start)?);
        }
        for pattern in &track.patterns {
            observe(match &pattern.time_range {
                Some(range) => clock.quarter_notes_at(&range.start)?,
                None => 0.0,
            });
        }
        for lane in track.automation() {
            // A clip is positioned by its span, so it can start past its last
            // point and still lie inside the rendered timeline.
            if let Some(range) = &lane.time_range {
                observe(clock.quarter_notes_at(&range.start)?);
            }
            for point in &lane.points {
                observe(automation_quarter(clock, lane, &point.position)?);
            }
        }
    }
    for clip in project.audio_clips() {
        let (start, _) = clock.audio_clip_range_seconds(clip)?;
        observe(clock.quarter_at_seconds(start)?);
    }
    for lane in project
        .buses
        .iter()
        .flat_map(|bus| bus.automation.iter())
        .chain(
            project
                .master
                .iter()
                .flat_map(|master| master.automation.iter()),
        )
    {
        if let Some(range) = &lane.time_range {
            observe(clock.quarter_notes_at(&range.start)?);
        }
        for point in &lane.points {
            observe(automation_quarter(clock, lane, &point.position)?);
        }
    }
    for gain in project
        .tracks
        .iter()
        .flat_map(|track| track.gain_automation.iter())
        .chain(
            project
                .buses
                .iter()
                .flat_map(|bus| bus.gain_automation.iter()),
        )
        .chain(
            project
                .master
                .iter()
                .flat_map(|master| master.gain_automation.iter()),
        )
    {
        observe(clock.quarter_notes_at(&gain.time_range.start)?);
    }
    Ok(maximum)
}

fn automation_quarter(
    clock: &ProjectClock,
    lane: &AutomationLane,
    position: &BarBeat,
) -> Result<f64, String> {
    if lane.ppq == 0 {
        clock.quarter_notes_at(position)
    } else {
        clock.quarter_notes_at_ppq(position, lane.ppq)
    }
}

/// Expand sections into concrete repeated content. Returns the project unchanged
/// when the sections already describe the written timeline, otherwise the result
/// carries rendered section spans with `repeat` cleared.
pub fn expand(project: &Project) -> Result<Project, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let arrangement = Arrangement::from_project(project)?;
    if arrangement.is_identity(project, &clock)? {
        return Ok(project.clone());
    }
    if project
        .tracks
        .iter()
        .any(|track| track.performance.is_some())
    {
        return Err(
            "sections that change the arrangement cannot be combined with performance timelines"
                .into(),
        );
    }

    let mut out = project.clone();
    // The rendered meter layout names every shifted position, so build it and
    // the repeated maps before any container is moved.
    let (grid, time_sig_map, tempo_map) = expand_maps(project, &arrangement, &clock)?;
    out.properties.time_sig_map = time_sig_map;
    out.properties.tempo_map = tempo_map;
    let mut inline_patterns = Vec::new();
    let mut clip_patterns = Vec::new();
    let mut pattern_names: HashSet<String> = project
        .patterns
        .iter()
        .map(|pattern| pattern.name.clone())
        .collect();
    for (index, track) in out.tracks.iter_mut().enumerate() {
        let source = &project.tracks[index];
        track.phrases = expand_phrases(source, &arrangement, &grid, &clock)?;
        let (clips, patterns) = expand_clips(
            source,
            &arrangement,
            &grid,
            &clock,
            &out.patterns,
            &mut pattern_names,
            index,
        )?;
        let mut rendered_clips = clips;
        track.patterns = expand_patterns(source, &arrangement, &grid, &clock)?;
        track.set_automation_lanes(expand_automation(
            source.automation(),
            &arrangement,
            &grid,
            &clock,
        )?);
        track.gain_automation = expand_gain(&source.gain_automation, &arrangement, &grid, &clock)?;
        let (kept, patterns2, clips2) =
            expand_inline_patterns(source, &arrangement, &grid, &mut pattern_names, index)?;
        track.inline_patterns = kept;
        rendered_clips.extend(clips2);
        track.set_pattern_clips(rendered_clips);
        inline_patterns.extend(patterns2);
        clip_patterns.extend(patterns);
    }
    out.patterns.extend(clip_patterns);
    out.patterns.extend(inline_patterns);
    let rendered_clock = ProjectClock::new(&out.properties)?;
    let audio_clips = expand_audio_clips(project, &arrangement, &grid, &clock, &rendered_clock)?;
    out.set_ordered_audio_clips(audio_clips.into_iter().enumerate())?;
    for (index, bus) in out.buses.iter_mut().enumerate() {
        bus.gain_automation = expand_gain(&bus.gain_automation, &arrangement, &grid, &clock)?;
        bus.automation = expand_automation(
            &project.buses[index].automation,
            &arrangement,
            &grid,
            &clock,
        )?;
    }
    if let Some(master) = &mut out.master {
        master.gain_automation = expand_gain(&master.gain_automation, &arrangement, &grid, &clock)?;
        if let Some(source) = &project.master {
            master.automation = expand_automation(&source.automation, &arrangement, &grid, &clock)?;
        }
    }
    out.sections = arrangement.rendered_sections(project, &grid)?;
    Ok(out)
}

fn shift_position(
    grid: &RenderedGrid,
    clock: &ProjectClock,
    position: &BarBeat,
    delta: f64,
    ppq: u32,
) -> Result<BarBeat, String> {
    if ppq == 0 {
        return grid.position_at(clock.quarter_notes_at(position)? + delta);
    }
    let quarter = clock.quarter_notes_at_ppq(position, ppq)? + delta;
    let (base, remainder) = grid.floor_position_at(quarter)?;
    let subdivision = (remainder * f64::from(ppq)).round();
    if !subdivision.is_finite() || subdivision < 0.0 || subdivision > f64::from(u32::MAX) {
        return Err("automation position exceeds the PPQ grid".into());
    }
    Ok(BarBeat::new(base.bar, base.beat, subdivision as u32))
}

fn shift_range(
    grid: &RenderedGrid,
    clock: &ProjectClock,
    range: &TimeRange,
    delta: f64,
) -> Result<TimeRange, String> {
    Ok(TimeRange {
        start: grid.position_at(clock.quarter_notes_at(&range.start)? + delta)?,
        end: grid.position_at(clock.quarter_notes_at(&range.end)? + delta)?,
    })
}

fn expand_phrases(
    track: &Track,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
) -> Result<Vec<Phrase>, String> {
    let mut phrases = Vec::new();
    for phrase in &track.phrases {
        let start = match &phrase.time_range {
            Some(range) => clock.quarter_notes_at(&range.start)?,
            None => 0.0,
        };
        for shift in arrangement.shifts(start) {
            let mut copy = phrase.clone();
            match &phrase.time_range {
                Some(range) => {
                    let mut range = *range;
                    if shift.cut_at_end {
                        clamp_range_end(clock, &mut range, shift.written_end)?;
                    }
                    copy.time_range = Some(shift_range(grid, clock, &range, shift.delta)?);
                    if shift.cut_at_end {
                        trim_notes(&mut copy.events, start + shift.delta, shift.rendered_end());
                        trim_pedals(&mut copy.pedals, start + shift.delta, shift.rendered_end());
                    }
                }
                None => {
                    for event in &mut copy.events {
                        event.start_beat += shift.delta;
                    }
                    for pedal in &mut copy.pedals {
                        pedal.beat += shift.delta;
                    }
                    if shift.cut_at_end {
                        trim_notes(&mut copy.events, shift.delta, shift.rendered_end());
                        trim_pedals(&mut copy.pedals, shift.delta, shift.rendered_end());
                    }
                }
            }
            phrases.push(copy);
        }
    }
    Ok(phrases)
}

/// Clip placements plus any trimmed pattern copies made for cut spans.
type ExpandedClips = (Vec<Clip>, Vec<PatternDef>);

fn expand_clips(
    track: &Track,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
    patterns: &[PatternDef],
    used_names: &mut HashSet<String>,
    track_index: usize,
) -> Result<ExpandedClips, String> {
    let mut clips = Vec::new();
    let mut generated = Vec::new();
    for clip in track.pattern_clips() {
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        for shift in arrangement.shifts(start) {
            let mut range = clip.time_range;
            if shift.cut_at_end {
                clamp_range_end(clock, &mut range, shift.written_end)?;
            }
            let mut copy = clip.clone();
            copy.time_range = shift_range(grid, clock, &range, shift.delta)?;
            if shift.cut_at_end
                && let Some(def) = patterns.iter().find(|def| def.name == clip.pattern)
            {
                let cut = shift.rendered_end();
                if let Some(mut trimmed) = trim_pattern_def(def, start + shift.delta, cut)? {
                    let base = format!(
                        "{} [track {} clip cut {}]",
                        def.name,
                        track_index + 1,
                        generated.len() + 1
                    );
                    let name = unique_name(&base, used_names);
                    trimmed.name = name.clone();
                    generated.push(trimmed);
                    copy.pattern = name;
                }
            }
            clips.push(copy);
        }
    }
    Ok((clips, generated))
}

fn expand_patterns(
    track: &Track,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
) -> Result<Vec<Pattern>, String> {
    let mut patterns = Vec::new();
    for pattern in &track.patterns {
        let start = match &pattern.time_range {
            Some(range) => clock.quarter_notes_at(&range.start)?,
            None => 0.0,
        };
        for shift in arrangement.shifts(start) {
            let mut copy = pattern.clone();
            if let Some(range) = &pattern.time_range
                && shift.delta != 0.0
            {
                let mut range = *range;
                if shift.cut_at_end {
                    clamp_range_end(clock, &mut range, shift.written_end)?;
                }
                copy.time_range = Some(shift_range(grid, clock, &range, shift.delta)?);
            }
            if shift.cut_at_end {
                let base = copy
                    .time_range
                    .map(|range| clock.quarter_notes_at(&range.start))
                    .transpose()?
                    .unwrap_or(0.0);
                trim_pattern(clock, &mut copy, base, shift.rendered_end())?;
            }
            patterns.push(copy);
        }
    }
    Ok(patterns)
}

fn expand_automation<'a>(
    source: impl IntoIterator<Item = &'a AutomationLane>,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
) -> Result<Vec<AutomationLane>, String> {
    let mut lanes = Vec::new();
    for lane in source {
        // A clip is a container: it belongs to the play holding its start, and
        // every play gets its own span and its own shifted points. The span is
        // what makes the seam a jump back to the clip's opening value instead
        // of a ramp between two plays.
        if let Some(range) = lane.time_range {
            let start = clock.quarter_notes_at(&range.start)?;
            for shift in arrangement.shifts(start) {
                let mut copy = lane.clone();
                let mut range = range;
                if shift.cut_at_end {
                    clamp_range_end(clock, &mut range, shift.written_end)?;
                }
                copy.time_range = Some(shift_range(grid, clock, &range, shift.delta)?);
                for point in &mut copy.points {
                    point.position =
                        shift_position(grid, clock, &point.position, shift.delta, lane.ppq)?;
                }
                lanes.push(copy);
            }
            continue;
        }
        let mut played = Vec::new();
        for point in &lane.points {
            let quarter = automation_quarter(clock, lane, &point.position)?;
            for shift in arrangement.shifts(quarter) {
                let position = shift_position(grid, clock, &point.position, shift.delta, lane.ppq)?;
                let rendered = automation_quarter(clock, lane, &position)?;
                played.push((
                    rendered,
                    AutomationPoint {
                        position,
                        value: point.value.clone(),
                        curve: point.curve,
                        tension: point.tension,
                    },
                ));
            }
        }
        if played.is_empty() {
            continue;
        }
        played.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        let mut copy = lane.clone();
        copy.points = played.into_iter().map(|(_, point)| point).collect();
        lanes.push(copy);
    }
    Ok(lanes)
}

type PatternClipSplit = (Vec<PatternDef>, Vec<PatternDef>, Vec<Clip>);

fn expand_inline_patterns(
    track: &Track,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    used_names: &mut HashSet<String>,
    track_index: usize,
) -> Result<PatternClipSplit, String> {
    let mut kept = Vec::new();
    let mut patterns = Vec::new();
    let mut clips = Vec::new();
    for pattern in &track.inline_patterns {
        for (occurrence, shift) in arrangement.shifts(0.0).into_iter().enumerate() {
            let mut copy = pattern.clone();
            if shift.cut_at_end {
                trim_notes(&mut copy.events, shift.delta, shift.rendered_end());
                trim_pedals(&mut copy.pedals, shift.delta, shift.rendered_end());
                trim_hits(&mut copy.hits, shift.delta, shift.rendered_end())?;
            }
            if occurrence == 0 {
                kept.push(copy);
                continue;
            }
            let mut name = format!(
                "{} [track {} section {}]",
                pattern.name,
                track_index + 1,
                occurrence + 1
            );
            let mut suffix = 2;
            while used_names.contains(&name) {
                name = format!(
                    "{} [track {} section {}:{}]",
                    pattern.name,
                    track_index + 1,
                    occurrence + 1,
                    suffix
                );
                suffix += 1;
            }
            used_names.insert(name.clone());
            copy.name = name.clone();
            let start = grid.position_at(shift.delta)?;
            clips.push(Clip {
                pattern: name,
                time_range: TimeRange { start, end: start },
                offset_quarters: 0.0,
                loop_quarters: 0.0,
                id: None,
            });
            patterns.push(copy);
        }
    }
    Ok((kept, patterns, clips))
}

/// Shift the actual musical boundary and retain its written anchor where possible.
/// Recompute the second residual in the rendered tempo map: a distant anchor
/// can cross a different tempo span when written gaps are removed. If that
/// anchor moves before project zero, re-anchor the actual boundary instead.
fn shift_audio_boundary(
    grid: &RenderedGrid,
    clock: &ProjectClock,
    rendered_clock: &ProjectClock,
    anchor: BarBeat,
    offset: f64,
    actual_seconds: f64,
    delta: f64,
) -> Result<(BarBeat, f64), String> {
    let actual_quarter = clock.quarter_at_seconds(actual_seconds)? + delta;
    if actual_quarter < 0.0 {
        return Err("arranged audio boundary precedes project zero".into());
    }
    let shifted_anchor = clock.quarter_notes_at(&anchor)? + delta;
    if shifted_anchor >= 0.0 {
        let anchor = grid.position_at(shifted_anchor)?;
        // Preserve exact legacy residuals whenever both tempo spans agree.
        let anchor_seconds = rendered_clock.seconds_at_quarter(shifted_anchor)?;
        let target_seconds = rendered_clock.seconds_at_quarter(actual_quarter)?;
        let residual = target_seconds - anchor_seconds;
        let residual = if (residual - offset).abs() <= 1e-12 {
            offset
        } else {
            residual
        };
        return Ok((anchor, residual));
    }
    let (anchor, _) = grid.floor_position_at(actual_quarter)?;
    let residual = rendered_clock.seconds_at_quarter(actual_quarter)?
        - rendered_clock.seconds_at_quarter(rendered_clock.quarter_notes_at(&anchor)?)?;
    Ok((anchor, residual))
}

fn expand_audio_clips(
    project: &Project,
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
    rendered_clock: &ProjectClock,
) -> Result<Vec<AudioClip>, String> {
    let mut clips = Vec::new();
    for clip in project.audio_clips() {
        let (start_seconds, end_seconds) = clock.audio_clip_range_seconds(clip)?;
        let start = clock.quarter_at_seconds(start_seconds)?;
        for shift in arrangement.shifts(start) {
            let mut copy = clip.clone();
            (copy.time_range.start, copy.placement.start_offset_seconds) = shift_audio_boundary(
                grid,
                clock,
                rendered_clock,
                clip.time_range.start,
                clip.placement.start_offset_seconds,
                start_seconds,
                shift.delta,
            )?;
            (copy.time_range.end, copy.placement.end_offset_seconds) = shift_audio_boundary(
                grid,
                clock,
                rendered_clock,
                clip.time_range.end,
                clip.placement.end_offset_seconds,
                end_seconds,
                shift.delta,
            )?;
            if shift.cut_at_end && !tempo_follows_range(clip) {
                let cut_seconds = rendered_clock.seconds_at_quarter(shift.rendered_end())?;
                let (_, copied_end) = rendered_clock.audio_clip_range_seconds(&copy)?;
                if copied_end > cut_seconds {
                    copy.time_range.end = grid.position_at(shift.rendered_end())?;
                    copy.placement.end_offset_seconds = 0.0;
                }
            }
            clips.push(copy);
        }
    }
    Ok(clips)
}

fn expand_gain(
    clips: &[GainAutomation],
    arrangement: &Arrangement,
    grid: &RenderedGrid,
    clock: &ProjectClock,
) -> Result<Vec<GainAutomation>, String> {
    let mut expanded: Vec<(f64, GainAutomation)> = Vec::new();
    for clip in clips {
        let start = clock.quarter_notes_at(&clip.time_range.start)?;
        for shift in arrangement.shifts(start) {
            let mut copy = clip.clone();
            copy.time_range = shift_range(grid, clock, &clip.time_range, shift.delta)?;
            for point in &mut copy.points {
                point.position =
                    shift_position(grid, clock, &point.position, shift.delta, clip.ppq)?;
            }
            let rendered = clock.quarter_notes_at(&copy.time_range.start)?;
            expanded.push((rendered, copy));
        }
    }
    expanded.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    Ok(expanded.into_iter().map(|(_, clip)| clip).collect())
}

/// Meter layout of the rendered timeline: where each segment starts in quarter
/// notes, the bar it starts and its signature.
struct RenderedGrid {
    segments: Vec<(f64, u32, TimeSignature)>,
}

fn bar_quarters(signature: TimeSignature) -> f64 {
    f64::from(signature.numerator) * 4.0 / f64::from(signature.denominator)
}

impl RenderedGrid {
    /// Start the grid at quarter 0 in the meter the project opens in.
    fn new(signature: TimeSignature) -> Self {
        Self {
            segments: vec![(0.0, 1, signature)],
        }
    }

    fn last(&self) -> (f64, u32, TimeSignature) {
        *self
            .segments
            .last()
            .expect("the rendered grid always starts at bar 1")
    }

    /// Notated position of one rendered quarter note.
    fn position_at(&self, quarter: f64) -> Result<BarBeat, String> {
        let (base, leftover) = self.floor_position_at(quarter)?;
        let beat_quarters = 4.0 / f64::from(self.signature_at(quarter).denominator);
        let subdivision = (leftover / beat_quarters * 4.0).round();
        if !subdivision.is_finite() || !(0.0..4.0).contains(&subdivision) {
            return Err(format!(
                "the arrangement cannot place a position at rendered quarter {quarter} on the notated grid"
            ));
        }
        Ok(BarBeat::new(base.bar, base.beat, subdivision as u32))
    }

    fn signature_at(&self, quarter: f64) -> TimeSignature {
        let index = self
            .segments
            .partition_point(|(start, _, _)| *start <= quarter + 1e-9)
            - 1;
        self.segments[index].2
    }

    /// Notated beat of one rendered quarter note and the quarters left over
    /// inside that beat, which a tick-positioned lane turns into ticks.
    fn floor_position_at(&self, quarter: f64) -> Result<(BarBeat, f64), String> {
        let index = self
            .segments
            .partition_point(|(start, _, _)| *start <= quarter + 1e-9)
            - 1;
        let (start, bar, signature) = self.segments[index];
        let length = bar_quarters(signature);
        let offset = quarter - start;
        let bars = (offset / length).floor();
        let remainder = offset - bars * length;
        let beat_quarters = 4.0 / f64::from(signature.denominator);
        let beat = (remainder / beat_quarters).floor();
        let leftover = remainder - beat * beat_quarters;
        if !leftover.is_finite() || leftover < -1e-9 {
            return Err(format!(
                "the arrangement cannot place a position at rendered quarter {quarter} on the notated grid"
            ));
        }
        let bar = u64::from(bar).checked_add(bars as u64);
        let bar = bar
            .and_then(|bar| u32::try_from(bar).ok())
            .ok_or_else(|| "bar overflow while repeating a position".to_string())?;
        Ok((BarBeat::new(bar, beat as u32 + 1, 0), leftover.max(0.0)))
    }

    /// Add a meter change; the quarter must start a bar of the current grid.
    fn push_meter(&mut self, quarter: f64, signature: TimeSignature) -> Result<(), String> {
        let (start, bar, current) = self.last();
        if quarter <= start + 1e-9 {
            return Err("a repeated meter change lands on the bar it already started".into());
        }
        let bars = (quarter - start) / bar_quarters(current);
        if (bars - bars.round()).abs() > 1e-7 {
            return Err(
                "a meter change inside a section that repeats it does not start a bar; sections that repeat a meter change must start and end on a bar"
                    .into(),
            );
        }
        let bar = u64::from(bar)
            .checked_add(bars.round() as u64)
            .and_then(|bar| u32::try_from(bar).ok())
            .ok_or_else(|| "bar overflow while repeating a meter map point".to_string())?;
        self.segments.push((quarter, bar, signature));
        Ok(())
    }
}

/// Names the section, the play and the written point a map refusal is about.
fn replay_label(project: &Project, play: Play, kind: &str, point: &BarBeat) -> String {
    let section = &project.sections[play.section];
    let plays = section.repeat.unwrap_or(1);
    format!(
        "section {:?} play {} of {plays}, {kind} point at {}:{}",
        section.name,
        play.repeat + 1,
        point.bar,
        point.beat
    )
}

/// Refuse a replayed point that does not move forward: merging two plays would
/// hide a tempo or meter change instead of reporting it.
fn check_rendered_order(
    placed: &mut Option<(f64, String)>,
    rendered: f64,
    label: &str,
) -> Result<(), String> {
    if let Some((quarter, before)) = placed {
        if (rendered - *quarter).abs() <= 1e-9 {
            return Err(format!(
                "{label} replays onto rendered quarter {rendered}, where {before} already places a point; two plays cannot share a rendered map position"
            ));
        }
        if rendered < *quarter {
            return Err(format!(
                "{label} replays onto rendered quarter {rendered}, before {before} at {quarter}"
            ));
        }
    }
    *placed = Some((rendered, label.to_string()));
    Ok(())
}

/// Rendered tempo and meter maps, built in rendered order. A repeat re-plays
/// written time, so every play of a section replays the points it holds.
fn expand_maps(
    project: &Project,
    arrangement: &Arrangement,
    clock: &ProjectClock,
) -> Result<(RenderedGrid, Vec<TimeSigPoint>, Vec<TempoPoint>), String> {
    let properties = &project.properties;
    let opening = properties
        .time_sig_map
        .iter()
        .find(|point| point.position.bar == 1)
        .map(|point| TimeSignature {
            numerator: point.numerator,
            denominator: point.denominator,
        })
        .unwrap_or(properties.time_signature);
    let mut grid = RenderedGrid::new(opening);
    let mut meters: Vec<(f64, TimeSigPoint)> = Vec::new();
    let mut placed_meter = None;
    for play in arrangement.plays.iter().copied() {
        for point in &properties.time_sig_map {
            let written = clock.quarter_notes_at(&point.position)?;
            if !play.holds(written) {
                continue;
            }
            let rendered = play.rendered(written);
            let label = replay_label(project, play, "meter", &point.position);
            let signature = TimeSignature {
                numerator: point.numerator,
                denominator: point.denominator,
            };
            if rendered <= 1e-9 {
                // The opening meter is already the grid's first segment.
                if signature != opening {
                    return Err(format!(
                        "{label}: a meter change repeated at the first bar cannot be placed"
                    ));
                }
                continue;
            }
            check_rendered_order(&mut placed_meter, rendered, &label)?;
            let position = grid.position_at(rendered)?;
            if position.beat != 1 || position.subdivision != 0 {
                return Err(format!(
                    "{label} replays onto rendered quarter {rendered}, which does not start a bar; a section that repeats a meter change must start and end on a bar"
                ));
            }
            grid.push_meter(rendered, signature).map_err(|error| {
                format!("{label} replays onto rendered quarter {rendered}: {error}")
            })?;
            meters.push((
                rendered,
                TimeSigPoint {
                    position,
                    numerator: signature.numerator,
                    denominator: signature.denominator,
                },
            ));
        }
    }
    let mut tempos: Vec<(f64, TempoPoint)> = Vec::new();
    let mut placed_tempo = None;
    for play in arrangement.plays.iter().copied() {
        for point in &properties.tempo_map {
            let written = clock.quarter_notes_at(&point.position)?;
            if !play.holds(written) {
                continue;
            }
            let rendered = play.rendered(written);
            let label = replay_label(project, play, "tempo", &point.position);
            check_rendered_order(&mut placed_tempo, rendered, &label)?;
            let position = grid.position_at(rendered)?;
            tempos.push((
                rendered,
                TempoPoint {
                    position,
                    offset_quarters: point.offset_quarters,
                    bpm: point.bpm,
                },
            ));
        }
    }
    // The expanded maps must name exactly the quarters the arrangement moved
    // them to; anything else would render at a tempo the document never asked for.
    let mut rendered = properties.clone();
    rendered.time_sig_map = meters.iter().map(|(_, point)| *point).collect();
    rendered.tempo_map = tempos.iter().map(|(_, point)| *point).collect();
    let rendered_clock = ProjectClock::new(&rendered)?;
    for (quarter, point) in &meters {
        let placed = rendered_clock.quarter_notes_at(&point.position)?;
        if (placed - quarter).abs() > 1e-9 {
            return Err(format!(
                "repeated meter map point at bar {} lands at quarter {placed} instead of {quarter}",
                point.position.bar
            ));
        }
    }
    for (quarter, point) in &tempos {
        let placed = rendered_clock.quarter_notes_at(&point.position)? + point.offset_quarters;
        if (placed - quarter).abs() > 1e-9 {
            return Err(format!(
                "repeated tempo map point at bar {} lands at quarter {placed} instead of {quarter}",
                point.position.bar
            ));
        }
    }
    Ok((
        grid,
        meters.into_iter().map(|(_, point)| point).collect(),
        tempos.into_iter().map(|(_, point)| point).collect(),
    ))
}

/// Clamp a written range end to the end of the play that holds its start.
fn clamp_range_end(
    clock: &ProjectClock,
    range: &mut TimeRange,
    written_end: f64,
) -> Result<(), String> {
    if clock.quarter_notes_at(&range.end)? > written_end + 1e-9 {
        range.end = clock.bar_beat_at_quarter(written_end)?;
    }
    Ok(())
}

/// Cut a note list at the rendered span end: notes starting at or after `cut`
/// are dropped and notes crossing it are released through an explicit gate.
fn trim_notes(events: &mut Vec<NoteEvent>, base: f64, cut: f64) -> bool {
    let mut changed = false;
    events.retain_mut(|event| {
        let Some(gate) = note_gate(event) else {
            return true;
        };
        let start = base + event.start_beat;
        if start >= cut - 1e-9 {
            changed = true;
            return false;
        }
        if start + gate > cut + 1e-9 {
            set_gate(&mut event.modifiers, cut - start);
            changed = true;
        }
        true
    });
    changed
}

/// Cut drum hits at the rendered span end, with the same gate boundary.
fn trim_hits(hits: &mut Vec<HitEvent>, base: f64, cut: f64) -> Result<bool, String> {
    let mut changed = false;
    let mut kept = Vec::with_capacity(hits.len());
    for mut hit in hits.drain(..) {
        let start = base + hit.written_quarters();
        if start >= cut - 1e-9 {
            changed = true;
            continue;
        }
        let gate = performance_gate_quarters(&hit.modifiers)?.unwrap_or(0.125);
        if start + gate > cut + 1e-9 {
            set_gate(&mut hit.modifiers, cut - start);
            changed = true;
        }
        kept.push(hit);
    }
    *hits = kept;
    Ok(changed)
}

/// Cut pedal marks at the rendered span end. A pedal still down at the cut
/// gets an explicit lift there, so a trimmed phrase cannot hold the next one.
fn trim_pedals(pedals: &mut Vec<PedalEvent>, base: f64, cut: f64) -> bool {
    let mut changed = false;
    let mut down = false;
    let mut kept = Vec::with_capacity(pedals.len());
    for pedal in pedals.drain(..) {
        if base + pedal.beat >= cut - 1e-9 {
            changed = true;
            continue;
        }
        down = pedal.down;
        kept.push(pedal);
    }
    if down {
        kept.push(PedalEvent {
            beat: cut - base,
            down: false,
        });
        changed = true;
    }
    *pedals = kept;
    changed
}

/// Cut a bar-repeated drum pattern: bars past the cut go by lowering `repeat`,
/// and hits in the bar that holds the cut are trimmed.
fn trim_pattern(
    clock: &ProjectClock,
    pattern: &mut Pattern,
    base: f64,
    cut: f64,
) -> Result<bool, String> {
    let first_bar = pattern.time_range.map(|range| range.start.bar).unwrap_or(1);
    let first_bar_start = clock.quarter_notes_at(&BarBeat::new(first_bar, 1, 0))?;
    let mut kept = 0;
    let mut last_bar_start = None;
    for rep in 0..pattern.repeat {
        let bar = first_bar
            .checked_add(rep)
            .ok_or("pattern repeat bar overflow")?;
        let bar_start = clock.quarter_notes_at(&BarBeat::new(bar, 1, 0))?;
        let rendered = base + (bar_start - first_bar_start);
        if rendered >= cut - 1e-9 {
            break;
        }
        kept += 1;
        last_bar_start = Some(rendered);
    }
    let mut changed = kept != pattern.repeat;
    // The validator rejects a zero repeat, so an empty cut keeps one empty bar.
    pattern.repeat = kept.max(1);
    let Some(last_bar_start) = last_bar_start else {
        changed |= !pattern.hits.is_empty();
        pattern.hits.clear();
        return Ok(changed);
    };
    changed |= trim_hits(&mut pattern.hits, 0.0, cut - last_bar_start)?;
    Ok(changed)
}

/// Trim a clip's pattern when its content crosses the rendered span end.
fn trim_pattern_def(def: &PatternDef, base: f64, cut: f64) -> Result<Option<PatternDef>, String> {
    let mut copy = def.clone();
    let notes = trim_notes(&mut copy.events, base, cut);
    let pedals = trim_pedals(&mut copy.pedals, base, cut);
    let hits = trim_hits(&mut copy.hits, base, cut)?;
    Ok((notes || pedals || hits).then_some(copy))
}

fn note_gate(event: &NoteEvent) -> Option<f64> {
    if event.muted || event.modifiers.iter().any(|(key, _)| key == "rest") {
        return None;
    }
    match performance_gate_quarters(&event.modifiers) {
        Ok(Some(gate)) => Some(gate),
        _ => Some(if event.grace {
            0.125
        } else {
            event.duration.to_beats()
        }),
    }
}

fn set_gate(modifiers: &mut Vec<(String, ParamValue)>, gate: f64) {
    for (key, value) in modifiers.iter_mut() {
        if key == "gate_quarters" {
            *value = ParamValue::Number(gate);
            return;
        }
    }
    modifiers.push(("gate_quarters".to_string(), ParamValue::Number(gate)));
}

fn unique_name(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}:{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// A tempo-follow clip derives its stretch from its range, so clamping the range
/// would re-time the media instead of trimming the placement.
fn tempo_follows_range(clip: &AudioClip) -> bool {
    clip.tempo_follow && !clip.bypass_processing && clip.stretch_mode != StretchMode::Off
}

/// Rendered arrangement length in quarter notes, or None without sections.
pub fn rendered_end_quarter(project: &Project) -> Result<Option<f64>, String> {
    let arrangement = Arrangement::from_project(project)?;
    Ok((!arrangement.is_empty()).then_some(arrangement.rendered_end()))
}

/// Rendered arrangement length in seconds, or None without sections.
///
/// The clock is the one the arrangement plays: a map point repeated with its
/// section must not be read at its written position.
pub fn rendered_end_seconds(project: &Project) -> Result<Option<f64>, String> {
    let Some(quarter) = rendered_end_quarter(project)? else {
        return Ok(None);
    };
    Ok(Some(rendered_clock(project)?.seconds_at_quarter(quarter)?))
}

/// Rendered arrangement length in samples, or None without sections.
pub fn rendered_end_sample(project: &Project, sample_rate: u32) -> Result<Option<u64>, String> {
    let Some(quarter) = rendered_end_quarter(project)? else {
        return Ok(None);
    };
    Ok(Some(
        rendered_clock(project)?.sample_at_quarter(quarter, sample_rate)?,
    ))
}

/// The clock the arrangement plays: repeated tempo and meter map points move to
/// the quarters the expansion puts them on.
pub fn rendered_clock(project: &Project) -> Result<ProjectClock, String> {
    let clock = ProjectClock::new(&project.properties)?;
    let arrangement = Arrangement::from_project(project)?;
    if arrangement.is_identity(project, &clock)? {
        return Ok(clock);
    }
    let (_, time_sig_map, tempo_map) = expand_maps(project, &arrangement, &clock)?;
    let mut properties = project.properties.clone();
    properties.time_sig_map = time_sig_map;
    properties.tempo_map = tempo_map;
    ProjectClock::new(&properties)
}

/// Written end of the project in quarter notes: expanded sections plus the last
/// phrase, clip, pattern lane, performance event or audio clip.
///
/// Unlike [`rendered_end_quarter`] this is defined without sections, so callers
/// can report a project's length as soon as it opens.
pub fn content_end_quarter(project: &Project) -> Result<f64, String> {
    let expanded = expand(project)?;
    let clock = ProjectClock::new(&expanded.properties)?;
    let mut end = rendered_end_quarter(&expanded)?.unwrap_or(0.0);
    for track in &expanded.tracks {
        for phrase in &track.phrases {
            if let Some(range) = &phrase.time_range {
                end = end.max(clock.quarter_notes_at(&range.end)?);
            }
        }
        for clip in track.pattern_clips() {
            end = end.max(clock.quarter_notes_at(&clip.time_range.end)?);
        }
        for pattern in &track.patterns {
            let Some(range) = &pattern.time_range else {
                continue;
            };
            let end_bar = range
                .start
                .bar
                .checked_add(pattern.repeat)
                .ok_or("pattern repeat bar overflow")?;
            end = end.max(clock.quarter_notes_at(&BarBeat::new(end_bar, 1, 0))?);
        }
        if let Some(performance) = &track.performance {
            for event in &performance.events {
                end = end.max(event.quarter);
            }
        }
    }
    for clip in expanded.audio_clips() {
        let (_, end_seconds) = clock.audio_clip_range_seconds(clip)?;
        end = end.max(clock.quarter_at_seconds(end_seconds)?);
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPEATED: &str = r#"@project "repeated"
  tempo: 120
  time_signature: 4/4

@section body [1:1 - 3:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase body [1:1 - 3:1]
    C4 w mf
    D4 w mf
  automation: dynamics {1:2=0.2, 2:2=0.8}

@master
  gain_automation: linear [1:1 - 3:1] {1:1=0, 2:4=0.5}
"#;

    #[test]
    fn repeated_section_duplicates_and_shifts_every_timed_container() {
        let project = crate::parse(REPEATED).unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();

        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(16.0));
        assert_eq!(expanded.tracks[0].phrases.len(), 2);
        assert_eq!(
            expanded.tracks[0].phrases[0].time_range.unwrap().start,
            BarBeat::new(1, 1, 0)
        );
        assert_eq!(
            expanded.tracks[0].phrases[1].time_range.unwrap().start,
            BarBeat::new(3, 1, 0)
        );

        let points: Vec<_> = expanded.tracks[0]
            .automation_lane(0)
            .unwrap()
            .points
            .iter()
            .map(|point| point.position)
            .collect();
        assert_eq!(
            points,
            [
                BarBeat::new(1, 2, 0),
                BarBeat::new(2, 2, 0),
                BarBeat::new(3, 2, 0),
                BarBeat::new(4, 2, 0),
            ]
        );

        assert_eq!(expanded.sections.len(), 2);
        assert_eq!(expanded.sections[0].repeat, None);
        assert_eq!(
            expanded.sections[1].time_range,
            TimeRange {
                start: BarBeat::new(3, 1, 0),
                end: BarBeat::new(5, 1, 0),
            }
        );

        let master = expanded.master.unwrap();
        assert_eq!(master.gain_automation.len(), 2);
        assert_eq!(
            master.gain_automation[1].points[1].position,
            BarBeat::new(4, 4, 0)
        );
    }

    /// A clip is a container: a repeated section duplicates the span and the
    /// points together, so the seam is a jump back to the clip's opening value
    /// instead of a ramp between the two plays.
    #[test]
    fn a_repeated_section_duplicates_an_automation_clip_not_its_points() {
        let project = crate::parse(
            "@project \"clip\"\n  tempo: 120\n  time_signature: 4/4\n\n@section body [1:1 - 3:1]\n  repeat: 2\n\n@track lead | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [1:1 - 3:1] {1:2=0.2, 2:2=0.8}\n",
        )
        .unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();

        let lanes: Vec<_> = expanded.tracks[0].automation().collect();
        assert_eq!(lanes.len(), 2, "the clip is duplicated, not its points");
        assert_eq!(
            lanes[0].time_range,
            Some(TimeRange {
                start: BarBeat::new(1, 1, 0),
                end: BarBeat::new(3, 1, 0),
            })
        );
        assert_eq!(
            lanes[1].time_range,
            Some(TimeRange {
                start: BarBeat::new(3, 1, 0),
                end: BarBeat::new(5, 1, 0),
            })
        );
        let positions = |lane: &AutomationLane| {
            lane.points
                .iter()
                .map(|point| point.position)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            positions(lanes[0]),
            [BarBeat::new(1, 2, 0), BarBeat::new(2, 2, 0)]
        );
        assert_eq!(
            positions(lanes[1]),
            [BarBeat::new(3, 2, 0), BarBeat::new(4, 2, 0)]
        );

        let clock = ProjectClock::new(&expanded.properties).unwrap();
        let read = |lane: &AutomationLane, quarter: f64| {
            crate::automation::value_at(lane, &clock, 48_000, quarter, |value| {
                value.as_f64().ok_or_else(|| "not numeric".to_string())
            })
            .unwrap()
        };
        // The first play holds its last value to its exclusive end at 3:1.
        assert_eq!(read(lanes[0], 7.99), Some(0.8));
        assert_eq!(read(lanes[0], 8.0), None);
        // At the seam the second play writes its opening value, which is its
        // first point held from the clip head: no ramp across the seam.
        assert_eq!(read(lanes[1], 8.0), Some(0.2));
        assert_eq!(read(lanes[1], 13.0), Some(0.8));
    }

    #[test]
    fn a_track_gain_clip_repeats_with_the_section_that_holds_it() {
        let project = crate::parse(REPEATED).unwrap();
        let mut project = project;
        project.tracks[0].gain_automation = crate::parse(
            "@project \"ramp\"\n@track lead | instrument: piano/acoustic_grand\n  gain_automation: linear [1:1 - 2:1] {1:1=1, 1:4=0.5}\n",
        )
        .unwrap()
        .tracks[0]
            .gain_automation
            .clone();
        let expanded = expand(&project).unwrap();
        assert_eq!(expanded.tracks[0].gain_automation.len(), 2);
        assert_eq!(
            expanded.tracks[0].gain_automation[0].time_range.start,
            BarBeat::new(1, 1, 0)
        );
        assert_eq!(
            expanded.tracks[0].gain_automation[1].time_range.start,
            BarBeat::new(3, 1, 0)
        );
        assert_eq!(
            expanded.tracks[0].gain_automation[1].points[1].position,
            BarBeat::new(3, 4, 0)
        );
        // The moved clip ends where the second play of the section does.
        assert_eq!(
            expanded.tracks[0].gain_automation[1].time_range.end,
            BarBeat::new(4, 1, 0)
        );
    }

    #[test]
    fn audio_clip_placement_selects_sections_and_cuts_in_rendered_tempo() {
        let project = crate::parse(
            r#"@project
  tempo: 120
  tempo_map:
    3:1 = 60
@section verse [3:1 - 5:1]
  repeat: 2
@audio_clip shifted_in [1:1 - 1:2] | track: tape
  file: "take.wav"
  placement_start: 4.25s
  placement_end: 13.5s
@audio_clip shifted_out [3:2 - 3:3] | track: tape
  file: "take.wav"
  placement_start: -2s
  placement_end: -1s
@track tape | instrument: audio/clip
"#,
        )
        .unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        // The distant written anchor is outside the section, but its actual
        // start is inside. Conversely shifted_out must not be duplicated.
        assert_eq!(expanded.audio_clip_count(), 2);
        assert!(
            expanded
                .audio_clips()
                .iter()
                .all(|clip| clip.name == "shifted_in")
        );
        let clock = ProjectClock::new(&expanded.properties).unwrap();
        let first = expanded.audio_clip(0).unwrap();
        let second = expanded.audio_clip(1).unwrap();
        assert_eq!(clock.audio_clip_range_seconds(first).unwrap(), (4.25, 12.0));
        assert_eq!(
            clock.audio_clip_range_seconds(second).unwrap(),
            (12.25, 22.0)
        );
        assert_eq!(first.placement.end_offset_seconds, 0.0);
        assert_eq!(clock.quarter_notes_at(&first.time_range.end).unwrap(), 16.0);
    }

    #[test]
    fn repeated_section_duplicates_audio_clips() {
        let mut project = crate::parse(REPEATED).unwrap();
        project
            .add_audio_clip(AudioClip {
                name: "take".into(),
                track: "lead".into(),
                file: "take.wav".into(),
                time_range: TimeRange {
                    start: BarBeat::new(1, 1, 0),
                    end: BarBeat::new(3, 1, 0),
                },
                placement: Default::default(),
                offset_seconds: 0.0,
                gain_db: 0.0,
                fade_in_seconds: 0.0,
                fade_out_seconds: 0.0,
                invert: false,
                crossfade_seconds: 0.0,
                stretch: 1.0,
                stretch_mode: crate::ast::StretchMode::Musical,
                tempo_follow: false,
                pitch: 0.0,
                formant: 0.0,
                bypass_processing: false,
                preserve_formants: false,
                warp_markers: Vec::new(),
                loop_seconds: 0.0,
                take_group: None,
                take: None,
                muted: false,
                gain_envelope: Vec::new(),
            })
            .unwrap();
        let expanded = expand(&project).unwrap();
        assert_eq!(expanded.audio_clip_count(), 2);
        assert_eq!(
            expanded.audio_clip(1).unwrap().time_range.start,
            BarBeat::new(3, 1, 0)
        );
    }

    #[test]
    fn inline_patterns_keep_the_first_play_and_gain_clips_for_later_plays() {
        let mut project = crate::parse(
            "@project\n  tempo: 120\n@section body [1:1 - 3:1]\n  repeat: 2\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:1]\n    C4 q mf\n",
        )
        .unwrap();
        let phrase = project.tracks[0].phrases.remove(0);
        let instrument = project.tracks[0].instrument.clone();
        project.tracks[0].inline_patterns.push(PatternDef {
            name: "riff".into(),
            instrument,
            preset: None,
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: Vec::new(),
            events: phrase.events,
            pedals: phrase.pedals,
            hairpins: Vec::new(),
            hits: Vec::new(),
            color: None,
        });
        let expanded = expand(&project).unwrap();
        assert_eq!(expanded.tracks[0].inline_patterns.len(), 1);
        assert_eq!(expanded.patterns.len(), 1);
        assert_eq!(expanded.tracks[0].pattern_clip_count(), 1);
        assert_eq!(
            expanded.tracks[0].pattern_clip(0).unwrap().pattern,
            expanded.patterns[0].name
        );
        assert_eq!(expanded.patterns[0].events[0].start_beat, 0.0);
    }

    /// A repeated verse holding a tempo change, then an outro at a new tempo.
    const REPEAT_TEMPO: &str = r#"@project "repeat tempo"
  tempo: 120
  tempo_map:
    3:1 = 240
    7:1 = 60

@section verse [1:1 - 5:1]
  repeat: 2

@section outro [5:1 - 9:1]

@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 2:1]
    C4 w mf
"#;

    #[test]
    fn the_extent_reads_the_clock_the_repeated_section_plays() {
        let project = crate::parse(REPEAT_TEMPO).unwrap();
        crate::validate(&project).unwrap();
        let quarter = rendered_end_quarter(&project).unwrap().unwrap();
        assert_eq!(quarter, 48.0);
        // The verse repeat plays its tempo change twice and the outro point
        // lands 8 quarters later: 20 s played, 32 s read on the written map.
        assert_eq!(rendered_end_seconds(&project).unwrap(), Some(20.0));
        assert_eq!(
            rendered_end_sample(&project, 48_000).unwrap(),
            Some(960_000)
        );
        let written = crate::timing::ProjectClock::new(&project.properties).unwrap();
        assert_eq!(written.seconds_at_quarter(quarter).unwrap(), 32.0);
    }

    /// The showcase's shape: a repeated verse before the tempo and meter maps.
    const REPEATED_CLOCK: &str = r#"@project "repeated clock"
  tempo: 100
  time_signature: 4/4
  tempo_map:
    7:1 = 88
    9:1 = 104
  time_sig_map:
    9:1 = 3/4

@section a [1:1 - 3:1]
@section b [3:1 - 5:1]
  repeat: 2
@section c [5:1 - 7:1]
@section d [7:1 - 9:1]
@section e [9:1 - 13:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 13:1]
    C4 w mf
"#;

    #[test]
    fn a_repeated_section_replays_the_tempo_and_meter_changes_it_holds() {
        let project = crate::parse(REPEATED_CLOCK).unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(64.0));
        let clock = crate::timing::ProjectClock::new(&expanded.properties).unwrap();
        // The verse repeat pushes written bar 7 to rendered bar 9, so the 88 BPM
        // point starts there instead of a section early.
        assert_eq!(clock.tempo_at_quarter(24.0).unwrap(), 100.0);
        assert_eq!(clock.tempo_at_quarter(32.0).unwrap(), 88.0);
        assert_eq!(clock.meter_at_bar(10).unwrap().numerator, 4);
        // The 3/4 and 104 BPM points written at 9:1 replay with both plays of
        // the repeated outro: rendered bars 11 and 15.
        assert_eq!(clock.meter_at_bar(11).unwrap().numerator, 3);
        assert_eq!(clock.meter_at_bar(15).unwrap().numerator, 3);
        assert_eq!(clock.tempo_at_quarter(40.0).unwrap(), 104.0);
        assert_eq!(clock.tempo_at_quarter(52.0).unwrap(), 104.0);
        // 32 quarters at 100 BPM, 8 at 88, then 24 at 104.
        let seconds = clock.seconds_at_quarter(64.0).unwrap();
        assert!((seconds - 38.5007).abs() < 0.001, "{seconds} s");
        // The rendered length uses the same clock a render allocates for.
        assert_eq!(
            rendered_end_sample(&project, 48_000).unwrap(),
            Some((seconds * 48_000.0) as u64)
        );
        assert_eq!(
            rendered_end_sample(&expanded, 48_000).unwrap(),
            Some((seconds * 48_000.0) as u64)
        );
    }

    /// The reported defect: two tempo points inside one repeated section.
    const TWO_TEMPO_POINTS: &str = r#"@project "two tempo points"
  tempo: 60
  time_signature: 4/4
  tempo_map:
    2:1 = 60
    3:1 = 120

@section verse [1:1 - 5:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 5:1]
    C4 w mf
"#;

    #[test]
    fn two_tempo_points_inside_one_repeated_section_keep_rendered_order() {
        let project = crate::parse(TWO_TEMPO_POINTS).unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        crate::validate(&expanded).unwrap();
        let bpm: Vec<f64> = expanded
            .properties
            .tempo_map
            .iter()
            .map(|point| point.bpm)
            .collect();
        assert_eq!(bpm, vec![60.0, 120.0, 60.0, 120.0]);
        let clock = ProjectClock::new(&expanded.properties).unwrap();
        let quarters: Vec<f64> = expanded
            .properties
            .tempo_map
            .iter()
            .map(|point| clock.quarter_notes_at(&point.position).unwrap())
            .collect();
        assert_eq!(quarters, vec![4.0, 8.0, 20.0, 24.0]);
        // The written section is 12 s; two plays replay both points. Each play
        // starts at the tempo the first one left, hence 22 s, not 24 s.
        let written = ProjectClock::new(&project.properties).unwrap();
        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(32.0));
        assert_eq!(written.seconds_at_quarter(16.0).unwrap(), 12.0);
        assert_eq!(rendered_end_seconds(&project).unwrap(), Some(22.0));
    }

    /// The same section with the tempo at its start written out and replayed.
    const PER_PLAY_TEMPO: &str = r#"@project "per play tempo"
  tempo: 60
  time_signature: 4/4
  tempo_map:
    1:1 = 60
    2:1 = 60
    3:1 = 120

@section verse [1:1 - 5:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 5:1]
    C4 w mf
"#;

    #[test]
    fn every_play_replays_a_tempo_point_at_its_own_start() {
        let project = crate::parse(PER_PLAY_TEMPO).unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        crate::validate(&expanded).unwrap();
        let bpm: Vec<f64> = expanded
            .properties
            .tempo_map
            .iter()
            .map(|point| point.bpm)
            .collect();
        assert_eq!(bpm, vec![60.0, 60.0, 120.0, 60.0, 60.0, 120.0]);
        // Two plays of 2 bars at 60 and 2 bars at 120: 12 s each, 24 s total.
        assert_eq!(rendered_end_seconds(&project).unwrap(), Some(24.0));
        let clock = crate::timing::ProjectClock::new(&expanded.properties).unwrap();
        assert_eq!(clock.tempo_at_quarter(16.0).unwrap(), 60.0);
        assert_eq!(clock.tempo_at_quarter(24.0).unwrap(), 120.0);
    }

    /// Two meter points inside one repeated section, both starting a bar.
    const TWO_METER_POINTS: &str = r#"@project "two meter points"
  tempo: 120
  time_signature: 4/4
  time_sig_map:
    2:1 = 3/4
    3:1 = 4/4

@section verse [1:1 - 5:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 5:1]
    C4 w mf
"#;

    #[test]
    fn two_meter_points_inside_one_repeated_section_start_bars_in_rendered_order() {
        let project = crate::parse(TWO_METER_POINTS).unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        crate::validate(&expanded).unwrap();
        let meters: Vec<(u32, u8, u8)> = expanded
            .properties
            .time_sig_map
            .iter()
            .map(|point| (point.position.bar, point.numerator, point.denominator))
            .collect();
        assert_eq!(meters, vec![(2, 3, 4), (3, 4, 4), (6, 3, 4), (7, 4, 4)]);
        let clock = crate::timing::ProjectClock::new(&expanded.properties).unwrap();
        assert_eq!(clock.meter_at_bar(6).unwrap().numerator, 3);
        assert_eq!(clock.meter_at_bar(7).unwrap().numerator, 4);
        // 4 written bars become 8 rendered bars, still 15 s at 120 BPM.
        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(30.0));
        assert_eq!(rendered_end_seconds(&project).unwrap(), Some(15.0));
    }

    #[test]
    fn a_repeated_meter_point_that_cannot_start_a_bar_names_its_play() {
        let source = TWO_METER_POINTS.replace(
            "  time_sig_map:\n    2:1 = 3/4\n    3:1 = 4/4",
            "  time_sig_map:\n    2:1 = 4/4\n    3:1 = 3/4",
        );
        let project = crate::parse(&source).unwrap();
        crate::validate(&project).unwrap();
        let error = expand(&project).unwrap_err();
        assert!(error.contains("\"verse\""), "{error}");
        assert!(error.contains("play 2 of 2"), "{error}");
        assert!(error.contains("2:1"), "{error}");
        assert!(error.contains("does not start a bar"), "{error}");
    }

    #[test]
    fn a_repeated_drum_pattern_keeps_a_shifted_span() {
        let project = crate::parse(
            r#"@project "kit repeat"
  tempo: 120

@section a [1:1 - 3:1]
@section b [3:1 - 5:1]
  repeat: 2

@track drums | instrument: drums/acoustic_kit
  @pattern groove [3:1 - 5:1] x2
    kick 3:0 mf
"#,
        )
        .unwrap();
        crate::validate(&project).unwrap();
        let expanded = expand(&project).unwrap();
        crate::validate(&expanded).unwrap();
        let patterns = &expanded.tracks[0].patterns;
        assert_eq!(patterns.len(), 2);
        assert_eq!(
            patterns[0].time_range.unwrap(),
            TimeRange {
                start: BarBeat::new(3, 1, 0),
                end: BarBeat::new(5, 1, 0),
            }
        );
        assert_eq!(
            patterns[1].time_range.unwrap(),
            TimeRange {
                start: BarBeat::new(5, 1, 0),
                end: BarBeat::new(7, 1, 0),
            }
        );
    }

    #[test]
    fn contiguous_single_play_sections_are_a_no_op() {
        let project = crate::parse(
            "@project\n  tempo: 120\n@section a [1:1 - 3:1]\n@section b [3:1 - 5:1]\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 5:1]\n    C4 w mf\n",
        )
        .unwrap();
        assert_eq!(expand(&project).unwrap(), project);
    }

    #[test]
    fn content_starting_outside_sections_is_skipped() {
        let project = crate::parse(
            "@project\n  tempo: 120\n  swing: 50%\n@section late [3:1 - 5:1]\n@track t | instrument: piano/acoustic_grand\n  @phrase early [1:1 - 3:1]\n    C4 w mf\n  @phrase late [3:1 - 5:1]\n    D4 w mf\n",
        )
        .unwrap();
        let expanded = expand(&project).unwrap();
        assert_eq!(expanded.tracks[0].phrases.len(), 1);
        assert_eq!(expanded.tracks[0].phrases[0].name, "late");
        // The arrangement is anchored at the first section's written start.
        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(16.0));
    }

    #[test]
    fn validator_rejects_overlapping_or_out_of_order_sections() {
        let project = crate::parse(
            "@project\n  tempo: 120\n@section a [3:1 - 5:1]\n@section b [1:1 - 3:1]\n",
        )
        .unwrap();
        let errors = crate::validate(&project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errors.contains("ordered and must not overlap"));
    }

    fn gate(event: &NoteEvent) -> Option<f64> {
        performance_gate_quarters(&event.modifiers).unwrap()
    }

    /// A whole note on bar 2 beat 4 of a two-bar section, ringing one bar past it.
    const RINGING: &str = r#"@project "ringing"
  tempo: 120

@section body [1:1 - 3:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase body [1:1 - 3:1]
    @7 C4 w mf
"#;

    #[test]
    fn a_note_ringing_past_a_repeat_seam_is_released_at_the_seam() {
        let project = crate::parse(RINGING).unwrap();
        let expanded = expand(&project).unwrap();
        let phrases = &expanded.tracks[0].phrases;
        assert_eq!(phrases.len(), 2);
        let note = &phrases[0].events[0];
        // Written start stays at bar 2 beat 4; the note is cut at the seam.
        assert_eq!(note.start_beat, 7.0);
        assert_eq!(gate(note), Some(1.0));
        assert_eq!(phrases[0].events.len(), 1);
        // The last play ends the arrangement: nothing follows, so it rings out.
        assert_eq!(gate(&phrases[1].events[0]), None);
        assert_eq!(rendered_end_quarter(&project).unwrap(), Some(16.0));
    }

    #[test]
    fn a_note_starting_in_a_cut_region_is_not_played_there() {
        let source = r#"@project "late"
  tempo: 120

@section body [1:1 - 3:1]
  repeat: 2

@track lead | instrument: piano/acoustic_grand
  @phrase body [1:1 - 5:1]
    @9 D4 w mf
"#;
        let project = crate::parse(source).unwrap();
        let expanded = expand(&project).unwrap();
        // The phrase starts inside the first play, so it is assigned there; the
        // event starts past the seam and must not sound in that play.
        assert_eq!(expanded.tracks[0].phrases.len(), 2);
        assert!(expanded.tracks[0].phrases[0].events.is_empty());
        // The final play has no cut, so content past its end rings into the tail.
        assert_eq!(expanded.tracks[0].phrases[1].events.len(), 1);
    }

    #[test]
    fn contiguous_sections_keep_a_release_that_continues_the_written_timeline() {
        let project = crate::parse(
            r#"@project "tiling"
  tempo: 120
@section a [1:1 - 3:1]
@section b [3:1 - 5:1]
@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 5:1]
    @7 C4 w mf
"#,
        )
        .unwrap();
        // Written time continues across 3:1, so the release is the authored tail.
        assert_eq!(expand(&project).unwrap(), project);
    }

    #[test]
    fn a_repeated_section_hands_its_last_play_to_the_next_written_section() {
        let project = crate::parse(
            r#"@project "handover"
  tempo: 120
@section verse [1:1 - 3:1]
  repeat: 2
@section chorus [3:1 - 5:1]
@track lead | instrument: piano/acoustic_grand
  @phrase line [1:1 - 3:1]
    @7 C4 w mf
"#,
        )
        .unwrap();
        let expanded = expand(&project).unwrap();
        let phrases = &expanded.tracks[0].phrases;
        assert_eq!(phrases.len(), 2);
        // verse -> verse is a cut; the last verse hands over to chorus as written.
        assert_eq!(gate(&phrases[0].events[0]), Some(1.0));
        assert_eq!(gate(&phrases[1].events[0]), None);
    }

    #[test]
    fn a_clip_pattern_crossing_a_repeat_seam_is_cut_without_touching_the_shared_pattern() {
        let mut project = crate::parse(RINGING).unwrap();
        let phrase = project.tracks[0].phrases.remove(0);
        let instrument = project.tracks[0].instrument.clone();
        project.patterns.push(PatternDef {
            name: "line".into(),
            instrument: instrument.clone(),
            preset: None,
            engine: None,
            transpose: None,
            swing: None,
            bars: None,
            effects: Vec::new(),
            events: phrase.events,
            pedals: phrase.pedals,
            hairpins: Vec::new(),
            hits: Vec::new(),
            color: None,
        });
        project.tracks[0].push_pattern_clip(Clip {
            pattern: "line".into(),
            time_range: TimeRange {
                start: BarBeat::new(1, 1, 0),
                end: BarBeat::new(3, 1, 0),
            },
            offset_quarters: 0.0,
            loop_quarters: 0.0,
            id: None,
        });
        let expanded = expand(&project).unwrap();
        assert_eq!(expanded.tracks[0].pattern_clip_count(), 2);
        // The authored pattern is untouched; a cut play gets its own copy.
        assert_eq!(gate(&expanded.patterns[0].events[0]), None);
        assert_eq!(expanded.patterns[0].name, "line");
        let first = expanded.tracks[0].pattern_clip(0).unwrap();
        let second = expanded.tracks[0].pattern_clip(1).unwrap();
        assert_ne!(first.pattern, "line");
        let def = expanded
            .patterns
            .iter()
            .find(|def| def.name == first.pattern)
            .unwrap();
        assert_eq!(gate(&def.events[0]), Some(1.0));
        // The final play ends the arrangement, so it keeps the authored pattern.
        assert_eq!(second.pattern, "line");
    }

    #[test]
    fn a_repeated_drum_pattern_stops_at_a_repeat_seam() {
        let source = r#"@project "drums"
  tempo: 120

@section body [1:1 - 3:1]
  repeat: 2

@track kit | instrument: drums/acoustic_kit
  @pattern groove [1:1 - 2:1] x4
    kick 4:0 mf gate_quarters=2
"#;
        let project = crate::parse(source).unwrap();
        let expanded = expand(&project).unwrap();
        let patterns = &expanded.tracks[0].patterns;
        assert_eq!(patterns.len(), 2);
        // Bars 3 and 4 start at or after the seam, so the cut play keeps two.
        assert_eq!(patterns[0].repeat, 2);
        assert_eq!(patterns[0].hits.len(), 1);
        assert_eq!(
            performance_gate_quarters(&patterns[0].hits[0].modifiers).unwrap(),
            Some(1.0)
        );
        // The final play ends the arrangement: the authored repeat and gate hold.
        assert_eq!(patterns[1].repeat, 4);
        assert_eq!(
            performance_gate_quarters(&patterns[1].hits[0].modifiers).unwrap(),
            Some(2.0)
        );
    }

    #[test]
    fn an_audio_clip_is_truncated_at_a_repeat_seam() {
        let project = crate::parse(
            r#"@project "tape"
  tempo: 120
@section body [1:1 - 3:1]
  repeat: 3
@audio_clip take [1:1 - 5:1] | track: tape
  file: "take.wav"
@audio_clip slow [1:1 - 5:1] | track: tape
  file: "slow.wav"
  tempo_follow: true
@track tape | instrument: audio/clip
"#,
        )
        .unwrap();
        let expanded = expand(&project).unwrap();
        let clips = expanded.audio_clips();
        assert_eq!(clips.len(), 6);
        // `take` is trimmed to each cut play's span and rings out in the last.
        assert_eq!(clips[0].time_range.end, BarBeat::new(3, 1, 0));
        assert_eq!(clips[1].time_range.start, BarBeat::new(3, 1, 0));
        assert_eq!(clips[1].time_range.end, BarBeat::new(5, 1, 0));
        assert_eq!(clips[2].time_range.start, BarBeat::new(5, 1, 0));
        assert_eq!(clips[2].time_range.end, BarBeat::new(9, 1, 0));
        // A tempo-follow clip derives its length from the range, so clamping the
        // range would re-time the media; it keeps the authored placement.
        assert_eq!(clips[3].time_range.end, BarBeat::new(5, 1, 0));
        assert_eq!(clips[4].time_range.start, BarBeat::new(3, 1, 0));
        assert_eq!(clips[4].time_range.end, BarBeat::new(7, 1, 0));
        assert_eq!(clips[5].time_range.start, BarBeat::new(5, 1, 0));
        assert_eq!(clips[5].time_range.end, BarBeat::new(9, 1, 0));
    }

    #[test]
    fn a_repeated_section_moves_pedal_marks_with_its_phrase() {
        let project = crate::parse(
            r#"@project "pedal repeat"
  tempo: 120

@section body [1:1 - 3:1]
  repeat: 2

@track piano | instrument: piano/acoustic_grand
  @phrase line [1:1 - 2:1]
    @0 pedal down
    C4 w mf
    @3 pedal up
"#,
        )
        .unwrap();
        let phrases = expand(&project).unwrap().tracks.remove(0).phrases;
        assert_eq!(phrases.len(), 2);
        let marks: Vec<f64> = phrases[1].pedals.iter().map(|pedal| pedal.beat).collect();
        assert_eq!(marks, vec![0.0, 3.0]);
        assert!(!phrases[1].pedals[1].down);
    }

    #[test]
    fn a_cut_section_lifts_a_pedal_still_down_at_its_seam() {
        let project = crate::parse(
            r#"@project "pedal cut"
  tempo: 120

@section body [1:1 - 2:1]
  repeat: 2

@track piano | instrument: piano/acoustic_grand
  @phrase line [1:1 - 3:1]
    @0 pedal down
    C4 w mf
    @7 pedal up
"#,
        )
        .unwrap();
        let phrases = expand(&project).unwrap().tracks.remove(0).phrases;
        // The first play is cut at the seam and gets an explicit lift there.
        let marks: Vec<(f64, bool)> = phrases[0]
            .pedals
            .iter()
            .map(|pedal| (pedal.beat, pedal.down))
            .collect();
        assert_eq!(marks, vec![(0.0, true), (4.0, false)]);
    }
}
