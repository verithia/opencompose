//! SMF → project import.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use midly::{MetaMessage, MidiMessage, TrackEventKind};
use oc_dsl::ast::*;
use oc_dsl::timing::ProjectClock;

use super::{
    IgnoredEvent, ImportOptions, ImportResult, ImportStage, ImportSummary, ImportTrackSummary,
    MidiError, TrackMapping, TrackTarget,
};
use super::{ImportLimit, ImportLimits, gm, smf};
use std::cell::Cell;

pub(crate) const TARGET_PPQ: u16 = 480;

#[path = "dataset.rs"]
mod dataset;

/// Absolute fractions where each import stage starts and ends.
pub(crate) const PARSE_END: f64 = 0.15;
const READ_END: f64 = 0.40;
const TRACK_END: f64 = 0.50;
const CLOCK_END: f64 = 0.58;
const NOTES_END: f64 = 0.90;
const PARAMS_END: f64 = 0.98;
const PROGRESS_STRIDE: usize = 256;

/// Cancellation flag plus monotonic fractional progress for one import run.
pub(crate) struct Progress<'a> {
    cancel: &'a AtomicBool,
    emit: &'a mut dyn FnMut(ImportStage, f64),
    fraction: f64,
    limits: ImportLimits,
    work: Cell<u64>,
    output_events: Cell<u64>,
    output_tracks: Cell<u64>,
    metadata: Cell<u64>,
}

impl<'a> Progress<'a> {
    fn new(
        cancel: &'a AtomicBool,
        emit: &'a mut dyn FnMut(ImportStage, f64),
        limits: ImportLimits,
    ) -> Self {
        Self {
            cancel,
            emit,
            fraction: 0.0,
            limits,
            work: Cell::new(0),
            output_events: Cell::new(0),
            output_tracks: Cell::new(0),
            metadata: Cell::new(0),
        }
    }

    pub(crate) fn check(&self) -> Result<(), MidiError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(MidiError::Cancelled)
        } else {
            self.charge_work(1)
        }
    }

    pub(crate) fn limits(&self) -> ImportLimits {
        self.limits
    }
    pub(crate) fn charge_work(&self, amount: u64) -> Result<(), MidiError> {
        let work = self.work.get().saturating_add(amount);
        ImportLimits::enforce(ImportLimit::Work, self.limits.max_work, work, 0)?;
        self.work.set(work);
        Ok(())
    }
    pub(crate) fn charge_output(&self, amount: u64, offset: usize) -> Result<(), MidiError> {
        let count = self.output_events.get().saturating_add(amount);
        ImportLimits::enforce(
            ImportLimit::OutputEvents,
            self.limits.max_output_events as u64,
            count,
            offset,
        )?;
        self.output_events.set(count);
        Ok(())
    }
    pub(crate) fn charge_metadata(&self, amount: u64, offset: usize) -> Result<(), MidiError> {
        let count = self.metadata.get().saturating_add(amount);
        ImportLimits::enforce(
            ImportLimit::Metadata,
            self.limits.max_metadata_bytes as u64,
            count,
            offset,
        )?;
        self.metadata.set(count);
        Ok(())
    }
    fn charge_track(&self) -> Result<(), MidiError> {
        let count = self.output_tracks.get().saturating_add(1);
        ImportLimits::enforce(
            ImportLimit::OutputTracks,
            self.limits.max_output_tracks as u64,
            count,
            0,
        )?;
        self.output_tracks.set(count);
        Ok(())
    }
    fn charge_sort(&self, len: usize) -> Result<(), MidiError> {
        let comparisons = (len as u64).saturating_mul(u64::from(usize::BITS - len.leading_zeros()));
        self.charge_work(comparisons)
    }

    pub(crate) fn stage(&mut self, stage: ImportStage, fraction: f64) {
        let fraction = fraction.clamp(self.fraction, 1.0);
        self.fraction = fraction;
        (self.emit)(stage, fraction);
    }

    pub(crate) fn span(
        &mut self,
        stage: ImportStage,
        start: f64,
        end: f64,
        done: usize,
        total: usize,
    ) {
        let ratio = if total == 0 {
            1.0
        } else {
            (done as f64 / total as f64).min(1.0)
        };
        self.stage(stage, start + (end - start) * ratio);
    }
}

/// Import SMF bytes with default limits.
pub fn import_smf(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: Option<&Project>,
) -> Result<ImportResult, MidiError> {
    import_smf_cancellable(
        data,
        name,
        options,
        base,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}

/// Import with cooperative cancellation and fractional progress.
///
/// `cancel` is polled per event and per note. Progress is monotonic and ends at
/// `ImportStage::Commit` 1.0; cancellation returns `MidiError::Cancelled`.
pub fn import_smf_cancellable(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: Option<&Project>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportStage, f64),
) -> Result<ImportResult, MidiError> {
    import_smf_cancellable_with_limits(
        data,
        name,
        options,
        base,
        cancel,
        progress,
        &ImportLimits::default(),
    )
}

/// Import SMF bytes under explicit limits.
pub fn import_smf_with_limits(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: Option<&Project>,
    limits: &ImportLimits,
) -> Result<ImportResult, MidiError> {
    import_smf_cancellable_with_limits(
        data,
        name,
        options,
        base,
        &AtomicBool::new(false),
        &mut |_, _| {},
        limits,
    )
}

/// Import with cancellation/progress and explicit limits.
pub fn import_smf_cancellable_with_limits(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: Option<&Project>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportStage, f64),
    limits: &ImportLimits,
) -> Result<ImportResult, MidiError> {
    import_with_profile(
        data,
        name,
        options,
        base,
        cancel,
        progress,
        ImportConfiguration {
            profile: ImportProfile::Application,
            limits: *limits,
        },
    )
}

/// Dataset conversion shares the application parser and output builders while
/// retaining source-PPQ timing and explicit-release overlap interpretation.
pub fn import_smf_dataset(data: &[u8], name: Option<String>) -> Result<ImportResult, MidiError> {
    import_smf_dataset_with_limits(data, name, &ImportLimits::default())
}

/// Convert an SMF dataset under explicit limits.
pub fn import_smf_dataset_with_limits(
    data: &[u8],
    name: Option<String>,
    limits: &ImportLimits,
) -> Result<ImportResult, MidiError> {
    let options = ImportOptions {
        split_channels: true,
        expand_sustain: false,
        ..Default::default()
    };
    import_with_profile(
        data,
        name,
        &options,
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
        ImportConfiguration {
            profile: ImportProfile::Dataset,
            limits: *limits,
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportProfile {
    Application,
    Dataset,
}

struct ImportConfiguration {
    profile: ImportProfile,
    limits: ImportLimits,
}

fn import_with_profile(
    data: &[u8],
    name: Option<String>,
    options: &ImportOptions,
    base: Option<&Project>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ImportStage, f64),
    configuration: ImportConfiguration,
) -> Result<ImportResult, MidiError> {
    let ImportConfiguration { profile, limits } = configuration;
    let mut progress = Progress::new(cancel, progress, limits);
    ImportLimits::enforce(
        ImportLimit::Bytes,
        limits.max_bytes as u64,
        data.len() as u64,
        0,
    )?;
    ImportLimits::enforce(
        ImportLimit::OutputTracks,
        limits.max_output_tracks as u64,
        options.mappings.len() as u64,
        0,
    )?;
    let mut metadata = name.as_ref().map_or(0, |name| name.len());
    for mapping in &options.mappings {
        progress.check()?;
        metadata = metadata.saturating_add(mapping.name.as_ref().map_or(0, |name| name.len()));
    }
    progress.charge_metadata(metadata as u64, 0)?;
    if let Some(base) = base {
        ImportLimits::enforce(
            ImportLimit::OutputTracks,
            limits.max_output_tracks as u64,
            base.tracks.len() as u64,
            0,
        )?;
        progress.output_tracks.set(base.tracks.len() as u64);
        for track in &base.tracks {
            progress.check()?;
            for phrase in &track.phrases {
                progress.check()?;
                progress.charge_output(phrase.events.len() as u64, 0)?;
            }
            for pattern in &track.patterns {
                progress.check()?;
                progress.charge_output(pattern.hits.len() as u64, 0)?;
            }
        }
    }
    progress.stage(ImportStage::Parse, 0.0);
    let raw = smf::scan(data, &progress)?;
    if profile == ImportProfile::Dataset && raw.format == midly::Format::Sequential {
        return Err(MidiError::UnsupportedTiming {
            offset: 8,
            message: "MIDI type 2 contains asynchronous sequences; import each sequence separately"
                .into(),
        });
    }
    let parsed = smf::parse(data, &raw, &mut progress)?;
    progress.stage(ImportStage::Parse, PARSE_END);
    progress.check()?;

    let mut warnings = Vec::new();
    if raw.unknown_chunks != 0 {
        warnings.push(format!(
            "Ignored {} unknown SMF chunk(s)",
            raw.unknown_chunks
        ));
    }

    let mut tick_map = TickMap::with_target(
        raw.ppq,
        if profile == ImportProfile::Dataset {
            raw.ppq
        } else {
            TARGET_PPQ
        },
    );
    tick_map.preserve_dataset = profile == ImportProfile::Dataset;
    let mut clocks = ClockCollector::default();
    let mut tracks = Vec::with_capacity(parsed.tracks.len());
    let total_events = parsed.tracks.iter().map(Vec::len).sum::<usize>().max(1);
    let mut scanned_events = 0_usize;
    for (index, events) in parsed.tracks.iter().enumerate() {
        progress.check()?;
        let offset = raw
            .chunks
            .get(index)
            .map(|chunk| chunk.offset)
            .unwrap_or(data.len());
        let mut track = read_track(
            events,
            index,
            offset,
            &mut clocks,
            &mut warnings,
            &mut progress,
            &mut scanned_events,
            total_events,
            profile,
        )?;
        progress.check()?;
        if options.expand_sustain {
            track.sustain_expanded = expand_sustain(&mut track, &mut progress)?;
        }
        for (note_index, note) in track.notes.iter_mut().enumerate() {
            progress.check()?;
            if note.end_tick <= note.start_tick {
                note.end_tick = note.start_tick + 1;
                track.zero_length += 1;
            }
            if note_index.is_multiple_of(PROGRESS_STRIDE) {
                progress.span(
                    ImportStage::Tracks,
                    PARSE_END,
                    READ_END,
                    index + 1,
                    parsed.tracks.len().max(1),
                );
            }
        }
        tracks.push(track);
        progress.span(
            ImportStage::Tracks,
            PARSE_END,
            READ_END,
            index + 1,
            parsed.tracks.len().max(1),
        );
    }
    progress.stage(ImportStage::Tracks, READ_END);
    progress.check()?;

    if profile == ImportProfile::Dataset {
        dataset::validate_clocks(&clocks)?;
    }
    let units = make_units(&tracks, options, &mut tick_map, &mut progress)?;
    progress.stage(ImportStage::Tracks, TRACK_END);
    progress.check()?;

    let mut properties = base
        .map(|project| project.properties.clone())
        .unwrap_or_default();
    if options.import_clock {
        properties = build_properties(&clocks, &mut tick_map, &mut warnings, &mut progress)?;
    }
    progress.stage(ImportStage::Clock, CLOCK_END);
    progress.check()?;
    let clock = ProjectClock::new(&properties).map_err(|message| MidiError::InvalidEvent {
        offset: clocks.first_offset(),
        message,
    })?;

    let mut project = base.cloned().unwrap_or_else(|| Project {
        name: name.clone(),
        ocp_version: Some(oc_dsl::DSL_VERSION.to_string()),
        properties: properties.clone(),
        presets: Vec::new(),
        tracks: Vec::new(),
        patterns: Vec::new(),
        buses: Vec::new(),
        master: None,
        sections: Vec::new(),
        markers: Vec::new(),
        groups: Vec::new(),
    });
    project.properties = properties;
    if project.name.is_none() {
        project.name = name;
    }

    let is_parallel = matches!(raw.format, midly::Format::Parallel);
    let unit_count = units.len().max(1);
    let mut prepared = Vec::with_capacity(units.len());
    for (index, unit) in units.into_iter().enumerate() {
        progress.check()?;
        let start = CLOCK_END + (NOTES_END - CLOCK_END) * (index as f64 / unit_count as f64);
        let end = CLOCK_END + (NOTES_END - CLOCK_END) * ((index + 1) as f64 / unit_count as f64);
        progress.charge_work(options.mappings.len() as u64)?;
        let mapping = mapping_for(options, &unit).cloned();
        if mapping
            .as_ref()
            .is_some_and(|mapping| mapping.target == TrackTarget::Drop)
        {
            prepared.push(PreparedUnit::Dropped(unit.summary(&clock, None, true)));
            progress.stage(ImportStage::Notes, end);
            continue;
        }
        let conductor =
            !unit.channel_events && (unit.has_clock || (is_parallel && unit.source_track == 0));
        if conductor {
            prepared.push(PreparedUnit::Dropped(unit.summary(&clock, None, true)));
            progress.stage(ImportStage::Notes, end);
            continue;
        }
        if unit.notes.is_empty() && options.drop_empty {
            prepared.push(PreparedUnit::Dropped(unit.summary(&clock, None, true)));
            progress.stage(ImportStage::Notes, end);
            continue;
        }
        let notes_end = start + (end - start) * 0.75;
        let phrase = if unit.notes.is_empty() {
            None
        } else {
            Some(notes_to_phrase(
                &unit,
                &clock,
                "main".to_string(),
                &mut progress,
                start,
                notes_end,
            )?)
        };
        let lanes = build_lanes(&unit, &clock, &mut progress, notes_end, end)?;
        prepared.push(PreparedUnit::Kept {
            unit,
            mapping,
            phrase,
            lanes,
        });
        progress.stage(ImportStage::Notes, end);
    }
    progress.stage(ImportStage::Notes, NOTES_END);
    progress.check()?;

    // Resolve every existing-track target once, against the document this
    // import applies to. All units that land on one track share the offset, so
    // the file's internal timing survives even when several units append.
    let mut append_offsets: BTreeMap<usize, f64> = BTreeMap::new();
    for mapping in &options.mappings {
        let TrackTarget::ExistingTrack(target_index) = &mapping.target else {
            continue;
        };
        if append_offsets.contains_key(target_index) {
            continue;
        }
        let Some(track) = project.tracks.get(*target_index) else {
            return Err(MidiError::InvalidMapping {
                source_track: mapping.source_track,
                message: format!("existing track {target_index} does not exist"),
            });
        };
        let offset = track_content_end_quarter(track, &clock)?;
        append_offsets.insert(*target_index, offset);
    }

    let mut summaries = Vec::new();
    let mut used_names: BTreeSet<String> = project.tracks.iter().map(|t| t.name.clone()).collect();
    let prepared_count = prepared.len().max(1);
    for (index, prepared) in prepared.into_iter().enumerate() {
        progress.check()?;
        match prepared {
            PreparedUnit::Dropped(summary) => summaries.push(summary),
            PreparedUnit::Kept {
                unit,
                mapping,
                phrase,
                lanes,
            } => {
                if let Some(TrackTarget::ExistingTrack(target_index)) =
                    mapping.as_ref().map(|mapping| &mapping.target)
                {
                    let offset = append_offsets.get(target_index).copied().ok_or_else(|| {
                        MidiError::InvalidMapping {
                            source_track: unit.source_track,
                            message: format!("existing track {target_index} does not exist"),
                        }
                    })?;
                    let Some(target) = project.tracks.get_mut(*target_index) else {
                        return Err(MidiError::InvalidMapping {
                            source_track: unit.source_track,
                            message: format!("existing track {target_index} does not exist"),
                        });
                    };
                    let mut added = 0;
                    if let Some(phrase) = phrase {
                        let mut phrase = append_phrase(phrase, offset, &clock)?;
                        phrase.name = unique_phrase_name(target, &progress)?;
                        added = phrase.events.len();
                        target.phrases.push(phrase);
                    }
                    let mut merged = target.automation().cloned().collect::<Vec<_>>();
                    merge_lanes(&mut merged, shift_lanes(lanes, offset, &clock)?);
                    target.set_automation_lanes(merged);
                    let (name, preset, instrument) = (
                        target.name.clone(),
                        target.preset.clone(),
                        target.instrument.as_path(),
                    );
                    let mut summary = unit.summary(&clock, Some(*target_index), false);
                    summary.name = name;
                    summary.preset = preset;
                    summary.instrument = Some(instrument);
                    summary.note_count = added;
                    summaries.push(summary);
                } else {
                    let base_name = mapping
                        .and_then(|mapping| mapping.name.clone())
                        .unwrap_or_else(|| unit.default_name());
                    let track_name =
                        unique_name(&sanitize_identifier(&base_name), &used_names, &progress)?;
                    used_names.insert(track_name.clone());

                    progress.charge_work(project.presets.len() as u64)?;
                    let program_name = unit.program.map(gm::gm_program_name);
                    let preset = program_name
                        .and_then(|program| matching_preset(&project, program, &track_name));
                    let instrument = if unit.is_drum() {
                        InstrumentPath::new("drums", "acoustic_kit")
                    } else {
                        gm::gm_program_instrument(unit.program.unwrap_or(0))
                    };
                    let mut engine_params = Vec::new();
                    if let Some(program) = unit.program {
                        engine_params.push((
                            "midi_program".to_string(),
                            ParamValue::Number(f64::from(program)),
                        ));
                    }
                    if let Some(range) = unit.bend_range
                        && (range - 2.0).abs() > f64::EPSILON
                    {
                        engine_params
                            .push(("midi_bend_range".to_string(), ParamValue::Number(range)));
                    }

                    let mut track = Track {
                        name: track_name.clone(),
                        channel: unit.channel,
                        instrument,
                        preset,
                        engine: None,
                        mix: MixState::default(),
                        effects: Vec::new(),
                        phrases: Vec::new(),
                        patterns: Vec::new(),
                        inline_patterns: Vec::new(),
                        clips: {
                            let mut pool = oc_dsl::ast::ClipPool::new();
                            pool.set_automation_lanes(lanes);
                            pool
                        },
                        gain_automation: Vec::new(),
                        engine_params: (!engine_params.is_empty()).then_some(engine_params),
                        performance: None,
                        output_port: None,
                        output_pair: None,
                        input_routes: vec![],
                    };
                    if let Some(phrase) = phrase {
                        if unit.dataset && unit.is_drum() {
                            track.patterns.push(dataset::drum_pattern(&unit, &clock)?);
                        } else {
                            track.phrases.push(phrase);
                        }
                    }
                    let target = project.tracks.len();
                    project.tracks.push(track);
                    let mut summary = unit.summary(&clock, Some(target), false);
                    summary.name = track_name;
                    summary.preset = project.tracks[target].preset.clone();
                    summary.instrument = Some(project.tracks[target].instrument.as_path());
                    summaries.push(summary);
                }
            }
        }
        progress.span(
            ImportStage::Params,
            NOTES_END,
            PARAMS_END,
            index + 1,
            prepared_count,
        );
    }

    let summary = ImportSummary {
        format: match raw.format {
            midly::Format::SingleTrack => 0,
            midly::Format::Parallel => 1,
            midly::Format::Sequential => 2,
        },
        source_ppq: raw.ppq,
        target_ppq: tick_map.target_ppq,
        max_tick_error: tick_map.max_error,
        ticks_shifted: tick_map.shifted,
        tempo_changes: clocks.tempos.len(),
        meter_changes: clocks.meters.len(),
        key_changes: clocks.keys.len(),
        sustain_expanded_notes: tracks.iter().map(|track| track.sustain_expanded).sum(),
        overlapping_notes: tracks.iter().map(|track| track.overlapping).sum(),
        zero_length_notes: tracks.iter().map(|track| track.zero_length).sum(),
        unterminated_notes: tracks.iter().map(|track| track.unterminated).sum(),
        tracks: summaries,
        warnings,
    };

    progress.check()?;
    progress.stage(ImportStage::Commit, 1.0);
    Ok(ImportResult { project, summary })
}

// ============================================================================
// Source ticks
// ============================================================================

struct TickMap {
    source_ppq: u16,
    target_ppq: u16,
    preserve_dataset: bool,
    max_error: f64,
    shifted: usize,
}

impl TickMap {
    fn with_target(source_ppq: u16, target_ppq: u16) -> Self {
        Self {
            source_ppq,
            target_ppq,
            preserve_dataset: false,
            max_error: 0.0,
            shifted: 0,
        }
    }

    fn convert(&mut self, tick: u64) -> u64 {
        let source = u128::from(tick) * u128::from(self.target_ppq);
        let divisor = u128::from(self.source_ppq.max(1));
        let internal = ((source + divisor / 2) / divisor) as u64;
        let back = internal as f64 * f64::from(self.source_ppq) / f64::from(self.target_ppq);
        let error = (back - tick as f64).abs();
        if error > 1e-9 {
            self.shifted += 1;
            self.max_error = self.max_error.max(error);
        }
        internal
    }
}

// ============================================================================
// SMF → raw tracks
// ============================================================================

#[derive(Default)]
struct ClockCollector {
    tempos: Vec<ClockTempo>,
    meters: Vec<ClockMeter>,
    keys: Vec<ClockKey>,
}

struct ClockTempo {
    tick: u64,
    micros: u32,
    offset: usize,
}

struct ClockMeter {
    tick: u64,
    numerator: u8,
    denominator: u8,
    offset: usize,
}

struct ClockKey {
    tick: u64,
    sharps_flats: i8,
    minor: bool,
    offset: usize,
}

impl ClockCollector {
    fn first_offset(&self) -> usize {
        self.tempos
            .iter()
            .map(|event| event.offset)
            .chain(self.meters.iter().map(|event| event.offset))
            .chain(self.keys.iter().map(|event| event.offset))
            .min()
            .unwrap_or(8)
    }
}

#[derive(Clone)]
struct RawNote {
    channel: u8,
    start_tick: u64,
    end_tick: u64,
    key: u8,
    velocity: u8,
}

#[derive(Clone)]
struct RawCc {
    tick: u64,
    channel: u8,
    controller: u8,
    value: u8,
}

#[derive(Clone)]
struct RawBend {
    tick: u64,
    channel: u8,
    value: i16,
}

#[derive(Clone)]
struct RawPressure {
    tick: u64,
    channel: u8,
    value: u8,
}

#[derive(Default)]
struct RawTrack {
    source_track: usize,
    name: Option<String>,
    instrument_name: Option<String>,
    notes: Vec<RawNote>,
    ccs: Vec<RawCc>,
    bends: Vec<RawBend>,
    pressures: Vec<RawPressure>,
    programs: BTreeMap<u8, u8>,
    bend_ranges: BTreeMap<u8, f64>,
    ignored: BTreeMap<String, usize>,
    has_clock: bool,
    overlapping: usize,
    zero_length: usize,
    unterminated: usize,
    sustain_expanded: usize,
    end_tick: u64,
}

impl RawTrack {
    fn ignore(&mut self, kind: impl Into<String>) {
        *self.ignored.entry(kind.into()).or_default() += 1;
    }

    fn channels(&self) -> BTreeSet<u8> {
        self.notes
            .iter()
            .map(|note| note.channel)
            .chain(self.ccs.iter().map(|cc| cc.channel))
            .chain(self.bends.iter().map(|bend| bend.channel))
            .chain(self.pressures.iter().map(|event| event.channel))
            .chain(self.programs.keys().copied())
            .collect()
    }
}

// Pipeline context passed straight through from the caller.
#[allow(clippy::too_many_arguments)]
fn read_track(
    events: &[midly::TrackEvent<'_>],
    source_track: usize,
    chunk_offset: usize,
    clocks: &mut ClockCollector,
    warnings: &mut Vec<String>,
    progress: &mut Progress<'_>,
    scanned: &mut usize,
    total: usize,
    profile: ImportProfile,
) -> Result<RawTrack, MidiError> {
    let mut track = RawTrack {
        source_track,
        ..Default::default()
    };
    let mut tick = 0_u64;
    let mut active: BTreeMap<(u8, u8), Vec<(u64, u8)>> = BTreeMap::new();
    let mut rpn: BTreeMap<u8, (u8, u8)> = BTreeMap::new();

    for event in events {
        progress.check()?;
        *scanned += 1;
        if scanned.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Tracks, PARSE_END, READ_END, *scanned, total);
        }
        tick = tick
            .checked_add(u64::from(event.delta.as_int()))
            .ok_or_else(|| MidiError::InvalidEvent {
                offset: chunk_offset,
                message: "tick overflow".into(),
            })?;
        track.end_tick = track.end_tick.max(tick);
        match &event.kind {
            TrackEventKind::Midi { channel, message } => {
                let channel = channel.as_int();
                let control = rpn.entry(channel).or_insert((127, 127));
                match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() != 0 => {
                        let key = key.as_int();
                        let voices = active.entry((channel, key)).or_default();
                        if !voices.is_empty() {
                            track.overlapping += 1;
                            if profile == ImportProfile::Application {
                                for (start, velocity) in voices.drain(..) {
                                    close_note(&mut track, channel, key, start, velocity, tick);
                                }
                            }
                        }
                        voices.push((tick, vel.as_int()));
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        let key = key.as_int();
                        if let Some(voices) = active.remove(&(channel, key)) {
                            for (start, velocity) in voices {
                                close_note(&mut track, channel, key, start, velocity, tick);
                            }
                        } else {
                            track.ignore("note_off_without_note_on");
                        }
                    }
                    MidiMessage::Controller { controller, value } => {
                        let controller = controller.as_int();
                        let value = value.as_int();
                        match controller {
                            101 => control.0 = value,
                            100 => control.1 = value,
                            6 => {
                                if *control == (0, 0) {
                                    track.bend_ranges.insert(channel, f64::from(value.max(1)));
                                    *control = (127, 127);
                                } else {
                                    track.ignore("cc_6");
                                }
                            }
                            38 => {
                                if *control != (0, 0) {
                                    track.ignore("cc_38");
                                }
                            }
                            _ => {
                                if gm::cc_to_lane(controller).is_some() {
                                    track.ccs.push(RawCc {
                                        tick,
                                        channel,
                                        controller,
                                        value,
                                    });
                                } else {
                                    track.ignore(format!("cc_{controller}"));
                                }
                            }
                        }
                    }
                    MidiMessage::ProgramChange { program } => {
                        let program = program.as_int();
                        match track.programs.entry(channel) {
                            Entry::Vacant(entry) => {
                                entry.insert(program);
                            }
                            Entry::Occupied(_) => track.ignore("program_change"),
                        }
                    }
                    MidiMessage::PitchBend { bend } => {
                        let value = bend.0.as_int() as i32 - 8192;
                        track.bends.push(RawBend {
                            tick,
                            channel,
                            value: value as i16,
                        });
                    }
                    MidiMessage::ChannelAftertouch { vel } => {
                        track.pressures.push(RawPressure {
                            tick,
                            channel,
                            value: vel.as_int(),
                        });
                    }
                    MidiMessage::Aftertouch { .. } => track.ignore("poly_aftertouch"),
                }
            }
            TrackEventKind::Meta(message) => match message {
                MetaMessage::TrackName(bytes) => {
                    if track.name.is_none() {
                        track.name = decode_text(bytes);
                    }
                }
                MetaMessage::InstrumentName(bytes) => {
                    if track.instrument_name.is_none() {
                        track.instrument_name = decode_text(bytes);
                    }
                }
                MetaMessage::Tempo(micros) => {
                    track.has_clock = true;
                    clocks.tempos.push(ClockTempo {
                        tick,
                        micros: micros.as_int(),
                        offset: chunk_offset,
                    });
                }
                MetaMessage::TimeSignature(numerator, power, _, _) => {
                    track.has_clock = true;
                    if *power > 6 {
                        warnings.push(format!(
                            "Dropped meter {numerator}/2^{power} at tick {tick}: unsupported denominator"
                        ));
                        track.ignore("time_signature");
                    } else {
                        clocks.meters.push(ClockMeter {
                            tick,
                            numerator: *numerator,
                            denominator: 1_u8 << power,
                            offset: chunk_offset,
                        });
                    }
                }
                MetaMessage::KeySignature(sharps_flats, minor) => {
                    track.has_clock = true;
                    clocks.keys.push(ClockKey {
                        tick,
                        sharps_flats: *sharps_flats,
                        minor: *minor,
                        offset: chunk_offset,
                    });
                }
                MetaMessage::EndOfTrack => track.end_tick = tick,
                MetaMessage::TrackNumber(_) => track.ignore("track_number"),
                MetaMessage::Text(_) => track.ignore("text"),
                MetaMessage::Copyright(_) => track.ignore("copyright"),
                MetaMessage::Lyric(_) => track.ignore("lyric"),
                MetaMessage::Marker(_) => track.ignore("marker"),
                MetaMessage::CuePoint(_) => track.ignore("cue_point"),
                MetaMessage::ProgramName(_) => track.ignore("program_name"),
                MetaMessage::DeviceName(_) => track.ignore("device_name"),
                MetaMessage::MidiChannel(_) => track.ignore("midi_channel_prefix"),
                MetaMessage::MidiPort(_) => track.ignore("midi_port"),
                MetaMessage::SmpteOffset(_) => track.ignore("smpte_offset"),
                MetaMessage::SequencerSpecific(_) => track.ignore("sequencer_specific"),
                MetaMessage::Unknown(id, _) => track.ignore(format!("meta_{id}")),
            },
            TrackEventKind::SysEx(_) => track.ignore("sysex"),
            TrackEventKind::Escape(_) => track.ignore("escape"),
        }
    }

    let dangling = track.end_tick.max(tick);
    let pending: Vec<_> = active.into_iter().collect();
    for ((channel, key), voices) in pending {
        for (start, velocity) in voices {
            track.unterminated += 1;
            close_note(&mut track, channel, key, start, velocity, dangling);
        }
    }
    progress.charge_sort(track.notes.len())?;
    track.notes.sort_by_key(|note| {
        (
            note.start_tick,
            note.channel,
            note.key,
            note.end_tick,
            note.velocity,
        )
    });
    Ok(track)
}

fn close_note(
    track: &mut RawTrack,
    channel: u8,
    key: u8,
    start_tick: u64,
    velocity: u8,
    end_tick: u64,
) {
    let end_tick = end_tick.max(start_tick);
    track.notes.push(RawNote {
        channel,
        start_tick,
        end_tick,
        key,
        velocity,
    });
}

fn expand_sustain(track: &mut RawTrack, progress: &mut Progress<'_>) -> Result<usize, MidiError> {
    let mut pedals: BTreeMap<u8, Vec<(u64, bool)>> = BTreeMap::new();
    for cc in &track.ccs {
        progress.check()?;
        if cc.controller == 64 {
            pedals
                .entry(cc.channel)
                .or_default()
                .push((cc.tick, cc.value >= 64));
        }
    }
    if pedals.is_empty() {
        return Ok(0);
    }
    for pedal in pedals.values_mut() {
        progress.charge_sort(pedal.len())?;
        pedal.sort_by_key(|(tick, _)| *tick);
    }
    let track_end = track.end_tick;
    let mut releases: BTreeMap<u8, Vec<Option<u64>>> = BTreeMap::new();
    for (channel, events) in &pedals {
        progress.charge_work(events.len() as u64)?;
        let mut next = vec![None; events.len() + 1];
        let mut found = None;
        for index in (0..events.len()).rev() {
            if !events[index].1 {
                found = Some(events[index].0);
            }
            next[index] = found;
        }
        releases.insert(*channel, next);
    }
    let mut expanded = 0;
    for note in &mut track.notes {
        progress.check()?;
        let Some(events) = pedals.get(&note.channel) else {
            continue;
        };
        let index = events.partition_point(|(tick, _)| *tick <= note.end_tick);
        let held = index != 0 && events[index - 1].1;
        if !held {
            continue;
        }
        let release = releases
            .get(&note.channel)
            .and_then(|next| next.get(index).copied().flatten());
        let new_end = release.unwrap_or_else(|| track_end.max(note.end_tick + 1));
        if new_end > note.end_tick {
            note.end_tick = new_end;
            expanded += 1;
        }
    }
    Ok(expanded)
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    (!text.is_empty()).then_some(text)
}

// ============================================================================
// Clock maps
// ============================================================================

fn build_properties(
    clocks: &ClockCollector,
    ticks: &mut TickMap,
    warnings: &mut Vec<String>,
    progress: &mut Progress<'_>,
) -> Result<ProjectProperties, MidiError> {
    let mut properties = ProjectProperties::default();
    let total = (clocks.meters.len() + clocks.keys.len() + clocks.tempos.len()).max(1);
    let mut done = 0_usize;

    let mut meters: BTreeMap<u64, (u8, u8)> = BTreeMap::new();
    for event in &clocks.meters {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Clock, TRACK_END, CLOCK_END, done, total);
        }
        if !(1..=32).contains(&event.numerator)
            || !matches!(event.denominator, 1 | 2 | 4 | 8 | 16 | 32 | 64)
        {
            warnings.push(format!(
                "Dropped invalid meter {}/{} at tick {}",
                event.numerator, event.denominator, event.tick
            ));
            continue;
        }
        meters.insert(event.tick, (event.numerator, event.denominator));
    }
    let mut meter_quarter = 0.0_f64;
    let mut meter_bar = 1_u32;
    let mut meter = (4_u8, 4_u8);
    for (source_tick, (numerator, denominator)) in meters {
        progress.check()?;
        let tick = ticks.convert(source_tick);
        let quarter = tick as f64 / f64::from(ticks.target_ppq);
        if tick == 0 {
            meter = (numerator, denominator);
            properties.time_signature = TimeSignature {
                numerator,
                denominator,
            };
            continue;
        }
        let old_length = f64::from(meter.0) * 4.0 / f64::from(meter.1);
        let bars = (quarter - meter_quarter) / old_length;
        if bars < 0.0 || (bars - bars.round()).abs() > 1e-6 {
            warnings.push(format!(
                "Dropped meter {numerator}/{denominator} at tick {source_tick}: not on a bar boundary"
            ));
            continue;
        }
        let delta = bars.round();
        if delta > f64::from(u32::MAX) {
            return Err(MidiError::InvalidEvent {
                offset: clocks.first_offset(),
                message: "meter bar exceeds the project range".into(),
            });
        }
        let bar = meter_bar
            .checked_add(delta as u32)
            .ok_or_else(|| MidiError::InvalidEvent {
                offset: clocks.first_offset(),
                message: "meter bar overflow".into(),
            })?;
        properties.time_sig_map.push(TimeSigPoint {
            position: BarBeat::new(bar, 1, 0),
            numerator,
            denominator,
        });
        meter_quarter = quarter;
        meter_bar = bar;
        meter = (numerator, denominator);
    }

    let mut keys: BTreeMap<u64, (i8, bool)> = BTreeMap::new();
    for event in &clocks.keys {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Clock, TRACK_END, CLOCK_END, done, total);
        }
        keys.insert(event.tick, (event.sharps_flats, event.minor));
    }
    let clock = ProjectClock::new(&properties).map_err(|message| MidiError::InvalidEvent {
        offset: clocks.first_offset(),
        message,
    })?;
    let mut key_points: BTreeMap<(u32, u32, u32), KeyPoint> = BTreeMap::new();
    for (source_tick, (sharps_flats, minor)) in keys {
        progress.check()?;
        let tick = ticks.convert(source_tick);
        let key = gm::midi_key_signature(sharps_flats, minor);
        if tick == 0 {
            properties.key = Some(key);
            continue;
        }
        let position = clock
            .bar_beat_floor_at_quarter(tick as f64 / f64::from(ticks.target_ppq))
            .map_err(|message| MidiError::InvalidEvent {
                offset: clocks.first_offset(),
                message,
            })?;
        key_points.insert(
            (position.bar, position.beat, position.subdivision),
            KeyPoint { position, key },
        );
    }
    properties.key_map = key_points.into_values().collect();

    let mut tempos: BTreeMap<u64, (u32, usize)> = BTreeMap::new();
    for event in &clocks.tempos {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Clock, TRACK_END, CLOCK_END, done, total);
        }
        tempos.insert(event.tick, (event.micros, event.offset));
    }
    let mut tempo_points = Vec::new();
    for (source_tick, (micros, offset)) in tempos {
        progress.check()?;
        if micros == 0 {
            return Err(MidiError::InvalidEvent {
                offset,
                message: format!("zero tempo at tick {source_tick}"),
            });
        }
        let mut bpm = 60_000_000.0 / f64::from(micros);
        if !(1.0..=400.0).contains(&bpm) {
            warnings.push(format!(
                "Clamped tempo {bpm:.3} bpm at tick {source_tick} into 1..=400"
            ));
            bpm = bpm.clamp(1.0, 400.0);
        }
        let tick = ticks.convert(source_tick);
        if tick == 0 {
            properties.tempo = bpm;
            continue;
        }
        let quarter = tick as f64 / f64::from(ticks.target_ppq);
        let position = clock
            .bar_beat_floor_at_quarter(quarter)
            .map_err(|message| MidiError::InvalidEvent { offset, message })?;
        let anchor = clock
            .quarter_notes_at(&position)
            .map_err(|message| MidiError::InvalidEvent { offset, message })?;
        tempo_points.push(TempoPoint {
            position,
            offset_quarters: quarter - anchor,
            bpm,
        });
    }
    properties.tempo_map = tempo_points;
    ProjectClock::new(&properties).map_err(|message| MidiError::InvalidEvent {
        offset: clocks.first_offset(),
        message,
    })?;
    Ok(properties)
}

// ============================================================================
// Track units
// ============================================================================

struct NoteTick {
    start_tick: u64,
    end_tick: u64,
    key: u8,
    velocity: u8,
}

struct CcTick {
    tick: u64,
    controller: u8,
    value: u8,
}

struct BendTick {
    tick: u64,
    semitones: f64,
}

struct PressureTick {
    tick: u64,
    value: u8,
}

struct Unit {
    ppq: u16,
    dataset: bool,
    source_track: usize,
    channel: Option<u8>,
    primary_channel: Option<u8>,
    source_name: Option<String>,
    split: bool,
    notes: Vec<NoteTick>,
    ccs: Vec<CcTick>,
    bends: Vec<BendTick>,
    pressures: Vec<PressureTick>,
    program: Option<u8>,
    bend_range: Option<f64>,
    ignored: Vec<IgnoredEvent>,
    has_clock: bool,
    channel_events: bool,
}

impl Unit {
    fn is_drum(&self) -> bool {
        self.channel.or(self.primary_channel) == Some(9)
    }

    fn default_name(&self) -> String {
        if let Some(name) = &self.source_name {
            return if self.split {
                let channel = self.channel.unwrap_or(0) + 1;
                format!("{name} ch{channel}")
            } else {
                name.clone()
            };
        }
        if self.is_drum() {
            "drums".to_string()
        } else {
            self.program
                .map(|program| gm::gm_program_name(program).to_string())
                .unwrap_or_else(|| format!("track_{}", self.source_track + 1))
        }
    }

    fn lane_names(&self) -> Vec<String> {
        let mut lanes = BTreeSet::new();
        for cc in &self.ccs {
            if let Some(lane) = gm::cc_to_lane(cc.controller) {
                lanes.insert(lane.to_string());
            }
        }
        if !self.bends.is_empty() {
            lanes.insert("pitch_bend".to_string());
        }
        if !self.pressures.is_empty() {
            lanes.insert("pressure".to_string());
        }
        lanes.into_iter().collect()
    }

    fn summary(
        &self,
        clock: &ProjectClock,
        target: Option<usize>,
        dropped: bool,
    ) -> ImportTrackSummary {
        let (first_bar, last_bar) = bar_range(clock, &self.notes, self.ppq);
        ImportTrackSummary {
            source_track: self.source_track,
            source_track_name: self.source_name.clone(),
            target_track: target,
            name: self.default_name(),
            channel: self
                .channel
                .or(self.primary_channel)
                .map(|channel| channel + 1),
            program: self.program,
            program_name: self.program.map(gm::gm_program_name).map(str::to_string),
            preset: None,
            instrument: None,
            note_count: self.notes.len(),
            lanes: self.lane_names(),
            first_bar,
            last_bar,
            ignored: self.ignored.clone(),
            dropped,
        }
    }
}

enum PreparedUnit {
    Dropped(ImportTrackSummary),
    Kept {
        unit: Unit,
        mapping: Option<TrackMapping>,
        phrase: Option<Phrase>,
        lanes: Vec<AutomationLane>,
    },
}

fn make_units(
    tracks: &[RawTrack],
    options: &ImportOptions,
    ticks: &mut TickMap,
    progress: &mut Progress<'_>,
) -> Result<Vec<Unit>, MidiError> {
    for mapping in &options.mappings {
        progress.check()?;
        if mapping.source_track >= tracks.len() {
            return Err(MidiError::InvalidMapping {
                source_track: mapping.source_track,
                message: "source track does not exist".into(),
            });
        }
        if let Some(channel) = mapping.channel
            && !(1..=16).contains(&channel)
        {
            return Err(MidiError::InvalidMapping {
                source_track: mapping.source_track,
                message: format!("channel {channel} is outside 1..=16"),
            });
        }
    }

    let total = tracks
        .iter()
        .map(|track| {
            track.notes.len() + track.ccs.len() + track.bends.len() + track.pressures.len()
        })
        .sum::<usize>()
        .max(1);
    let mut done = 0_usize;
    let mut units = Vec::new();
    for track in tracks {
        progress.check()?;
        progress.charge_work(
            (track.notes.len() + track.ccs.len() + track.bends.len() + track.pressures.len())
                as u64,
        )?;
        progress.charge_work(options.mappings.len() as u64)?;
        let channels = track.channels();
        let mut split: BTreeSet<u8> = BTreeSet::new();
        if options.split_channels {
            split.extend(channels.iter().copied());
        } else {
            for mapping in options
                .mappings
                .iter()
                .filter(|mapping| mapping.source_track == track.source_track)
            {
                if let Some(channel) = mapping.channel {
                    let channel = channel - 1;
                    if channels.contains(&channel) {
                        split.insert(channel);
                    }
                }
            }
            if channels.len() > 1 && channels.contains(&9) {
                split.insert(9);
            }
        }
        let ignored = track
            .ignored
            .iter()
            .map(|(kind, count)| IgnoredEvent {
                kind: kind.clone(),
                count: *count,
            })
            .collect::<Vec<_>>();
        let mut produced = false;
        for channel in &split {
            progress.check()?;
            progress.charge_track()?;
            units.push(build_unit(
                track,
                Some(*channel),
                &channels,
                true,
                if produced {
                    Vec::new()
                } else {
                    ignored.clone()
                },
                ticks,
                progress,
                &mut done,
                total,
            )?);
            produced = true;
        }
        let rest: BTreeSet<u8> = channels.difference(&split).copied().collect();
        if !rest.is_empty() || channels.is_empty() {
            progress.charge_track()?;
            units.push(build_unit(
                track,
                None,
                &rest,
                false,
                if produced { Vec::new() } else { ignored },
                ticks,
                progress,
                &mut done,
                total,
            )?);
        }
    }
    Ok(units)
}

#[allow(clippy::too_many_arguments)]
fn build_unit(
    track: &RawTrack,
    channel: Option<u8>,
    channels: &BTreeSet<u8>,
    split: bool,
    ignored: Vec<IgnoredEvent>,
    ticks: &mut TickMap,
    progress: &mut Progress<'_>,
    done: &mut usize,
    total: usize,
) -> Result<Unit, MidiError> {
    let wanted = |event_channel: u8| match channel {
        Some(channel) => event_channel == channel,
        None => channels.contains(&event_channel),
    };
    let mut notes = Vec::new();
    for note in &track.notes {
        progress.check()?;
        if !wanted(note.channel) {
            continue;
        }
        *done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Tracks, READ_END, TRACK_END, *done, total);
        }
        let start_tick = ticks.convert(note.start_tick);
        let mut end_tick = ticks.convert(note.end_tick);
        if end_tick <= start_tick {
            end_tick = start_tick + 1;
        }
        notes.push(NoteTick {
            start_tick,
            end_tick,
            key: note.key,
            velocity: note.velocity,
        });
    }
    progress.charge_sort(notes.len())?;
    notes.sort_by_key(|note| (note.start_tick, note.key, note.end_tick));
    let mut ccs = Vec::new();
    for cc in &track.ccs {
        progress.check()?;
        if !wanted(cc.channel) {
            continue;
        }
        *done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Tracks, READ_END, TRACK_END, *done, total);
        }
        ccs.push(CcTick {
            tick: ticks.convert(cc.tick),
            controller: cc.controller,
            value: cc.value,
        });
    }
    let channel_range = channel.and_then(|channel| track.bend_ranges.get(&channel).copied());
    let mut bends = Vec::new();
    for bend in &track.bends {
        progress.check()?;
        if !wanted(bend.channel) {
            continue;
        }
        *done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Tracks, READ_END, TRACK_END, *done, total);
        }
        let range = channel_range
            .or_else(|| track.bend_ranges.get(&bend.channel).copied())
            .unwrap_or(2.0);
        bends.push(BendTick {
            tick: ticks.convert(bend.tick),
            semitones: gm::pitch_bend_to_semitones(bend.value, range),
        });
    }
    let mut pressures = Vec::new();
    for pressure in &track.pressures {
        progress.check()?;
        if !wanted(pressure.channel) {
            continue;
        }
        *done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(ImportStage::Tracks, READ_END, TRACK_END, *done, total);
        }
        pressures.push(PressureTick {
            tick: ticks.convert(pressure.tick),
            value: pressure.value,
        });
    }
    let program = channel
        .and_then(|channel| track.programs.get(&channel).copied())
        .or_else(|| {
            channels
                .iter()
                .find_map(|channel| track.programs.get(channel).copied())
        });
    let bend_range = channel
        .and_then(|channel| track.bend_ranges.get(&channel).copied())
        .or_else(|| {
            let mut ranges = channels
                .iter()
                .filter_map(|channel| track.bend_ranges.get(channel).copied());
            let first = ranges.next()?;
            ranges
                .all(|range| (range - first).abs() < f64::EPSILON)
                .then_some(first)
        });
    let primary_channel = channel
        .or_else(|| {
            track
                .notes
                .iter()
                .find(|note| wanted(note.channel))
                .map(|note| note.channel)
        })
        .or_else(|| channels.iter().next().copied());
    let channel_events =
        !notes.is_empty() || !ccs.is_empty() || !bends.is_empty() || !pressures.is_empty();
    Ok(Unit {
        ppq: ticks.target_ppq,
        dataset: ticks.target_ppq == ticks.source_ppq && ticks.preserve_dataset,
        source_track: track.source_track,
        channel,
        primary_channel,
        source_name: track.name.clone().or_else(|| track.instrument_name.clone()),
        split,
        notes,
        ccs,
        bends,
        pressures,
        program,
        bend_range,
        ignored,
        has_clock: track.has_clock && channel.is_none(),
        channel_events,
    })
}

fn mapping_for<'a>(options: &'a ImportOptions, unit: &Unit) -> Option<&'a TrackMapping> {
    let channel = unit.channel.map(|channel| channel + 1);
    options
        .mappings
        .iter()
        .find(|mapping| mapping.source_track == unit.source_track && mapping.channel == channel)
}

// ============================================================================
// Notes and automation
// ============================================================================

fn notes_to_phrase(
    unit: &Unit,
    clock: &ProjectClock,
    name: String,
    progress: &mut Progress<'_>,
    start_fraction: f64,
    end_fraction: f64,
) -> Result<Phrase, MidiError> {
    let notes = &unit.notes;
    let mut events = Vec::with_capacity(notes.len());
    let mut last_quarter = 0.0_f64;
    for (index, note) in notes.iter().enumerate() {
        progress.check()?;
        if index.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(
                ImportStage::Notes,
                start_fraction,
                end_fraction,
                index,
                notes.len(),
            );
        }
        let start = note.start_tick as f64 / f64::from(unit.ppq);
        let duration = (note.end_tick - note.start_tick) as f64 / f64::from(unit.ppq);
        last_quarter = last_quarter.max(
            start
                + duration.max(if unit.dataset {
                    dataset::notation_duration(duration).to_beats()
                } else {
                    0.0
                }),
        );
        events.push(NoteEvent {
            start_beat: start,
            explicit_position: true,
            pitch: midi_to_pitch(note.key),
            duration: if unit.dataset {
                dataset::notation_duration(duration)
            } else {
                duration_for_beats(duration)
            },
            dynamics: Some(velocity_to_dynamics(note.velocity)),
            velocity: Some(note.velocity.max(1)),
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: if unit.dataset {
                dataset::note_modifiers(note, unit.ppq)
            } else {
                vec![("gate_quarters".to_string(), ParamValue::Number(duration))]
            },
        });
    }
    let end_bar = enclosing_bar(clock, last_quarter)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?;
    Ok(Phrase {
        name,
        channel: None,
        pedals: Vec::new(),
        hairpins: Vec::new(),
        time_range: Some(TimeRange {
            start: BarBeat::new(1, 1, 0),
            end: BarBeat::new(end_bar, 1, 0),
        }),
        events,
    })
}

fn build_lanes(
    unit: &Unit,
    clock: &ProjectClock,
    progress: &mut Progress<'_>,
    start_fraction: f64,
    end_fraction: f64,
) -> Result<Vec<AutomationLane>, MidiError> {
    let total = (unit.ccs.len() + unit.bends.len() + unit.pressures.len()).max(1);
    let mut done = 0_usize;
    let mut lanes: BTreeMap<&'static str, BTreeMap<u64, AutomationPoint>> = BTreeMap::new();
    for cc in &unit.ccs {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(
                ImportStage::Notes,
                start_fraction,
                end_fraction,
                done,
                total,
            );
        }
        let Some(param) = gm::cc_to_lane(cc.controller) else {
            continue;
        };
        let value = ParamValue::Percentage(f64::from(cc.value) / 127.0 * 100.0);
        lanes
            .entry(param)
            .or_default()
            .insert(cc.tick, automation_point(clock, unit.ppq, cc.tick, value)?);
    }
    for bend in &unit.bends {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(
                ImportStage::Notes,
                start_fraction,
                end_fraction,
                done,
                total,
            );
        }
        lanes.entry("pitch_bend").or_default().insert(
            bend.tick,
            automation_point(
                clock,
                unit.ppq,
                bend.tick,
                ParamValue::Number(bend.semitones),
            )?,
        );
    }
    for pressure in &unit.pressures {
        progress.check()?;
        done += 1;
        if done.is_multiple_of(PROGRESS_STRIDE) {
            progress.span(
                ImportStage::Notes,
                start_fraction,
                end_fraction,
                done,
                total,
            );
        }
        let value = ParamValue::Percentage(f64::from(pressure.value) / 127.0 * 100.0);
        lanes.entry("pressure").or_default().insert(
            pressure.tick,
            automation_point(clock, unit.ppq, pressure.tick, value)?,
        );
    }
    Ok(lanes
        .into_iter()
        .map(|(param, points)| AutomationLane {
            param: param.to_string(),
            ppq: u32::from(unit.ppq),
            smoothing_ms: 0.0,
            points: points.into_values().collect(),
            ..Default::default()
        })
        .collect())
}

fn automation_point(
    clock: &ProjectClock,
    ppq: u16,
    tick: u64,
    value: ParamValue,
) -> Result<AutomationPoint, MidiError> {
    let quarter = tick as f64 / f64::from(ppq);
    let floored = clock
        .bar_beat_floor_at_quarter(quarter)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?;
    let beat = BarBeat::new(floored.bar, floored.beat, 0);
    let base = clock
        .quarter_notes_at(&beat)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?;
    let base_tick = (base * f64::from(ppq)).round() as u64;
    let subdivision =
        u32::try_from(tick.saturating_sub(base_tick)).map_err(|_| MidiError::InvalidEvent {
            offset: 0,
            message: "automation subdivision overflow".into(),
        })?;
    Ok(AutomationPoint {
        position: BarBeat::new(floored.bar, floored.beat, subdivision),
        value,
        curve: Curve::Linear,
        tension: None,
    })
}

fn merge_lanes(target: &mut Vec<AutomationLane>, lanes: Vec<AutomationLane>) {
    for lane in lanes {
        if let Some(existing) = target
            .iter_mut()
            .find(|existing| existing.param == lane.param && existing.ppq == lane.ppq)
        {
            let mut points: BTreeMap<(u32, u32, u32), AutomationPoint> = existing
                .points
                .drain(..)
                .map(|point| {
                    (
                        (
                            point.position.bar,
                            point.position.beat,
                            point.position.subdivision,
                        ),
                        point,
                    )
                })
                .collect();
            for point in lane.points {
                points.insert(
                    (
                        point.position.bar,
                        point.position.beat,
                        point.position.subdivision,
                    ),
                    point,
                );
            }
            existing.points = points.into_values().collect();
        } else {
            target.push(lane);
        }
    }
}

// ============================================================================
// Appending to an existing track
// ============================================================================

/// Written end of one track's content in quarter notes.
///
/// Phrase, pattern and clip ranges count, as does the written content of an
/// inline pattern. Automation lanes do not: the contract is written notes,
/// hits and clips.
fn track_content_end_quarter(track: &Track, clock: &ProjectClock) -> Result<f64, MidiError> {
    let mut end = 0.0_f64;
    for phrase in &track.phrases {
        end = end.max(match &phrase.time_range {
            Some(range) => quarter_at(clock, &range.end)?,
            None => bar_end_quarter(clock, phrase_content_quarters(phrase))?,
        });
    }
    for pattern in &track.patterns {
        let Some(range) = &pattern.time_range else {
            continue;
        };
        let end_bar =
            range
                .start
                .bar
                .checked_add(pattern.repeat)
                .ok_or_else(|| MidiError::InvalidEvent {
                    offset: 0,
                    message: "pattern repeat bar overflow".into(),
                })?;
        end = end.max(quarter_at(clock, &BarBeat::new(end_bar, 1, 0))?);
    }
    for clip in track.pattern_clips() {
        end = end.max(quarter_at(clock, &clip.time_range.end)?);
    }
    for clip in track.audio_clips() {
        end = end.max(quarter_at(clock, &clip.time_range.end)?);
    }
    for pattern in &track.inline_patterns {
        end = end.max(bar_end_quarter(clock, pattern.content_quarters())?);
    }
    Ok(end)
}

/// Quarter notes the written notes of a phrase span.
fn phrase_content_quarters(phrase: &Phrase) -> f64 {
    phrase
        .events
        .iter()
        .map(|event| event.start_beat + event.duration.to_beats())
        .fold(0.0_f64, f64::max)
}

/// Move a phrase to `offset` quarter notes, keeping its events relative to its
/// start.
fn append_phrase(
    mut phrase: Phrase,
    offset: f64,
    clock: &ProjectClock,
) -> Result<Phrase, MidiError> {
    let length = match &phrase.time_range {
        Some(range) => quarter_at(clock, &range.end)? - quarter_at(clock, &range.start)?,
        None => bar_end_quarter(clock, phrase_content_quarters(&phrase))?,
    };
    phrase.time_range = Some(TimeRange {
        start: position_at_quarter(clock, offset)?,
        end: position_at_quarter(clock, offset + length)?,
    });
    Ok(phrase)
}

/// Move every lane point by `offset` quarter notes.
fn shift_lanes(
    lanes: Vec<AutomationLane>,
    offset: f64,
    clock: &ProjectClock,
) -> Result<Vec<AutomationLane>, MidiError> {
    let mut shifted = Vec::with_capacity(lanes.len());
    for mut lane in lanes {
        for point in &mut lane.points {
            let quarter = if lane.ppq == 0 {
                quarter_at(clock, &point.position)?
            } else {
                clock
                    .quarter_notes_at_ppq(&point.position, lane.ppq)
                    .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?
            };
            let tick = ((quarter + offset) * f64::from(TARGET_PPQ)).round();
            let tick = if tick <= 0.0 { 0 } else { tick as u64 };
            point.position =
                automation_point(clock, TARGET_PPQ, tick, point.value.clone())?.position;
        }
        shifted.push(lane);
    }
    Ok(shifted)
}

fn quarter_at(clock: &ProjectClock, position: &BarBeat) -> Result<f64, MidiError> {
    clock
        .quarter_notes_at(position)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })
}

fn position_at_quarter(clock: &ProjectClock, quarter: f64) -> Result<BarBeat, MidiError> {
    clock
        .bar_beat_at_quarter(quarter)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })
}

/// Start of the bar that contains `quarter`, or of the bar that starts at it.
fn bar_end_quarter(clock: &ProjectClock, quarter: f64) -> Result<f64, MidiError> {
    let bar = enclosing_bar(clock, quarter)
        .map_err(|message| MidiError::InvalidEvent { offset: 0, message })?;
    quarter_at(clock, &BarBeat::new(bar, 1, 0))
}

// ============================================================================
// Naming and matching
// ============================================================================

fn sanitize_identifier(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut previous_underscore = false;
    for character in raw.chars() {
        let character = if character.is_ascii_alphanumeric() || character == '_' || character == '/'
        {
            character
        } else {
            '_'
        };
        if character == '_' {
            if previous_underscore {
                continue;
            }
            previous_underscore = true;
        } else {
            previous_underscore = false;
        }
        out.push(character);
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        return "track".to_string();
    }
    if out.starts_with(|character: char| character.is_ascii_digit()) {
        format!("_{out}")
    } else {
        out
    }
}

fn unique_name(
    base: &str,
    used: &BTreeSet<String>,
    progress: &Progress<'_>,
) -> Result<String, MidiError> {
    if !used.contains(base) {
        return Ok(base.to_string());
    }
    for suffix in 2.. {
        progress.check()?;
        let candidate = format!("{base}_{suffix}");
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!()
}

fn unique_phrase_name(track: &Track, progress: &Progress<'_>) -> Result<String, MidiError> {
    let mut used: BTreeSet<String> = BTreeSet::new();
    for phrase in &track.phrases {
        progress.check()?;
        used.insert(phrase.name.clone());
    }
    unique_name("main", &used, progress)
}

fn matching_preset(project: &Project, program: &str, track_name: &str) -> Option<String> {
    let program = gm::normalize_param(program);
    let track = gm::normalize_param(track_name);
    project
        .presets
        .iter()
        .find(|preset| {
            let name = gm::normalize_param(&preset.name);
            let patch = preset
                .patch
                .as_deref()
                .map(gm::normalize_param)
                .unwrap_or_default();
            name == program || patch == program || name == track
        })
        .map(|preset| preset.name.clone())
}

// ============================================================================
// Musical helpers
// ============================================================================

fn midi_to_pitch(midi: u8) -> Pitch {
    Pitch::from_midi(midi).expect("parsed SMF keys are seven-bit MIDI notes")
}

fn velocity_to_dynamics(velocity: u8) -> Dynamics {
    match velocity {
        0..=20 => Dynamics::PPP,
        21..=40 => Dynamics::PP,
        41..=55 => Dynamics::P,
        56..=70 => Dynamics::MP,
        71..=85 => Dynamics::MF,
        86..=100 => Dynamics::F,
        101..=115 => Dynamics::FF,
        _ => Dynamics::FFF,
    }
}

fn duration_for_beats(beats: f64) -> Duration {
    let base = if beats >= 3.0 {
        BaseDuration::Whole
    } else if beats >= 1.5 {
        BaseDuration::Half
    } else if beats >= 0.75 {
        BaseDuration::Quarter
    } else if beats >= 0.375 {
        BaseDuration::Eighth
    } else if beats >= 0.1875 {
        BaseDuration::Sixteenth
    } else if beats >= 0.09375 {
        BaseDuration::ThirtySecond
    } else {
        BaseDuration::SixtyFourth
    };
    Duration {
        base,
        dots: 0,
        triplet: false,
        tuplet: None,
        ties: Vec::new(),
    }
}

fn enclosing_bar(clock: &ProjectClock, quarter: f64) -> Result<u32, String> {
    if quarter <= 0.0 {
        return Ok(1);
    }
    let mut low = 1_u32;
    let mut high = u32::MAX;
    while low < high {
        let middle = low + (high - low) / 2;
        let start = clock.quarter_notes_at(&BarBeat::new(middle, 1, 0))?;
        if start >= quarter {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    Ok(low)
}

fn bar_range(clock: &ProjectClock, notes: &[NoteTick], ppq: u16) -> (Option<u32>, Option<u32>) {
    if notes.is_empty() {
        return (None, None);
    }
    let first = notes.iter().map(|note| note.start_tick).min().unwrap_or(0);
    let last = notes.iter().map(|note| note.end_tick).max().unwrap_or(0);
    let first = bar_at_quarter(clock, first as f64 / f64::from(ppq));
    let last = bar_at_quarter(
        clock,
        (last as f64 / f64::from(ppq) - 1e-9).max(first as f64),
    );
    (Some(first), Some(last))
}

fn bar_at_quarter(clock: &ProjectClock, quarter: f64) -> u32 {
    if quarter <= 0.0 {
        return 1;
    }
    let mut low = 1_u32;
    let mut high = u32::MAX;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        match clock.quarter_notes_at(&BarBeat::new(middle, 1, 0)) {
            Ok(start) if start <= quarter => low = middle,
            _ => high = middle - 1,
        }
    }
    low
}

#[cfg(test)]
mod tests {
    #[test]
    fn work_budget_bounds_collision_search_instead_of_running_an_unbounded_suffix_loop() {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut emit = |_, _| {};
        let progress = super::Progress::new(
            &cancel,
            &mut emit,
            super::ImportLimits {
                max_work: 2,
                ..Default::default()
            },
        );
        let used = ["main", "main_2", "main_3", "main_4"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let error = super::unique_name("main", &used, &progress).unwrap_err();
        assert_eq!(error.code(), super::ImportLimit::Work.code());
    }

    use super::*;
    use midly::num::{u4, u7, u24, u28};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };

    fn channel(value: u8) -> u4 {
        u4::try_from(value).unwrap()
    }

    fn value(value: u8) -> u7 {
        u7::try_from(value).unwrap()
    }

    fn synthetic_smf(notes: usize) -> Vec<u8> {
        let header = Header::new(Format::Parallel, Timing::Metrical(480.into()));
        let mut smf = Smf::new(header);
        smf.tracks.push(vec![
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Conductor")),
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::try_from(500_000).unwrap())),
            },
            TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
            },
        ]);
        let mut events = Vec::with_capacity(notes * 2 + 3);
        events.push(TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Synthetic")),
        });
        events.push(TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Midi {
                channel: channel(0),
                message: MidiMessage::ProgramChange { program: value(0) },
            },
        });
        for index in 0..notes {
            let key = 60 + (index % 12) as u8;
            events.push(TrackEvent {
                delta: u28::try_from(30).unwrap(),
                kind: TrackEventKind::Midi {
                    channel: channel(0),
                    message: MidiMessage::NoteOn {
                        key: value(key),
                        vel: value(100),
                    },
                },
            });
            events.push(TrackEvent {
                delta: u28::try_from(15).unwrap(),
                kind: TrackEventKind::Midi {
                    channel: channel(0),
                    message: MidiMessage::NoteOff {
                        key: value(key),
                        vel: value(0),
                    },
                },
            });
        }
        events.push(TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(events);
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        bytes
    }

    fn base_project() -> Project {
        Project {
            name: Some("Base".into()),
            ocp_version: Some("0.2.0".into()),
            properties: ProjectProperties::default(),
            presets: Vec::new(),
            tracks: Vec::new(),
            patterns: Vec::new(),
            buses: Vec::new(),
            master: None,
            sections: Vec::new(),
            markers: Vec::new(),
            groups: Vec::new(),
        }
    }

    #[test]
    fn cancellation_aborts_at_each_stage_without_touching_the_project() {
        let data = synthetic_smf(4_000);
        let base = base_project();
        let before = base.clone();
        for threshold in [0.05, 0.30, 0.55, 0.70, 0.95] {
            let cancel = AtomicBool::new(false);
            let mut reports = Vec::new();
            let result = import_smf_cancellable(
                &data,
                Some("Synthetic".into()),
                &ImportOptions::default(),
                Some(&base),
                &cancel,
                &mut |stage, fraction| {
                    reports.push((stage, fraction));
                    if fraction >= threshold {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            );
            assert!(
                matches!(result, Err(MidiError::Cancelled)),
                "threshold {threshold} returned {result:?}"
            );
            assert_eq!(base, before, "base project changed at {threshold}");
            let (stage, fraction) = *reports.last().expect("no progress reported");
            assert_ne!(stage, ImportStage::Commit, "commit reported at {threshold}");
            assert!(fraction < 1.0, "progress reached 100% at {threshold}");
            assert!(
                fraction <= threshold + 0.1,
                "cancel at {threshold} was only observed at {fraction}"
            );
        }
    }

    #[test]
    fn progress_is_monotonic_and_reaches_one_hundred_percent() {
        let data = synthetic_smf(2_000);
        let cancel = AtomicBool::new(false);
        let mut reports = Vec::new();
        let result = import_smf_cancellable(
            &data,
            None,
            &ImportOptions::default(),
            None,
            &cancel,
            &mut |stage, fraction| reports.push((stage, fraction)),
        )
        .unwrap();

        assert!(!reports.is_empty());
        assert!(reports.windows(2).all(|pair| pair[1].1 >= pair[0].1));
        assert_eq!(
            reports
                .iter()
                .filter(|(_, fraction)| *fraction == 1.0)
                .count(),
            1
        );
        let last = reports.last().unwrap();
        assert_eq!(last.0, ImportStage::Commit);
        assert_eq!(last.1, 1.0);
        for stage in [
            ImportStage::Parse,
            ImportStage::Tracks,
            ImportStage::Clock,
            ImportStage::Notes,
            ImportStage::Params,
            ImportStage::Commit,
        ] {
            assert!(
                reports.iter().any(|(seen, _)| *seen == stage),
                "missing {stage:?} progress"
            );
        }
        assert_eq!(result.project.tracks.len(), 1);
        assert_eq!(result.project.tracks[0].phrases[0].events.len(), 2_000);
    }
}
