//! Instruments that load in the background after playback can start.
//!
//! A project with many hosted instruments spends most of its load restoring
//! plugin state, one instance after another. Only the instruments the cursor
//! is about to hear have to exist before the first block; the rest load in the
//! order playback will need them and join the mix before their first note.
//!
//! A lane that has not loaded yet renders as a silent placeholder. It is never
//! allowed to stand in for audio: a block that would need it waits for it
//! instead, the same way playback waits for an audio clip still decoding.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use oc_dsl::ast::{PresetDef, Project, Track};
use oc_engine::interpret::{MidiEvent, MidiMessage};

/// Lanes whose first note is this close to the cursor load before playback
/// starts; later ones load in the background.
pub(crate) const DEFERRED_LANE_WINDOW_SECONDS: f64 = 2.0;

/// Latency reserved for a lane whose instrument has not loaded yet and has no
/// recorded latency. It covers the lookahead hosted instruments report, so a
/// lane can join without rebuilding the mixer's delay compensation.
const LATENCY_ALLOWANCE_SECONDS: f64 = 0.02;

/// Most lane latencies remembered across loads in one session.
const LATENCY_CACHE_ENTRIES: usize = 4096;

/// Spans in which a lane is holding at least one note, sorted and disjoint.
///
/// A span runs from a note-on to the note-off that releases the last held
/// note; the release tail after it belongs to the instrument already playing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NoteSpans(Vec<(u64, u64)>);

impl NoteSpans {
    /// Spans of one lane's score, given its events in time order.
    pub(crate) fn of<'a>(events: impl IntoIterator<Item = &'a MidiEvent>) -> Self {
        let mut spans: Vec<(u64, u64)> = Vec::new();
        let mut held: Vec<(u8, u8)> = Vec::new();
        let mut open: Option<u64> = None;
        for event in events {
            let time = event.time_samples;
            match &event.event {
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                } if *velocity > 0 => {
                    if !held.contains(&(*channel, *note)) {
                        held.push((*channel, *note));
                    }
                    open.get_or_insert(time);
                }
                MidiMessage::NoteOff { channel, note }
                | MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity: 0,
                } => {
                    held.retain(|&pair| pair != (*channel, *note));
                    if held.is_empty()
                        && let Some(start) = open.take()
                    {
                        push_span(&mut spans, start, time.max(start + 1));
                    }
                }
                // A native voice belongs to a built-in instrument; mark the
                // frame it starts at so the lane is still needed there.
                MidiMessage::NativeVoice(_) => push_span(&mut spans, time, time + 1),
                _ => {}
            }
        }
        if let Some(start) = open {
            push_span(&mut spans, start, u64::MAX);
        }
        Self(spans)
    }

    /// True when a note sounds anywhere in `[start, end)`.
    pub(crate) fn sounds_in(&self, start: u64, end: u64) -> bool {
        let index = self.0.partition_point(|&(_, span_end)| span_end <= start);
        self.0
            .get(index)
            .is_some_and(|&(span_start, _)| span_start < end)
    }

    /// Frames of playback from `cursor` until the lane is first needed, or
    /// `None` when it has no notes at all.
    ///
    /// With a loop, playback reaches the loop start right after the loop end,
    /// so a lane needed early in the loop is close even when the cursor is
    /// late in it.
    pub(crate) fn distance(&self, cursor: u64, wrap: Option<(u64, u64)>) -> Option<u64> {
        let after = |from: u64| -> Option<u64> {
            let index = self.0.partition_point(|&(_, end)| end <= from);
            self.0.get(index).map(|&(start, _)| start.max(from) - from)
        };
        match wrap {
            Some((start, end)) if start < end && (start..end).contains(&cursor) => {
                let ahead = after(cursor).filter(|distance| cursor + distance < end);
                ahead.or_else(|| {
                    after(start)
                        .filter(|distance| start + distance < end)
                        .map(|distance| end - cursor + distance)
                })
            }
            _ => after(cursor).or_else(|| {
                // Only earlier notes are left: a backward seek or the next
                // load can still need them, after everything ahead.
                self.0.first().map(|&(start, _)| u64::MAX / 2 + start)
            }),
        }
    }
}

fn push_span(spans: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    match spans.last_mut() {
        Some(last) if start <= last.1 => last.1 = last.1.max(end),
        _ => spans.push((start, end)),
    }
}

/// Lanes still loading in the background for one project build.
pub(crate) struct DeferredLanes {
    /// Identifies the build; a result for another build is discarded.
    pub generation: u64,
    /// The resolved project the lanes belong to.
    pub project: Arc<Project>,
    /// The lanes' own tracks, as the full load built them.
    pub tracks: Arc<Vec<Track>>,
    /// Lanes not built yet, by lane index.
    pending: Vec<bool>,
    remaining: usize,
    /// Lane the loader is building now.
    pub in_flight: Option<usize>,
    /// When each lane sounds.
    pub spans: Vec<NoteSpans>,
    /// Latency the mixer reserved for each pending lane.
    pub reserve: Vec<u32>,
    /// Latency-cache key of each lane.
    pub keys: Vec<u64>,
    /// How many lanes the build deferred, for progress.
    pub total: usize,
}

impl DeferredLanes {
    pub(crate) fn new(
        generation: u64,
        project: Arc<Project>,
        tracks: Arc<Vec<Track>>,
        spans: Vec<NoteSpans>,
    ) -> Self {
        let lanes = tracks.len();
        Self {
            generation,
            project,
            tracks,
            pending: vec![false; lanes],
            remaining: 0,
            in_flight: None,
            spans,
            reserve: vec![0; lanes],
            keys: vec![0; lanes],
            total: 0,
        }
    }

    /// Leave `lane` for the background loader, reserving `latency` for it.
    pub(crate) fn defer(&mut self, lane: usize, key: u64, latency: u32) {
        if !self.pending[lane] {
            self.pending[lane] = true;
            self.remaining += 1;
            self.total += 1;
        }
        self.keys[lane] = key;
        self.reserve[lane] = latency;
    }

    pub(crate) fn is_pending(&self, lane: usize) -> bool {
        self.pending.get(lane).copied().unwrap_or(false)
    }

    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }

    /// The lane has joined the mix.
    pub(crate) fn admit(&mut self, lane: usize) {
        if self.pending.get(lane).copied().unwrap_or(false) {
            self.pending[lane] = false;
            self.remaining -= 1;
        }
        if self.in_flight == Some(lane) {
            self.in_flight = None;
        }
    }

    /// True when a pending lane sounds in `[start, end)`.
    pub(crate) fn blocks(&self, start: u64, end: u64) -> bool {
        self.remaining > 0
            && self
                .pending
                .iter()
                .zip(&self.spans)
                .any(|(&pending, spans)| pending && spans.sounds_in(start, end))
    }

    /// The pending lane playback needs soonest from `cursor`.
    ///
    /// Lanes without notes go last; they still load, because their effect
    /// chains belong to the project.
    pub(crate) fn next(&self, cursor: u64, wrap: Option<(u64, u64)>) -> Option<usize> {
        if self.in_flight.is_some() {
            return None;
        }
        (0..self.pending.len())
            .filter(|&lane| self.pending[lane])
            .min_by_key(|&lane| {
                (
                    self.spans
                        .get(lane)
                        .and_then(|spans| spans.distance(cursor, wrap))
                        .unwrap_or(u64::MAX),
                    lane,
                )
            })
    }
}

/// Key a lane's latency by everything its instrument and lane chain are
/// built from at one sample rate.
pub(crate) fn latency_key(preset: Option<&PresetDef>, track: &Track, sample_rate: u32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sample_rate.hash(&mut hasher);
    format!("{preset:?}").hash(&mut hasher);
    format!("{:?}", track.instrument).hash(&mut hasher);
    format!("{:?}", track.preset).hash(&mut hasher);
    format!("{:?}", track.engine_params).hash(&mut hasher);
    format!("{:?}", track.effects).hash(&mut hasher);
    hasher.finish()
}

/// Latencies lanes reported when they loaded, so the next load of the same
/// lane reserves exactly that instead of the allowance. One render worker owns
/// it across project loads.
#[derive(Default)]
pub(crate) struct LaneLatencies(Mutex<HashMap<u64, u32>>);

impl LaneLatencies {
    /// Latency to reserve for a lane that has not loaded: what it reported the
    /// last time it loaded, or the allowance.
    pub(crate) fn reserve(&self, key: u64, sample_rate: u32) -> u32 {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&key)
            .copied()
            .unwrap_or_else(|| latency_allowance(sample_rate))
    }

    /// Remember the latency a lane reported once it loaded.
    pub(crate) fn record(&self, key: u64, latency: u32) {
        let mut cache = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if cache.len() >= LATENCY_CACHE_ENTRIES && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, latency);
    }
}

pub(crate) fn latency_allowance(sample_rate: u32) -> u32 {
    (f64::from(sample_rate) * LATENCY_ALLOWANCE_SECONDS).ceil() as u32
}

impl super::RenderState {
    /// Hand the loader the pending lane playback needs soonest, if it is idle.
    ///
    /// The choice is made here, against the live cursor and score, so a seek
    /// or an edit reorders the queue without telling the loader anything.
    pub(super) fn request_deferred_lane(
        &mut self,
        build_tx: &std::sync::mpsc::Sender<super::SourceWork>,
        wrap: Option<(u64, u64)>,
    ) {
        let cursor = self.seek_target.unwrap_or(self.dsp_cursor);
        let Some(deferred) = self.deferred.as_mut() else {
            return;
        };
        let Some(lane) = deferred.next(cursor, wrap) else {
            return;
        };
        let events: Vec<MidiMessage> = self.events_by_track[lane]
            .iter()
            .map(|&index| self.events[index].event.clone())
            .collect();
        let has_musical = events.iter().any(super::is_musical_event);
        let request = super::LaneLoadRequest {
            generation: deferred.generation,
            lane,
            project: Arc::clone(&deferred.project),
            tracks: Arc::clone(&deferred.tracks),
            events,
            has_musical,
            soundfont_path: self.soundfont_path.clone(),
            sample_rate: self.sample_rate,
            root: self.root.clone(),
            workers: self.worker_options,
            instantiations: Arc::clone(&self.instrument_instantiations),
        };
        if build_tx
            .send(super::SourceWork::Lane(Box::new(request)))
            .is_ok()
        {
            deferred.in_flight = Some(lane);
        }
    }

    /// Tell the panel how many deferred lanes have joined, and whether
    /// playback is waiting for one. Publishes only when that changes.
    pub(super) fn publish_lane_progress(&mut self) {
        use crate::build_progress::Stage;
        let now = self.deferred.as_ref().map(|deferred| {
            let stage = if self.waiting_for_sources {
                Stage::Waiting
            } else {
                Stage::Background
            };
            (stage, deferred.total - deferred.remaining(), deferred.total)
        });
        if now == self.reported_lane_progress {
            return;
        }
        let handle = crate::build_progress::handle();
        match now {
            Some((stage, done, total)) => handle.background(stage, done, total),
            None => handle.finish(),
        }
        self.reported_lane_progress = now;
    }

    /// Put a lane built in the background into the mix at the current DSP
    /// position.
    ///
    /// The lane missed the score before this point while it was silent, so it
    /// first receives the channel state that score left behind and the notes
    /// still held here. Its latency fits the delay the mixer reserved for it;
    /// only a lane that reports more rebuilds the delay compensation.
    pub(super) fn admit_lane(
        &mut self,
        lane: usize,
        built: super::lane_build::Lane,
    ) -> Result<(), String> {
        let Some(deferred) = self.deferred.as_mut() else {
            return Ok(());
        };
        if !deferred.is_pending(lane) {
            return Ok(());
        }
        let key = deferred.keys[lane];
        deferred.admit(lane);
        if deferred.remaining() == 0 {
            self.deferred = None;
        }
        let super::lane_build::Lane {
            mut source,
            mut chain,
            spec,
        } = built;
        let latency = source.latency_samples() + chain.latency_samples();
        self.lane_latencies.record(key, latency);
        chain
            .prepare_block(super::CHUNK_FRAMES)
            .map_err(|error| format!("Source chain preparation failed: {error:?}"))?;
        source.set_project_frame(self.dsp_cursor);
        self.renderers[lane] = source;
        self.source_chains[lane] = chain;
        self.source_specs[lane] = spec;
        self.replay_channel_state(lane);

        let target = self.source_tracks[lane];
        let planned = self.track_latency_plan.get(target).copied().unwrap_or(0);
        let Some(project) = self.project.clone() else {
            return Ok(());
        };
        if latency > planned {
            log::info!(
                "[render_thread] a lane reports {latency} frames of latency, more than the {planned} reserved; rebuilding delay compensation"
            );
            self.sync_aux_plan(&project);
            self.rebuild_compensation_and_graph(&project)?;
            if let Some(graph) = self.graph.as_mut() {
                graph.apply_mixer_state(&project);
                graph.apply_send_state(&project);
                graph.set_master_gain_db(project.master.as_ref().map_or(0.0, |m| m.volume_db));
            }
            return self.prepare_streaming();
        }
        // The stand-in was silent, so the lane's delay lines start empty.
        self.source_delays[lane] =
            std::collections::VecDeque::from(vec![0.0; (planned - latency) as usize * 2]);
        self.admit_lane_outputs(&project, lane, planned);
        Ok(())
    }

    /// Plan the admitted lane's output pairs and their delay lines without
    /// touching any other lane's audio in flight.
    fn admit_lane_outputs(&mut self, project: &Project, lane: usize, planned: u32) {
        let track = self.source_tracks[lane];
        let requested = oc_engine::render_cache::aux_pairs_by_track(project);
        let pairs = requested.get(track).map(Vec::as_slice).unwrap_or(&[]);
        let name = project
            .tracks
            .get(track)
            .map_or("", |track| track.name.as_str());
        let mut warned = std::mem::take(&mut self.warned_aux);
        let mut warned_ports = std::mem::take(&mut self.warned_ports);
        let main_pair = self.lane_main_pair(project, lane, track, name, &mut warned_ports);
        let wanted =
            super::supported_aux_pairs(&self.renderers[lane], pairs, track, name, &mut warned);
        self.warned_aux = warned;
        self.warned_ports = warned_ports;
        self.source_outputs[lane] = oc_engine::block_source::SourceOutput::new(main_pair, &wanted);
        let delay = planned.saturating_sub(self.renderers[lane].latency_samples()) as usize * 2;
        self.aux_delays[lane] = wanted
            .iter()
            .map(|_| {
                let mut line = std::collections::VecDeque::with_capacity(
                    delay + oc_engine::block_source::MAX_SOURCE_BLOCK_FRAMES * 2,
                );
                line.extend(std::iter::repeat_n(0.0, delay));
                line
            })
            .collect();
        if !wanted.is_empty() {
            self.rebuild_aux_buffers();
        }
    }

    /// Send a joining lane the program, controller, bend and pressure values
    /// its score set before the DSP position, then the notes held there.
    fn replay_channel_state(&mut self, lane: usize) {
        let cursor = self.track_event_cursors[lane];
        let mut latest: Vec<usize> = Vec::new();
        let mut held: Vec<usize> = Vec::new();
        let same_slot = |a: &MidiMessage, b: &MidiMessage| -> bool {
            use MidiMessage::*;
            match (a, b) {
                (ProgramChange { channel: x, .. }, ProgramChange { channel: y, .. })
                | (PitchBend { channel: x, .. }, PitchBend { channel: y, .. })
                | (ChannelPressure { channel: x, .. }, ChannelPressure { channel: y, .. }) => {
                    x == y
                }
                (
                    ControlChange {
                        channel: x,
                        controller: cx,
                        ..
                    }
                    | ControlChange14 {
                        channel: x,
                        controller: cx,
                        ..
                    },
                    ControlChange {
                        channel: y,
                        controller: cy,
                        ..
                    }
                    | ControlChange14 {
                        channel: y,
                        controller: cy,
                        ..
                    },
                ) => x == y && cx == cy,
                _ => false,
            }
        };
        for &index in &self.events_by_track[lane][..cursor] {
            let event = &self.events[index].event;
            match event {
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                } if *velocity > 0 => {
                    held.retain(|&other| {
                        !matches!(self.events[other].event,
                            MidiMessage::NoteOn { channel: c, note: n, .. } if c == *channel && n == *note)
                    });
                    held.push(index);
                }
                MidiMessage::NoteOff { channel, note }
                | MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity: 0,
                } => held.retain(|&other| {
                    !matches!(self.events[other].event,
                        MidiMessage::NoteOn { channel: c, note: n, .. } if c == *channel && n == *note)
                }),
                MidiMessage::ProgramChange { .. }
                | MidiMessage::PitchBend { .. }
                | MidiMessage::ChannelPressure { .. }
                | MidiMessage::ControlChange { .. }
                | MidiMessage::ControlChange14 { .. } => {
                    latest.retain(|&other| !same_slot(&self.events[other].event, event));
                    latest.push(index);
                }
                _ => {}
            }
        }
        // Programs first, so controllers land on the selected program.
        latest.sort_by_key(|&index| {
            (
                !matches!(self.events[index].event, MidiMessage::ProgramChange { .. }),
                index,
            )
        });
        for index in latest.into_iter().chain(held) {
            let event = self.events[index].event.clone();
            if let Err(error) = self.renderers[lane].send(&event) {
                log::warn!("[render_thread] a joining lane rejected its channel state: {error}");
                continue;
            }
            super::track_sounding(super::lane_sounding(&mut self.sounding, lane), &event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(time: u64, note: u8) -> MidiEvent {
        MidiEvent {
            time_samples: time,
            track_index: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity: 100,
            },
        }
    }

    fn off(time: u64, note: u8) -> MidiEvent {
        MidiEvent {
            time_samples: time,
            track_index: 0,
            event: MidiMessage::NoteOff { channel: 0, note },
        }
    }

    #[test]
    fn overlapping_notes_make_one_span_and_controls_make_none() {
        let control = MidiEvent {
            time_samples: 0,
            track_index: 0,
            event: MidiMessage::ControlChange {
                channel: 0,
                controller: 7,
                value: 90,
            },
        };
        let events = [
            control,
            on(100, 60),
            on(150, 64),
            off(200, 60),
            off(300, 64),
            on(500, 60),
            off(600, 60),
        ];
        let spans = NoteSpans::of(&events);
        assert_eq!(spans, NoteSpans(vec![(100, 300), (500, 600)]));
        assert!(!spans.sounds_in(0, 100));
        assert!(spans.sounds_in(0, 101));
        assert!(spans.sounds_in(299, 400));
        assert!(!spans.sounds_in(300, 500));
    }

    #[test]
    fn a_note_without_its_release_sounds_to_the_end() {
        let spans = NoteSpans::of(&[on(10, 60)]);
        assert!(spans.sounds_in(1_000_000, 1_000_001));
    }

    #[test]
    fn distance_counts_playback_order_through_a_loop() {
        let spans = NoteSpans::of(&[on(100, 60), off(200, 60), on(900, 60), off(950, 60)]);
        assert_eq!(spans.distance(0, None), Some(100));
        assert_eq!(spans.distance(150, None), Some(0));
        assert_eq!(spans.distance(300, None), Some(600));
        // Past every note: needed only after everything ahead.
        assert!(spans.distance(1_000, None).unwrap() > u64::MAX / 4);
        // Looping 0..500 from 400: the loop reaches 100 after 100 + 100 frames,
        // and the note at 900 is never played.
        assert_eq!(spans.distance(400, Some((0, 500))), Some(200));
        assert_eq!(NoteSpans::default().distance(0, None), None);
    }

    #[test]
    fn the_lane_needed_soonest_loads_first_and_one_at_a_time() {
        let project = oc_dsl::parse(
            "@project\n@track a | instrument: audio/clip\n@track b | instrument: audio/clip\n@track c | instrument: audio/clip\n",
        )
        .unwrap();
        let tracks = Arc::new(project.tracks.clone());
        let spans = vec![
            NoteSpans::of(&[on(5_000, 60), off(6_000, 60)]),
            NoteSpans::of(&[on(1_000, 60), off(2_000, 60)]),
            NoteSpans::default(),
        ];
        let mut lanes = DeferredLanes::new(1, Arc::new(project), tracks, spans);
        for lane in 0..3 {
            lanes.defer(lane, 0, 0);
        }
        assert_eq!(lanes.next(0, None), Some(1));
        // After a seek past the second lane's note, the first is sooner.
        assert_eq!(lanes.next(3_000, None), Some(0));
        assert!(lanes.blocks(1_500, 1_600));
        lanes.in_flight = Some(1);
        assert_eq!(lanes.next(0, None), None);
        lanes.admit(1);
        assert!(!lanes.blocks(1_500, 1_600));
        assert_eq!(lanes.next(0, None), Some(0));
        lanes.admit(0);
        // A lane without notes still loads, last.
        assert_eq!(lanes.next(0, None), Some(2));
        lanes.admit(2);
        assert_eq!(lanes.remaining(), 0);
        assert_eq!(lanes.next(0, None), None);
    }
}
