//! Drum-kit instrument: a pad grid over decoded samples.
//!
//! A kit is a preset with `pad.*` / `layer.*` params (see `@kit` in the DSL).
//! Every pad owns velocity layers; overlapping ranges act as round-robin
//! alternatives selected by a per-pad counter. Chokes and stolen voices move to
//! preallocated fade slots so the audio callback never cuts a waveform.
//!
//! Threading: [`DrumKit::from_params`] decodes every sample once. Note-on,
//! note-off, and [`DrumKit::next_stereo`] never allocate, lock, or spawn.

use crate::sampler::{DecodedSample, decode_sample, read_stereo};
use oc_dsl::ast::ParamValue;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Built-in plugin id that hosts a drum kit.
pub const PLUGIN_ID: &str = oc_dsl::DRUMKIT_PLUGIN_ID;

/// Hard polyphony cap. Slots are preallocated at construction.
pub const MAX_VOICES: usize = 64;
/// Default polyphony when a kit does not declare `max_voices`.
pub const DEFAULT_MAX_VOICES: usize = 32;
/// Extra slots that hold voices fading after a choke or a steal.
pub const MAX_FADE_TAILS: usize = 32;
/// Choke and steal fades. Both are linear amplitude ramps to silence.
pub const CHOKE_FADE_SECONDS: f64 = 0.004;
/// Linear fade applied to a voice stolen for a new hit.
pub const STEAL_FADE_SECONDS: f64 = 0.008;
/// Onset ramp; a hard start on a non-zero sample would click.
pub const ATTACK_SECONDS: f64 = 0.0005;
/// Voices below this VCA level are considered finished.
const SILENCE_FLOOR: f64 = 1.0e-5;
/// A synthesised voice ends once its closed-form envelope is this quiet (-80 dB).
const SYNTH_ENV_FLOOR: f64 = 1.0e-4;

// ============================================================================
// Synthesised voices
// ============================================================================

/// Fallback voice for a pad that has no playable sample layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceKind {
    Kick,
    Snare,
    HatClosed,
    HatOpen,
    Tom,
    Clap,
    Rim,
}

impl VoiceKind {
    /// Canonical `voice:` names, in the order pads try them.
    pub const NAMES: [&'static str; 7] = [
        "kick",
        "snare",
        "hat_closed",
        "hat_open",
        "tom",
        "clap",
        "rim",
    ];

    /// Canonical `voice:` names, matched case-insensitively.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.eq_ignore_ascii_case("kick") {
            Some(Self::Kick)
        } else if name.eq_ignore_ascii_case("snare") {
            Some(Self::Snare)
        } else if name.eq_ignore_ascii_case("hat_closed") {
            Some(Self::HatClosed)
        } else if name.eq_ignore_ascii_case("hat_open") {
            Some(Self::HatOpen)
        } else if name.eq_ignore_ascii_case("tom") {
            Some(Self::Tom)
        } else if name.eq_ignore_ascii_case("clap") {
            Some(Self::Clap)
        } else if name.eq_ignore_ascii_case("rim") {
            Some(Self::Rim)
        } else {
            None
        }
    }

    /// The canonical `voice:` name for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Snare => "snare",
            Self::HatClosed => "hat_closed",
            Self::HatOpen => "hat_open",
            Self::Tom => "tom",
            Self::Clap => "clap",
            Self::Rim => "rim",
        }
    }

    /// General-MIDI percussion notes for the seven voices this kit synthesises.
    pub fn from_note(note: u8) -> Option<Self> {
        match note {
            35 | 36 => Some(Self::Kick),
            37 => Some(Self::Rim),
            38 | 40 => Some(Self::Snare),
            39 => Some(Self::Clap),
            41 | 43 | 45 | 47 | 48 | 50 => Some(Self::Tom),
            42 | 44 => Some(Self::HatClosed),
            46 => Some(Self::HatOpen),
            _ => None,
        }
    }

    /// Peak target at velocity 127, before the pad gain.
    fn peak(self) -> f64 {
        let db = match self {
            Self::Kick => -6.0,
            Self::Snare => -10.0,
            Self::HatClosed | Self::HatOpen => -16.0,
            Self::Tom => -12.0,
            Self::Clap => -14.0,
            Self::Rim => -18.0,
        };
        10f64.powf(db / 20.0)
    }

    /// Exponential decay times for the body and the noise component, in seconds.
    fn envelope_seconds(self) -> (f64, f64) {
        match self {
            Self::Kick => (0.250, 0.002),
            Self::Snare => (0.120, 0.180),
            Self::HatClosed => (0.040, 0.0),
            Self::HatOpen => (0.320, 0.0),
            Self::Tom => (0.300, 0.0),
            Self::Clap => (0.120, 0.0),
            Self::Rim => (0.040, 0.0),
        }
    }

    /// Measured gain that brings the raw waveform to the target peak.
    fn calibration(self) -> f64 {
        match self {
            Self::Kick => 1.0,
            Self::Snare => 0.61,
            Self::HatClosed => 6.2,
            Self::HatOpen => 3.6,
            Self::Tom => 1.0,
            Self::Clap => 1.28,
            Self::Rim => 1.25,
        }
    }

    /// Time until both envelopes fall below `SYNTH_ENV_FLOOR`.
    fn tail_seconds(self) -> f64 {
        let (body, noise) = self.envelope_seconds();
        body.max(noise) * 4.0 / 3.0 + ATTACK_SECONDS
    }

    fn synthesis(self, sample_rate: f64) -> SynthParams {
        let rate = sample_rate.max(1.0);
        let decay = |seconds: f64| {
            if seconds > 0.0 {
                0.001f64.powf(1.0 / (seconds * rate))
            } else {
                1.0
            }
        };
        let (body, noise) = self.envelope_seconds();
        let glide_seconds = match self {
            Self::Kick => 0.045,
            Self::Tom => 0.060,
            _ => 0.0,
        };
        SynthParams {
            body_coeff: decay(body),
            noise_coeff: decay(noise),
            glide_coeff: if glide_seconds > 0.0 {
                (-1.0 / (glide_seconds * rate)).exp()
            } else {
                1.0
            },
            // One-pole shaping for the kick click and the hats / snare noise.
            filter_coeff: match self {
                Self::Kick => 1.0 - (-std::f64::consts::TAU * 10_000.0 / rate).exp(),
                Self::Snare => 1.0 / (1.0 + std::f64::consts::TAU * 2_000.0 / rate),
                Self::HatClosed | Self::HatOpen => {
                    1.0 / (1.0 + std::f64::consts::TAU * 7_000.0 / rate)
                }
                _ => 0.0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SynthParams {
    body_coeff: f64,
    noise_coeff: f64,
    glide_coeff: f64,
    filter_coeff: f64,
}

/// One deterministic xorshift stream per pad; the seed comes from the pad index.
fn pad_noise_seed(index: usize) -> u64 {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (index as u64).wrapping_add(1);
    state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state ^= state >> 27;
    state | 1
}

fn next_noise(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0) * 2.0 - 1.0
}

fn pad_voice(pad: &Pad) -> Option<VoiceKind> {
    pad.voice
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .and_then(VoiceKind::from_name)
        .or_else(|| VoiceKind::from_name(&pad.name))
        .or_else(|| VoiceKind::from_note(pad.note))
}

// ============================================================================
// Document model
// ============================================================================

/// A kit document: its polyphony setting and pads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kit {
    pub max_voices: usize,
    pub pads: Vec<Pad>,
}

impl Default for Kit {
    fn default() -> Self {
        Self {
            max_voices: DEFAULT_MAX_VOICES,
            pads: Vec::new(),
        }
    }
}

/// One playable pad with its trigger note, mix controls and sample layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pad {
    pub name: String,
    pub note: u8,
    /// Linear gain derived from the pad's dB value.
    pub gain: f64,
    pub pan: f64,
    pub tune_semitones: f64,
    pub tune_cents: f64,
    /// VCA decay to -60 dB in seconds; `0` keeps the sample's own envelope.
    pub decay: f64,
    pub release: f64,
    pub reverse: bool,
    pub one_shot: bool,
    /// `0` disables chokes; pads sharing a non-zero group cut each other.
    pub choke: u8,
    /// Explicit fallback voice name; overrides the pad name and the note map.
    #[serde(default)]
    pub voice: Option<String>,
    pub layers: Vec<Layer>,
}

/// One velocity-range sample layer of a pad.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub low: u8,
    pub high: u8,
    pub sample: PathBuf,
    pub gain: f64,
}

/// Build a kit from a preset document. Missing samples are collected as
/// warnings later; this only rejects malformed numbers.
pub fn kit_from_params(
    params: &[(String, ParamValue)],
    root: Option<&Path>,
) -> Result<Kit, String> {
    let mut kit = Kit::default();
    let mut order: Vec<String> = Vec::new();
    let mut pads: HashMap<String, Pad> = HashMap::new();
    let mut layers: HashMap<String, Vec<(u32, Layer)>> = HashMap::new();

    for (key, value) in params {
        if key == "kit.max_voices" {
            if let Some(voices) = value.as_f64() {
                kit.max_voices = (voices.round().max(1.0) as usize).min(MAX_VOICES);
            }
            continue;
        }
        if let Some(rest) = key.strip_prefix("pad.")
            && let Some((name, field)) = rest.split_once('.')
        {
            if !pads.contains_key(name) {
                order.push(name.to_string());
                pads.insert(name.to_string(), default_pad(name));
            }
            let pad = pads.get_mut(name).expect("pad was just inserted");
            apply_pad_field(pad, field, value)?;
            continue;
        }
        if let Some(rest) = key.strip_prefix("layer.")
            && let Some((name, tail)) = rest.split_once('.')
            && let Some((index, field)) = tail.split_once('.')
            && let Ok(index) = index.parse::<u32>()
        {
            let entry = layers.entry(name.to_string()).or_default();
            let position = match entry.iter().position(|(slot, _)| *slot == index) {
                Some(position) => position,
                None => {
                    entry.push((index, default_layer()));
                    entry.len() - 1
                }
            };
            let layer = &mut entry[position].1;
            match field {
                "low" => layer.low = value.as_f64().unwrap_or(1.0).round().clamp(0.0, 127.0) as u8,
                "high" => {
                    layer.high = value.as_f64().unwrap_or(127.0).round().clamp(0.0, 127.0) as u8
                }
                "sample" => {
                    layer.sample = match value {
                        ParamValue::String(text) => resolve_sample(text, root),
                        _ => PathBuf::new(),
                    }
                }
                "gain" => layer.gain = db_to_linear(value.as_f64().unwrap_or(0.0)),
                _ => {}
            }
        }
    }

    kit.pads = order
        .into_iter()
        .map(|name| {
            let mut pad = pads.remove(&name).unwrap_or_else(|| default_pad(&name));
            if let Some(mut declared) = layers.remove(&name) {
                declared.sort_by_key(|(index, _)| *index);
                pad.layers = declared.into_iter().map(|(_, layer)| layer).collect();
            }
            pad
        })
        .collect();
    Ok(kit)
}

fn default_pad(name: &str) -> Pad {
    Pad {
        name: name.to_string(),
        note: 36,
        gain: 1.0,
        pan: 0.0,
        tune_semitones: 0.0,
        tune_cents: 0.0,
        decay: 0.0,
        release: 0.005,
        reverse: false,
        one_shot: true,
        choke: 0,
        voice: None,
        layers: Vec::new(),
    }
}

fn default_layer() -> Layer {
    Layer {
        low: 1,
        high: 127,
        sample: PathBuf::new(),
        gain: 1.0,
    }
}

fn apply_pad_field(pad: &mut Pad, field: &str, value: &ParamValue) -> Result<(), String> {
    let number = || value.as_f64();
    match field {
        "note" => {
            let note = number().ok_or_else(|| "pad note must be a MIDI number".to_string())?;
            pad.note = note.round().clamp(0.0, 127.0) as u8;
        }
        "gain" => pad.gain = db_to_linear(number().unwrap_or(0.0)),
        "pan" => pad.pan = number().unwrap_or(0.0).clamp(-1.0, 1.0),
        "tune" => pad.tune_semitones = number().unwrap_or(0.0),
        "cents" => pad.tune_cents = number().unwrap_or(0.0),
        "decay" => pad.decay = number().unwrap_or(0.0).max(0.0),
        "release" => pad.release = number().unwrap_or(0.005).max(0.0),
        "reverse" => pad.reverse = bool_value(value).unwrap_or(false),
        "one_shot" => pad.one_shot = bool_value(value).unwrap_or(true),
        "choke" => pad.choke = number().unwrap_or(0.0).round().clamp(0.0, 127.0) as u8,
        "voice" => match value {
            ParamValue::String(text) => {
                pad.voice = Some(text.clone()).filter(|name| !name.trim().is_empty());
            }
            _ => return Err("pad voice must be a name".to_string()),
        },
        _ => return Err(format!("unknown pad field '{field}'")),
    }
    Ok(())
}

fn bool_value(value: &ParamValue) -> Option<bool> {
    match value {
        ParamValue::Bool(value) => Some(*value),
        ParamValue::String(text) => Some(text.eq_ignore_ascii_case("true")),
        ParamValue::Number(value) => Some(*value != 0.0),
        _ => None,
    }
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn resolve_sample(raw: &str, root: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        root.map(|root| root.join(&path)).unwrap_or(path)
    }
}

/// Longest sample or synthesised tail declared by a kit, capped by the
/// renderer's bound.
pub fn params_tail_seconds(params: &[(String, ParamValue)], root: Option<&Path>) -> Option<f64> {
    let kit = kit_from_params(params, root).ok()?;
    let mut longest: f64 = 0.0;
    for pad in &kit.pads {
        let mut sample_tail: f64 = 0.0;
        for layer in &pad.layers {
            if layer.sample.as_os_str().is_empty() {
                continue;
            }
            if let Ok(sample) = decode_sample(&layer.sample)
                && sample.rate() > 0
            {
                let seconds = sample.frame_count() as f64 / f64::from(sample.rate());
                sample_tail = sample_tail.max(seconds + pad.release);
            }
        }
        longest = longest.max(sample_tail);
        // Only pads that would actually fall back keep the synth tail.
        if sample_tail == 0.0
            && let Some(kind) = pad_voice(pad)
        {
            longest = longest.max(kind.tail_seconds() + pad.release);
        }
    }
    Some(longest.min(30.0))
}

// ============================================================================
// Runtime
// ============================================================================

struct RuntimeLayer {
    low: u8,
    high: u8,
    gain: f64,
    sample: Option<Arc<DecodedSample>>,
}

struct RuntimePad {
    note: u8,
    gain: f64,
    pan: f64,
    tune_ratio: f64,
    decay_coeff: f64,
    release: f64,
    one_shot: bool,
    choke: u8,
    /// Resolved fallback voice; `None` leaves the pad silent without a sample.
    voice: Option<VoiceKind>,
    /// Noise seed derived from the declaration index at kit build time.
    noise_seed: u64,
    layers: Vec<RuntimeLayer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Attack,
    Sustain,
    Release,
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    pad: usize,
    layer: usize,
    position: f64,
    increment: f64,
    level: f64,
    attack_step: f64,
    release_step: f64,
    stage: Stage,
    gain: f64,
    pan: f64,
    started: u64,
    /// `Some` for a synthesised voice; sample voices keep it `None`.
    kind: Option<VoiceKind>,
    /// Frames elapsed since note-on, the time base of the voice.
    frames: u64,
    tune: f64,
    body_env: f64,
    noise_env: f64,
    body_coeff: f64,
    noise_coeff: f64,
    /// Decaying pitch-glide term, `1` at note-on.
    glide: f64,
    glide_coeff: f64,
    /// xorshift state, seeded from the pad at note-on.
    noise: u64,
    velocity: f64,
    /// One-pole filter history and coefficient for the click / noise shaping.
    filter_x: f64,
    filter_y: f64,
    filter_coeff: f64,
}

/// Decoded, preallocated kit. All heap growth happens at construction.
pub struct DrumKit {
    pads: Vec<RuntimePad>,
    /// `0..max_voices` are playable slots; the rest hold fading tails.
    voices: Vec<Voice>,
    max_voices: usize,
    sample_rate: u32,
    age: u64,
    /// Per-pad trigger counter; drives deterministic round-robin.
    round_robin: Vec<u64>,
    warnings: Vec<String>,
}

impl DrumKit {
    /// Build the playable kit from a document, reporting missing samples once.
    pub fn from_kit(kit: &Kit, sample_rate: u32) -> Result<Self, String> {
        let sample_rate = sample_rate.max(1);
        let rate = f64::from(sample_rate);
        let max_voices = kit.max_voices.clamp(1, MAX_VOICES);
        let mut warnings = Vec::new();
        let mut missing: HashSet<PathBuf> = HashSet::new();
        let mut pads = Vec::with_capacity(kit.pads.len());

        for (pad_index, pad) in kit.pads.iter().enumerate() {
            let mut layers = Vec::with_capacity(pad.layers.len());
            let mut layer_warned = false;
            for (index, layer) in pad.layers.iter().enumerate() {
                let decoded = if layer.sample.as_os_str().is_empty() {
                    None
                } else {
                    match decode_sample(&layer.sample) {
                        Ok(mut sample) => {
                            if pad.reverse {
                                sample.reverse_in_place();
                            }
                            Some(Arc::new(sample))
                        }
                        Err(error) => {
                            layer_warned = true;
                            if missing.insert(layer.sample.clone()) {
                                let warning = format!(
                                    "drumkit pad '{}' layer {}: {} (silent)",
                                    pad.name, index, error
                                );
                                log::warn!("{warning}");
                                warnings.push(warning);
                            }
                            None
                        }
                    }
                };
                layers.push(RuntimeLayer {
                    low: layer.low,
                    high: layer.high,
                    gain: layer.gain,
                    sample: decoded,
                });
            }
            let voice = pad_voice(pad);
            let playable = layers.iter().any(|layer| layer.sample.is_some());
            if !playable && voice.is_none() && !layer_warned {
                let warning = format!(
                    "drumkit pad '{}' (note {}): no sample layer and no synthesised voice for name '{}' or MIDI note {}, so its hits are silent; the voices it can use are {}",
                    pad.name,
                    pad.note,
                    pad.name,
                    pad.note,
                    VoiceKind::NAMES.join(", ")
                );
                log::warn!("{warning}");
                warnings.push(warning);
            }
            if let Some(name) = pad.voice.as_deref().filter(|name| !name.trim().is_empty())
                && VoiceKind::from_name(name).is_none()
            {
                let warning = format!(
                    "drumkit pad '{}': unknown voice '{name}' (ignored)",
                    pad.name
                );
                log::warn!("{warning}");
                warnings.push(warning);
            }
            pads.push(RuntimePad {
                note: pad.note,
                gain: pad.gain,
                pan: pad.pan,
                tune_ratio: 2f64.powf((pad.tune_semitones + pad.tune_cents / 100.0) / 12.0),
                decay_coeff: if pad.decay > 0.0 {
                    0.001f64.powf(1.0 / (pad.decay * rate))
                } else {
                    1.0
                },
                release: pad.release,
                one_shot: pad.one_shot,
                choke: pad.choke,
                voice,
                noise_seed: pad_noise_seed(pad_index),
                layers,
            });
        }

        let mut voices = vec![Voice::default(); max_voices + MAX_FADE_TAILS];
        for voice in &mut voices {
            voice.attack_step = 1.0 / (ATTACK_SECONDS * rate);
        }
        let round_robin = vec![0u64; pads.len()];
        Ok(Self {
            pads,
            voices,
            max_voices,
            sample_rate,
            age: 0,
            round_robin,
            warnings,
        })
    }

    /// Build the playable kit from a preset's parameters and resource root.
    pub fn from_params(
        params: &[(String, ParamValue)],
        root: Option<&Path>,
        sample_rate: u32,
    ) -> Result<Self, String> {
        let kit = kit_from_params(params, root)?;
        Self::from_kit(&kit, sample_rate)
    }

    /// Missing or unreadable samples reported once at construction.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The prepared voice capacity.
    pub fn max_voices(&self) -> usize {
        self.max_voices
    }

    /// The number of pads the document declares.
    pub fn pad_count(&self) -> usize {
        self.pads.len()
    }

    /// Whether any voice is still sounding.
    pub fn is_active(&self) -> bool {
        self.voices.iter().any(|voice| voice.active)
    }

    /// The MIDI note that triggers a pad.
    pub fn note_for_pad(&self, pad: usize) -> Option<u8> {
        self.pads.get(pad).map(|pad| pad.note)
    }

    /// Trigger `note`. Returns the pad index even when its samples are missing,
    /// so the GUI can still flash the pad.
    pub fn note_on(&mut self, note: u8, velocity: u8) -> Option<usize> {
        let pad_index = self.pads.iter().position(|pad| pad.note == note)?;
        let choke = self.pads[pad_index].choke;
        if choke != 0 {
            for index in 0..self.max_voices {
                if self.voices[index].active && self.pads[self.voices[index].pad].choke == choke {
                    self.fade_slot(index, CHOKE_FADE_SECONDS);
                }
            }
        }

        // Sample layers win; a pad without a playable layer falls back to its
        // synthesised voice, and then to silence.
        let layer_index = self.select_layer(pad_index, velocity);
        let sample_info = self
            .pads
            .get(pad_index)
            .and_then(|pad| pad.layers.get(layer_index))
            .and_then(|layer| {
                layer
                    .sample
                    .as_ref()
                    .map(|sample| (layer.gain, sample.rate().max(1)))
            });
        if sample_info.is_none() && self.pads[pad_index].voice.is_none() {
            return Some(pad_index);
        }

        let slot = self.next_slot();
        self.age = self.age.wrapping_add(1);
        let velocity_gain = f64::from(velocity.clamp(1, 127)) / 127.0;
        let pad = &self.pads[pad_index];

        if let Some((layer_gain, asset_rate)) = sample_info {
            self.voices[slot] = Voice {
                active: true,
                pad: pad_index,
                layer: layer_index,
                position: 0.0,
                increment: pad.tune_ratio * f64::from(asset_rate) / f64::from(self.sample_rate),
                level: 0.0,
                attack_step: 1.0 / (ATTACK_SECONDS * f64::from(self.sample_rate)),
                release_step: 0.0,
                stage: Stage::Attack,
                gain: pad.gain * layer_gain * velocity_gain,
                pan: pad.pan,
                started: self.age,
                ..Voice::default()
            };
            return Some(pad_index);
        }

        let Some(kind) = pad.voice else {
            return Some(pad_index);
        };
        self.voices[slot] = Voice::start_synth(
            kind,
            pad_index,
            pad.tune_ratio,
            pad.noise_seed,
            velocity_gain,
            pad.gain * velocity_gain,
            pad.pan,
            self.age,
            f64::from(self.sample_rate),
        );
        Some(pad_index)
    }

    /// Release every voice started by `note`.
    pub fn note_off(&mut self, note: u8) {
        for index in 0..self.voices.len() {
            let voice = self.voices[index];
            if !voice.active {
                continue;
            }
            let Some(pad) = self.pads.get(voice.pad) else {
                continue;
            };
            if pad.note != note || pad.one_shot || voice.stage == Stage::Release {
                continue;
            }
            self.release_slot(index, pad.release);
        }
    }

    /// Return to the state a freshly constructed kit has. Decoded layers stay
    /// loaded; every voice, round-robin cursor and age resets.
    pub fn reset(&mut self) {
        let rate = f64::from(self.sample_rate.max(1));
        for voice in &mut self.voices {
            *voice = Voice::default();
            voice.attack_step = 1.0 / (ATTACK_SECONDS * rate);
        }
        self.age = 0;
        self.round_robin.fill(0);
    }

    /// Release every gated pad; one-shots keep playing.
    pub fn all_notes_off(&mut self) {
        for index in 0..self.voices.len() {
            let voice = self.voices[index];
            if !voice.active || voice.stage == Stage::Release {
                continue;
            }
            let one_shot = self
                .pads
                .get(voice.pad)
                .map(|pad| pad.one_shot)
                .unwrap_or(true);
            if one_shot {
                continue;
            }
            let release = self.pads[voice.pad].release;
            self.release_slot(index, release);
        }
    }

    /// Fade out everything, including one-shots, without cutting a waveform.
    pub fn all_sound_off(&mut self) {
        for index in 0..self.max_voices {
            if self.voices[index].active {
                self.fade_slot(index, STEAL_FADE_SECONDS);
            }
        }
    }

    /// Render `frames` of interleaved stereo output.
    pub fn render_interleaved(&mut self, frames: usize, sample_rate: u32) -> Vec<f32> {
        let mut output = vec![0.0; frames * 2];
        self.render_into(&mut output, sample_rate)
            .expect("complete stereo output frames");
        output
    }

    /// Render into caller-owned interleaved stereo storage without heap
    /// operations. Empty output is valid. An incomplete frame is rejected
    /// before touching the output or advancing any voice or envelope.
    pub fn render_into(
        &mut self,
        output: &mut [f32],
        sample_rate: u32,
    ) -> Result<(), oc_audio::StereoBufferError> {
        if !output.len().is_multiple_of(2) {
            return Err(oc_audio::StereoBufferError::IncompleteFrame);
        }
        for frame in output.chunks_exact_mut(2) {
            frame.copy_from_slice(&self.next_stereo(sample_rate));
        }
        Ok(())
    }

    /// Render one stereo frame, updating the sample rate when it changes.
    pub fn next_stereo(&mut self, sample_rate: u32) -> [f32; 2] {
        let sample_rate = sample_rate.max(1);
        if sample_rate != self.sample_rate {
            self.set_sample_rate(sample_rate);
        }
        let pads = &self.pads;
        let mut left = 0.0;
        let mut right = 0.0;
        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }
            let Some(pad) = pads.get(voice.pad) else {
                voice.active = false;
                continue;
            };
            if voice.kind.is_some() {
                if !voice.advance_synth(pad) {
                    continue;
                }
                let value = voice.render_synth(f64::from(sample_rate));
                let gain = voice.level * voice.gain;
                let pan = voice.pan;
                left += value * gain * (1.0 - pan.max(0.0));
                right += value * gain * (1.0 + pan.min(0.0));
                continue;
            }
            let Some(layer) = pad.layers.get(voice.layer) else {
                voice.active = false;
                continue;
            };
            let Some(sample) = &layer.sample else {
                voice.active = false;
                continue;
            };
            voice.advance(pad, sample);
            if !voice.active {
                continue;
            }
            let (sample_left, sample_right) = read_stereo(sample, voice.position);
            voice.position += voice.increment;
            let gain = voice.level * voice.gain;
            let pan = voice.pan;
            left += sample_left * gain * (1.0 - pan.max(0.0));
            right += sample_right * gain * (1.0 + pan.min(0.0));
        }
        [left as f32, right as f32]
    }

    fn set_sample_rate(&mut self, sample_rate: u32) {
        self.sample_rate = sample_rate;
        let rate = f64::from(sample_rate);
        for index in 0..self.voices.len() {
            if !self.voices[index].active {
                continue;
            }
            if self.voices[index].kind.is_some() {
                self.voices[index].reconfigure_synth(rate);
                continue;
            }
            let pad_index = self.voices[index].pad;
            let layer_index = self.voices[index].layer;
            let Some(layer) = self
                .pads
                .get(pad_index)
                .and_then(|pad| pad.layers.get(layer_index))
            else {
                continue;
            };
            let Some(sample) = &layer.sample else {
                continue;
            };
            self.voices[index].increment =
                self.pads[pad_index].tune_ratio * f64::from(sample.rate().max(1)) / rate;
            self.voices[index].attack_step = 1.0 / (ATTACK_SECONDS * rate);
        }
    }

    /// Pick a free primary slot, stealing the quietest voice when full.
    fn next_slot(&mut self) -> usize {
        match (0..self.max_voices).find(|&index| !self.voices[index].active) {
            Some(index) => index,
            None => {
                let victim = self.quietest_primary().unwrap_or(0);
                self.fade_slot(victim, STEAL_FADE_SECONDS);
                victim
            }
        }
    }

    /// Deterministic velocity-layer / round-robin selection.
    fn select_layer(&mut self, pad_index: usize, velocity: u8) -> usize {
        let Some(pad) = self.pads.get(pad_index) else {
            return 0;
        };
        let playable = |layer: &RuntimeLayer| {
            layer.sample.is_some() && velocity >= layer.low && velocity <= layer.high
        };
        let count = pad.layers.iter().filter(|layer| playable(layer)).count();
        if count > 0 {
            let counter = self.round_robin[pad_index];
            self.round_robin[pad_index] = counter.wrapping_add(1);
            let wanted = (counter % count as u64) as usize;
            return pad
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| playable(layer))
                .nth(wanted)
                .map(|(index, _)| index)
                .unwrap_or(0);
        }
        // Velocity outside every playable layer: use the nearest range.
        let mut best = (0usize, u32::MAX);
        for (index, layer) in pad.layers.iter().enumerate() {
            if layer.sample.is_none() {
                continue;
            }
            let distance = if velocity < layer.low {
                u32::from(layer.low - velocity)
            } else if velocity > layer.high {
                u32::from(velocity - layer.high)
            } else {
                0
            };
            if distance < best.1 {
                best = (index, distance);
            }
        }
        best.0
    }

    fn quietest_primary(&self) -> Option<usize> {
        let mut best: Option<(usize, f64, u64)> = None;
        for index in 0..self.max_voices {
            let voice = &self.voices[index];
            if !voice.active {
                continue;
            }
            let output = voice.level * voice.gain;
            match best {
                Some((_, level, started))
                    if output > level || (output == level && voice.started >= started) => {}
                _ => best = Some((index, output, voice.started)),
            }
        }
        best.map(|(index, _, _)| index)
    }

    fn release_slot(&mut self, index: usize, seconds: f64) {
        let voice = &mut self.voices[index];
        if !voice.active {
            return;
        }
        if seconds <= 0.0 {
            voice.active = false;
            return;
        }
        voice.stage = Stage::Release;
        voice.release_step = voice.level.max(1.0e-9) / (seconds * f64::from(self.sample_rate));
    }

    /// Move a voice into a fade slot so the cut is a short ramp, not a click.
    fn fade_slot(&mut self, index: usize, seconds: f64) {
        let mut voice = self.voices[index];
        if !voice.active {
            return;
        }
        voice.stage = Stage::Release;
        voice.release_step =
            voice.level.max(1.0e-6) / (seconds.max(0.0005) * f64::from(self.sample_rate));
        let tail_start = self.max_voices;
        let slot = (tail_start..self.voices.len())
            .find(|&slot| !self.voices[slot].active)
            .or_else(|| {
                // All fade slots busy: drop the quietest tail (already fading).
                (tail_start..self.voices.len()).min_by(|&a, &b| {
                    let left = self.voices[a].level * self.voices[a].gain;
                    let right = self.voices[b].level * self.voices[b].gain;
                    left.partial_cmp(&right)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });
        if let Some(slot) = slot {
            self.voices[slot] = voice;
            self.voices[index].active = false;
        }
    }
}

/// Four 6 ms noise bursts 8 ms apart; the 120 ms tail is the voice envelope.
fn clap_burst(t: f64) -> f64 {
    const OFFSETS: [f64; 4] = [0.0, 0.008, 0.016, 0.024];
    const BURST: f64 = 0.006;
    for offset in OFFSETS {
        let local = t - offset;
        if (0.0..BURST).contains(&local) {
            return 0.001f64.powf(local / BURST);
        }
    }
    0.0
}

impl Voice {
    /// Start a synthesised voice from closed-form state; no buffer is touched.
    #[allow(clippy::too_many_arguments)]
    fn start_synth(
        kind: VoiceKind,
        pad: usize,
        tune: f64,
        noise_seed: u64,
        velocity: f64,
        gain: f64,
        pan: f64,
        started: u64,
        sample_rate: f64,
    ) -> Self {
        let params = kind.synthesis(sample_rate);
        Self {
            active: true,
            pad,
            kind: Some(kind),
            tune,
            noise: noise_seed,
            velocity,
            gain,
            pan,
            started,
            attack_step: 1.0 / (ATTACK_SECONDS * sample_rate),
            body_env: 1.0,
            noise_env: 1.0,
            glide: 1.0,
            body_coeff: params.body_coeff,
            noise_coeff: params.noise_coeff,
            glide_coeff: params.glide_coeff,
            filter_coeff: params.filter_coeff,
            ..Self::default()
        }
    }

    fn reconfigure_synth(&mut self, sample_rate: f64) {
        let Some(kind) = self.kind else {
            return;
        };
        let params = kind.synthesis(sample_rate);
        self.body_coeff = params.body_coeff;
        self.noise_coeff = params.noise_coeff;
        self.glide_coeff = params.glide_coeff;
        self.filter_coeff = params.filter_coeff;
        self.attack_step = 1.0 / (ATTACK_SECONDS * sample_rate);
    }

    /// Advance the VCA and the synth envelopes; `false` once the voice is done.
    fn advance_synth(&mut self, pad: &RuntimePad) -> bool {
        match self.stage {
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                self.level *= pad.decay_coeff;
                if self.level <= SILENCE_FLOOR {
                    self.active = false;
                }
            }
            Stage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 {
                    self.active = false;
                }
            }
        }
        let noise_finished =
            !matches!(self.kind, Some(VoiceKind::Snare)) || self.noise_env <= SYNTH_ENV_FLOOR;
        if self.body_env <= SYNTH_ENV_FLOOR && noise_finished {
            self.active = false;
        }
        self.frames = self.frames.wrapping_add(1);
        self.body_env *= self.body_coeff;
        self.noise_env *= self.noise_coeff;
        self.glide *= self.glide_coeff;
        self.active
    }

    /// One sample of the fallback voice, before the VCA and pan law.
    fn render_synth(&mut self, sample_rate: f64) -> f64 {
        let Some(kind) = self.kind else {
            return 0.0;
        };
        let t = self.frames as f64 / sample_rate.max(1.0);
        let tune = self.tune;
        let tau = std::f64::consts::TAU;
        let value = match kind {
            VoiceKind::Kick => {
                // 150 Hz falling to 50 Hz with a 45 ms exponential glide.
                let start = 150.0 * tune;
                let end = 50.0 * tune;
                let phase = tau * (end * t + (start - end) * 0.045 * (1.0 - self.glide));
                let noise = next_noise(&mut self.noise);
                self.filter_y += self.filter_coeff * (noise - self.filter_y);
                phase.sin() * self.body_env + self.filter_y * self.noise_env * 0.1259
            }
            VoiceKind::Snare => {
                let body = (0.65 * (tau * 185.0 * tune * t).sin()
                    + 0.35 * (tau * 330.0 * tune * t).sin())
                    * self.body_env;
                let noise = next_noise(&mut self.noise);
                let high = self.filter_coeff * (self.filter_y + noise - self.filter_x);
                self.filter_x = noise;
                self.filter_y = high;
                let noise_weight = 0.35 + 0.65 * self.velocity;
                body + high * noise_weight * self.noise_env
            }
            VoiceKind::HatClosed | VoiceKind::HatOpen => {
                const RATIOS: [f64; 6] = [1.0, 1.34, 1.71, 2.06, 2.53, 3.11];
                let base = 320.0 * tune;
                let mut sum = 0.0;
                for ratio in RATIOS {
                    sum += (tau * base * ratio * t).sin().signum();
                }
                let sum = sum / RATIOS.len() as f64;
                let high = self.filter_coeff * (self.filter_y + sum - self.filter_x);
                self.filter_x = sum;
                self.filter_y = high;
                high * self.body_env
            }
            VoiceKind::Tom => {
                // 220 Hz falling to 150 Hz with a 60 ms exponential glide.
                let start = 220.0 * tune;
                let end = 150.0 * tune;
                let phase = tau * (end * t + (start - end) * 0.060 * (1.0 - self.glide));
                phase.sin() * self.body_env
            }
            VoiceKind::Clap => {
                let noise = next_noise(&mut self.noise);
                (noise * (clap_burst(t) + 0.45 * self.body_env) / 1.45).clamp(-1.0, 1.0)
            }
            VoiceKind::Rim => {
                (0.7 * (tau * 800.0 * tune * t).sin() + 0.3 * (tau * 1_700.0 * tune * t).sin())
                    * self.body_env
            }
        };
        value * kind.peak() * kind.calibration()
    }

    /// Advance the VCA and sample position; deactivates finished voices.
    fn advance(&mut self, pad: &RuntimePad, sample: &DecodedSample) {
        match self.stage {
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                self.level *= pad.decay_coeff;
                if self.level <= SILENCE_FLOOR {
                    self.active = false;
                    return;
                }
            }
            Stage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 {
                    self.active = false;
                    return;
                }
            }
        }
        if self.position >= sample.frame_count() as f64 {
            self.active = false;
        }
    }
}

/// Render a `builtin.drumkit` track from interpreted MIDI events.
pub fn try_render_track(
    params: &[(String, ParamValue)],
    events: &[&crate::MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Vec<f32>, String> {
    try_render_track_with_warnings(params, events, total_samples, sample_rate, root)
        .map(|(audio, _)| audio)
}

/// [`try_render_track`] plus the kit's own diagnostics, so a pad that cannot
/// sound reaches the renderer's warning list.
pub fn try_render_track_with_warnings(
    params: &[(String, ParamValue)],
    events: &[&crate::MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<(Vec<f32>, Vec<String>), String> {
    if sample_rate == 0 || total_samples.checked_mul(2).is_none() {
        return Err("invalid drum-kit render size".into());
    }
    let mut kit = DrumKit::from_params(params, root, sample_rate)?;
    let warnings = kit.warnings().to_vec();
    let mut output = vec![0.0; total_samples * 2];
    let mut event_index = 0usize;
    for frame in 0..total_samples {
        while event_index < events.len() && events[event_index].time_samples as usize <= frame {
            match &events[event_index].event {
                crate::MidiMessage::NoteOn { note, velocity, .. } => {
                    kit.note_on(*note, *velocity);
                }
                crate::MidiMessage::NoteOff { note, .. } => kit.note_off(*note),
                _ => {}
            }
            event_index += 1;
        }
        let [left, right] = kit.next_stereo(sample_rate);
        output[frame * 2] = left;
        output[frame * 2 + 1] = right;
    }
    Ok((output, warnings))
}

/// One-line summary for logs and the plugin index.
pub fn describe_kit(params: &[(String, ParamValue)], root: Option<&Path>) -> String {
    match kit_from_params(params, root) {
        Ok(kit) => format!(
            "{} pads, max {} voices",
            kit.pads.len(),
            kit.max_voices.min(MAX_VOICES)
        ),
        Err(error) => format!("invalid kit: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "opencompose-drumkit-{}-{label}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_dc(path: &Path, frames: usize, amplitude: f32, rate: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let value = (f64::from(amplitude) * f64::from(i16::MAX)) as i16;
        for _ in 0..frames {
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn write_ramp(path: &Path, frames: usize, rate: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let value = frame as f32 / frames as f32 * 0.8;
            writer
                .write_sample((f64::from(value) * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn params(entries: &[(&str, ParamValue)]) -> Vec<(String, ParamValue)> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn pad_params(note: u8, one_shot: bool, choke: u8) -> Vec<(&'static str, ParamValue)> {
        vec![
            ("pad.kick.note", ParamValue::Number(f64::from(note))),
            ("pad.kick.gain", ParamValue::Db(0.0)),
            ("pad.kick.pan", ParamValue::Number(0.0)),
            ("pad.kick.decay", ParamValue::Time(0.0)),
            ("pad.kick.release", ParamValue::Time(0.01)),
            ("pad.kick.reverse", ParamValue::Bool(false)),
            ("pad.kick.one_shot", ParamValue::Bool(one_shot)),
            ("pad.kick.choke", ParamValue::Number(f64::from(choke))),
        ]
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()))
    }

    #[test]
    fn one_shot_ignores_note_off_while_gate_releases() {
        let dir = TempDir::new("gate");
        let path = dir.join("long.wav");
        write_dc(&path, 48_000, 0.5, 48_000);

        for (one_shot, expect_active) in [(true, true), (false, false)] {
            let mut entries = params(&pad_params(36, one_shot, 0));
            entries.push(("layer.kick.0.low".to_string(), ParamValue::Number(1.0)));
            entries.push(("layer.kick.0.high".to_string(), ParamValue::Number(127.0)));
            entries.push((
                "layer.kick.0.sample".to_string(),
                ParamValue::String(path.display().to_string()),
            ));
            let mut kit = DrumKit::from_params(&entries, None, 48_000).expect("kit builds");
            kit.note_on(36, 100);
            kit.render_interleaved(4_800, 48_000);
            kit.note_off(36);
            let tail = kit.render_interleaved(960, 48_000);
            assert_eq!(kit.is_active(), expect_active, "one_shot={one_shot}");
            if one_shot {
                assert!(peak(&tail) > 0.1, "one-shot must survive note-off");
            } else {
                // 10 ms release = 480 frames; measure only after it completes.
                assert!(
                    peak(&tail[960..]) < 0.05,
                    "gate must release after note-off"
                );
            }
        }
    }

    #[test]
    fn velocity_layers_select_the_matching_sample() {
        let dir = TempDir::new("velocity");
        let soft = dir.join("soft.wav");
        let hard = dir.join("hard.wav");
        write_dc(&soft, 4_800, 0.1, 48_000);
        write_dc(&hard, 4_800, 0.4, 48_000);
        let entries = params(&[
            ("pad.snare.note", ParamValue::Number(38.0)),
            ("pad.snare.one_shot", ParamValue::Bool(false)),
            ("pad.snare.release", ParamValue::Time(0.001)),
            ("layer.snare.0.low", ParamValue::Number(1.0)),
            ("layer.snare.0.high", ParamValue::Number(63.0)),
            (
                "layer.snare.0.sample",
                ParamValue::String(soft.display().to_string()),
            ),
            ("layer.snare.1.low", ParamValue::Number(64.0)),
            ("layer.snare.1.high", ParamValue::Number(127.0)),
            (
                "layer.snare.1.sample",
                ParamValue::String(hard.display().to_string()),
            ),
        ]);
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        kit.note_on(38, 63);
        let soft_peak = peak(&kit.render_interleaved(480, 48_000));
        kit.note_off(38);
        kit.render_interleaved(480, 48_000);
        kit.note_on(38, 64);
        let hard_peak = peak(&kit.render_interleaved(480, 48_000));
        assert!(
            (hard_peak / soft_peak - 4.0).abs() < 0.2,
            "{hard_peak} vs {soft_peak}"
        );
    }

    #[test]
    fn round_robin_cycles_deterministically_across_repeats() {
        let dir = TempDir::new("round-robin");
        let names = ["a.wav", "b.wav", "c.wav"];
        let amplitudes = [0.1f32, 0.2, 0.3];
        let mut entries = vec![
            ("pad.hat.note".to_string(), ParamValue::Number(42.0)),
            ("pad.hat.one_shot".to_string(), ParamValue::Bool(true)),
        ];
        for (index, (name, amplitude)) in names.iter().zip(amplitudes).enumerate() {
            let path = dir.join(name);
            write_dc(&path, 512, amplitude, 48_000);
            entries.push((format!("layer.hat.{index}.low"), ParamValue::Number(1.0)));
            entries.push((format!("layer.hat.{index}.high"), ParamValue::Number(127.0)));
            entries.push((
                format!("layer.hat.{index}.sample"),
                ParamValue::String(path.display().to_string()),
            ));
        }
        let mut first = DrumKit::from_params(&entries, None, 48_000).unwrap();
        let mut second = DrumKit::from_params(&entries, None, 48_000).unwrap();
        let mut peaks = Vec::new();
        for _ in 0..6 {
            assert_eq!(first.note_on(42, 100), Some(0));
            peaks.push(peak(&first.render_interleaved(1_024, 48_000)));
            second.note_on(42, 100);
            let repeat = peak(&second.render_interleaved(1_024, 48_000));
            assert!((peaks.last().unwrap() - repeat).abs() < 1.0e-6);
        }
        let velocity_gain = 100.0f32 / 127.0;
        for index in 0..6 {
            let expected = amplitudes[index % 3] * velocity_gain;
            assert!((peaks[index] - expected).abs() < 0.01, "{peaks:?}");
        }
    }

    #[test]
    fn choke_cuts_the_previous_voice_within_the_fade() {
        let dir = TempDir::new("choke");
        let open = dir.join("open.wav");
        let silent = dir.join("silent.wav");
        write_dc(&open, 48_000, 0.5, 48_000);
        write_dc(&silent, 4_800, 0.0, 48_000);
        let entries = params(&[
            ("pad.open.note", ParamValue::Number(46.0)),
            ("pad.open.one_shot", ParamValue::Bool(true)),
            ("pad.open.choke", ParamValue::Number(3.0)),
            (
                "layer.open.0.sample",
                ParamValue::String(open.display().to_string()),
            ),
            ("pad.closed.note", ParamValue::Number(42.0)),
            ("pad.closed.one_shot", ParamValue::Bool(true)),
            ("pad.closed.choke", ParamValue::Number(3.0)),
            (
                "layer.closed.0.sample",
                ParamValue::String(silent.display().to_string()),
            ),
        ]);
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        kit.note_on(46, 127);
        let before = kit.render_interleaved(2_400, 48_000);
        assert!(peak(&before) > 0.4);
        kit.note_on(42, 127);
        let fade_frames = (CHOKE_FADE_SECONDS * 48_000.0).ceil() as usize;
        let after = kit.render_interleaved(fade_frames + 480, 48_000);
        let faded = &after[fade_frames * 2..];
        assert!(
            peak(faded) < 0.001,
            "choked voice still audible: {}",
            peak(faded)
        );
        // The ramp bounds the largest per-sample jump at the choke point.
        let mut previous = before[before.len() - 1];
        let mut largest = 0.0f32;
        for sample in &after {
            largest = largest.max((sample - previous).abs());
            previous = *sample;
        }
        assert!(largest < 0.01, "choke clicked: {largest}");
    }

    #[test]
    fn voice_stealing_fades_the_quietest_voice() {
        let dir = TempDir::new("steal");
        let path = dir.join("long.wav");
        write_dc(&path, 48_000, 0.2, 48_000);
        let mut entries = params(&[
            ("kit.max_voices", ParamValue::Number(4.0)),
            ("pad.kick.note", ParamValue::Number(36.0)),
            ("pad.kick.one_shot", ParamValue::Bool(true)),
            ("layer.kick.0.low", ParamValue::Number(1.0)),
            ("layer.kick.0.high", ParamValue::Number(127.0)),
        ]);
        entries.push((
            "layer.kick.0.sample".to_string(),
            ParamValue::String(path.display().to_string()),
        ));
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        for velocity in [127, 100, 80, 10] {
            kit.note_on(36, velocity);
            kit.render_interleaved(48, 48_000);
        }
        let mut rendered = kit.render_interleaved(480, 48_000);
        kit.note_on(36, 90);
        let tails: Vec<f64> = kit.voices[kit.max_voices..]
            .iter()
            .filter(|voice| voice.active)
            .map(|voice| voice.gain)
            .collect();
        assert_eq!(tails.len(), 1, "one voice should be fading");
        // The quietest (velocity 10) voice is the stolen one.
        assert!((tails[0] - 10.0 / 127.0).abs() < 0.01, "{tails:?}");
        rendered.extend(kit.render_interleaved(960, 48_000));
        let mut previous = rendered[0];
        let mut largest = 0.0f32;
        for sample in &rendered[1..] {
            largest = largest.max((sample - previous).abs());
            previous = *sample;
        }
        assert!(largest < 0.02, "steal clicked: {largest}");
    }

    #[test]
    fn missing_sample_warns_once_and_renders_silence() {
        let dir = TempDir::new("missing");
        // "aux" has no sample and no resolvable voice, so it stays silent.
        let entries = params(&[
            ("pad.aux.note", ParamValue::Number(60.0)),
            ("layer.aux.0.low", ParamValue::Number(1.0)),
            ("layer.aux.0.high", ParamValue::Number(127.0)),
            (
                "layer.aux.0.sample",
                ParamValue::String(dir.join("absent.wav").display().to_string()),
            ),
        ]);
        let mut kit =
            DrumKit::from_params(&entries, None, 48_000).expect("missing file must not fail");
        assert_eq!(kit.warnings().len(), 1);
        kit.note_on(60, 127);
        let output = kit.render_interleaved(480, 48_000);
        assert_eq!(peak(&output), 0.0);
    }

    #[test]
    fn named_pad_falls_back_to_its_voice_when_the_sample_is_missing() {
        let dir = TempDir::new("missing-voice");
        let entries = params(&[
            ("pad.kick.note", ParamValue::Number(36.0)),
            ("layer.kick.0.low", ParamValue::Number(1.0)),
            ("layer.kick.0.high", ParamValue::Number(127.0)),
            (
                "layer.kick.0.sample",
                ParamValue::String(dir.join("absent.wav").display().to_string()),
            ),
        ]);
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        assert_eq!(kit.warnings().len(), 1);
        kit.note_on(36, 127);
        let output = kit.render_interleaved(4_800, 48_000);
        assert!(peak(&output) > 0.1, "kick fallback was silent");
    }

    #[test]
    fn decay_reaches_minus_60_db_over_the_declared_time() {
        let dir = TempDir::new("decay");
        let path = dir.join("dc.wav");
        write_dc(&path, 48_000, 0.5, 48_000);
        let entries = params(&[
            ("pad.kick.note", ParamValue::Number(36.0)),
            ("pad.kick.decay", ParamValue::Time(0.1)),
            ("pad.kick.one_shot", ParamValue::Bool(true)),
            (
                "layer.kick.0.sample",
                ParamValue::String(path.display().to_string()),
            ),
        ]);
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        kit.note_on(36, 127);
        let output = kit.render_interleaved(4_800, 48_000);
        let early = output[30 * 2].abs();
        let late = output[4_790 * 2].abs();
        assert!((early - 0.5).abs() < 0.02, "early {early}");
        let ratio_db = 20.0 * (f64::from(late) / f64::from(early)).log10();
        assert!((ratio_db + 60.0).abs() < 0.5, "decay was {ratio_db:.2} dB");
    }

    #[test]
    fn reverse_plays_the_sample_backwards() {
        let dir = TempDir::new("reverse");
        let path = dir.join("ramp.wav");
        write_ramp(&path, 4_800, 48_000);
        let forward = DrumKit::from_params(
            &params(&[
                ("pad.kick.note", ParamValue::Number(36.0)),
                ("pad.kick.reverse", ParamValue::Bool(false)),
                (
                    "layer.kick.0.sample",
                    ParamValue::String(path.display().to_string()),
                ),
            ]),
            None,
            48_000,
        )
        .unwrap();
        let reversed = DrumKit::from_params(
            &params(&[
                ("pad.kick.note", ParamValue::Number(36.0)),
                ("pad.kick.reverse", ParamValue::Bool(true)),
                (
                    "layer.kick.0.sample",
                    ParamValue::String(path.display().to_string()),
                ),
            ]),
            None,
            48_000,
        )
        .unwrap();
        let render = |mut kit: DrumKit| {
            kit.note_on(36, 127);
            kit.render_interleaved(200, 48_000)
        };
        let forward = render(forward);
        let reversed = render(reversed);
        // Skip the 0.5 ms onset ramp.
        assert!(reversed[60] > forward[60] * 4.0);
    }

    fn fnv1a(samples: &[f32]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for sample in samples {
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    fn one_pad(name: &str, note: u8) -> Vec<(String, ParamValue)> {
        vec![
            (
                format!("pad.{name}.note"),
                ParamValue::Number(f64::from(note)),
            ),
            (format!("pad.{name}.one_shot"), ParamValue::Bool(true)),
        ]
    }

    fn render_pad(
        entries: &[(String, ParamValue)],
        note: u8,
        velocity: u8,
        frames: usize,
    ) -> Vec<f32> {
        let mut kit = DrumKit::from_params(entries, None, 48_000).unwrap();
        assert_eq!(kit.note_on(note, velocity), Some(0));
        kit.render_interleaved(frames, 48_000)
    }

    fn note_event(frame: u64, note: u8, velocity: u8) -> crate::MidiEvent {
        crate::MidiEvent {
            time_samples: frame,
            event: crate::MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity,
            },
            track_index: 0,
        }
    }

    fn render_events(
        entries: &[(String, ParamValue)],
        events: &[crate::MidiEvent],
        frames: usize,
    ) -> Vec<f32> {
        let references: Vec<&crate::MidiEvent> = events.iter().collect();
        try_render_track(entries, &references, frames, 48_000, None).unwrap()
    }

    fn tail_ratio(samples: &[f32], from: usize, to: usize) -> f32 {
        peak(&samples[from * 2..to * 2]) / peak(samples)
    }

    #[test]
    fn sample_kit_render_matches_the_recorded_hash() {
        // Hash of the sample-only kit path.
        let dir = TempDir::new("sample-hash");
        let kick = dir.join("kick.wav");
        let snare = dir.join("snare.wav");
        write_ramp(&kick, 12_000, 48_000);
        write_dc(&snare, 9_600, 0.3, 48_000);
        let entries = params(&[
            ("pad.kick.note", ParamValue::Number(36.0)),
            ("pad.kick.gain", ParamValue::Db(-2.0)),
            ("pad.kick.pan", ParamValue::Number(-0.3)),
            ("pad.kick.tune", ParamValue::Number(2.0)),
            ("pad.kick.decay", ParamValue::Time(0.2)),
            ("pad.kick.one_shot", ParamValue::Bool(true)),
            (
                "layer.kick.0.sample",
                ParamValue::String(kick.display().to_string()),
            ),
            ("pad.snare.note", ParamValue::Number(38.0)),
            ("pad.snare.pan", ParamValue::Number(0.4)),
            ("pad.snare.cents", ParamValue::Number(-15.0)),
            ("pad.snare.release", ParamValue::Time(0.02)),
            ("pad.snare.one_shot", ParamValue::Bool(false)),
            ("pad.snare.choke", ParamValue::Number(2.0)),
            (
                "layer.snare.0.sample",
                ParamValue::String(snare.display().to_string()),
            ),
        ]);
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        kit.note_on(36, 127);
        kit.note_on(38, 90);
        let mut output = kit.render_interleaved(24_000, 48_000);
        kit.note_on(36, 64);
        output.extend(kit.render_interleaved(12_000, 48_000));
        assert_eq!(
            fnv1a(&output),
            0x85ce_55fe_49b9_cfc3,
            "sample-path rendering changed"
        );
    }

    #[test]
    fn pad_names_explicit_voices_and_gm_notes_resolve_alike() {
        let voices = [
            ("kick", 36u8),
            ("snare", 38),
            ("hat_closed", 42),
            ("hat_open", 46),
            ("tom", 41),
            ("clap", 39),
            ("rim", 37),
        ];
        for (voice, note) in voices {
            let by_name = render_pad(&one_pad(voice, 60), 60, 127, 4_800);
            assert!(peak(&by_name) > 0.02, "{voice} is silent");

            let mut explicit = one_pad("aux", 60);
            explicit.push((
                "pad.aux.voice".to_string(),
                ParamValue::String(voice.to_string()),
            ));
            let by_voice = render_pad(&explicit, 60, 127, 4_800);
            assert_eq!(
                fnv1a(&by_name),
                fnv1a(&by_voice),
                "{voice}: explicit voice differs from the name"
            );

            let by_note = render_pad(&one_pad("aux", note), note, 127, 4_800);
            assert_eq!(
                fnv1a(&by_name),
                fnv1a(&by_note),
                "{voice}: GM note {note} differs from the name"
            );
        }
    }

    #[test]
    fn explicit_voice_overrides_the_pad_name() {
        let mut entries = one_pad("kick", 60);
        entries.push((
            "pad.kick.voice".to_string(),
            ParamValue::String("snare".to_string()),
        ));
        let overridden = render_pad(&entries, 60, 127, 4_800);
        let snare = render_pad(&one_pad("snare", 60), 60, 127, 4_800);
        let kick = render_pad(&one_pad("kick", 60), 60, 127, 4_800);
        assert_eq!(fnv1a(&overridden), fnv1a(&snare));
        assert_ne!(fnv1a(&overridden), fnv1a(&kick));
    }

    #[test]
    fn unknown_voice_warns_and_falls_through_to_the_pad_name() {
        let mut entries = one_pad("snare", 60);
        entries.push((
            "pad.snare.voice".to_string(),
            ParamValue::String("woodblock".to_string()),
        ));
        let mut kit = DrumKit::from_params(&entries, None, 48_000).unwrap();
        assert_eq!(kit.warnings().len(), 1);
        assert!(kit.warnings()[0].contains("woodblock"));
        kit.note_on(60, 127);
        let output = kit.render_interleaved(4_800, 48_000);
        let reference = render_pad(&one_pad("snare", 60), 60, 127, 4_800);
        assert_eq!(fnv1a(&output), fnv1a(&reference));
    }

    #[test]
    fn velocity_scales_level_and_snare_noise_balance() {
        let loud = peak(&render_pad(&one_pad("kick", 36), 36, 127, 4_800));
        let mid = peak(&render_pad(&one_pad("kick", 36), 36, 64, 4_800));
        assert!(
            (loud / mid - 127.0 / 64.0).abs() < 0.15,
            "kick velocity law: {loud} vs {mid}"
        );

        let high = render_pad(&one_pad("snare", 38), 38, 127, 12_000);
        let low = render_pad(&one_pad("snare", 38), 38, 20, 12_000);
        let high_ratio = tail_ratio(&high, 7_200, 10_000);
        let low_ratio = tail_ratio(&low, 7_200, 10_000);
        assert!(
            high_ratio > low_ratio * 1.2,
            "snare noise balance: {high_ratio} vs {low_ratio}"
        );
    }

    #[test]
    fn sample_less_render_is_byte_identical_across_runs() {
        let entries = params(&[
            ("pad.kick.note", ParamValue::Number(36.0)),
            ("pad.kick.choke", ParamValue::Number(1.0)),
            ("pad.snare.note", ParamValue::Number(38.0)),
            ("pad.hat_closed.note", ParamValue::Number(42.0)),
            ("pad.hat_closed.choke", ParamValue::Number(2.0)),
            ("pad.hat_open.note", ParamValue::Number(46.0)),
            ("pad.hat_open.choke", ParamValue::Number(2.0)),
        ]);
        let events = [
            note_event(0, 36, 127),
            note_event(0, 42, 80),
            note_event(12_000, 38, 100),
            note_event(12_000, 46, 90),
            note_event(24_000, 36, 110),
            note_event(24_000, 42, 60),
        ];
        let first = render_events(&entries, &events, 48_000);
        let second = render_events(&entries, &events, 48_000);
        assert!(peak(&first) > 0.05, "sample-less kit was silent");
        assert_eq!(
            fnv1a(&first),
            fnv1a(&second),
            "two renders of one project must be identical"
        );
        assert_eq!(
            fnv1a(&first),
            fnv1a(&render_events(&entries, &events, 48_000)),
            "a fresh kit must reuse the same per-pad noise seed"
        );
    }

    #[test]
    fn voice_peaks_hit_the_documented_targets() {
        let cases = [
            ("kick", -6.0),
            ("snare", -10.0),
            ("hat_closed", -16.0),
            ("hat_open", -16.0),
            ("tom", -12.0),
            ("clap", -14.0),
            ("rim", -18.0),
        ];
        for (voice, target_db) in cases {
            let output = render_pad(&one_pad(voice, 60), 60, 127, 24_000);
            let peak = f64::from(peak(&output));
            let db = 20.0 * peak.log10();
            assert!(
                (db - target_db).abs() < 1.0,
                "{voice} peaked at {db:.2} dB, target {target_db}"
            );
        }
    }
}
