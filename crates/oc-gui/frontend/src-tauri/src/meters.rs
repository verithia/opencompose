//! Atomic meter state shared between the streaming render thread and the UI.
//!
//! The render thread publishes gain reduction, peak/RMS levels, short-term
//! loudness and the stereo image after each mixed chunk; Tauri commands only
//! read atomics, so the audio callback never blocks.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use oc_engine::graph::{AudioGraph, EffectChain};
use oc_engine::spectrum::{SPECTRUM_BANDS, SPECTRUM_FLOOR_DB};
use serde::Serialize;

/// Maximum meters for tracks.
pub const MAX_TRACK_METERS: usize = 256;
/// Maximum meters for buses.
pub const MAX_BUS_METERS: usize = 64;
/// Maximum meters for effects.
pub const MAX_EFFECT_METERS: usize = 32;
/// Most bands any built-in device meters individually (dynamic EQ has six).
pub const MAX_EFFECT_BANDS: usize = 8;

/// Points in one goniometer frame; each point is one left/right pair.
pub const GONIOMETER_POINTS: usize = 64;
/// Values in one published goniometer frame (interleaved pairs).
pub const GONIOMETER_VALUES: usize = GONIOMETER_POINTS * 2;

/// Level floor in dBFS; a quieter tap reads nothing rather than a number.
pub const MIN_LEVEL_DB: f64 = -120.0;
/// Level ceiling in dBFS; a hotter reading clamps here.
pub const MAX_LEVEL_DB: f64 = 12.0;
/// Span of the published short-term window, in seconds.
pub const SHORT_TERM_SECONDS: f64 = oc_engine::analysis::SHORT_TERM_SECONDS;
/// Time constant of the correlation readout, in seconds.
const CORRELATION_SECONDS: f64 = 0.3;
/// Mean square below which the stereo image counts as silence.
const MIN_CORRELATION_ENERGY: f64 = 1e-12;

/// Display clamp; dynamic EQ boosts may report a small negative reduction.
const MIN_GR_DB: f32 = -60.0;
const MAX_GR_DB: f32 = 60.0;

/// One tap's linear peak and RMS; 0.0 means no signal reached the tap.
#[derive(Debug, Clone, Copy, Default)]
struct Level {
    peak: f32,
    rms: f32,
}

impl Level {
    /// Peak and RMS of an interleaved stereo buffer, ignoring non-finite
    /// samples. Both are capped at 16.0 linear (24 dBFS) before the dB clamp.
    fn of(buffer: &[f32]) -> Self {
        let mut peak = 0.0f32;
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for &sample in buffer {
            if sample.is_finite() {
                peak = peak.max(sample.abs());
                sum += f64::from(sample) * f64::from(sample);
                count += 1;
            }
        }
        let rms = if count == 0 {
            0.0
        } else {
            (sum / count as f64).sqrt() as f32
        };
        Self {
            peak: peak.min(16.0),
            rms: rms.min(16.0),
        }
    }
}

/// One graph node: chain total, input/output levels, and per-effect reduction.
pub struct NodeMeter {
    gain_reduction_db: AtomicU32,
    input_peak: AtomicU32,
    output_peak: AtomicU32,
    output_rms: AtomicU32,
    /// Latched until the UI resets it or the channel is gated to silence;
    /// never cleared by a stop or a seek.
    clipped: AtomicBool,
    effect_count: AtomicU32,
    effects: Box<[AtomicU32]>,
    band_counts: Box<[AtomicU32]>,
    /// Flattened `effect * MAX_EFFECT_BANDS + band`, signed dB.
    bands: Box<[AtomicU32]>,
    effect_max: Box<[AtomicU32]>,
    /// Ceiling margin in dB; NaN means the effect has no ceiling.
    effect_margin: Box<[AtomicU32]>,
}

impl NodeMeter {
    fn new() -> Self {
        Self {
            gain_reduction_db: AtomicU32::new(0.0f32.to_bits()),
            input_peak: AtomicU32::new(0.0f32.to_bits()),
            output_peak: AtomicU32::new(0.0f32.to_bits()),
            output_rms: AtomicU32::new(0.0f32.to_bits()),
            clipped: AtomicBool::new(false),
            effect_count: AtomicU32::new(0),
            effects: (0..MAX_EFFECT_METERS)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            band_counts: (0..MAX_EFFECT_METERS).map(|_| AtomicU32::new(0)).collect(),
            bands: (0..MAX_EFFECT_METERS * MAX_EFFECT_BANDS)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            effect_max: (0..MAX_EFFECT_METERS)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            effect_margin: (0..MAX_EFFECT_METERS)
                .map(|_| AtomicU32::new(f32::NAN.to_bits()))
                .collect(),
        }
    }

    fn set_chain(
        &self,
        gain_reduction_db: f32,
        input_peak: f32,
        output: Level,
        effect_count: usize,
    ) {
        store_db(&self.gain_reduction_db, gain_reduction_db);
        store_unit(&self.input_peak, input_peak);
        store_unit(&self.output_peak, output.peak);
        store_unit(&self.output_rms, output.rms);
        // A sample peak at or above full scale latches; a reset clears it.
        if output.peak >= 1.0 {
            self.clipped.store(true, Ordering::Relaxed);
        }
        self.effect_count.store(
            effect_count.min(MAX_EFFECT_METERS) as u32,
            Ordering::Relaxed,
        );
    }

    #[cfg(test)]
    fn set_effect(&self, index: usize, value: f32, max: f32, margin: Option<f32>) {
        if let Some(slot) = self.effects.get(index) {
            store_db(slot, value);
        }
        if let Some(slot) = self.effect_max.get(index) {
            store_db(slot, max);
        }
        if let Some(slot) = self.effect_margin.get(index) {
            slot.store(margin.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
        }
    }

    /// Publish captured values; mirrors `capture` so a delayed frame lands
    /// exactly as if it had been published with the chunk.
    fn apply(&self, values: &NodeValues) {
        self.set_chain(
            values.gain_reduction_db,
            values.input_peak,
            values.output,
            values.effect_count,
        );
        // A gated channel has no signal to clip, so the latch clears rather
        // than holding a clip from audio that is no longer in the mix.
        if values.silenced {
            self.clipped.store(false, Ordering::Relaxed);
        }
        for (index, &gain_reduction) in values.effects[..values.effect_count].iter().enumerate() {
            store_db(&self.effects[index], gain_reduction);
            store_db(&self.effect_max[index], values.effects_max[index]);
            self.effect_margin[index].store(values.margins[index].to_bits(), Ordering::Relaxed);
            let bands = &values.bands[index][..values.band_counts[index]];
            self.band_counts[index].store(bands.len() as u32, Ordering::Relaxed);
            for band in 0..MAX_EFFECT_BANDS {
                store_db(
                    &self.bands[index * MAX_EFFECT_BANDS + band],
                    bands.get(band).copied().unwrap_or(0.0),
                );
            }
        }
    }

    fn band_snapshot(&self, effect_count: usize) -> Vec<Vec<f64>> {
        let counts: Vec<usize> = (0..effect_count)
            .map(|index| self.band_counts[index].load(Ordering::Relaxed) as usize)
            .collect();
        if counts.iter().all(|count| *count == 0) {
            return Vec::new();
        }
        counts
            .iter()
            .enumerate()
            .map(|(effect, &count)| {
                (0..count.min(MAX_EFFECT_BANDS))
                    .map(|band| load_db(&self.bands[effect * MAX_EFFECT_BANDS + band]) as f64)
                    .collect()
            })
            .collect()
    }

    fn clear(&self) {
        self.set_chain(0.0, 0.0, Level::default(), 0);
        for slot in self.effects.iter() {
            store_db(slot, 0.0);
        }
        for slot in self.band_counts.iter() {
            slot.store(0, Ordering::Relaxed);
        }
        for slot in self.bands.iter() {
            store_db(slot, 0.0);
        }
        for slot in self.effect_max.iter() {
            store_db(slot, 0.0);
        }
        for slot in self.effect_margin.iter() {
            slot.store(f32::NAN.to_bits(), Ordering::Relaxed);
        }
    }

    fn reset_clip(&self) {
        self.clipped.store(false, Ordering::Relaxed);
    }

    fn snapshot(&self) -> NodeMeterSnapshot {
        let count = self.effect_count.load(Ordering::Relaxed) as usize;
        let count = count.min(MAX_EFFECT_METERS);
        let effects: Vec<f64> = self.effects[..count]
            .iter()
            .map(|slot| load_db(slot) as f64)
            .collect();
        NodeMeterSnapshot {
            gain_reduction_db: load_db(&self.gain_reduction_db) as f64,
            input_peak_db: load_level(&self.input_peak),
            output_peak_db: load_level(&self.output_peak),
            output_rms_db: load_level(&self.output_rms),
            clipped: self.clipped.load(Ordering::Relaxed),
            effect_bands: self.band_snapshot(count),
            effects,
            effects_max_gain_reduction_db: self.effect_max[..count]
                .iter()
                .map(|slot| load_db(slot) as f64)
                .collect(),
            effects_ceiling_margin_db: self.effect_margin[..count]
                .iter()
                .map(load_margin)
                .collect(),
        }
    }
}

/// Meter values for one graph node, captured ahead of the audio reaching the
/// device and published when it does.
pub struct NodeValues {
    gain_reduction_db: f32,
    input_peak: f32,
    output: Level,
    /// The channel is gated to silence this block, so its clip latch clears.
    silenced: bool,
    effect_count: usize,
    effects: [f32; MAX_EFFECT_METERS],
    effects_max: [f32; MAX_EFFECT_METERS],
    margins: [f32; MAX_EFFECT_METERS],
    band_counts: [usize; MAX_EFFECT_METERS],
    bands: [[f32; MAX_EFFECT_BANDS]; MAX_EFFECT_METERS],
}

impl NodeValues {
    fn capture(chain: &EffectChain, input_peak: f32, output: Level, silenced: bool) -> Self {
        let mut values = Self {
            gain_reduction_db: chain.gain_reduction_db() as f32,
            input_peak,
            output,
            silenced,
            effect_count: chain.effects.len().min(MAX_EFFECT_METERS),
            effects: [0.0; MAX_EFFECT_METERS],
            effects_max: [0.0; MAX_EFFECT_METERS],
            margins: [f32::NAN; MAX_EFFECT_METERS],
            band_counts: [0; MAX_EFFECT_METERS],
            bands: [[0.0; MAX_EFFECT_BANDS]; MAX_EFFECT_METERS],
        };
        for (index, effect) in chain.effects.iter().take(MAX_EFFECT_METERS).enumerate() {
            values.effects[index] = effect.gain_reduction_db() as f32;
            values.effects_max[index] = effect.max_gain_reduction_db() as f32;
            values.margins[index] = effect
                .ceiling_margin_db()
                .map_or(f32::NAN, |value| value as f32);
            values.band_counts[index] = effect.band_count().min(MAX_EFFECT_BANDS);
            for band in 0..values.band_counts[index] {
                values.bands[index][band] = effect.band_gain_db(band).unwrap_or(0.0) as f32;
            }
        }
        values
    }
}

/// Master-wide readings that arrive with one chunk's meters.
pub struct MonitorValues {
    /// Short-term loudness of the chunk's audio; `None` while the window fills
    /// or when the window is silent.
    pub short_term_lufs: Option<f32>,
    /// Left/right correlation of the chunk's audio before any mono fold.
    pub correlation: Option<f32>,
    /// Interleaved left/right pairs, [`GONIOMETER_VALUES`] long.
    pub goniometer: [f32; GONIOMETER_VALUES],
    /// How many of `goniometer`'s points carry audio.
    pub goniometer_points: usize,
}

impl Default for MonitorValues {
    fn default() -> Self {
        Self {
            short_term_lufs: None,
            correlation: None,
            goniometer: [0.0; GONIOMETER_VALUES],
            goniometer_points: 0,
        }
    }
}

/// One mixed chunk's meters, waiting for the device to reach that audio.
pub struct MeterFrame {
    tracks: Vec<NodeValues>,
    buses: Vec<NodeValues>,
    master: NodeValues,
    monitor: MonitorValues,
    spectrum: Option<[f32; SPECTRUM_BANDS]>,
}

/// Shared meter hub. Values are atomics; the layout mutex is touched only by
/// the render thread on project changes and by UI snapshots.
pub struct MeterHub {
    available: AtomicBool,
    reason: Mutex<Option<String>>,
    track_count: AtomicU32,
    bus_count: AtomicU32,
    /// Channel count of the monitored master; 0 until a graph is loaded.
    master_channels: AtomicU32,
    /// What the UI asked for; survives a stopped transport.
    mono_requested: AtomicBool,
    /// What the render thread actually folded into the last chunk.
    mono_applied: AtomicBool,
    tracks: Box<[NodeMeter]>,
    buses: Box<[NodeMeter]>,
    bus_names: Mutex<Vec<String>>,
    master: NodeMeter,
    short_term_lufs: AtomicU32,
    correlation: AtomicU32,
    goniometer_available: AtomicBool,
    goniometer_points: AtomicU32,
    goniometer: Box<[AtomicU32]>,
    spectrum_available: AtomicBool,
    spectrum: Box<[AtomicU32]>,
}

impl Default for MeterHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MeterHub {
    pub fn new() -> Self {
        Self {
            available: AtomicBool::new(false),
            reason: Mutex::new(None),
            track_count: AtomicU32::new(0),
            bus_count: AtomicU32::new(0),
            master_channels: AtomicU32::new(0),
            mono_requested: AtomicBool::new(false),
            mono_applied: AtomicBool::new(false),
            tracks: (0..MAX_TRACK_METERS).map(|_| NodeMeter::new()).collect(),
            buses: (0..MAX_BUS_METERS).map(|_| NodeMeter::new()).collect(),
            bus_names: Mutex::new(Vec::new()),
            master: NodeMeter::new(),
            short_term_lufs: AtomicU32::new(f32::NAN.to_bits()),
            correlation: AtomicU32::new(f32::NAN.to_bits()),
            goniometer_available: AtomicBool::new(false),
            goniometer_points: AtomicU32::new(0),
            goniometer: (0..GONIOMETER_VALUES)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            spectrum_available: AtomicBool::new(false),
            spectrum: (0..SPECTRUM_BANDS)
                .map(|_| AtomicU32::new(SPECTRUM_FLOOR_DB.to_bits()))
                .collect(),
        }
    }

    pub fn set_available(&self, available: bool) {
        if available {
            *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.available.store(available, Ordering::Release);
    }

    /// Publish why meters are unavailable. The UI shows this instead of a
    /// frozen reading, so a dead stream never looks like a quiet one.
    pub fn set_unavailable(&self, reason: impl Into<String>) {
        *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.into());
        self.available.store(false, Ordering::Release);
    }

    /// Channel count of the monitored master. `0` means no graph is loaded, so
    /// the mono check has nothing to fold.
    pub fn set_master_channels(&self, channels: u16) {
        self.master_channels
            .store(u32::from(channels), Ordering::Relaxed);
    }

    pub fn master_channels(&self) -> u16 {
        self.master_channels.load(Ordering::Relaxed) as u16
    }

    pub fn set_mono_requested(&self, enabled: bool) {
        self.mono_requested.store(enabled, Ordering::Release);
    }

    pub fn mono_requested(&self) -> bool {
        self.mono_requested.load(Ordering::Acquire)
    }

    pub fn set_mono_applied(&self, applied: bool) {
        self.mono_applied.store(applied, Ordering::Release);
    }

    pub fn mono_applied(&self) -> bool {
        self.mono_applied.load(Ordering::Acquire)
    }

    /// Clear every node's clip latch; a gated channel also clears its own.
    pub fn reset_clips(&self) {
        for node in self.tracks.iter() {
            node.reset_clip();
        }
        for node in self.buses.iter() {
            node.reset_clip();
        }
        self.master.reset_clip();
    }

    pub fn clear(&self) {
        for node in self.tracks.iter() {
            node.clear();
        }
        for node in self.buses.iter() {
            node.clear();
        }
        self.master.clear();
        // A stop ends the fold even though the request stays armed.
        self.mono_applied.store(false, Ordering::Release);
        self.short_term_lufs
            .store(f32::NAN.to_bits(), Ordering::Relaxed);
        self.correlation
            .store(f32::NAN.to_bits(), Ordering::Relaxed);
        self.goniometer_available.store(false, Ordering::Release);
        self.goniometer_points.store(0, Ordering::Relaxed);
        self.clear_spectrum();
    }

    /// Publish analyser bands; None stops the overlay without leaving stale bins.
    pub fn publish_spectrum(&self, bands: Option<&[f32]>) {
        match bands {
            Some(bands) => {
                for (slot, value) in self.spectrum.iter().zip(bands.iter()) {
                    slot.store(value.to_bits(), Ordering::Relaxed);
                }
                self.spectrum_available.store(true, Ordering::Release);
            }
            None => self.clear_spectrum(),
        }
    }

    pub fn clear_spectrum(&self) {
        self.spectrum_available.store(false, Ordering::Release);
        for slot in self.spectrum.iter() {
            slot.store(SPECTRUM_FLOOR_DB.to_bits(), Ordering::Relaxed);
        }
    }

    /// Update the published layout when a project loads; usually a no-op.
    pub fn set_layout(&self, track_count: usize, bus_names: &[String], master_channels: u16) {
        self.track_count
            .store(track_count.min(MAX_TRACK_METERS) as u32, Ordering::Relaxed);
        self.bus_count.store(
            bus_names.len().min(MAX_BUS_METERS) as u32,
            Ordering::Relaxed,
        );
        self.set_master_channels(master_channels);
        let mut names = self.bus_names.lock().unwrap_or_else(|e| e.into_inner());
        if names.as_slice() != bus_names {
            names.clear();
            names.extend_from_slice(bus_names);
        }
    }

    /// Read one mixed chunk's meter values without publishing them.
    /// The render thread holds the result until the device reaches that audio.
    pub fn capture(
        graph: &AudioGraph,
        input_peaks: &[f32],
        track_outputs: &[Vec<f32>],
        master_output: &[f32],
        monitor: MonitorValues,
    ) -> MeterFrame {
        let mut frame = MeterFrame {
            tracks: Vec::with_capacity(graph.track_chains.len().min(MAX_TRACK_METERS)),
            buses: Vec::with_capacity(graph.bus_names().len().min(MAX_BUS_METERS)),
            master: NodeValues::capture(&graph.master_chain, 0.0, Level::default(), false),
            monitor: MonitorValues::default(),
            spectrum: None,
        };
        Self::capture_into(
            graph,
            input_peaks,
            track_outputs,
            master_output,
            monitor,
            &mut frame,
        );
        frame
    }

    /// Update a frame allocated for this graph on the control plane. Node and
    /// spectrum payloads are bounded inline values; repeated capture never
    /// changes heap ownership while the graph layout remains unchanged.
    pub fn capture_into(
        graph: &AudioGraph,
        input_peaks: &[f32],
        track_outputs: &[Vec<f32>],
        master_output: &[f32],
        monitor: MonitorValues,
        frame: &mut MeterFrame,
    ) {
        let bus_names = graph.bus_names();
        frame.tracks.clear();
        frame.buses.clear();
        frame.master =
            NodeValues::capture(&graph.master_chain, 0.0, Level::of(master_output), false);
        frame.monitor = monitor;
        frame.spectrum = graph.spectrum_bins().map(|bands| {
            let mut values = [SPECTRUM_FLOOR_DB; SPECTRUM_BANDS];
            for (slot, value) in values.iter_mut().zip(bands) {
                *slot = *value;
            }
            values
        });
        for (index, chain) in graph.track_chains.iter().enumerate() {
            if index >= MAX_TRACK_METERS {
                break;
            }
            // A gated channel reads zero even if the buffer still holds its
            // live input: nothing of it reaches the mix.
            let silenced = graph.track_is_silenced(index);
            let (input, output) = if silenced {
                (0.0, Level::default())
            } else {
                (
                    input_peaks.get(index).copied().unwrap_or(0.0),
                    track_outputs
                        .get(index)
                        .map(|buffer| Level::of(buffer))
                        .unwrap_or_default(),
                )
            };
            frame
                .tracks
                .push(NodeValues::capture(chain, input, output, silenced));
        }
        for (index, name) in bus_names.iter().enumerate() {
            if index >= MAX_BUS_METERS {
                break;
            }
            let Some(bus) = graph.bus_chains.get(name) else {
                continue;
            };
            // The bus buffer holds its post-chain output for this chunk.
            let silenced = !bus.audible;
            let output = if silenced {
                Level::default()
            } else {
                Level::of(&bus.buffer)
            };
            frame
                .buses
                .push(NodeValues::capture(&bus.chain, 0.0, output, silenced));
        }
    }

    /// Publish values captured by `capture`, keeping the bus layout order.
    pub fn apply(&self, frame: &MeterFrame) {
        for (index, values) in frame.tracks.iter().enumerate() {
            if index >= MAX_TRACK_METERS {
                break;
            }
            self.tracks[index].apply(values);
        }
        for (index, values) in frame.buses.iter().enumerate() {
            if index >= MAX_BUS_METERS {
                break;
            }
            self.buses[index].apply(values);
        }
        self.master.apply(&frame.master);
        self.publish_monitor(&frame.monitor);
        self.publish_spectrum(frame.spectrum.as_ref().map(|bands| bands.as_slice()));
    }

    fn publish_monitor(&self, monitor: &MonitorValues) {
        store_optional(&self.short_term_lufs, monitor.short_term_lufs);
        store_optional(&self.correlation, monitor.correlation);
        if monitor.goniometer_points == 0 {
            self.goniometer_available.store(false, Ordering::Release);
            self.goniometer_points.store(0, Ordering::Relaxed);
            return;
        }
        let points = monitor.goniometer_points.min(GONIOMETER_POINTS);
        for (slot, value) in self
            .goniometer
            .iter()
            .zip(monitor.goniometer.iter().take(points * 2))
        {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        self.goniometer_points
            .store(points as u32, Ordering::Relaxed);
        self.goniometer_available.store(true, Ordering::Release);
    }

    pub fn snapshot(&self, playing: bool) -> MeterSnapshot {
        let track_count = self.track_count.load(Ordering::Relaxed) as usize;
        let bus_count = self.bus_count.load(Ordering::Relaxed) as usize;
        let names = self.bus_names.lock().unwrap_or_else(|e| e.into_inner());
        let tracks = (0..track_count.min(MAX_TRACK_METERS))
            .map(|index| TrackMeterSnapshot {
                index,
                node: self.tracks[index].snapshot(),
            })
            .collect();
        let buses = names
            .iter()
            .take(bus_count.min(MAX_BUS_METERS))
            .enumerate()
            .map(|(index, name)| BusMeterSnapshot {
                name: name.clone(),
                node: self.buses[index].snapshot(),
            })
            .collect();
        let spectrum =
            self.spectrum_available
                .load(Ordering::Acquire)
                .then(|| SpectrumMeterSnapshot {
                    bands: self
                        .spectrum
                        .iter()
                        .map(|slot| load_db(slot) as f64)
                        .collect(),
                });
        MeterSnapshot {
            available: self.available.load(Ordering::Acquire),
            reason: self
                .reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            playing,
            master_channels: self.master_channels(),
            mono: MonoCheckSnapshot {
                requested: self.mono_requested(),
                applied: self.mono_applied(),
            },
            master: MasterMeterSnapshot {
                node: self.master.snapshot(),
                short_term_lufs: load_optional(&self.short_term_lufs),
                short_term_window_seconds: SHORT_TERM_SECONDS,
                correlation: load_optional(&self.correlation),
                goniometer: self.goniometer_snapshot(),
            },
            tracks,
            buses,
            spectrum,
        }
    }

    fn goniometer_snapshot(&self) -> Vec<f64> {
        if !self.goniometer_available.load(Ordering::Acquire) {
            return Vec::new();
        }
        let values = self.goniometer_points.load(Ordering::Relaxed) as usize * 2;
        self.goniometer[..values.min(GONIOMETER_VALUES)]
            .iter()
            .map(|slot| {
                // Raw sample, not a level: the display clamps it to its frame.
                let value = f32::from_bits(slot.load(Ordering::Relaxed));
                if value.is_finite() {
                    f64::from(value.clamp(-16.0, 16.0))
                } else {
                    0.0
                }
            })
            .collect()
    }
}

#[cfg(test)]
impl MeterHub {
    /// Latch the master's clip indicator, as a full-scale chunk would.
    pub(crate) fn latch_master_clip(&self) {
        self.master.clipped.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeMeterSnapshot {
    pub gain_reduction_db: f64,
    /// `None` when the node has no silent-safe input tap (bus/master input).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_peak_db: Option<f64>,
    /// `None` for silence, rather than a non-finite float.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_peak_db: Option<f64>,
    /// Mean level of the same tap; `None` for silence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_rms_db: Option<f64>,
    /// Latched until `reset_meter_clip` or the channel is gated to silence;
    /// a sample peak reached full scale.
    #[serde(default)]
    pub clipped: bool,
    /// Per-effect signed band gain in dB; present only for banded devices.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effect_bands: Vec<Vec<f64>>,
    pub effects: Vec<f64>,
    /// Peak gain reduction per effect row since reset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects_max_gain_reduction_db: Vec<f64>,
    /// Per-effect ceiling headroom; `None` for devices without a ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects_ceiling_margin_db: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MasterMeterSnapshot {
    #[serde(flatten)]
    pub node: NodeMeterSnapshot,
    /// Short-term loudness over [`SHORT_TERM_SECONDS`]; `None` while the window
    /// fills, when it is silent, or when the transport is stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_term_lufs: Option<f64>,
    pub short_term_window_seconds: f64,
    /// Left/right correlation read ahead of the mono fold, so the reading stays
    /// meaningful while MONO is engaged. `None` for a silent master.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<f64>,
    /// Interleaved left/right points of the latest chunk, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub goniometer: Vec<f64>,
}

/// The MONO control's request and what the last chunk actually did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MonoCheckSnapshot {
    pub requested: bool,
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackMeterSnapshot {
    pub index: usize,
    #[serde(flatten)]
    pub node: NodeMeterSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct BusMeterSnapshot {
    pub name: String,
    #[serde(flatten)]
    pub node: NodeMeterSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpectrumMeterSnapshot {
    /// dBFS per display band, 20 Hz to 20 kHz, low to high.
    pub bands: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeterSnapshot {
    /// True when a streaming graph is loaded and publishing meters.
    pub available: bool,
    /// Why meters are unavailable, when the engine can say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub playing: bool,
    /// Channel count of the monitored master; `0` means no graph is loaded.
    pub master_channels: u16,
    pub mono: MonoCheckSnapshot,
    pub master: MasterMeterSnapshot,
    pub tracks: Vec<TrackMeterSnapshot>,
    pub buses: Vec<BusMeterSnapshot>,
    /// Present only while an analyser panel is listening.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<SpectrumMeterSnapshot>,
}

impl MeterSnapshot {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            reason: None,
            playing: false,
            master_channels: 0,
            mono: MonoCheckSnapshot::default(),
            master: MasterMeterSnapshot::default(),
            tracks: Vec::new(),
            buses: Vec::new(),
            spectrum: None,
        }
    }
}

/// Left/right correlation of the master, smoothed over
/// [`CORRELATION_SECONDS`] so one chunk cannot make the readout jump.
pub struct Correlation {
    lr: f64,
    ll: f64,
    rr: f64,
    sample_rate: u32,
    primed: bool,
}

impl Correlation {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            lr: 0.0,
            ll: 0.0,
            rr: 0.0,
            sample_rate: sample_rate.max(1),
            primed: false,
        }
    }

    pub fn reset(&mut self) {
        self.lr = 0.0;
        self.ll = 0.0;
        self.rr = 0.0;
        self.primed = false;
    }

    /// Fold one interleaved stereo chunk into the running correlation.
    pub fn push(&mut self, buffer: &[f32]) {
        let frames = buffer.len() / 2;
        if frames == 0 {
            return;
        }
        let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
        for frame in buffer.chunks_exact(2) {
            if !frame[0].is_finite() || !frame[1].is_finite() {
                continue;
            }
            let left = f64::from(frame[0]);
            let right = f64::from(frame[1]);
            lr += left * right;
            ll += left * left;
            rr += right * right;
        }
        let frames = frames as f64;
        let (lr, ll, rr) = (lr / frames, ll / frames, rr / frames);
        if self.primed {
            let seconds = frames / f64::from(self.sample_rate);
            let coefficient = 1.0 - (-seconds / CORRELATION_SECONDS).exp();
            self.lr += (lr - self.lr) * coefficient;
            self.ll += (ll - self.ll) * coefficient;
            self.rr += (rr - self.rr) * coefficient;
        } else {
            self.lr = lr;
            self.ll = ll;
            self.rr = rr;
            self.primed = true;
        }
    }

    /// Pearson correlation in `-1.0..=1.0`; `None` for a silent master.
    pub fn value(&self) -> Option<f32> {
        if !self.primed {
            return None;
        }
        let energy = 0.5 * (self.ll + self.rr);
        if energy <= MIN_CORRELATION_ENERGY {
            return None;
        }
        let denominator = (self.ll * self.rr).sqrt();
        if denominator <= f64::EPSILON {
            return None;
        }
        Some((self.lr / denominator).clamp(-1.0, 1.0) as f32)
    }
}

/// Sample up to [`GONIOMETER_POINTS`] frames evenly across one chunk, so the
/// display shows the chunk's stereo image rather than one instant.
pub fn goniometer(buffer: &[f32], out: &mut [f32; GONIOMETER_VALUES]) -> usize {
    let frames = buffer.len() / 2;
    if frames == 0 {
        return 0;
    }
    let points = frames.clamp(1, GONIOMETER_POINTS);
    for index in 0..points {
        let frame = if points == 1 {
            0
        } else {
            index * (frames - 1) / (points - 1)
        };
        out[index * 2] = buffer[frame * 2];
        out[index * 2 + 1] = buffer[frame * 2 + 1];
    }
    points
}

fn store_db(slot: &AtomicU32, db: f32) {
    let value = if db.is_nan() {
        0.0
    } else {
        db.clamp(MIN_GR_DB, MAX_GR_DB)
    };
    slot.store(value.to_bits(), Ordering::Relaxed);
}

fn store_unit(slot: &AtomicU32, value: f32) {
    let value = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 16.0)
    };
    slot.store(value.to_bits(), Ordering::Relaxed);
}

fn store_optional(slot: &AtomicU32, value: Option<f32>) {
    let bits = value
        .filter(|value| value.is_finite())
        .map_or(f32::NAN.to_bits(), f32::to_bits);
    slot.store(bits, Ordering::Relaxed);
}

fn load_db(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.load(Ordering::Relaxed))
}

fn load_optional(slot: &AtomicU32) -> Option<f64> {
    let value = f32::from_bits(slot.load(Ordering::Relaxed));
    value.is_finite().then(|| f64::from(value))
}

fn load_level(slot: &AtomicU32) -> Option<f64> {
    let value = f32::from_bits(slot.load(Ordering::Relaxed));
    (value > 0.0).then(|| (20.0 * f64::from(value).log10()).clamp(MIN_LEVEL_DB, MAX_LEVEL_DB))
}

fn load_margin(slot: &AtomicU32) -> Option<f64> {
    let margin = f32::from_bits(slot.load(Ordering::Relaxed));
    (!margin.is_nan()).then(|| (margin as f64).clamp(MIN_GR_DB as f64, MAX_GR_DB as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Interleaved stereo sine: same signal on both channels.
    fn sine(amplitude: f32, seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds).round() as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let value =
                amplitude * (std::f32::consts::TAU * 1_000.0 * index as f32 / rate as f32).sin();
            buffer.push(value);
            buffer.push(value);
        }
        buffer
    }

    fn hub() -> MeterHub {
        let hub = MeterHub::new();
        hub.set_available(true);
        hub
    }

    fn set_master(hub: &MeterHub, level: Level) {
        hub.master.set_chain(0.0, 0.0, level, 0);
    }

    #[test]
    fn a_full_scale_sine_reads_zero_peak_and_minus_three_rms() {
        let level = Level::of(&sine(1.0, 1.0, RATE));
        let hub = hub();
        set_master(&hub, level);
        let master = hub.snapshot(true).master;
        assert!(
            (master.node.output_peak_db.expect("peak") - 0.0).abs() < 0.001,
            "{master:?}"
        );
        assert!(
            (master.node.output_rms_db.expect("rms") + 3.0103).abs() < 0.01,
            "{master:?}"
        );
        // Full scale is exactly the clip threshold, so the latch must be set.
        assert!(master.node.clipped);
    }

    #[test]
    fn a_half_scale_sine_reads_minus_six_and_minus_nine() {
        let level = Level::of(&sine(0.5, 1.0, RATE));
        let hub = hub();
        set_master(&hub, level);
        let master = hub.snapshot(true).master;
        assert!((master.node.output_peak_db.unwrap() + 6.0206).abs() < 0.001);
        assert!((master.node.output_rms_db.unwrap() + 9.0309).abs() < 0.01);
        assert!(!master.node.clipped);
    }

    #[test]
    fn silence_reads_nothing_rather_than_a_number() {
        let hub = hub();
        set_master(&hub, Level::of(&vec![0.0; 4_800 * 2]));
        let master = hub.snapshot(true).master;
        assert_eq!(master.node.output_peak_db, None);
        assert_eq!(master.node.output_rms_db, None);
        assert!(!master.node.clipped);
        assert_eq!(master.correlation, None);
        assert_eq!(master.short_term_lufs, None);
        assert!(master.goniometer.is_empty());
    }

    /// A level above the display ceiling clamps at +12 dBFS instead of
    /// reporting a number the UI could not draw.
    #[test]
    fn a_level_above_the_ceiling_clamps() {
        let hub = hub();
        set_master(
            &hub,
            Level {
                peak: 4.0,
                rms: 4.0,
            },
        );
        let master = hub.snapshot(true).master;
        assert_eq!(master.node.output_peak_db, Some(MAX_LEVEL_DB));
        assert_eq!(master.node.output_rms_db, Some(MAX_LEVEL_DB));
        // A very quiet but non-zero level clamps at the floor instead.
        set_master(
            &hub,
            Level {
                peak: 0.0,
                rms: 1e-9,
            },
        );
        assert_eq!(
            hub.snapshot(true).master.node.output_rms_db,
            Some(MIN_LEVEL_DB)
        );
    }

    #[test]
    fn a_clip_latch_survives_quiet_chunks_and_stops_until_reset() {
        let hub = hub();
        set_master(
            &hub,
            Level {
                peak: 1.2,
                rms: 0.5,
            },
        );
        assert!(hub.snapshot(true).master.node.clipped);
        set_master(
            &hub,
            Level {
                peak: 0.1,
                rms: 0.05,
            },
        );
        assert!(
            hub.snapshot(true).master.node.clipped,
            "a quiet chunk must not clear a latch"
        );
        hub.clear();
        assert!(
            hub.snapshot(true).master.node.clipped,
            "a stop must not clear a latch"
        );
        hub.reset_clips();
        assert!(!hub.snapshot(true).master.node.clipped);
    }

    /// A gated channel reads zero on every tap and drops its latch instead of
    /// holding a clip from audio that no longer reaches the mix.
    #[test]
    fn a_gated_channel_reads_zero_and_clears_its_clip() {
        let hub = hub();
        let chain = EffectChain::new();
        let loud = NodeValues::capture(
            &chain,
            1.5,
            Level {
                peak: 1.5,
                rms: 0.5,
            },
            false,
        );
        hub.tracks[0].apply(&loud);
        assert!(hub.tracks[0].snapshot().clipped);
        let gated = NodeValues::capture(&chain, 0.0, Level::default(), true);
        hub.tracks[0].apply(&gated);
        let node = hub.tracks[0].snapshot();
        assert_eq!(node.input_peak_db, None);
        assert_eq!(node.output_peak_db, None);
        assert_eq!(node.output_rms_db, None);
        assert!(!node.clipped, "a gated channel has no clip to hold");
    }

    /// `capture` publishes zero for a gated track even when the buffer it is
    /// handed still holds the lane's live input, while the other track meters.
    #[test]
    fn capture_reads_zero_for_a_gated_track() {
        let project = oc_dsl::parse(
            "@project\n@track a | instrument: keys/piano\n@track b | instrument: keys/piano\n",
        )
        .unwrap();
        let mut graph = AudioGraph::from_project(&project, 48_000);
        graph.routing[0].mute = true;
        let live = vec![0.5_f32; 8];
        let frame = MeterHub::capture(
            &graph,
            &[0.5, 0.5],
            &[live.clone(), live],
            &[0.5_f32; 8],
            MonitorValues::default(),
        );
        let hub = hub();
        hub.set_layout(2, &[], 2);
        hub.apply(&frame);
        let published = hub.snapshot(true);
        let muted = &published.tracks[0].node;
        assert_eq!(muted.input_peak_db, None);
        assert_eq!(muted.output_peak_db, None);
        assert_eq!(muted.output_rms_db, None);
        assert!(!muted.clipped);
        assert!(
            published.tracks[1].node.output_peak_db.is_some(),
            "an unmuted track still meters"
        );
    }

    #[test]
    fn correlation_reads_plus_one_for_identical_channels() {
        let mut correlation = Correlation::new(RATE);
        correlation.push(&sine(0.5, 1.0, RATE));
        let value = correlation.value().expect("correlated");
        assert!((value - 1.0).abs() < 1e-3, "{value}");
    }

    #[test]
    fn correlation_reads_minus_one_for_inverted_channels() {
        let mut correlation = Correlation::new(RATE);
        let left = sine(0.5, 1.0, RATE);
        let mut inverted = Vec::with_capacity(left.len());
        for frame in left.chunks_exact(2) {
            inverted.push(frame[0]);
            inverted.push(-frame[1]);
        }
        correlation.push(&inverted);
        let value = correlation.value().expect("anti-correlated");
        assert!((value + 1.0).abs() < 1e-3, "{value}");
    }

    #[test]
    fn correlation_of_unrelated_channels_sits_near_zero() {
        let mut correlation = Correlation::new(RATE);
        let frames = RATE as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let left = 0.5 * (std::f32::consts::TAU * 1_000.0 * index as f32 / RATE as f32).sin();
            let right = 0.5 * (std::f32::consts::TAU * 3_000.0 * index as f32 / RATE as f32).sin();
            buffer.push(left);
            buffer.push(right);
        }
        correlation.push(&buffer);
        let value = correlation.value().expect("a level is present");
        assert!(value.abs() < 0.1, "{value}");
    }

    #[test]
    fn correlation_of_silence_is_absent() {
        let mut correlation = Correlation::new(RATE);
        correlation.push(&vec![0.0; 4_800 * 2]);
        assert_eq!(correlation.value(), None);
        assert_eq!(Correlation::new(RATE).value(), None, "before any chunk");
    }

    #[test]
    fn the_goniometer_samples_the_chunk_evenly() {
        let frames = 128usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            buffer.push(index as f32);
            buffer.push(-(index as f32));
        }
        let mut out = [0.0f32; GONIOMETER_VALUES];
        let points = goniometer(&buffer, &mut out);
        assert_eq!(points, GONIOMETER_POINTS);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.0);
        assert_eq!(out[GONIOMETER_VALUES - 2], (frames - 1) as f32);
        assert_eq!(out[GONIOMETER_VALUES - 1], -((frames - 1) as f32));
        assert_eq!(goniometer(&[], &mut out), 0);
    }

    #[test]
    fn live_meter_agent_dispatch_preserves_the_actual_full_snapshot_and_unavailable_reason() {
        use crate::agent_tools::metering;
        use crate::tool_registry::ToolId;
        use serde_json::json;
        let state = crate::state::AppState::new();
        let render = crate::render_thread::RenderThreadHandle::spawn(RATE);
        let hub = render.playback.meters.clone();
        hub.set_layout(1, &["Vocal return".to_owned()], 2);
        hub.set_available(true);
        hub.set_mono_requested(true);
        hub.set_mono_applied(false);
        hub.master.set_chain(
            2.5,
            0.5,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
            1,
        );
        hub.master.set_effect(0, 1.5, 4.0, Some(2.0));
        hub.master.band_counts[0].store(2, Ordering::Relaxed);
        store_db(&hub.master.bands[0], -3.0);
        store_db(&hub.master.bands[1], 1.25);
        hub.tracks[0].set_chain(
            1.0,
            0.25,
            Level {
                peak: 1.0,
                rms: 0.5,
            },
            0,
        );
        hub.buses[0].set_chain(
            0.0,
            0.0,
            Level {
                peak: 0.125,
                rms: 0.0625,
            },
            0,
        );
        let mut points = [0.0; GONIOMETER_VALUES];
        points[0] = 0.25;
        points[1] = -0.5;
        hub.publish_monitor(&MonitorValues {
            short_term_lufs: Some(-23.5),
            correlation: Some(-0.25),
            goniometer: points,
            goniometer_points: 1,
        });
        hub.publish_spectrum(Some(&[-12.0; SPECTRUM_BANDS]));
        render.playback.active.store(true, Ordering::Release);
        state.lock_inner().audio.render_thread = Some(render);
        let read = || {
            metering::execute(
                &state,
                metering::decode(ToolId::MeterSnapshot, &json!({})).unwrap(),
            )
            .unwrap()
        };
        let actual = read();
        assert_eq!(actual, serde_json::to_value(hub.snapshot(true)).unwrap());
        assert_eq!(
            actual,
            serde_json::to_value(crate::commands::meter_snapshot_for_state(&state)).unwrap()
        );
        assert_eq!(actual["available"], true);
        assert_eq!(actual["playing"], true);
        assert_eq!(actual["master_channels"], 2);
        assert_eq!(actual["mono"], json!({"requested":true,"applied":false}));
        assert_eq!(actual["master"]["short_term_lufs"], -23.5);
        assert_eq!(actual["master"]["short_term_window_seconds"], 3.0);
        assert_eq!(actual["master"]["correlation"], -0.25);
        assert_eq!(actual["master"]["goniometer"], json!([0.25, -0.5]));
        assert_eq!(actual["master"]["gain_reduction_db"], 2.5);
        assert_eq!(actual["master"]["effects"], json!([1.5]));
        assert_eq!(actual["master"]["effect_bands"], json!([[-3.0, 1.25]]));
        assert_eq!(
            actual["master"]["effects_max_gain_reduction_db"],
            json!([4.0])
        );
        assert_eq!(actual["master"]["effects_ceiling_margin_db"], json!([2.0]));
        assert!((actual["master"]["output_peak_db"].as_f64().unwrap() + 6.0206).abs() < 0.001);
        assert!((actual["master"]["output_rms_db"].as_f64().unwrap() + 12.0412).abs() < 0.001);
        assert_eq!(actual["tracks"][0]["index"], 0);
        assert_eq!(actual["tracks"][0]["clipped"], true);
        assert_eq!(actual["buses"][0]["name"], "Vocal return");
        assert_eq!(
            actual["spectrum"]["bands"].as_array().unwrap().len(),
            SPECTRUM_BANDS
        );
        hub.set_unavailable("The output stream disconnected");
        state
            .lock_inner()
            .audio
            .render_thread
            .as_ref()
            .unwrap()
            .playback
            .active
            .store(false, Ordering::Release);
        let unavailable = read();
        assert_eq!(
            unavailable,
            serde_json::to_value(hub.snapshot(false)).unwrap()
        );
        assert_eq!(unavailable["available"], false);
        assert_eq!(unavailable["playing"], false);
        assert_eq!(unavailable["reason"], "The output stream disconnected");
        // Unavailable values are not replaced with offline or zero readings.
        assert_eq!(unavailable["master"], actual["master"]);
        hub.clear();
        hub.clear_spectrum();
        let cleared = read();
        assert!(cleared["master"].get("short_term_lufs").is_none());
        assert!(cleared["master"].get("correlation").is_none());
        assert!(cleared["master"].get("goniometer").is_none());
        assert!(cleared.get("spectrum").is_none());
    }

    #[test]
    fn published_monitor_values_land_in_the_snapshot() {
        let hub = hub();
        // Values stay inside the display range; the clamp is checked below.
        let mut goniometer = [0.0f32; GONIOMETER_VALUES];
        for (index, slot) in goniometer.iter_mut().enumerate() {
            *slot = -1.0 + index as f32 / 64.0;
        }
        hub.publish_monitor(&MonitorValues {
            short_term_lufs: Some(-23.5),
            correlation: Some(-0.25),
            goniometer,
            goniometer_points: GONIOMETER_POINTS,
        });
        let master = hub.snapshot(true).master;
        assert_eq!(master.short_term_lufs, Some(-23.5));
        assert_eq!(master.short_term_window_seconds, 3.0);
        assert_eq!(master.correlation, Some(-0.25));
        assert_eq!(master.goniometer.len(), GONIOMETER_VALUES);
        assert_eq!(master.goniometer[0], -1.0);
        assert_eq!(
            master.goniometer[GONIOMETER_VALUES - 1],
            -1.0 + (GONIOMETER_VALUES - 1) as f64 / 64.0
        );
        // A point outside the display range clamps instead of stretching it.
        let mut hot = [0.0f32; GONIOMETER_VALUES];
        hot[0] = 100.0;
        hub.publish_monitor(&MonitorValues {
            goniometer: hot,
            goniometer_points: 1,
            ..MonitorValues::default()
        });
        assert_eq!(hub.snapshot(true).master.goniometer[0], 16.0);
        hub.clear();
        let master = hub.snapshot(true).master;
        assert_eq!(master.short_term_lufs, None);
        assert_eq!(master.correlation, None);
        assert!(master.goniometer.is_empty());
    }

    #[test]
    fn a_non_finite_monitor_reading_is_absent() {
        let hub = hub();
        hub.publish_monitor(&MonitorValues {
            short_term_lufs: Some(f32::NAN),
            correlation: Some(f32::INFINITY),
            ..MonitorValues::default()
        });
        let master = hub.snapshot(true).master;
        assert_eq!(master.short_term_lufs, None);
        assert_eq!(master.correlation, None);
    }

    #[test]
    fn mono_state_reports_the_request_and_what_was_applied() {
        let hub = hub();
        assert_eq!(hub.snapshot(true).mono, MonoCheckSnapshot::default());
        hub.set_mono_requested(true);
        assert!(hub.snapshot(true).mono.requested);
        assert!(!hub.snapshot(true).mono.applied, "no chunk folded it yet");
        hub.set_mono_applied(true);
        assert_eq!(
            hub.snapshot(true).mono,
            MonoCheckSnapshot {
                requested: true,
                applied: true,
            }
        );
        hub.clear();
        let mono = hub.snapshot(true).mono;
        assert!(mono.requested, "a stop keeps the request");
        assert!(!mono.applied, "a stop ends the fold");
    }

    #[test]
    fn master_channels_reflect_the_loaded_graph() {
        let hub = hub();
        assert_eq!(hub.snapshot(true).master_channels, 0);
        hub.set_layout(0, &[], 2);
        assert_eq!(hub.snapshot(true).master_channels, 2);
        hub.set_layout(0, &[], 1);
        assert_eq!(hub.snapshot(true).master_channels, 1);
    }

    /// The field names here are the UI contract in `types/meters.ts`, which a
    /// component test cannot see: it feeds a hand-written snapshot.
    #[test]
    fn the_snapshot_wire_shape_matches_the_ui_contract() {
        let hub = hub();
        hub.set_layout(1, &["fx".to_string()], 2);
        hub.set_mono_requested(true);
        set_master(
            &hub,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
        );
        hub.publish_monitor(&MonitorValues {
            short_term_lufs: Some(-14.0),
            correlation: Some(-0.5),
            ..MonitorValues::default()
        });
        let json = serde_json::to_value(hub.snapshot(true)).expect("a snapshot serializes");
        assert_eq!(json["available"], true);
        assert_eq!(json["playing"], true);
        assert_eq!(json["master_channels"], 2);
        assert_eq!(json["mono"]["requested"], true);
        assert_eq!(json["mono"]["applied"], false);
        assert!(json["master"]["output_peak_db"].is_number());
        assert!(json["master"]["output_rms_db"].is_number());
        assert!(json["master"]["short_term_lufs"].is_number());
        assert_eq!(json["master"]["short_term_window_seconds"], 3.0);
        assert!(json["master"]["correlation"].is_number());
        assert!(json["master"]["clipped"].is_boolean());
        assert_eq!(json["tracks"][0]["index"], 0);
        assert!(json["tracks"][0]["clipped"].is_boolean());
        assert_eq!(json["buses"][0]["name"], "fx");
    }

    #[test]
    fn an_unavailable_hub_carries_its_reason() {
        let hub = MeterHub::new();
        hub.set_unavailable("no streaming graph is loaded");
        let snapshot = hub.snapshot(false);
        assert!(!snapshot.available);
        assert_eq!(
            snapshot.reason.as_deref(),
            Some("no streaming graph is loaded")
        );
        hub.set_available(true);
        assert_eq!(hub.snapshot(false).reason, None);
    }

    #[test]
    fn non_finite_and_extreme_values_are_clamped() {
        let hub = hub();
        hub.tracks[0].set_chain(f32::NAN, f32::INFINITY, Level::default(), 1);
        hub.tracks[0].set_effect(0, f32::NAN, f32::NAN, None);
        hub.tracks[1].set_chain(
            500.0,
            0.5,
            Level {
                peak: 2.0,
                rms: 0.5,
            },
            1,
        );
        hub.tracks[1].set_effect(0, f32::INFINITY, f32::INFINITY, Some(f32::NEG_INFINITY));
        hub.set_layout(2, &[], 2);
        let snapshot = hub.snapshot(true);
        assert_eq!(snapshot.tracks[0].node.gain_reduction_db, 0.0);
        assert_eq!(snapshot.tracks[0].node.output_peak_db, None);
        assert_eq!(snapshot.tracks[0].node.output_rms_db, None);
        assert_eq!(snapshot.tracks[1].node.gain_reduction_db, 60.0);
        assert!((snapshot.tracks[1].node.input_peak_db.unwrap() + 6.0206).abs() < 1e-3);
        assert_eq!(snapshot.tracks[1].node.effects, vec![60.0]);
        assert_eq!(
            snapshot.tracks[1].node.effects_max_gain_reduction_db,
            vec![60.0]
        );
        assert_eq!(
            snapshot.tracks[1].node.effects_ceiling_margin_db,
            vec![Some(-60.0)]
        );
        assert_eq!(
            snapshot.tracks[0].node.effects_ceiling_margin_db,
            vec![None]
        );
    }

    #[test]
    fn limiter_effect_rows_report_current_max_and_ceiling_margin() {
        let hub = hub();
        hub.master.set_chain(2.5, 0.0, Level::default(), 2);
        hub.master.set_effect(0, 2.5, 9.0, Some(1.25));
        hub.master.set_effect(1, 0.0, 0.0, None);
        let node = hub.snapshot(true).master;
        assert_eq!(node.node.effects, vec![2.5, 0.0]);
        assert_eq!(node.node.effects_max_gain_reduction_db, vec![9.0, 0.0]);
        assert_eq!(node.node.effects_ceiling_margin_db, vec![Some(1.25), None]);
    }

    #[test]
    fn bus_layout_follows_published_names() {
        let hub = hub();
        hub.set_layout(0, &["fx".to_string(), "verb".to_string()], 2);
        hub.buses[1].set_chain(
            3.5,
            0.0,
            Level {
                peak: 0.5,
                rms: 0.25,
            },
            1,
        );
        hub.buses[1].set_effect(0, 3.5, 3.5, None);
        let snapshot = hub.snapshot(true);
        assert_eq!(snapshot.buses.len(), 2);
        assert_eq!(snapshot.buses[0].name, "fx");
        assert_eq!(snapshot.buses[1].name, "verb");
        assert_eq!(snapshot.buses[1].node.gain_reduction_db, 3.5);
        assert_eq!(snapshot.buses[1].node.effects, vec![3.5]);
        assert!((snapshot.buses[1].node.output_peak_db.unwrap() + 6.0206).abs() < 1e-3);
        assert!((snapshot.buses[1].node.output_rms_db.unwrap() + 12.0412).abs() < 1e-3);
    }
}
