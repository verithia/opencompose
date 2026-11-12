//! SoundFont 2 bank model: `INFO`, `sdta` and `pdta`, plus zone resolution.
//!
//! The parser is strict about structure (chunk tree, record sizes) and tolerant
//! about content: a zone that names a missing sample, an inverted key range or
//! a truncated record is reported in [`SoundFont::warnings`] and dropped instead
//! of failing the whole bank. Modulator records are read: the subset that drives
//! the pitch, cutoff, resonance, volume, envelope-time, pan and effect-send
//! destinations is applied, the rest is counted in [`SoundFont::warnings`].

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::SystemTime;

use super::generator::{GenSet, Generator};
use super::modulation::{
    self, DEFAULT_MODULATORS, MAX_ZONE_MODULATORS, MODULATOR_RECORD_SIZE, ModInput, Modulation,
    Modulator,
};
use super::riff::{self, Chunk, ChunkReader};
use super::vorbis;

mod prepared;
pub(super) use prepared::prepare_streaming;

/// Record sizes from the specification's `pdta` table.
const PHDR_SIZE: usize = 38;
const BAG_SIZE: usize = 4;
const GEN_SIZE: usize = 4;
const INST_SIZE: usize = 22;
const SHDR_SIZE: usize = 46;

/// `initialAttenuation` is applied at 0.4 centibels per unit — the EMU8k
/// factor the format multiplies into the generator, so one raw unit is
/// 0.04 dB. A modulator record's amount is already centibels and takes no
/// factor.
const ATTENUATION_DB_PER_UNIT: f64 = 0.04;
const ATTENUATION_CB_PER_UNIT: f32 = 0.4;

/// `sustainVolEnv` is a centibel scale; the engine applies 0.96 dB per unit,
/// measured over 50..800 units.
const SUSTAIN_DB_PER_UNIT: f32 = 0.096;

/// The format's ceiling for `initialFilterQ`, in centibels of peak gain.
const MAX_FILTER_Q_CB: f32 = 960.0;

/// The reader's legal envelope-stage range in seconds: [`timecents_to_seconds`]
/// clamps to the format's -12000..8000 timecents.
const MIN_ENVELOPE_SECONDS: f64 = 0.000_976_562_5;
const MAX_ENVELOPE_SECONDS: f64 = 101.593_667_325_964_8;

/// Rate assumed when a header omits one. Every bank in circulation writes the
/// rate, so this only guards a corrupt header.
const FALLBACK_SAMPLE_RATE: u32 = 44_100;

/// Why a bank could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Structure(#[from] riff::Error),
    #[error("SoundFont bank has no {0}")]
    Missing(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("SoundFont decoded waveform exceeds the {limit}-byte streaming preparation limit")]
    PreparationLimit { limit: usize },
}

/// Loop behaviour from the `sampleModes` generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleMode {
    NoLoop,
    /// The loop runs for the whole note, release included.
    Continuous,
    /// The loop runs while the key is down, then playback continues to the end.
    UntilRelease,
}

impl SampleMode {
    fn from_generator(value: i16) -> Self {
        match value {
            1 => Self::Continuous,
            3 => Self::UntilRelease,
            // 2 is not a valid mode; every unsupported value plays once.
            _ => Self::NoLoop,
        }
    }

    /// True while the key is down or the mode ignores release.
    pub fn loops_now(self, key_down: bool) -> bool {
        match self {
            Self::NoLoop => false,
            Self::Continuous => true,
            Self::UntilRelease => key_down,
        }
    }
}

/// An envelope's shape, already converted to seconds and decibels.
///
/// The volume and modulation envelopes share this shape: delay, attack, hold,
/// decay, sustain (centibels of attenuation) and release, plus the key-number
/// scaling of hold and decay. The modulation envelope's sustain of 0 is full
/// scale, so its output multiplies a depth directly.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeShape {
    pub delay: f64,
    pub attack: f64,
    pub hold: f64,
    pub decay: f64,
    /// Sustained attenuation in centibels (0 = unity, 1440 = silence).
    pub sustain_cb: f32,
    pub release: f64,
    /// Hold/decay time change in timecents per key, measured from key 60.
    pub hold_key_scale: f32,
    pub decay_key_scale: f32,
}

impl EnvelopeShape {
    /// Sustained attenuation in decibels.
    pub fn sustain_db(self) -> f32 {
        self.sustain_cb * SUSTAIN_DB_PER_UNIT
    }

    /// Hold length for one key after the keynum-to-hold generator.
    pub fn hold_for(self, key: u8) -> f64 {
        scale_time_for_key(self.hold, self.hold_key_scale, key)
    }

    /// Decay length for one key after the keynum-to-decay generator.
    pub fn decay_for(self, key: u8) -> f64 {
        scale_time_for_key(self.decay, self.decay_key_scale, key)
    }

    /// The shape with a modulator record's attack, decay and release offsets
    /// folded in, in timecents.
    ///
    /// Scaling the stored seconds by `2^(timecents/1200)` is the same law as
    /// adding timecents before the reader converts, and the result is clamped
    /// to the converter's legal range so a stage already at a limit stays
    /// there. A zero-length stage stays instant. The voice calls this once, at
    /// note-on.
    pub fn with_time_offsets(self, attack_tc: f32, decay_tc: f32, release_tc: f32) -> Self {
        Self {
            attack: offset_stage_time(self.attack, attack_tc),
            decay: offset_stage_time(self.decay, decay_tc),
            release: offset_stage_time(self.release, release_tc),
            ..self
        }
    }

    /// The shape with a modulator record's sustain offset folded in, in the
    /// generator's 0.1 % units.
    pub fn with_sustain_offset(self, units: f32) -> Self {
        Self {
            sustain_cb: (self.sustain_cb + units).clamp(0.0, 1440.0),
            ..self
        }
    }
}

/// One envelope stage after a note-on timecents offset.
fn offset_stage_time(seconds: f64, timecents: f32) -> f64 {
    if timecents == 0.0 || seconds <= 0.0 {
        return seconds;
    }
    (seconds * 2f64.powf(f64::from(timecents) / 1200.0))
        .clamp(MIN_ENVELOPE_SECONDS, MAX_ENVELOPE_SECONDS)
}

/// Timecents per key relative to key 60; a positive scale shortens high keys.
fn scale_time_for_key(seconds: f64, timecents_per_key: f32, key: u8) -> f64 {
    if timecents_per_key == 0.0 {
        return seconds;
    }
    let shift = f32::from(60i16.saturating_sub(i16::from(key))) * timecents_per_key;
    seconds * 2f64.powf(f64::from(shift) / 1200.0)
}

/// One playable region: a preset zone crossed with an instrument zone and a
/// sample header, with all generators merged.
#[derive(Debug, Clone)]
pub struct Zone {
    pub key_low: u8,
    pub key_high: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    /// Index into [`SoundFont::sample_headers`].
    pub sample: usize,
    /// First frame to play, absolute in the bank's sample data.
    pub start: u64,
    /// One past the last frame to play.
    pub end: u64,
    pub loop_start: u64,
    pub loop_end: u64,
    pub root_key: u8,
    pub coarse_tune: i16,
    pub fine_tune: i16,
    pub scale_tuning: i16,
    /// `keynum`: the key this zone synthesizes as, whatever key triggered it.
    /// `None` is the format's -1 sentinel, "use the note's own key".
    pub keynum: Option<u8>,
    /// `velocity`: the velocity this zone synthesizes as, whatever velocity
    /// triggered it. `None` is the format's -1 sentinel.
    pub velocity: Option<u8>,
    pub attenuation_cb: i16,
    /// -1.0 (left) .. 1.0 (right).
    pub pan: f32,
    pub sample_mode: SampleMode,
    pub envelope: EnvelopeShape,
    /// The modulation envelope's own delay/attack/hold/decay/sustain/release.
    pub mod_env: EnvelopeShape,
    /// Absolute filter cutoff in cents above 8.176 Hz.
    pub filter_cutoff_cents: i16,
    /// Filter resonance in centibels.
    pub filter_q_cb: i16,
    pub exclusive_class: i16,
    /// `vibLfoToPitch`: vibrato depth in cents at the LFO's peak.
    pub vib_lfo_to_pitch: f32,
    /// `freqVibLFO` in hertz, from the format's 8.176 Hz origin.
    pub vibrato_hz: f32,
    /// `delayVibLFO` in seconds.
    pub vibrato_delay: f64,
    /// `modLfoToPitch`: modulation-LFO depth in cents.
    pub mod_lfo_to_pitch: f32,
    /// `freqModLFO` in hertz.
    pub mod_lfo_hz: f32,
    /// `delayModLFO` in seconds.
    pub mod_lfo_delay: f64,
    /// `modLfoToFilterFc`: cutoff depth in cents.
    pub mod_lfo_to_filter_fc: f32,
    /// `modLfoToVolume`: volume depth in centibels of attenuation.
    pub mod_lfo_to_volume: f32,
    /// `modEnvToPitch`: depth in cents at the envelope's peak.
    pub mod_env_to_pitch: f32,
    /// `modEnvToFilterFc`: cutoff depth in cents at the envelope's peak.
    pub mod_env_to_filter_fc: f32,
    /// `chorusEffectsSend` in tenths of a percent, 0..1000.
    pub chorus_send: i16,
    /// `reverbEffectsSend` in tenths of a percent, 0..1000.
    pub reverb_send: i16,
    /// The bank's own modulator records for this zone, in the modelled subset.
    pub modulators: Vec<Modulator>,
}

impl Zone {
    /// Whether the zone covers a key and velocity.
    pub fn matches(&self, key: u8, velocity: u8) -> bool {
        (self.key_low..=self.key_high).contains(&key)
            && (self.vel_low..=self.vel_high).contains(&velocity)
    }

    /// The key the synthesis uses: the `keynum` generator when the zone
    /// declares one, else the key that triggered it.
    pub fn effective_key(&self, key: u8) -> u8 {
        self.keynum.unwrap_or(key)
    }

    /// The velocity the synthesis uses, with the `velocity` generator applied.
    pub fn effective_velocity(&self, velocity: u8) -> u8 {
        self.velocity.unwrap_or(velocity)
    }

    /// True when the zone asks for a low-pass: its cutoff sits below the
    /// bypass value, one of the sources that moves the cutoff is declared, or
    /// a record with a nonzero amount targets the cutoff.
    ///
    /// The velocity-to-cutoff default modulator is deliberately not part of
    /// this test: a zone without a filter stays unfiltered at every velocity,
    /// which keeps the unmodulated path bit-identical. A record that reaches
    /// `initialFilterQ` does not enable the filter either: the format's bypass
    /// cutoff leaves no peak to sharpen.
    pub fn uses_filter(&self) -> bool {
        if self.filter_cutoff_cents < 13_500
            || self.mod_lfo_to_filter_fc != 0.0
            || self.mod_env_to_filter_fc != 0.0
        {
            return true;
        }
        self.modulators.iter().any(|modulator| {
            modulator.amount != 0.0
                && matches!(
                    modulator.destination,
                    modulation::Destination::InitialFilterFc
                        | modulation::Destination::ModLfoToFilterFc
                        | modulation::Destination::ModEnvToFilterFc
                )
        })
    }

    /// Cutoff in hertz, or `None` when the filter is at its bypass default.
    pub fn filter_cutoff_hz(&self) -> Option<f64> {
        if self.filter_cutoff_cents >= 13_500 {
            return None;
        }
        Some(8.176 * 2f64.powf(f64::from(self.filter_cutoff_cents) / 1200.0))
    }

    /// Resonance as a biquad Q, from the zone's `initialFilterQ` alone.
    pub fn filter_q(&self) -> f64 {
        filter_q_from_centibels(f32::from(self.filter_q_cb))
    }

    /// Linear gain from `initialAttenuation`.
    pub fn attenuation_gain(&self) -> f64 {
        attenuation_to_gain(f64::from(self.attenuation_cb.clamp(0, 1440)))
    }

    /// The zone's `initialAttenuation` in centibels, the unit a modulator
    /// record's amount uses.
    pub fn attenuation_cb(&self) -> f32 {
        f32::from(self.attenuation_cb.clamp(0, 1440)) * ATTENUATION_CB_PER_UNIT
    }

    /// Sustained attenuation of the volume envelope in decibels.
    pub fn sustain_db(&self) -> f32 {
        self.envelope.sustain_db()
    }

    /// Modulated depths for the current controller state.
    ///
    /// The zone's generators come first, then the format's default modulators,
    /// then the bank's own records, all summed in each destination's unit.
    /// Pitch and cutoff are cents, volume is centibels of attenuation, the
    /// sends are tenths of a percent. The Q and envelope-time destinations are
    /// summed here too, but only the note-on read applies them; see
    /// [`Modulation`].
    pub fn modulation(&self, input: &ModInput<'_>) -> Modulation {
        let mut modulation = Modulation::from_generators(
            self.vib_lfo_to_pitch,
            self.mod_lfo_to_pitch,
            self.mod_env_to_pitch,
            self.mod_lfo_to_filter_fc,
            self.mod_env_to_filter_fc,
            self.mod_lfo_to_volume,
            f32::from(self.chorus_send),
            f32::from(self.reverb_send),
        );
        for modulator in DEFAULT_MODULATORS.iter().chain(&self.modulators) {
            modulation.add(modulator.destination, modulator.value(input));
        }
        modulation
    }
}

/// Generator attenuation units to a linear gain factor.
pub fn attenuation_to_gain(units: f64) -> f64 {
    10f64.powf(-units * ATTENUATION_DB_PER_UNIT / 20.0)
}

/// Biquad Q from a summed `initialFilterQ`, in centibels of peak gain.
///
/// The zone's generator and a record's amount are both centibels, so the voice
/// adds them before the clamp to the format's 0..960 range.
pub fn filter_q_from_centibels(centibels: f32) -> f64 {
    10f64.powf(f64::from(centibels.clamp(0.0, MAX_FILTER_Q_CB)) / 200.0)
}

/// A `shdr` record.
#[derive(Debug, Clone)]
pub struct SampleHeader {
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub rate: u32,
    pub root_key: u8,
    /// Pitch correction in cents applied on top of the root key.
    pub correction: i8,
    /// 1 = mono, 2 = right of a pair, 4 = left of a pair, 8 = linked.
    pub sample_type: u16,
}

impl SampleHeader {
    /// Whether the sample is a stereo pair.
    pub fn is_stereo(&self) -> bool {
        self.sample_type & 0b0110 != 0
    }
}

/// The `smpl` waveform, one point per frame.
#[derive(Debug)]
pub enum SampleBank {
    Points16(Vec<i16>),
    /// 24-bit points sign-extended into `i32`.
    Points24(Vec<i32>),
}

impl SampleBank {
    /// Sample frame count.
    pub fn frames(&self) -> usize {
        match self {
            Self::Points16(points) => points.len(),
            Self::Points24(points) => points.len(),
        }
    }

    /// Sample bit depth.
    pub fn bit_depth(&self) -> u8 {
        match self {
            Self::Points16(_) => 16,
            Self::Points24(_) => 24,
        }
    }

    /// One frame as a normalized sample; out-of-range frames read as silence.
    pub fn point(&self, frame: usize) -> f32 {
        match self {
            Self::Points16(points) => points
                .get(frame)
                .map_or(0.0, |value| f32::from(*value) * (1.0 / 32768.0)),
            Self::Points24(points) => points.get(frame).map_or(0.0, |value| {
                // 2^23 keeps the full negative range symmetric with 16-bit reads.
                *value as f32 * (1.0 / 8_388_608.0)
            }),
        }
    }

    /// Linearly interpolated read; the frame index is clamped at the last point.
    pub fn read(&self, position: f64) -> f32 {
        if position < 0.0 {
            return self.point(0);
        }
        let frame = position.floor();
        let fraction = (position - frame) as f32;
        let frame = frame as usize;
        let first = self.point(frame);
        if fraction == 0.0 {
            return first;
        }
        first + (self.point(frame + 1) - first) * fraction
    }
}

/// A `phdr` record and its resolved playable zones.
#[derive(Debug)]
pub struct Preset {
    pub name: String,
    pub bank: u16,
    pub program: u16,
    pub zones: Vec<Zone>,
}

/// An `inst` record; kept for diagnostics and parser tests.
#[derive(Debug)]
pub struct Instrument {
    pub name: String,
    pub zone_count: usize,
}

/// A parsed bank. Share one instance per file: it owns the whole waveform.
#[derive(Debug)]
pub struct SoundFont {
    pub name: Option<String>,
    /// `ifil` version as `(major, minor)`.
    pub version: (u16, u16),
    /// True when the sample data was stored as Ogg Vorbis per sample (an SF3
    /// bank), decided by content: an Ogg page header at a sample's start.
    pub compressed: bool,
    pub sample_bank: SampleBank,
    pub sample_headers: Vec<SampleHeader>,
    pub presets: Vec<Preset>,
    pub instruments: Vec<Instrument>,
    /// Recoverable defects found while parsing or resolving zones.
    pub warnings: Vec<String>,
}

/// How many parsed banks stay resident in [`SoundFont::load_shared`].
///
/// A project normally names one to four banks; the cap only bounds a session
/// that auditions many. A bank a player still holds keeps playing from its own
/// [`Arc`] after its cache entry goes, and the next load reuses that parse
/// instead of reading the file again; only a bank nobody holds is parsed anew.
const SHARED_BANKS_RESIDENT: usize = 8;

/// The file a cached parse was read from. Replacing the file at the same path
/// changes this identity, so the next load parses the new content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BankIdentity {
    modified: Option<SystemTime>,
    length: u64,
}

impl BankIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            length: metadata.len(),
        }
    }
}

/// Process-wide cache of parsed banks, least recently used first.
#[derive(Default)]
struct SharedBanks {
    entries: HashMap<PathBuf, (BankIdentity, Arc<SoundFont>)>,
    order: VecDeque<PathBuf>,
    /// Every parse handed out, resident or not. An evicted bank that a player
    /// still holds is found here, so two players never hold two copies.
    held: HashMap<PathBuf, (BankIdentity, Weak<SoundFont>)>,
}

impl SharedBanks {
    fn get(&mut self, key: &Path, identity: &BankIdentity) -> Option<Arc<SoundFont>> {
        if let Some((cached, font)) = self.entries.get(key) {
            return (cached == identity).then(|| Arc::clone(font));
        }
        let (cached, weak) = self.held.get(key)?;
        if cached != identity {
            return None;
        }
        let font = weak.upgrade()?;
        self.insert(key.to_path_buf(), identity.clone(), Arc::clone(&font));
        Some(font)
    }

    fn touch(&mut self, key: &Path) {
        if let Some(index) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.to_path_buf());
    }

    fn insert(&mut self, key: PathBuf, identity: BankIdentity, font: Arc<SoundFont>) {
        self.held.retain(|_, (_, weak)| weak.strong_count() > 0);
        self.held
            .insert(key.clone(), (identity.clone(), Arc::downgrade(&font)));
        self.entries.insert(key.clone(), (identity, font));
        self.touch(&key);
        while self.entries.len() > SHARED_BANKS_RESIDENT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn remove(&mut self, key: &Path) {
        self.entries.remove(key);
        self.held.remove(key);
        if let Some(index) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(index);
        }
    }
}

fn shared_banks() -> &'static Mutex<SharedBanks> {
    static BANKS: OnceLock<Mutex<SharedBanks>> = OnceLock::new();
    BANKS.get_or_init(|| Mutex::new(SharedBanks::default()))
}

/// The canonical path, or the path itself when it does not resolve.
///
/// A bank that was just deleted no longer canonicalizes; its directory still
/// does, so an invalidation after a removal lands on the entry the load made.
fn cache_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

fn file_identity(path: &Path) -> Option<BankIdentity> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(BankIdentity::from_metadata(&metadata))
}

impl SoundFont {
    /// Read a bank from disk. The whole waveform is kept in memory once.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let bytes = std::fs::read(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&bytes)
    }

    /// Read a shared bank, parsing each file at most once while it is unchanged.
    ///
    /// A bank replaced or reinstalled at the same path is read again, because
    /// the cache entry is valid only for the file's modification time and
    /// length. [`Self::invalidate_shared`] drops an entry outright.
    pub fn load_shared(path: &Path) -> Result<Arc<Self>, Error> {
        let key = cache_key(path);
        let identity = file_identity(&key);
        // One lock across the parse: parallel track workers must not each read
        // a hundred-megabyte bank into memory.
        let mut cache = shared_banks()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(identity) = identity.as_ref()
            && let Some(font) = cache.get(&key, identity)
        {
            cache.touch(&key);
            return Ok(font);
        }
        let font = Arc::new(Self::load(path)?);
        if let Some(identity) = identity {
            cache.insert(key, identity, Arc::clone(&font));
        }
        Ok(font)
    }

    /// Reuse a completed full-bank entry without waiting behind another load.
    /// This only probes file identity and the cache; it never reads sample data
    /// or takes the global lock while another bank is being decoded.
    fn ready_shared(path: &Path) -> Option<Arc<Self>> {
        let key = cache_key(path);
        let identity = file_identity(&key)?;
        Self::ready_shared_identity(&key, &identity)
    }

    fn ready_shared_identity(key: &Path, identity: &BankIdentity) -> Option<Arc<Self>> {
        let mut cache = shared_banks().try_lock().ok()?;
        let font = cache.get(key, identity)?;
        cache.touch(key);
        Some(font)
    }

    /// Drop the cached parse of `path`.
    ///
    /// Call this after a bank is installed or removed, so a reinstall is
    /// visible immediately even when the filesystem's timestamps are coarse.
    pub fn invalidate_shared(path: &Path) {
        let key = cache_key(path);
        shared_banks()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&key);
    }

    /// Parse a bank from memory.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut raw = RawBank::parse(bytes)?;
        let (sample_bank, sample_headers, compressed) = parse_sdta(
            &raw.sdta,
            &raw.region.sample_headers,
            &mut raw.warnings,
            None,
        )?;
        raw.finish(sample_bank, sample_headers, compressed)
    }

    fn parse_prepared_fallback(bytes: &[u8], limit: usize) -> Result<Self, Error> {
        let mut raw = RawBank::parse(bytes)?;
        let (bank, headers, compressed) = parse_sdta(
            &raw.sdta,
            &raw.region.sample_headers,
            &mut raw.warnings,
            Some(limit),
        )?;
        raw.finish(bank, headers, compressed)
    }

    fn check_preparation_limit(&self, limit: usize) -> Result<(), Error> {
        let point_bytes = match self.sample_bank {
            SampleBank::Points16(_) => size_of::<i16>(),
            SampleBank::Points24(_) => size_of::<i32>(),
        };
        check_sample_bytes(self.sample_bank.frames(), point_bytes, Some(limit))
    }

    /// Exact bank/program lookup.
    pub fn preset_index(&self, bank: u16, program: u16) -> Option<usize> {
        self.presets
            .iter()
            .position(|preset| preset.bank == bank && preset.program == program)
    }

    /// Drum-kit lookup: kits live in bank 128.
    pub fn drum_preset_index(&self, program: u16) -> Option<usize> {
        self.preset_index(128, program)
    }

    /// Preset lookup with the fallbacks a program change needs: exact match,
    /// then the same program in any bank, then the lowest program of bank 0.
    pub fn find_preset(&self, bank: u16, program: u16) -> Option<usize> {
        if let Some(index) = self.preset_index(bank, program) {
            return Some(index);
        }
        if let Some(index) = self.presets.iter().position(|p| p.program == program) {
            return Some(index);
        }
        self.presets
            .iter()
            .enumerate()
            .filter(|(_, preset)| preset.bank == 0)
            .min_by_key(|(_, preset)| preset.program)
            .map(|(index, _)| index)
            .or(if self.presets.is_empty() {
                None
            } else {
                Some(0)
            })
    }

    /// Sample rate a zone's waveform should be read at.
    pub fn sample_rate_of(&self, zone: &Zone) -> u32 {
        self.sample_headers
            .get(zone.sample)
            .map(|header| header.rate)
            .filter(|rate| *rate > 0)
            .unwrap_or(FALLBACK_SAMPLE_RATE)
    }
}

// ============================================================================
// INFO and sdta
// ============================================================================

fn parse_info(
    list: &Chunk<'_>,
    warnings: &mut Vec<String>,
) -> Result<(Option<String>, (u16, u16)), Error> {
    let mut name = None;
    let mut version = (2, 1);
    for chunk in ChunkReader::list(list.data, b"INFO")? {
        let chunk = chunk?;
        match &chunk.id {
            b"ifil" if chunk.data.len() >= 4 => {
                version = (riff::u16_at(chunk.data, 0), riff::u16_at(chunk.data, 2));
            }
            b"INAM" => {
                let value = String::from_utf8_lossy(chunk.data)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string();
                if !value.is_empty() {
                    name = Some(value);
                }
            }
            b"isng" => {}
            _ => {}
        }
    }
    if version.0 < 2 {
        warnings.push(format!(
            "bank version {}.{} predates SoundFont 2; read as 2.01",
            version.0, version.1
        ));
    }
    Ok((name, version))
}

/// Read `smpl`/`sm24` into one flat waveform and the headers that describe it.
///
/// A bank whose sample data starts with an Ogg page header is an SF3: its
/// sample headers carry byte offsets into `smpl` and per-sample loop points
/// relative to the sample's start. Content decides, not the file name, so a
/// `.sf3` that is really raw PCM takes the SF2 path unchanged, and the two may
/// mix inside one bank.
fn parse_sdta(
    list: &Chunk<'_>,
    headers: &[SampleHeader],
    warnings: &mut Vec<String>,
    max_bytes: Option<usize>,
) -> Result<(SampleBank, Vec<SampleHeader>, bool), Error> {
    let mut smpl: Option<&[u8]> = None;
    let mut sm24: Option<&[u8]> = None;
    for chunk in ChunkReader::list(list.data, b"sdta")? {
        let chunk = chunk?;
        match &chunk.id {
            b"smpl" => smpl = Some(chunk.data),
            b"sm24" => sm24 = Some(chunk.data),
            _ => {}
        }
    }
    let smpl = smpl.ok_or(Error::Missing("smpl chunk"))?;
    let compressed = headers.iter().any(|header| {
        smpl.get(header.start as usize..)
            .is_some_and(vorbis::is_ogg)
    });
    if compressed {
        return decode_compressed(smpl, headers, warnings, max_bytes);
    }
    let max_sample_end = headers.iter().map(|header| header.end).max().unwrap_or(0);
    // A 24-bit bank either splits the low byte into `sm24` or stores complete
    // three-byte points in `smpl`; the header offsets decide which.
    if let Some(low) = sm24.filter(|low| !low.is_empty() && low.len() * 2 == smpl.len()) {
        check_sample_bytes(low.len(), size_of::<i32>(), max_bytes)?;
        let points = (0..low.len())
            .map(|index| {
                let high = riff::i16_at(smpl, index * 2) as i32;
                (high << 8) | i32::from(low[index])
            })
            .collect();
        return Ok((SampleBank::Points24(points), headers.to_vec(), false));
    }
    let frames16 = smpl.len() / 2;
    let wants_24 =
        max_sample_end as usize > frames16 && (max_sample_end as usize) * 3 <= smpl.len();
    if wants_24 {
        check_sample_bytes(max_sample_end as usize, size_of::<i32>(), max_bytes)?;
        warnings
            .push("sample data is 3-byte framed; offsets are read as 24-bit points".to_string());
        let points = (0..max_sample_end as usize)
            .map(|index| {
                let bytes = smpl.get(index * 3..index * 3 + 3).unwrap_or_default();
                let raw = u32::from_le_bytes([
                    bytes.first().copied().unwrap_or(0),
                    bytes.get(1).copied().unwrap_or(0),
                    bytes.get(2).copied().unwrap_or(0),
                    0,
                ]) as i32;
                (raw << 8) >> 8
            })
            .collect();
        return Ok((SampleBank::Points24(points), headers.to_vec(), false));
    }
    check_sample_bytes(frames16, size_of::<i16>(), max_bytes)?;
    let points = (0..frames16)
        .map(|index| riff::i16_at(smpl, index * 2))
        .collect();
    Ok((SampleBank::Points16(points), headers.to_vec(), false))
}

fn check_sample_bytes(
    frames: usize,
    point_bytes: usize,
    limit: Option<usize>,
) -> Result<(), Error> {
    if let Some(limit) = limit
        && frames
            .checked_mul(point_bytes)
            .is_none_or(|bytes| bytes > limit)
    {
        return Err(Error::PreparationLimit { limit });
    }
    Ok(())
}

/// Samples decoded per worker before the results are joined, so a bank never
/// holds more than a few hundred decoded samples at once.
const DECODE_BATCH: usize = 32;

/// The SF3 path: decode every sample region and rebuild the flat waveform,
/// rewriting the headers into frame offsets that the zone resolver already
/// understands.
fn decode_compressed(
    smpl: &[u8],
    headers: &[SampleHeader],
    warnings: &mut Vec<String>,
    max_bytes: Option<usize>,
) -> Result<(SampleBank, Vec<SampleHeader>, bool), Error> {
    // Refuse a cut bank before spending time on decoding: every region the
    // table names has to sit inside the sample data.
    for header in headers {
        let start = header.start as usize;
        let end = header.end as usize;
        if start > end || end > smpl.len() {
            return Err(Error::Invalid(format!(
                "sample '{}' spans bytes {start}..{end} but the sample data chunk holds {} bytes; the bank is truncated",
                header.name,
                smpl.len()
            )));
        }
    }
    // Decoding one sample says nothing about the next, and playback needs the
    // whole waveform, so fan the work out and join before the first note. Eight
    // workers is enough for the largest banks and leaves the rest of the
    // process alone. Batches keep only a few hundred decoded samples alive at
    // once, so the peak stays near the final waveform instead of its double.
    let workers = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1)
        .clamp(1, 8);
    let batch = workers * DECODE_BATCH;
    let mut points: Vec<i16> = Vec::new();
    let mut rewritten = Vec::with_capacity(headers.len());
    let mut ogg_samples = 0usize;
    let written = std::cell::Cell::new(0_usize);
    let mut append = |header: &SampleHeader,
                      sample: Result<DecodedSample, SampleDecodeError>|
     -> Result<(), Error> {
        let sample = match sample {
            Ok(sample) => sample,
            Err(SampleDecodeError::Limit) => {
                return Err(match max_bytes {
                    Some(limit) => Error::PreparationLimit { limit },
                    None => Error::Invalid(
                        "unexpected bounded sample refusal during complete decoding".into(),
                    ),
                });
            }
            Err(reason) => {
                return Err(Error::Invalid(format!(
                    "sample '{}': {reason}",
                    header.name
                )));
            }
        };
        if sample.compressed {
            ogg_samples += 1;
            if let Some(warning) = sample.warning {
                warnings.push(warning);
            }
        }
        let first = points.len();
        if let Some(limit) = max_bytes {
            let total = first
                .checked_add(sample.points.len())
                .ok_or(Error::PreparationLimit { limit })?;
            check_sample_bytes(total, size_of::<i16>(), Some(limit))?;
            points
                .try_reserve_exact(sample.points.len())
                .map_err(|error| {
                    Error::Invalid(format!(
                        "cannot allocate bounded SoundFont waveform: {error}"
                    ))
                })?;
        }
        points.extend(sample.points);
        let last = points.len();
        if last > u32::MAX as usize {
            return Err(Error::Invalid(
                "decoded sample data exceeds the format's 32-bit frame offsets".to_string(),
            ));
        }
        // SF3 stores loop points as frame offsets from the sample's start;
        // an SF2 header stores absolute offsets into the whole waveform.
        let loop_start = (first + header.loop_start as usize).min(last);
        let loop_end = (first + header.loop_end as usize).min(last);
        written.set(points.len());
        rewritten.push(SampleHeader {
            start: first as u32,
            end: last as u32,
            loop_start: loop_start as u32,
            loop_end: loop_end as u32,
            ..header.clone()
        });
        Ok(())
    };
    if let Some(limit) = max_bytes {
        for header in headers {
            let remaining = limit / size_of::<i16>() - written.get();
            append(header, decode_sample(smpl, header, Some(remaining)))?;
        }
    } else {
        for batch_headers in headers.chunks(batch) {
            let mut decoded = Vec::with_capacity(batch_headers.len());
            std::thread::scope(|scope| {
                let handles: Vec<_> = batch_headers
                    .chunks(DECODE_BATCH)
                    .map(|samples| scope.spawn(move || decode_samples(smpl, samples)))
                    .collect();
                for handle in handles {
                    decoded.extend(handle.join().expect("sample decoder thread panicked"));
                }
            });
            for (header, sample) in batch_headers.iter().zip(decoded) {
                append(header, sample)?;
            }
        }
    }
    if points.is_empty() {
        return Err(Error::Invalid(
            "compressed sample data decodes to no audio".to_string(),
        ));
    }
    if ogg_samples < headers.len() {
        warnings.push(format!(
            "{} of {} samples are raw PCM inside an Ogg Vorbis bank",
            headers.len() - ogg_samples,
            headers.len()
        ));
    }
    warnings.push(format!(
        "decoded {ogg_samples} Ogg Vorbis samples into {} points",
        points.len()
    ));
    Ok((SampleBank::Points16(points), rewritten, true))
}

/// One sample's decoded points, whether it was compressed, and a defect worth
/// reporting without refusing the bank.
struct DecodedSample {
    points: Vec<i16>,
    compressed: bool,
    warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum SampleDecodeError {
    #[error("sample exceeds its indexed preparation frame limit")]
    Limit,
    #[error("{0}")]
    Invalid(String),
}

/// Decode a slice of the sample table in file order; the caller joins the
/// results back into one waveform.
fn decode_samples(
    smpl: &[u8],
    headers: &[SampleHeader],
) -> Vec<Result<DecodedSample, SampleDecodeError>> {
    headers
        .iter()
        .map(|header| decode_sample(smpl, header, None))
        .collect()
}

fn decode_sample(
    smpl: &[u8],
    header: &SampleHeader,
    frame_limit: Option<usize>,
) -> Result<DecodedSample, SampleDecodeError> {
    let region = &smpl[header.start as usize..header.end as usize];
    if !vorbis::is_ogg(region) {
        if frame_limit.is_some_and(|limit| region.len() / 2 > limit) {
            return Err(SampleDecodeError::Limit);
        }
        let points = region
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return Ok(DecodedSample {
            points,
            compressed: false,
            warning: None,
        });
    }
    // A decoder bug must surface as a refusal that names the sample,
    // not as a panic inside the loader.
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match frame_limit {
        Some(limit) => vorbis::decode_capped(region, limit).map_err(|error| match error {
            vorbis::DecodeError::FrameLimit => SampleDecodeError::Limit,
            vorbis::DecodeError::Invalid(reason) => SampleDecodeError::Invalid(reason),
        }),
        None => vorbis::decode(region).map_err(SampleDecodeError::Invalid),
    }))
    .unwrap_or_else(|_| {
        Err(SampleDecodeError::Invalid(
            "the Vorbis decoder panicked on this stream".into(),
        ))
    })?;
    let (rate, points) = decoded;
    let warning = (rate != header.rate).then(|| {
        format!(
            "sample '{}' streams at {rate} Hz but its header declares {} Hz; playback uses the header",
            header.name, header.rate
        )
    });
    Ok(DecodedSample {
        points,
        compressed: true,
        warning,
    })
}

// ============================================================================
// pdta
// ============================================================================

struct PresetHeader {
    name: String,
    bank: u16,
    program: u16,
    bag: u16,
}

struct InstrumentHeader {
    name: String,
    bag: u16,
    zone_count: usize,
}

struct RawRegion {
    instruments: Vec<InstrumentHeader>,
    sample_headers: Vec<SampleHeader>,
    preset_headers: Vec<PresetHeader>,
    /// First bag of the terminal preset record: the end of the last preset's
    /// zone list. The terminal record is not a preset, so it must not add a
    /// zone.
    preset_bag_end: usize,
    preset_zone_sets: Vec<GenSet>,
    instrument_zone_sets: Vec<GenSet>,
    preset_zone_modulators: Vec<Vec<Modulator>>,
    instrument_zone_modulators: Vec<Vec<Modulator>>,
}

fn parse_pdta(list: &Chunk<'_>, warnings: &mut Vec<String>) -> Result<RawRegion, Error> {
    let mut phdr: Option<&[u8]> = None;
    let mut pbag: Option<&[u8]> = None;
    let mut pgen: Option<&[u8]> = None;
    let mut inst: Option<&[u8]> = None;
    let mut ibag: Option<&[u8]> = None;
    let mut igen: Option<&[u8]> = None;
    let mut shdr: Option<&[u8]> = None;
    let mut pmod: Vec<[u8; MODULATOR_RECORD_SIZE]> = Vec::new();
    let mut imod: Vec<[u8; MODULATOR_RECORD_SIZE]> = Vec::new();
    for chunk in ChunkReader::list(list.data, b"pdta")? {
        let chunk = chunk?;
        match &chunk.id {
            b"phdr" => phdr = Some(chunk.data),
            b"pbag" => pbag = Some(chunk.data),
            b"pgen" => pgen = Some(chunk.data),
            b"pmod" => pmod = decode_modulators(chunk.data),
            b"inst" => inst = Some(chunk.data),
            b"ibag" => ibag = Some(chunk.data),
            b"igen" => igen = Some(chunk.data),
            b"imod" => imod = decode_modulators(chunk.data),
            b"shdr" => shdr = Some(chunk.data),
            _ => {}
        }
    }
    let phdr = phdr.ok_or(Error::Missing("phdr chunk"))?;
    let pbag = pbag.ok_or(Error::Missing("pbag chunk"))?;
    let pgen = pgen.ok_or(Error::Missing("pgen chunk"))?;
    let inst = inst.ok_or(Error::Missing("inst chunk"))?;
    let ibag = ibag.ok_or(Error::Missing("ibag chunk"))?;
    let igen = igen.ok_or(Error::Missing("igen chunk"))?;
    let shdr = shdr.ok_or(Error::Missing("shdr chunk"))?;

    let preset_bags = decode_bags(pbag)?;
    let instrument_bags = decode_bags(ibag)?;
    let preset_gens = decode_generators(pgen);
    let instrument_gens = decode_generators(igen);
    // Bag records index the generator list, so unknown opcodes stay in place.
    let unsupported = preset_gens
        .iter()
        .chain(instrument_gens.iter())
        .filter(|(opcode, _)| Generator::from_opcode(*opcode).is_none())
        .count();
    if unsupported > 0 {
        warnings.push(format!(
            "ignored {unsupported} generator records outside the audibility subset"
        ));
    }

    let (preset_headers, preset_bag_end) = decode_preset_headers(phdr)?;
    let sample_headers = decode_sample_headers(shdr)?;
    let instrument_headers = decode_instrument_headers(inst, instrument_bags.len())?;

    let preset_zone_sets = zone_generator_sets(&preset_bags, &preset_gens);
    let instrument_zone_sets = zone_generator_sets(&instrument_bags, &instrument_gens);
    let mut applied_modulators = 0usize;
    let preset_zone_modulators = zone_modulator_lists(&preset_bags, &pmod, &mut applied_modulators);
    let instrument_zone_modulators =
        zone_modulator_lists(&instrument_bags, &imod, &mut applied_modulators);
    let modulator_records = pmod.len() + imod.len();
    if modulator_records > applied_modulators {
        warnings.push(format!(
            "honoured {applied_modulators} of {modulator_records} modulator records (modelled destinations, sources and transforms only)"
        ));
    }
    Ok(RawRegion {
        instruments: instrument_headers,
        sample_headers,
        preset_headers,
        preset_bag_end,
        preset_zone_sets,
        instrument_zone_sets,
        preset_zone_modulators,
        instrument_zone_modulators,
    })
}

/// One [`Modulator`] list per zone, read from the zone's first record until the
/// terminal record or the next zone's first record. Records outside the modelled
/// subset are dropped and counted.
///
/// The next bag bounds the list because not every bank writes the terminal
/// record the format asks for: a bank that relies on the index alone would
/// otherwise lend the next zone's records to this one.
fn zone_modulator_lists(
    bags: &[Bag],
    records: &[[u8; MODULATOR_RECORD_SIZE]],
    applied: &mut usize,
) -> Vec<Vec<Modulator>> {
    let mut zones = Vec::with_capacity(bags.len());
    for (index, bag) in bags.iter().enumerate() {
        let start = bag.modulator as usize;
        let end = bags
            .get(index + 1)
            .map(|next| next.modulator as usize)
            .unwrap_or(records.len())
            .clamp(start, records.len());
        let mut list = Vec::new();
        for record in records[start..end].iter().take(MAX_ZONE_MODULATORS) {
            match modulation::decode(record) {
                modulation::Decoded::Terminal => break,
                modulation::Decoded::Modulator(modulator) => {
                    *applied += 1;
                    list.push(modulator);
                }
                modulation::Decoded::Unsupported => {}
            }
        }
        zones.push(list);
    }
    zones
}

/// One `pbag`/`ibag` record: the first generator and the first modulator of the
/// zone's lists.
#[derive(Debug, Clone, Copy, Default)]
struct Bag {
    generator: u16,
    modulator: u16,
}

fn decode_bags(payload: &[u8]) -> Result<Vec<Bag>, Error> {
    let count = payload.len() / BAG_SIZE;
    if count == 0 {
        return Err(Error::Invalid("empty bag chunk".to_string()));
    }
    Ok((0..count)
        .map(|index| {
            riff::record(payload, index, BAG_SIZE)
                .map(|record| Bag {
                    generator: riff::u16_at(record, 0),
                    modulator: riff::u16_at(record, 2),
                })
                .unwrap_or_default()
        })
        .collect())
}

/// Raw `pmod`/`imod` records in file order.
fn decode_modulators(payload: &[u8]) -> Vec<[u8; MODULATOR_RECORD_SIZE]> {
    (0..payload.len() / MODULATOR_RECORD_SIZE)
        .filter_map(|index| riff::record(payload, index, MODULATOR_RECORD_SIZE))
        .map(|record| {
            let mut bytes = [0u8; MODULATOR_RECORD_SIZE];
            bytes.copy_from_slice(record);
            bytes
        })
        .collect()
}

/// Raw generator records, keeping their file positions: bag records index them.
fn decode_generators(payload: &[u8]) -> Vec<(u16, i16)> {
    (0..payload.len() / GEN_SIZE)
        .filter_map(|index| riff::record(payload, index, GEN_SIZE))
        .map(|record| (riff::u16_at(record, 0), riff::i16_at(record, 2)))
        .collect()
}

/// Preset headers, plus the first bag of the terminal record so the last preset
/// ends where its zones end instead of swallowing the terminal bag.
fn decode_preset_headers(payload: &[u8]) -> Result<(Vec<PresetHeader>, usize), Error> {
    let count = payload.len() / PHDR_SIZE;
    if count < 2 {
        return Err(Error::Invalid(format!(
            "phdr chunk holds {count} records; a terminal record is required"
        )));
    }
    let headers = (0..count - 1)
        .filter_map(|index| riff::record(payload, index, PHDR_SIZE))
        .map(|record| PresetHeader {
            name: riff::name_at(record, 0),
            program: riff::u16_at(record, 20),
            bank: riff::u16_at(record, 22),
            bag: riff::u16_at(record, 24),
        })
        .collect();
    let bag_end = riff::record(payload, count - 1, PHDR_SIZE)
        .map(|record| usize::from(riff::u16_at(record, 24)))
        .unwrap_or(0);
    Ok((headers, bag_end))
}

fn decode_instrument_headers(
    payload: &[u8],
    bag_count: usize,
) -> Result<Vec<InstrumentHeader>, Error> {
    let count = payload.len() / INST_SIZE;
    if count < 2 {
        return Err(Error::Invalid(format!(
            "inst chunk holds {count} records; a terminal record is required"
        )));
    }
    let mut headers = Vec::with_capacity(count - 1);
    for index in 0..count - 1 {
        let Some(record) = riff::record(payload, index, INST_SIZE) else {
            continue;
        };
        let bag = riff::u16_at(record, 20);
        let next_bag = riff::record(payload, index + 1, INST_SIZE)
            .map(|next| riff::u16_at(next, 20))
            .unwrap_or(bag_count as u16);
        headers.push(InstrumentHeader {
            name: riff::name_at(record, 0),
            bag,
            zone_count: usize::from(next_bag.saturating_sub(bag)),
        });
    }
    Ok(headers)
}

fn decode_sample_headers(payload: &[u8]) -> Result<Vec<SampleHeader>, Error> {
    let count = payload.len() / SHDR_SIZE;
    if count < 2 {
        return Err(Error::Invalid(format!(
            "shdr chunk holds {count} records; a terminal record is required"
        )));
    }
    let headers = (0..count - 1)
        .filter_map(|index| riff::record(payload, index, SHDR_SIZE))
        .map(|record| SampleHeader {
            name: riff::name_at(record, 0),
            start: riff::u32_at(record, 20),
            end: riff::u32_at(record, 24),
            loop_start: riff::u32_at(record, 28),
            loop_end: riff::u32_at(record, 32),
            rate: riff::u32_at(record, 36),
            root_key: record.get(40).copied().unwrap_or(60),
            correction: record.get(41).copied().unwrap_or(0) as i8,
            sample_type: riff::u16_at(record, 44),
        })
        .collect();
    Ok(headers)
}

/// Turn bag records plus generator records into one [`GenSet`] per zone.
fn zone_generator_sets(bags: &[Bag], generators: &[(u16, i16)]) -> Vec<GenSet> {
    let mut zones = Vec::with_capacity(bags.len());
    for index in 0..bags.len() {
        let start = bags[index].generator as usize;
        let end = bags
            .get(index + 1)
            .map(|next| next.generator as usize)
            .unwrap_or(generators.len());
        let mut set = GenSet::default();
        for &(opcode, amount) in generators
            .get(start..end.min(generators.len()))
            .unwrap_or_default()
        {
            if let Some(generator) = Generator::from_opcode(opcode) {
                set.insert(generator, amount);
            }
        }
        zones.push(set);
    }
    zones
}

/// Split a zone list into its optional global zone and the remaining zones.
fn split_global(zones: &[GenSet], identity: Generator) -> (Option<GenSet>, &[GenSet]) {
    match zones.split_first() {
        // The global zone is the one that does not name the next layer.
        Some((first, rest)) if first.is_absent(identity) && !rest.is_empty() => {
            (Some(*first), rest)
        }
        _ => (None, zones),
    }
}

// ============================================================================
// Zone resolution
// ============================================================================

fn resolve_presets(
    region: &RawRegion,
    sample_bank: &SampleBank,
    warnings: &mut Vec<String>,
) -> Vec<Preset> {
    let frames = sample_bank.frames() as i64;
    let mut presets = Vec::with_capacity(region.preset_headers.len());
    for (index, header) in region.preset_headers.iter().enumerate() {
        let bag_start = header.bag as usize;
        let bag_end = region
            .preset_headers
            .get(index + 1)
            .map(|next| next.bag as usize)
            .unwrap_or(region.preset_bag_end);
        let zone_sets = region
            .preset_zone_sets
            .get(bag_start..bag_end)
            .unwrap_or_default();
        let (preset_global, preset_zones) = split_global(zone_sets, Generator::Instrument);
        let preset_modulators = region
            .preset_zone_modulators
            .get(bag_start..bag_end)
            .unwrap_or_default();
        // A global zone, when present, is the first entry of both lists.
        let preset_offset = zone_sets.len().saturating_sub(preset_zones.len());
        let mut zones = Vec::new();
        for (local_index, preset_local) in preset_zones.iter().enumerate() {
            let preset_modulator = preset_modulators
                .get(preset_offset + local_index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut preset_set = preset_global.unwrap_or_default();
            preset_set.overlay(preset_local);
            let Some(instrument_index) = preset_set.get(Generator::Instrument) else {
                warnings.push(format!(
                    "preset '{}' has a zone without an instrument",
                    header.name
                ));
                continue;
            };
            let instrument_index = instrument_index.max(0) as usize;
            let Some(instrument) = region.instruments.get(instrument_index) else {
                warnings.push(format!(
                    "preset '{}' names instrument {instrument_index}, which the bank does not declare",
                    header.name
                ));
                continue;
            };
            let inst_bag_start = instrument.bag as usize;
            let inst_bag_end = inst_bag_start + instrument.zone_count;
            let inst_sets = region
                .instrument_zone_sets
                .get(inst_bag_start..inst_bag_end)
                .unwrap_or_default();
            let (inst_global, inst_zones) = split_global(inst_sets, Generator::SampleId);
            let inst_modulators = region
                .instrument_zone_modulators
                .get(inst_bag_start..inst_bag_end)
                .unwrap_or_default();
            let inst_offset = inst_sets.len().saturating_sub(inst_zones.len());
            for (local_index, inst_local) in inst_zones.iter().enumerate() {
                let inst_modulator = inst_modulators
                    .get(inst_offset + local_index)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut merged = inst_global.unwrap_or_default();
                merged.overlay(inst_local);
                match resolve_zone(
                    &merged,
                    &preset_set,
                    inst_modulator,
                    preset_modulator,
                    region,
                    frames,
                ) {
                    Ok(zone) => zones.push(zone),
                    Err(reason) => warnings.push(format!(
                        "preset '{}' instrument '{}': {reason}",
                        header.name, instrument.name
                    )),
                }
            }
        }
        presets.push(Preset {
            name: header.name.clone(),
            bank: header.bank,
            program: header.program,
            zones,
        });
    }
    presets
}

fn resolve_zone(
    instrument: &GenSet,
    preset: &GenSet,
    instrument_modulators: &[Modulator],
    preset_modulators: &[Modulator],
    region: &RawRegion,
    frames: i64,
) -> Result<Zone, String> {
    let sample_index = instrument
        .get(Generator::SampleId)
        .ok_or("zone does not name a sample")?;
    let sample_index = sample_index.max(0) as usize;
    let header = region
        .sample_headers
        .get(sample_index)
        .ok_or_else(|| format!("sample {sample_index} is not declared"))?;

    let (inst_key_low, inst_key_high) = instrument.key_range(Generator::KeyRange);
    let (preset_key_low, preset_key_high) = preset.key_range(Generator::KeyRange);
    let key_low = inst_key_low.max(preset_key_low);
    let key_high = inst_key_high.min(preset_key_high);
    let (inst_vel_low, inst_vel_high) = instrument.key_range(Generator::VelRange);
    let (preset_vel_low, preset_vel_high) = preset.key_range(Generator::VelRange);
    let vel_low = inst_vel_low.max(preset_vel_low);
    let vel_high = inst_vel_high.min(preset_vel_high);
    if key_low > key_high || vel_low > vel_high {
        return Err("empty key or velocity range".to_string());
    }

    // Preset-level numeric generators are offsets from the instrument's.
    let mut merged = *instrument;
    merged.add_offsets(preset);
    // `keynum` and `velocity` are instrument-level generators: they replace the
    // key and velocity after range selection, and a preset zone does not add
    // to them.
    let keynum = fixed_note_value(instrument, Generator::Keynum);
    let velocity = fixed_note_value(instrument, Generator::Velocity);

    let coarse = i64::from(merged.value(Generator::StartAddrsCoarseOffset)) * 32_768;
    let end_coarse = i64::from(merged.value(Generator::EndAddrsCoarseOffset)) * 32_768;
    let loop_start_coarse = i64::from(merged.value(Generator::StartLoopAddrsCoarseOffset)) * 32_768;
    let loop_end_coarse = i64::from(merged.value(Generator::EndLoopAddrsCoarseOffset)) * 32_768;

    let start =
        i64::from(header.start) + coarse + i64::from(merged.value(Generator::StartAddrsOffset));
    let end =
        i64::from(header.end) + end_coarse + i64::from(merged.value(Generator::EndAddrsOffset));
    let loop_start = i64::from(header.loop_start)
        + loop_start_coarse
        + i64::from(merged.value(Generator::StartLoopAddrsOffset));
    let loop_end = i64::from(header.loop_end)
        + loop_end_coarse
        + i64::from(merged.value(Generator::EndLoopAddrsOffset));

    if frames <= 0 {
        return Err(format!("sample '{}' has no audio", header.name));
    }
    let start = start.clamp(0, frames - 1);
    let end = end.clamp(start + 1, frames);
    let loop_start = loop_start.clamp(start, end);
    let loop_end = loop_end.clamp(loop_start, end);
    let (loop_start, loop_end) = if loop_end > loop_start {
        (loop_start, loop_end)
    } else {
        (0, 0)
    };

    let root_key = merged
        .get(Generator::OverridingRootKey)
        .filter(|value| (0..=127).contains(value))
        .map(|value| value as u8)
        .unwrap_or(header.root_key)
        .min(127);

    let sustain_cb = merged.value(Generator::SustainVolEnv).clamp(0, 1440) as f32;
    let envelope = EnvelopeShape {
        delay: timecents_to_seconds(merged.value(Generator::DelayVolEnv)),
        attack: timecents_to_seconds(merged.value(Generator::AttackVolEnv)),
        hold: timecents_to_seconds(merged.value(Generator::HoldVolEnv)),
        decay: timecents_to_seconds(merged.value(Generator::DecayVolEnv)),
        sustain_cb,
        release: timecents_to_seconds(merged.value(Generator::ReleaseVolEnv)),
        hold_key_scale: f32::from(merged.value(Generator::KeynumToVolEnvHold)),
        decay_key_scale: f32::from(merged.value(Generator::KeynumToVolEnvDecay)),
    };

    let mod_env = EnvelopeShape {
        delay: timecents_to_seconds(merged.value(Generator::DelayModEnv)),
        attack: timecents_to_seconds(merged.value(Generator::AttackModEnv)),
        hold: timecents_to_seconds(merged.value(Generator::HoldModEnv)),
        decay: timecents_to_seconds(merged.value(Generator::DecayModEnv)),
        sustain_cb: merged.value(Generator::SustainModEnv).clamp(0, 1440) as f32,
        release: timecents_to_seconds(merged.value(Generator::ReleaseModEnv)),
        hold_key_scale: f32::from(merged.value(Generator::KeynumToModEnvHold)),
        decay_key_scale: f32::from(merged.value(Generator::KeynumToModEnvDecay)),
    };

    let sample_mode = SampleMode::from_generator(merged.value(Generator::SampleModes));
    Ok(Zone {
        key_low,
        key_high,
        vel_low,
        vel_high,
        sample: sample_index,
        start: start as u64,
        end: end as u64,
        loop_start: loop_start as u64,
        loop_end: loop_end as u64,
        root_key,
        coarse_tune: merged.value(Generator::CoarseTune),
        fine_tune: merged.value(Generator::FineTune),
        scale_tuning: merged.value(Generator::ScaleTuning),
        keynum,
        velocity,
        attenuation_cb: merged.value(Generator::InitialAttenuation).clamp(0, 1440),
        pan: (f32::from(merged.value(Generator::Pan)) / 500.0).clamp(-1.0, 1.0),
        sample_mode,
        envelope,
        mod_env,
        filter_cutoff_cents: merged.value(Generator::InitialFilterFc).clamp(1500, 13_500),
        filter_q_cb: merged.value(Generator::InitialFilterQ).max(0),
        exclusive_class: merged.value(Generator::ExclusiveClass).max(0),
        vib_lfo_to_pitch: f32::from(merged.value(Generator::VibLfoToPitch)),
        vibrato_hz: lfo_frequency(merged.value(Generator::FreqVibLfo)),
        vibrato_delay: timecents_to_seconds(merged.value(Generator::DelayVibLfo)),
        mod_lfo_to_pitch: f32::from(merged.value(Generator::ModLfoToPitch)),
        mod_lfo_hz: lfo_frequency(merged.value(Generator::FreqModLfo)),
        mod_lfo_delay: timecents_to_seconds(merged.value(Generator::DelayModLfo)),
        mod_lfo_to_filter_fc: f32::from(merged.value(Generator::ModLfoToFilterFc)),
        mod_lfo_to_volume: f32::from(merged.value(Generator::ModLfoToVolume)),
        mod_env_to_pitch: f32::from(merged.value(Generator::ModEnvToPitch)),
        mod_env_to_filter_fc: f32::from(merged.value(Generator::ModEnvToFilterFc)),
        chorus_send: merged.value(Generator::ChorusSend).clamp(0, 1000),
        reverb_send: merged.value(Generator::ReverbSend).clamp(0, 1000),
        modulators: instrument_modulators
            .iter()
            .chain(preset_modulators)
            .copied()
            .collect(),
    })
}

/// A fixed key/velocity generator's value, or `None` when the zone leaves it
/// at the format's -1 sentinel or names an unreachable number.
fn fixed_note_value(set: &GenSet, generator: Generator) -> Option<u8> {
    set.get(generator)
        .filter(|value| (0..=127).contains(value))
        .map(|value| value as u8)
}

/// LFO frequency from absolute cents; the origin is 8.176 Hz.
fn lfo_frequency(cents: i16) -> f32 {
    8.176 * 2f32.powf(f32::from(cents) / 1200.0)
}

/// Timecents to seconds; the `-32768` sentinel and the specification's
/// `-12000..8000` clamp both apply.
fn timecents_to_seconds(timecents: i16) -> f64 {
    if timecents == i16::MIN {
        return 0.0;
    }
    let clamped = f64::from(timecents.clamp(-12_000, 8_000));
    2f64.powf(clamped / 1200.0)
}

struct RawBank<'a> {
    name: Option<String>,
    version: (u16, u16),
    region: RawRegion,
    sdta: Chunk<'a>,
    warnings: Vec<String>,
}

impl<'a> RawBank<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut info: Option<Chunk<'_>> = None;
        let mut sdta: Option<Chunk<'_>> = None;
        let mut pdta: Option<Chunk<'_>> = None;
        for chunk in ChunkReader::open(bytes)? {
            let chunk = chunk?;
            if &chunk.id != b"LIST" {
                continue;
            }
            match chunk.data.get(..4).unwrap_or_default() {
                b"INFO" => info = Some(chunk),
                b"sdta" => sdta = Some(chunk),
                b"pdta" => pdta = Some(chunk),
                _ => {}
            }
        }
        let info = info.ok_or(Error::Missing("INFO list"))?;
        let sdta = sdta.ok_or(Error::Missing("sdta list"))?;
        let pdta = pdta.ok_or(Error::Missing("pdta list"))?;

        let mut warnings = Vec::new();
        let (name, version) = parse_info(&info, &mut warnings)?;
        let region = parse_pdta(&pdta, &mut warnings)?;
        Ok(Self {
            name,
            version,
            region,
            sdta,
            warnings,
        })
    }

    fn finish(
        mut self,
        sample_bank: SampleBank,
        sample_headers: Vec<SampleHeader>,
        compressed: bool,
    ) -> Result<SoundFont, Error> {
        self.region.sample_headers = sample_headers;
        let presets = resolve_presets(&self.region, &sample_bank, &mut self.warnings);

        if presets.is_empty() {
            return Err(Error::Invalid(
                "bank declares no playable preset zone".to_string(),
            ));
        }
        let instruments = self
            .region
            .instruments
            .iter()
            .map(|instrument| Instrument {
                name: instrument.name.clone(),
                zone_count: instrument.zone_count,
            })
            .collect();
        Ok(SoundFont {
            name: self.name,
            version: self.version,
            compressed,
            sample_bank,
            sample_headers: self.region.sample_headers,
            presets,
            instruments,
            warnings: self.warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared cache is process-wide, so the cache tests take turns.
    static CACHE_TESTS: Mutex<()> = Mutex::new(());

    /// A directory removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "oc-font-cache-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the temporary bank directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The checked-in bank, copied to a path a test may rewrite.
    fn fixture_bytes() -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2");
        std::fs::read(&path).expect("read the bank fixture")
    }

    fn write_fixture(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("write the bank fixture");
    }

    /// Set the first `shdr` record's sample rate: a 20-byte name, four offsets,
    /// then the rate. A bank rewritten this way parses into different content.
    fn set_sample_rate(bytes: &mut [u8], rate: u32) {
        let chunk = bytes
            .windows(4)
            .position(|window| window == b"shdr")
            .expect("the fixture has a shdr chunk");
        let record = chunk + 8;
        bytes[record + 36..record + 40].copy_from_slice(&rate.to_le_bytes());
    }

    pub(super) fn take_cache_for_test() -> std::sync::MutexGuard<'static, ()> {
        CACHE_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn a_bank_replaced_at_the_same_path_is_read_again() {
        let _guard = take_cache_for_test();
        let dir = TempDir::new("replace");
        let path = dir.0.join("bank.sf2");
        write_fixture(&path, &fixture_bytes());
        let first = SoundFont::load_shared(&path).expect("the first bank parses");
        assert_eq!(first.sample_headers[0].rate, 44_100);

        // An overwrite or a reinstall keeps the path and changes the file: the
        // replacement is longer and declares another rate.
        let mut replacement = fixture_bytes();
        set_sample_rate(&mut replacement, 22_050);
        replacement.extend_from_slice(b"pad!");
        write_fixture(&path, &replacement);
        let second = SoundFont::load_shared(&path).expect("the replacement parses");
        assert_eq!(second.sample_headers[0].rate, 22_050);
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn invalidating_drops_the_parse_of_an_unchanged_file() {
        let _guard = take_cache_for_test();
        let dir = TempDir::new("invalidate");
        let path = dir.0.join("bank.sf2");
        write_fixture(&path, &fixture_bytes());
        let first = SoundFont::load_shared(&path).expect("the bank parses");
        assert!(Arc::ptr_eq(
            &first,
            &SoundFont::load_shared(&path).expect("the second load hits the cache")
        ));

        SoundFont::invalidate_shared(&path);
        let second = SoundFont::load_shared(&path).expect("the bank parses again");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.sample_headers[0].rate, 44_100);
    }

    #[test]
    fn the_cache_evicts_the_least_recently_used_bank() {
        let _guard = take_cache_for_test();
        let dir = TempDir::new("bounded");
        let bytes = fixture_bytes();
        let paths: Vec<PathBuf> = (0..=SHARED_BANKS_RESIDENT)
            .map(|index| {
                let path = dir.0.join(format!("bank-{index}.sf2"));
                write_fixture(&path, &bytes);
                path
            })
            .collect();
        let loaded: Vec<Arc<SoundFont>> = paths[..SHARED_BANKS_RESIDENT]
            .iter()
            .map(|path| SoundFont::load_shared(path).expect("the bank parses"))
            .collect();
        // Reloading the first bank makes it the most recently used, so the
        // second bank is the one the next distinct load evicts.
        assert!(Arc::ptr_eq(
            &loaded[0],
            &SoundFont::load_shared(&paths[0]).expect("the first bank is cached")
        ));

        SoundFont::load_shared(&paths[SHARED_BANKS_RESIDENT]).expect("the extra bank parses");
        let evicted = SoundFont::load_shared(&paths[1]).expect("the evicted bank parses again");
        assert!(!Arc::ptr_eq(&loaded[1], &evicted));
        assert!(Arc::ptr_eq(
            &loaded[0],
            &SoundFont::load_shared(&paths[0]).expect("the recent bank is cached")
        ));
    }

    #[test]
    fn timecents_follow_the_specification() {
        assert_eq!(timecents_to_seconds(i16::MIN), 0.0);
        assert!((timecents_to_seconds(0) - 1.0).abs() < 1e-9);
        assert!((timecents_to_seconds(1200) - 2.0).abs() < 1e-9);
        assert!((timecents_to_seconds(-1200) - 0.5).abs() < 1e-9);
        assert!((timecents_to_seconds(-12_000) - 2f64.powi(-10)).abs() < 1e-9);
    }

    #[test]
    fn keynum_scaling_shortens_high_keys() {
        let envelope = EnvelopeShape {
            delay: 0.0,
            attack: 0.0,
            hold: 0.5,
            decay: 0.5,
            sustain_cb: 0.0,
            release: 0.0,
            hold_key_scale: 100.0,
            decay_key_scale: 100.0,
        };
        // A positive decay scale shortens keys above 60 and lengthens below.
        assert!((envelope.decay_for(72) - 0.25).abs() < 1e-9);
        assert!((envelope.decay_for(48) - 1.0).abs() < 1e-9);
        assert!((envelope.hold_for(60) - 0.5).abs() < 1e-9);
        assert!((envelope.hold_for(48) - 0.5 * 2f64.powf(12.0 / 12.0)).abs() < 1e-9);
    }

    #[test]
    fn sample_modes_only_loop_when_asked() {
        assert_eq!(SampleMode::from_generator(0), SampleMode::NoLoop);
        assert_eq!(SampleMode::from_generator(1), SampleMode::Continuous);
        assert_eq!(SampleMode::from_generator(3), SampleMode::UntilRelease);
        assert_eq!(SampleMode::from_generator(2), SampleMode::NoLoop);
        assert!(!SampleMode::NoLoop.loops_now(true));
        assert!(SampleMode::Continuous.loops_now(false));
        assert!(SampleMode::UntilRelease.loops_now(true));
        assert!(!SampleMode::UntilRelease.loops_now(false));
    }

    #[test]
    fn a_zone_modulator_list_stops_at_the_next_zone() {
        // A bank that writes no terminal record: each zone owns the records
        // between its own index and the next zone's. Without the bound, the
        // first zone would read the second zone's attenuation record.
        let mut record = [0u8; MODULATOR_RECORD_SIZE];
        record[..2].copy_from_slice(&(0x80u16 | 2).to_le_bytes());
        record[2..4].copy_from_slice(&48u16.to_le_bytes());
        record[4..6].copy_from_slice(&800i16.to_le_bytes());
        let bags = [
            Bag {
                generator: 0,
                modulator: 0,
            },
            Bag {
                generator: 0,
                modulator: 0,
            },
            Bag {
                generator: 0,
                modulator: 1,
            },
            Bag {
                generator: 0,
                modulator: 2,
            },
        ];
        let records = [record, record];
        let mut applied = 0;
        let zones = zone_modulator_lists(&bags, &records, &mut applied);
        assert_eq!(zones.len(), 4);
        assert!(zones[0].is_empty(), "a zero-length list stays empty");
        assert_eq!(zones[1].len(), 1);
        assert_eq!(
            zones[1][0].destination,
            modulation::Destination::InitialAttenuation
        );
        assert_eq!(zones[2].len(), 1);
        assert!(zones[3].is_empty());
        assert_eq!(applied, 2);
    }

    #[test]
    fn global_zone_is_the_first_zone_without_identity() {
        let mut global = GenSet::default();
        global.insert(Generator::AttackVolEnv, -1200);
        let mut local = GenSet::default();
        local.insert(Generator::SampleId, 3);
        let zones = [global, local];
        let (found, locals) = split_global(&zones, Generator::SampleId);
        assert!(found.is_some());
        assert_eq!(locals.len(), 1);
        // Without a global zone nothing is dropped.
        let (found, locals) = split_global(&zones[1..], Generator::SampleId);
        assert!(found.is_none());
        assert_eq!(locals.len(), 1);
    }

    #[test]
    fn generator_sets_slice_by_bag_index() {
        let generators = [
            (Generator::Instrument.opcode(), 2),
            (
                Generator::KeyRange.opcode(),
                super::super::generator::pack_range(0, 64),
            ),
            (Generator::SampleId.opcode(), 7),
        ];
        let bags = [
            Bag {
                generator: 0,
                modulator: 0,
            },
            Bag {
                generator: 2,
                modulator: 0,
            },
            Bag {
                generator: 3,
                modulator: 0,
            },
        ];
        let zones = zone_generator_sets(&bags, &generators);
        assert_eq!(zones.len(), 3);
        assert_eq!(zones[0].value(Generator::Instrument), 2);
        assert_eq!(zones[0].key_range(Generator::KeyRange), (0, 64));
        assert_eq!(zones[1].value(Generator::SampleId), 7);
        assert!(zones[2].is_absent(Generator::SampleId));
    }

    #[test]
    fn a_time_offset_scales_a_stage_and_clamps_at_the_converter_limits() {
        let shape = EnvelopeShape {
            delay: 0.0,
            attack: 1.0,
            hold: 0.0,
            decay: 2.0,
            sustain_cb: 100.0,
            release: 4.0,
            hold_key_scale: 0.0,
            decay_key_scale: 0.0,
        };
        let shifted = shape.with_time_offsets(1200.0, -1200.0, 0.0);
        assert!((shifted.attack - 2.0).abs() < 1e-12);
        assert!((shifted.decay - 1.0).abs() < 1e-12);
        assert!((shifted.release - 4.0).abs() < 1e-12);
        // A stage already at the converter's ceiling stays there.
        let top = EnvelopeShape {
            attack: timecents_to_seconds(8000),
            ..shape
        };
        assert!(
            (top.with_time_offsets(1200.0, 0.0, 0.0).attack - MAX_ENVELOPE_SECONDS).abs() < 1e-9
        );
        // A zero-length stage stays instant.
        let instant = EnvelopeShape {
            attack: 0.0,
            ..shape
        };
        assert_eq!(instant.with_time_offsets(1200.0, 0.0, 0.0).attack, 0.0);
        // The sustain offset uses the generator's range.
        assert_eq!(shape.with_sustain_offset(-1000.0).sustain_cb, 0.0);
        assert_eq!(shape.with_sustain_offset(1200.0).sustain_cb, 1300.0);
        assert_eq!(shape.with_sustain_offset(2000.0).sustain_cb, 1440.0);
    }

    #[test]
    fn filter_q_maps_centibels_and_clamps() {
        assert!((filter_q_from_centibels(0.0) - 1.0).abs() < 1e-12);
        assert!((filter_q_from_centibels(300.0) - 10f64.powf(1.5)).abs() < 1e-12);
        assert!((filter_q_from_centibels(-100.0) - 1.0).abs() < 1e-12);
        assert!((filter_q_from_centibels(2000.0) - 10f64.powf(4.8)).abs() < 1e-9);
    }
}
