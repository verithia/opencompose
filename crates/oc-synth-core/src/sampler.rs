//! Minimal SFZ sampler used by `builtin.sampler` and by imported sample layers.
//!
//! This is deliberately a *subset* of SFZ, not a full sfizz replacement. It
//! decodes WAV and FLAC at load/render time, keeps decoded PCM in memory, and
//! performs pitch resampling per output sample. No decoding or heap allocation
//! happens inside [`Sampler::next_stereo`].
//!
//! # Supported SFZ subset
//!
//! Headers: `<group>` (inherited defaults) and `<region>`.
//!
//! Opcodes:
//! - `sample`, `lokey`, `hikey`, `key`, `lovel`, `hivel`
//! - `pitch_keycenter`, `tune`, `volume`, `pan`, `amp_veltrack`
//! - `loop_start`, `loop_end`, `loop_mode`, `end`
//! - `seq_position`, `group`, `off_by`
//! - `ampeg_attack`, `ampeg_decay`, `ampeg_sustain`, `ampeg_release`
//!
//! Unsupported opcodes and headers are collected in
//! [`SamplerPatch::diagnostics`] and logged when the patch is instantiated.
//! They are not silently interpreted as something else.
//!
//! # Known gaps
//!
//! The sampler does not implement filters, `pitch_keytrack`, round-robin banks
//! beyond a single global sequence, keyswitches, release samples, SFZ
//! `effect`/`master`/`global` sections, or embedded audio. Imported
//! granular/spectral oscillators are intentionally not mapped.

use oc_dsl::ast::ParamValue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Built-in sampler plugin id.
pub const PLUGIN_ID: &str = "builtin.sampler";

/// Hard polyphony cap. Voices are preallocated at construction so note-on does
/// not allocate inside the audio callback.
pub const MAX_VOICES: usize = 64;
/// One sample oscillator can stack up to 16 unison voices per zone.
pub const MAX_UNISON_VOICES: u32 = 16;
const MAX_SFZ_BYTES: usize = 4 * 1024 * 1024;
/// Guard against accidentally loading a multi-gigabyte asset into RAM.
const MAX_DECODED_SAMPLES: usize = 64 * 1024 * 1024;
/// Header lengths are untrusted; never reserve more than this up front.
const MAX_DECODE_PREALLOC: usize = 64 * 1024;
const DEFAULT_RELEASE_SECONDS: f64 = 0.05;

// ============================================================================
// Patch model (serializable; decoded PCM is built from it)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// How a sample region responds to note-off.
pub enum LoopMode {
    #[default]
    NoLoop,
    OneShot,
    LoopContinuous,
    LoopSustain,
    /// Forward, then backward, then forward again between the loop points.
    LoopPingPong,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
/// ADSR envelope in seconds, with sustain as a fraction.
pub struct AmpEnvelope {
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
}

impl Default for AmpEnvelope {
    fn default() -> Self {
        Self {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: DEFAULT_RELEASE_SECONDS,
        }
    }
}

/// One SFZ region after sample-path resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleRegion {
    pub sample: PathBuf,
    #[serde(default)]
    pub lokey: u8,
    #[serde(default = "hikey_default")]
    pub hikey: u8,
    #[serde(default)]
    pub lovel: u8,
    #[serde(default = "hivel_default")]
    pub hivel: u8,
    #[serde(default = "root_default")]
    pub pitch_keycenter: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_end: Option<u64>,
    #[serde(default)]
    pub loop_mode: LoopMode,
    /// First frame to play (SFZ `offset`); `None` starts at frame 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Playback end frame. `None` means the end of the decoded sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    #[serde(default)]
    pub tune_cents: f64,
    /// Linear gain after SFZ `volume` and any imported oscillator level.
    #[serde(default = "gain_default")]
    pub gain: f64,
    /// 0..=100, SFZ's percentage of velocity-to-amplitude tracking.
    #[serde(default = "veltrack_default")]
    pub amp_veltrack: f64,
    #[serde(default)]
    pub pan: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq_position: Option<u32>,
    /// Round-robin cycle length (SFZ `seq_length`); the default cycle is the
    /// largest `seq_position` in the note's zone set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_by: Option<u32>,
    /// Unison voices for one zone; the sample oscillator's `kParamUnison`.
    #[serde(default = "unison_default")]
    pub unison: u32,
    /// Symmetric detune spread in cents across the unison voices.
    #[serde(default)]
    pub unison_detune_cents: f64,
    /// Stereo spread of the unison voices, 0 = mono (the oscillator's
    /// `kParamUnisonStereo`).
    #[serde(default)]
    pub unison_width: f64,
    #[serde(default)]
    pub ampeg: AmpEnvelope,
}

fn unison_default() -> u32 {
    1
}

fn hikey_default() -> u8 {
    127
}
fn hivel_default() -> u8 {
    127
}
fn root_default() -> f64 {
    60.0
}
fn gain_default() -> f64 {
    1.0
}
fn veltrack_default() -> f64 {
    100.0
}

/// How a region's SFZ `amp_veltrack` maps velocity to amplitude.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VelocityCurve {
    /// Standard SFZ: linear tracking, `1 - (1 - v) * amp_veltrack / 100`.
    #[default]
    Linear,
    /// The imported law: the percentage is an exponent,
    /// `v ^ (2 * amp_veltrack / 100)`.
    #[serde(alias = "Serum")] // allow-re-name: stored-patch compatibility alias
    Preset,
}

/// Serializable description of a sample layer. The embedded SFZ source is
/// retained so a saved patch stays reproducible even without the original
/// preset container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplerPatch {
    #[serde(default)]
    pub regions: Vec<SampleRegion>,
    #[serde(default = "gain_default")]
    pub gain: f64,
    /// Downmix every region to mono before panning. Imported sample and
    /// multisample oscillators are single mono layers.
    #[serde(default)]
    pub mono: bool,
    /// Velocity response law for `amp_veltrack`; imports use [`VelocityCurve::Preset`].
    #[serde(default)]
    pub velocity_curve: VelocityCurve,
    #[serde(default)]
    pub pan: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sfz: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl Default for SamplerPatch {
    fn default() -> Self {
        Self {
            regions: Vec::new(),
            gain: 1.0,
            mono: false,
            velocity_curve: VelocityCurve::Linear,
            pan: 0.0,
            sfz: None,
            diagnostics: Vec::new(),
        }
    }
}

impl SamplerPatch {
    /// Longest release across regions.
    pub fn max_release_seconds(&self) -> f64 {
        self.regions
            .iter()
            .map(|region| region.ampeg.release)
            .fold(0.0, f64::max)
    }
}

// ============================================================================
// Decoding
// ============================================================================

#[derive(Debug)]
/// Decoded interleaved sample frames.
pub struct DecodedSample {
    channels: usize,
    rate: u32,
    /// Interleaved f32 frames.
    frames: Vec<f32>,
}

impl DecodedSample {
    /// Sample rate in Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Reverse frame order in place. Used by kit pads with `reverse: true`.
    pub fn reverse_in_place(&mut self) {
        let channels = self.channels.max(1);
        let frames = self.frame_count();
        for frame in 0..frames / 2 {
            let opposite = frames - 1 - frame;
            for channel in 0..channels {
                self.frames
                    .swap(frame * channels + channel, opposite * channels + channel);
            }
        }
    }

    /// Number of frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len().checked_div(self.channels).unwrap_or(0)
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels.max(1)
    }

    pub(crate) fn at(&self, frame: usize, channel: usize) -> f32 {
        self.frames
            .get(frame * self.channels + channel)
            .copied()
            .unwrap_or(0.0)
    }
}

/// Decode a sample file, choosing the decoder by extension.
pub fn decode_sample(path: &Path) -> Result<DecodedSample, String> {
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("wav" | "wave") => decode_wav(path),
        Some("flac") => decode_flac(path),
        // Extensionless and unusual names: sniff the first four magic bytes.
        _ if std::fs::File::open(path)
            .and_then(|mut file| {
                use std::io::Read;
                let mut magic = [0u8; 4];
                file.read_exact(&mut magic).map(|_| magic)
            })
            .map(|magic| &magic == b"fLaC")
            .unwrap_or(false) =>
        {
            decode_flac(path)
        }
        _ => decode_wav(path),
    }
}

fn sample_bound_error(path: &Path) -> String {
    format!(
        "sample '{}' exceeds the {MAX_DECODED_SAMPLES} sample memory bound",
        path.display()
    )
}

fn allocation_error(path: &Path) -> String {
    format!(
        "sample '{}' could not allocate its decoded buffer",
        path.display()
    )
}

/// Reserve the decoded-buffer hint. The header count is untrusted, so the
/// reservation is clamped and a failure is reported instead of aborting.
fn reserve_decoded(frames: &mut Vec<f32>, declared: usize, path: &Path) -> Result<(), String> {
    let hint = declared.min(MAX_DECODE_PREALLOC);
    frames
        .try_reserve_exact(hint)
        .map_err(|_| allocation_error(path))
}

#[inline]
fn push_decoded(frames: &mut Vec<f32>, sample: f32, path: &Path) -> Result<(), String> {
    if frames.len() >= MAX_DECODED_SAMPLES {
        return Err(sample_bound_error(path));
    }
    if frames.len() == frames.capacity() {
        let additional = frames
            .capacity()
            .clamp(MAX_DECODE_PREALLOC, MAX_DECODED_SAMPLES);
        frames
            .try_reserve(additional)
            .map_err(|_| allocation_error(path))?;
    }
    frames.push(sample);
    Ok(())
}

fn decode_wav(path: &Path) -> Result<DecodedSample, String> {
    let mut reader = oc_audio::wav::open_reader(path)
        .map_err(|error| format!("could not open WAV '{}': {error}", path.display()))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(format!("WAV '{}' has no channels", path.display()));
    }
    let channels = spec.channels as usize;
    let mut frames = Vec::new();
    reserve_decoded(&mut frames, reader.len() as usize, path)?;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                push_decoded(
                    &mut frames,
                    sample.map_err(|error| error.to_string())?,
                    path,
                )?;
            }
        }
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(format!(
                    "WAV '{}' has unsupported bit depth {}",
                    path.display(),
                    spec.bits_per_sample
                ));
            }
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            for sample in reader.samples::<i32>() {
                push_decoded(
                    &mut frames,
                    sample.map_err(|error| error.to_string())? as f32 * scale,
                    path,
                )?;
            }
        }
    }
    finish_decoded(frames, channels, spec.sample_rate, path)
}

fn decode_flac(path: &Path) -> Result<DecodedSample, String> {
    let mut reader = claxon::FlacReader::open(path)
        .map_err(|error| format!("could not open FLAC '{}': {error}", path.display()))?;
    let info = reader.streaminfo();
    if info.channels == 0 || info.bits_per_sample == 0 || info.bits_per_sample > 32 {
        return Err(format!(
            "FLAC '{}' has unsupported channel/bit depth",
            path.display()
        ));
    }
    let channels = info.channels as usize;
    let scale = 1.0 / (1i64 << (info.bits_per_sample - 1)) as f32;
    let mut frames = Vec::new();
    let declared = (info.samples.unwrap_or(0) as usize).saturating_mul(channels);
    reserve_decoded(&mut frames, declared, path)?;
    for sample in reader.samples() {
        push_decoded(
            &mut frames,
            sample.map_err(|error| error.to_string())? as f32 * scale,
            path,
        )?;
    }
    finish_decoded(frames, channels, info.sample_rate, path)
}

fn finish_decoded(
    mut frames: Vec<f32>,
    channels: usize,
    rate: u32,
    path: &Path,
) -> Result<DecodedSample, String> {
    if rate == 0 {
        return Err(format!("sample '{}' has no sample rate", path.display()));
    }
    if frames.len() > MAX_DECODED_SAMPLES {
        return Err(sample_bound_error(path));
    }
    if channels > 2 {
        // Downmix the first two channels; the sampler's public model is stereo.
        let mut stereo = Vec::with_capacity(frames.len() / channels * 2);
        for frame in frames.chunks_exact(channels) {
            stereo.push(frame[0]);
            stereo.push(frame[1]);
        }
        frames = stereo;
    }
    Ok(DecodedSample {
        channels: channels.min(2),
        rate,
        frames,
    })
}

// ============================================================================
// SFZ parsing
// ============================================================================

/// Symmetric unison position in -1..=1; one voice stays centred.
fn unison_position(index: u32, count: u32) -> f64 {
    if count <= 1 {
        0.0
    } else {
        index as f64 / (count - 1) as f64 * 2.0 - 1.0
    }
}

/// Every non-centre unison voice goes to one side; width is the pan amount.
fn unison_pan(position: f64, width: f64) -> f64 {
    let side = if position < 0.0 {
        -1.0
    } else if position > 0.0 {
        1.0
    } else {
        0.0
    };
    side * width
}

/// Per-voice gain for a sample-layer unison stack. Detuned voices are mutually
/// incoherent, so the stack's power is `N * g^2`; the incoherent normalisation
/// `g = 1 / sqrt(N)` keeps that power flat instead of letting the layer grow as
/// `N^0.68` (+16 dB at sixteen voices). See `docs/synth-levels.md`.
fn sample_unison_voice_gain(count: u32) -> f64 {
    if count <= 1 {
        1.0
    } else {
        f64::from(count).sqrt().recip()
    }
}

#[derive(Debug, Clone, Default)]
struct RawRegion {
    sample: Option<String>,
    key: Option<u8>,
    lokey: Option<u8>,
    hikey: Option<u8>,
    lovel: Option<u8>,
    hivel: Option<u8>,
    pitch_keycenter: Option<f64>,
    loop_start: Option<u64>,
    loop_end: Option<u64>,
    loop_mode: Option<LoopMode>,
    offset: Option<u64>,
    end: Option<u64>,
    tune: Option<f64>,
    volume: Option<f64>,
    amp_veltrack: Option<f64>,
    pan: Option<f64>,
    seq_position: Option<u32>,
    seq_length: Option<u32>,
    group: Option<u32>,
    off_by: Option<u32>,
    ampeg_attack: Option<f64>,
    ampeg_decay: Option<f64>,
    ampeg_sustain: Option<f64>,
    ampeg_release: Option<f64>,
    default_path: Option<String>,
}

impl RawRegion {
    /// Copy every field `parent` sets, so later sections override earlier ones:
    /// `<global>`, then `<group>`, then the `<region>` line.
    fn overlay(&mut self, parent: &RawRegion) {
        macro_rules! fill {
            ($($field:ident),+ $(,)?) => {$(
                if parent.$field.is_some() {
                    self.$field = parent.$field.clone();
                }
            )+};
        }
        fill!(
            sample,
            key,
            lokey,
            hikey,
            lovel,
            hivel,
            pitch_keycenter,
            loop_start,
            loop_end,
            loop_mode,
            offset,
            end,
            tune,
            volume,
            amp_veltrack,
            pan,
            seq_position,
            seq_length,
            group,
            off_by,
            ampeg_attack,
            ampeg_decay,
            ampeg_sustain,
            ampeg_release,
            default_path,
        );
    }
}

/// Which SFZ header the following opcode lines belong to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Control,
    Global,
    Group,
    Region,
}

/// Parse an embedded or sidecar SFZ document into a resolved patch.
///
/// `roots` are tried in order for relative sample paths. The first existing
/// file wins. Missing paths and unsupported opcodes become diagnostics.
pub fn patch_from_sfz_text(text: &str, roots: &[PathBuf]) -> SamplerPatch {
    let mut patch = SamplerPatch {
        sfz: Some(text.to_string()),
        ..SamplerPatch::default()
    };
    let mut global = RawRegion::default();
    let mut group = RawRegion::default();
    let mut control = RawRegion::default();
    let mut current: Option<RawRegion> = None;
    let mut section = Section::None;
    let mut diagnostics = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('<') {
            // SFZ opcodes normally continue on their own lines after a header.
            let target = match section {
                Section::Control => Some(&mut control),
                Section::Global => Some(&mut global),
                Section::Group => Some(&mut group),
                Section::Region => current.as_mut(),
                Section::None => None,
            };
            match target {
                Some(target) => apply_opcodes(line, target, &mut diagnostics, line_number),
                None => diagnostics.push(format!(
                    "line {line_number}: opcodes outside a header ignored"
                )),
            }
            continue;
        }
        let Some(end) = line.find('>') else {
            diagnostics.push(format!("line {line_number}: unterminated header ignored"));
            continue;
        };
        let header = line[1..end].trim().to_ascii_lowercase();
        let rest = &line[end + 1..];
        if let Some(region) = current.take() {
            finish_region(
                region,
                roots,
                control.default_path.as_deref(),
                &mut patch,
                &mut diagnostics,
                line_number,
            );
        }
        section = match header.as_str() {
            "control" => {
                // `<control>` only carries document defaults (sample paths).
                control = RawRegion::default();
                apply_opcodes(rest, &mut control, &mut diagnostics, line_number);
                Section::Control
            }
            "global" => {
                global = RawRegion::default();
                apply_opcodes(rest, &mut global, &mut diagnostics, line_number);
                Section::Global
            }
            "group" => {
                group = RawRegion::default();
                apply_opcodes(rest, &mut group, &mut diagnostics, line_number);
                Section::Group
            }
            "region" => {
                let mut region = RawRegion::default();
                region.overlay(&global);
                region.overlay(&group);
                region.sample = None;
                apply_opcodes(rest, &mut region, &mut diagnostics, line_number);
                current = Some(region);
                Section::Region
            }
            other => {
                diagnostics.push(format!(
                    "line {line_number}: unsupported header <{other}> ignored"
                ));
                Section::None
            }
        };
    }
    if let Some(region) = current {
        finish_region(
            region,
            roots,
            control.default_path.as_deref(),
            &mut patch,
            &mut diagnostics,
            usize::MAX,
        );
    }
    patch.diagnostics.extend(diagnostics);
    patch
}

/// Load a sidecar SFZ file. Relative sample paths resolve against the SFZ
/// directory first, then `extra_roots`.
pub fn patch_from_sfz_file(path: &Path, extra_roots: &[PathBuf]) -> Result<SamplerPatch, String> {
    let text = read_sfz(path)?;
    let mut roots = Vec::with_capacity(extra_roots.len() + 1);
    if let Some(parent) = path.parent() {
        roots.push(parent.to_path_buf());
    }
    roots.extend(extra_roots.iter().cloned());
    Ok(patch_from_sfz_text(&text, &roots))
}

fn read_sfz(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read SFZ '{}': {error}", path.display()))?;
    if bytes.len() > MAX_SFZ_BYTES {
        return Err(format!("SFZ '{}' exceeds 4 MiB", path.display()));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("SFZ '{}' is not UTF-8: {error}", path.display()))
}

fn finish_region(
    raw: RawRegion,
    roots: &[PathBuf],
    default_path: Option<&str>,
    patch: &mut SamplerPatch,
    diagnostics: &mut Vec<String>,
    line_number: usize,
) {
    let Some(raw_sample) = raw
        .sample
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        diagnostics.push(format!("line {line_number}: <region> has no sample="));
        return;
    };
    let Some(sample) = resolve_default_path(raw_sample, default_path, roots) else {
        diagnostics.push(format!(
            "line {line_number}: sample '{raw_sample}' was not found under any search root"
        ));
        return;
    };
    let key = raw.key;
    let lokey = raw.lokey.or(key).unwrap_or(0);
    let hikey = raw.hikey.or(key).unwrap_or(127);
    let lovel = raw.lovel.unwrap_or(0);
    let hivel = raw.hivel.unwrap_or(127);
    if lokey > hikey || lovel > hivel {
        diagnostics.push(format!(
            "line {line_number}: inverted key/velocity zone ignored"
        ));
        return;
    }
    let gain = raw.volume.map(|db| 10f64.powf(db / 20.0)).unwrap_or(1.0);
    let region = SampleRegion {
        sample,
        lokey,
        hikey,
        lovel,
        hivel,
        pitch_keycenter: raw
            .pitch_keycenter
            .or_else(|| key.map(f64::from))
            // libSFZ omits pitch_keycenter for single-key regions; using the
            // region key as the root avoids transposing those samples.
            .unwrap_or(60.0),
        loop_start: raw.loop_start,
        loop_end: raw.loop_end,
        loop_mode: raw.loop_mode.unwrap_or_default(),
        offset: raw.offset,
        end: raw.end,
        tune_cents: raw.tune.unwrap_or(0.0),
        gain,
        amp_veltrack: raw.amp_veltrack.unwrap_or(100.0).clamp(0.0, 100.0),
        pan: raw.pan.unwrap_or(0.0).clamp(-100.0, 100.0) / 100.0,
        seq_position: raw.seq_position,
        seq_length: raw.seq_length,
        group: raw.group,
        off_by: raw.off_by,
        unison: unison_default(),
        unison_detune_cents: 0.0,
        unison_width: 0.0,
        ampeg: AmpEnvelope {
            attack: raw.ampeg_attack.unwrap_or(0.0).max(0.0),
            decay: raw.ampeg_decay.unwrap_or(0.0).max(0.0),
            sustain: raw.ampeg_sustain.unwrap_or(1.0).clamp(0.0, 1.0),
            release: raw
                .ampeg_release
                .unwrap_or(DEFAULT_RELEASE_SECONDS)
                .max(0.0),
        },
    };
    patch.regions.push(region);
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' | '\'' if quote == Some(character) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(character),
            // A comment starts a token. libSFZ writes doubled separators inside
            // unquoted sample paths (`Kit//Kick.wav`), which are not comments.
            '/' if quote.is_none()
                && line[index..].starts_with("//")
                && (index == 0 || line[..index].trim_end().len() != index) =>
            {
                return &line[..index];
            }
            _ => {}
        }
    }
    line
}

fn apply_opcodes(
    text: &str,
    target: &mut RawRegion,
    diagnostics: &mut Vec<String>,
    line_number: usize,
) {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || text[index..].starts_with("//") {
            break;
        }
        let key_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'.' | b'-'))
        {
            index += 1;
        }
        if key_start == index {
            index += 1;
            continue;
        }
        let key = text[key_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            diagnostics.push(format!("line {line_number}: '{key}' is missing '='"));
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let value = if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
            let quote = bytes[index];
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            let value = text[start..index].to_string();
            if index < bytes.len() {
                index += 1;
            }
            value
        } else if matches!(key.as_str(), "sample" | "default_path") {
            // libSFZ writes unquoted paths containing spaces; consume until
            // the next `key=` token.
            let start = index;
            while index < bytes.len() {
                if bytes[index].is_ascii_whitespace() {
                    let mut next = index;
                    while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                        next += 1;
                    }
                    let mut end = next;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric()
                            || matches!(bytes[end], b'_' | b'.' | b'-'))
                    {
                        end += 1;
                    }
                    if end > next && end < bytes.len() && bytes[end] == b'=' {
                        index = next;
                        break;
                    }
                }
                index += 1;
            }
            text[start..index].trim().to_string()
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            text[start..index].to_string()
        };
        if let Err(message) = set_opcode(&key, &value, target) {
            diagnostics.push(format!("line {line_number}: {message}"));
        }
    }
}

fn set_opcode(key: &str, value: &str, region: &mut RawRegion) -> Result<(), String> {
    let numeric = |value: &str| -> Result<f64, String> {
        value
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("invalid number for '{key}': '{value}'"))
    };
    match key {
        "sample" => region.sample = Some(value.to_string()),
        "key" => region.key = Some(note_value(value, key)?),
        "lokey" => region.lokey = Some(note_value(value, key)?),
        "hikey" => region.hikey = Some(note_value(value, key)?),
        "lovel" => region.lovel = Some(velocity_value(value, key)?),
        "hivel" => region.hivel = Some(velocity_value(value, key)?),
        "pitch_keycenter" => region.pitch_keycenter = Some(note_number(value, key)?),
        "loop_start" => region.loop_start = Some(frame_value(value, key)?),
        "loop_end" => region.loop_end = Some(frame_value(value, key)?),
        "offset" => region.offset = Some(frame_value(value, key)?),
        "end" => region.end = Some(frame_value(value, key)?),
        "loop_mode" => {
            region.loop_mode = Some(match value.trim().to_ascii_lowercase().as_str() {
                "no_loop" => LoopMode::NoLoop,
                "one_shot" => LoopMode::OneShot,
                "loop_continuous" => LoopMode::LoopContinuous,
                "loop_sustain" => LoopMode::LoopSustain,
                "loop_pingpong" | "pingpong" => LoopMode::LoopPingPong,
                other => return Err(format!("unsupported loop_mode '{other}'")),
            })
        }
        "tune" => region.tune = Some(numeric(value)?),
        "volume" => region.volume = Some(numeric(value)?),
        "amp_veltrack" => region.amp_veltrack = Some(numeric(value)?),
        "pan" => region.pan = Some(numeric(value)?),
        "seq_position" => region.seq_position = Some(frame_value(value, key)? as u32),
        "seq_length" => region.seq_length = Some(frame_value(value, key)? as u32),
        "group" => region.group = Some(frame_value(value, key)? as u32),
        "off_by" => region.off_by = Some(frame_value(value, key)? as u32),
        "ampeg_attack" => region.ampeg_attack = Some(numeric(value)?),
        "ampeg_decay" => region.ampeg_decay = Some(numeric(value)?),
        "ampeg_sustain" => region.ampeg_sustain = Some(numeric(value)?),
        "ampeg_release" => region.ampeg_release = Some(numeric(value)?),
        "default_path" => region.default_path = Some(value.trim().to_string()),
        _ => return Err(format!("unsupported opcode '{key}' ignored")),
    }
    Ok(())
}

fn note_value(value: &str, key: &str) -> Result<u8, String> {
    note_number(value, key).map(|note| note.round().clamp(0.0, 127.0) as u8)
}

fn velocity_value(value: &str, key: &str) -> Result<u8, String> {
    value
        .trim()
        .parse::<f64>()
        .map(|value| value.round().clamp(0.0, 127.0) as u8)
        .map_err(|_| format!("invalid number for '{key}': '{value}'"))
}

fn frame_value(value: &str, key: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<f64>()
        .map(|value| value.max(0.0).round() as u64)
        .map_err(|_| format!("invalid frame number for '{key}': '{value}'"))
}

/// Parse either a MIDI number or a scientific note name (`C4`, `A#3`, `Bb2`).
fn note_number(value: &str, key: &str) -> Result<f64, String> {
    let value = value.trim();
    if let Ok(number) = value.parse::<f64>() {
        return Ok(number);
    }
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(format!("invalid note for '{key}': '{value}'"));
    }
    let semitone = match bytes[0].to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return Err(format!("invalid note for '{key}': '{value}'")),
    };
    let mut index = 1usize;
    let mut accidental = 0i32;
    if index < bytes.len() {
        match bytes[index] {
            b'#' => {
                accidental = 1;
                index += 1;
            }
            b'b' => {
                accidental = -1;
                index += 1;
            }
            _ => {}
        }
    }
    let octave = value[index..]
        .parse::<i32>()
        .map_err(|_| format!("invalid note octave for '{key}': '{value}'"))?;
    Ok(((octave + 1) * 12 + semitone + accidental) as f64)
}

/// Resolve a region sample, trying the `<control> default_path` prefix first.
///
/// Imported SFZ files keep bare file names and put the directory in
/// `<control>`; libSFZ-written files usually inline the directory instead.
fn resolve_default_path(
    raw: &str,
    default_path: Option<&str>,
    roots: &[PathBuf],
) -> Option<PathBuf> {
    let Some(default_path) = default_path.map(str::trim).filter(|path| !path.is_empty()) else {
        return resolve_sample_path(raw, roots);
    };
    if Path::new(raw.trim().trim_matches(['"', '\''])).is_absolute() {
        return resolve_sample_path(raw, roots);
    }
    let joined = format!("{}/{raw}", default_path.trim_end_matches(['/', '\\']));
    resolve_sample_path(&joined, roots).or_else(|| resolve_sample_path(raw, roots))
}

/// Resolve a project-relative sample path against the roots.
pub fn resolve_sample_path(raw: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let cleaned = raw.trim().trim_matches(['"', '\'']);
    if cleaned.is_empty() {
        return None;
    }
    let normalized = cleaned.replace('\\', "/");
    // Collapse the doubled separators libSFZ emits (`samples/uuid//file`).
    let mut collapsed = String::with_capacity(normalized.len());
    let mut previous_slash = false;
    for character in normalized.chars() {
        if character == '/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        collapsed.push(character);
    }
    let path = Path::new(&collapsed);
    if path.is_absolute() && path.exists() {
        return Some(path.to_path_buf());
    }
    // A Windows drive path cannot be absolute on this host. Try it relative to
    // the search roots after removing the drive prefix.
    let relative = collapsed
        .split_once(':')
        .map(|(_, rest)| rest.trim_start_matches('/'))
        .unwrap_or(collapsed.as_str());
    for root in roots {
        let candidate = root.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if path.exists() {
        return Some(path.to_path_buf());
    }
    None
}

// ============================================================================
// Parameter entry points
// ============================================================================

/// Build a patch from `builtin.sampler` project parameters.
///
/// Recognised keys: `sampler.sfz_file`, `sampler.sfz_text`,
/// `sampler.sample_file`, `sampler.root_note`, `sampler.loop_start`,
/// `sampler.loop_end`, `sampler.loop_mode`, `sampler.gain`, `sampler.pan`,
/// and `sampler.root` (an extra sample search directory).
pub fn patch_from_params(
    params: &[(String, ParamValue)],
    root: Option<&Path>,
) -> Result<SamplerPatch, String> {
    let string = |key: &str| -> Option<&str> {
        params.iter().rev().find_map(|(name, value)| {
            if name == key
                && let ParamValue::String(text) = value
            {
                return Some(text.as_str());
            }
            None
        })
    };
    let number = |key: &str| -> Option<f64> {
        params
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_f64())
    };
    let resolve = |path: &str| -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            root.map(|root| root.join(&path)).unwrap_or(path)
        }
    };

    let mut extra_roots = Vec::new();
    if let Some(extra) = string("sampler.root") {
        extra_roots.push(resolve(extra));
    }

    let mut patch = if let Some(text) = string("sampler.sfz_text") {
        let mut roots = Vec::new();
        if let Some(root) = root {
            roots.push(root.to_path_buf());
        }
        roots.extend(extra_roots.iter().cloned());
        patch_from_sfz_text(text, &roots)
    } else if let Some(file) = string("sampler.sfz_file") {
        let path = resolve(file);
        patch_from_sfz_file(&path, &extra_roots)?
    } else if let Some(file) = string("sampler.sample_file") {
        let sample = resolve(file);
        if !sample.exists() {
            return Err(format!(
                "builtin.sampler sample '{}' does not exist",
                sample.display()
            ));
        }
        let root_note = number("sampler.root_note").unwrap_or(60.0);
        let loop_start = number("sampler.loop_start").map(|value| value.max(0.0) as u64);
        let loop_end = number("sampler.loop_end").map(|value| value.max(0.0) as u64);
        let loop_mode = match string("sampler.loop_mode").unwrap_or("no_loop") {
            "one_shot" => LoopMode::OneShot,
            "loop_continuous" => LoopMode::LoopContinuous,
            "loop_sustain" => LoopMode::LoopSustain,
            "loop_pingpong" | "pingpong" => LoopMode::LoopPingPong,
            _ => LoopMode::NoLoop,
        };
        SamplerPatch {
            regions: vec![SampleRegion {
                sample,
                lokey: 0,
                hikey: 127,
                lovel: 0,
                hivel: 127,
                pitch_keycenter: root_note,
                loop_start,
                loop_end,
                loop_mode,
                offset: None,
                end: None,
                tune_cents: 0.0,
                gain: 1.0,
                amp_veltrack: 100.0,
                pan: 0.0,
                seq_position: None,
                seq_length: None,
                group: None,
                off_by: None,
                unison: 1,
                unison_detune_cents: 0.0,
                unison_width: 0.0,
                ampeg: AmpEnvelope::default(),
            }],
            sfz: None,
            ..SamplerPatch::default()
        }
    } else {
        return Err(
            "builtin.sampler requires sampler.sfz_file, sampler.sfz_text, or sampler.sample_file"
                .into(),
        );
    };
    patch.gain = number("sampler.gain").unwrap_or(1.0).max(0.0);
    patch.pan = number("sampler.pan").unwrap_or(0.0).clamp(-1.0, 1.0);
    Ok(patch)
}

/// Longest release tail declared by a `builtin.sampler` preset, capped by the
/// renderer's global plugin-tail bound.
pub fn params_tail_seconds(params: &[(String, ParamValue)], root: Option<&Path>) -> Option<f64> {
    let patch = patch_from_params(params, root).ok()?;
    Some(patch.max_release_seconds().min(30.0))
}

// ============================================================================
// Runtime
// ============================================================================

struct RuntimeRegion {
    sample: Arc<DecodedSample>,
    lokey: u8,
    hikey: u8,
    lovel: u8,
    hivel: u8,
    root: f64,
    tune_cents: f64,
    gain: f64,
    pan: f64,
    amp_veltrack: f64,
    loop_start: u64,
    loop_end: u64,
    start: u64,
    end: u64,
    loop_mode: LoopMode,
    group: Option<u32>,
    off_by: Option<u32>,
    seq_position: Option<u32>,
    seq_length: Option<u32>,
    unison: u32,
    unison_detune_cents: f64,
    unison_width: f64,
    envelope: AmpEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvStage {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Stereo output of the sample voices owned by one OpenSynth voice, summed so
/// a note covering several SFZ regions reaches its voice filter only once.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnedSample {
    /// Owning synth voice id; `0` for the standalone sampler instrument.
    pub owner: u64,
    pub left: f64,
    pub right: f64,
}

struct SampleVoice {
    channel: u8,
    note: u8,
    region: usize,
    /// Synth voice that started this sample voice, or `0` when unowned.
    owner: u64,
    /// Note-to-root and tuning ratio. Set at note-on.
    note_ratio: f64,
    /// Pitch-bend ratio and asset/output rate scale; refreshed when either
    /// changes so `next_stereo` never calls `powf`.
    bend_ratio: f64,
    rate_scale: f64,
    position: f64,
    /// Ping-pong direction: `true` scans the loop backwards.
    reverse: bool,
    /// Extra pan from this voice's position in the unison stack.
    unison_pan: f64,
    velocity_gain: f64,
    envelope: AmpEnvelope,
    env_level: f64,
    env_stage: EnvStage,
    release_step: f64,
    key_down: bool,
    group: Option<u32>,
    age: u64,
    finished: bool,
}

impl SampleVoice {
    fn advance_envelope(&mut self, sample_rate: f64) -> bool {
        match self.env_stage {
            EnvStage::Attack => {
                if self.envelope.attack <= 0.0 {
                    self.env_level = 1.0;
                    self.env_stage = EnvStage::Decay;
                } else {
                    self.env_level += 1.0 / (self.envelope.attack * sample_rate);
                    if self.env_level >= 1.0 {
                        self.env_level = 1.0;
                        self.env_stage = EnvStage::Decay;
                    }
                }
            }
            EnvStage::Decay => {
                if self.envelope.decay <= 0.0 || self.envelope.sustain >= 1.0 {
                    self.env_level = self.envelope.sustain;
                    self.env_stage = EnvStage::Sustain;
                } else {
                    self.env_level -=
                        (1.0 - self.envelope.sustain) / (self.envelope.decay * sample_rate);
                    if self.env_level <= self.envelope.sustain {
                        self.env_level = self.envelope.sustain;
                        self.env_stage = EnvStage::Sustain;
                    }
                }
            }
            EnvStage::Sustain => {}
            EnvStage::Release => {
                self.env_level -= self.release_step;
                if self.env_level <= 0.0 {
                    self.env_level = 0.0;
                    self.finished = true;
                }
            }
        }
        !self.finished
    }

    fn release(&mut self, sample_rate: f64) {
        if self.finished || self.env_stage == EnvStage::Release {
            return;
        }
        self.key_down = false;
        self.env_stage = EnvStage::Release;
        self.release_step = if self.envelope.release <= 0.0 {
            self.env_level
        } else {
            self.env_level / (self.envelope.release * sample_rate)
        };
        if self.release_step <= 0.0 {
            self.finished = true;
        }
    }
}

/// Decoded, preallocated sampler. All heap growth happens in
/// [`Sampler::from_patch`], never in [`Sampler::next_stereo`].
pub struct Sampler {
    regions: Vec<RuntimeRegion>,
    voices: Vec<SampleVoice>,
    gain: f64,
    mono: bool,
    velocity_curve: VelocityCurve,
    pan: f64,
    sample_rate: u32,
    pitch_bend: [f64; 16],
    sustain: [bool; 16],
    round_robin: u64,
    age: u64,
}

impl Sampler {
    /// Build a sampler from a resolved patch, decoding each region.
    pub fn from_patch(patch: &SamplerPatch) -> Result<Self, String> {
        for diagnostic in &patch.diagnostics {
            log::warn!("builtin.sampler: {diagnostic}");
        }
        let mut decoded: HashMap<PathBuf, Arc<DecodedSample>> = HashMap::new();
        let mut regions = Vec::with_capacity(patch.regions.len());
        for spec in &patch.regions {
            if spec.lokey > spec.hikey || spec.lovel > spec.hivel {
                log::warn!(
                    "builtin.sampler: region '{}' has an inverted zone; skipped",
                    spec.sample.display()
                );
                continue;
            }
            let sample = match decoded.get(&spec.sample) {
                Some(sample) => Arc::clone(sample),
                None => match decode_sample(&spec.sample) {
                    Ok(sample) => {
                        let sample = Arc::new(sample);
                        decoded.insert(spec.sample.clone(), Arc::clone(&sample));
                        sample
                    }
                    Err(error) => {
                        log::warn!(
                            "builtin.sampler: could not decode '{}': {error}",
                            spec.sample.display()
                        );
                        continue;
                    }
                },
            };
            let frames = sample.frame_count() as u64;
            if frames == 0 {
                log::warn!(
                    "builtin.sampler: sample '{}' is empty; skipped",
                    spec.sample.display()
                );
                continue;
            }
            let end = spec.end.unwrap_or(frames).min(frames);
            let mut loop_start = spec.loop_start.unwrap_or(0).min(end.saturating_sub(1));
            let mut loop_end = spec.loop_end.unwrap_or(end).min(end);
            if loop_end <= loop_start {
                loop_start = 0;
                loop_end = 0;
            }
            let start = spec.offset.unwrap_or(0).min(end.saturating_sub(1));
            regions.push(RuntimeRegion {
                sample,
                lokey: spec.lokey,
                hikey: spec.hikey,
                lovel: spec.lovel,
                hivel: spec.hivel,
                root: spec.pitch_keycenter,
                tune_cents: spec.tune_cents,
                gain: spec.gain,
                pan: spec.pan.clamp(-1.0, 1.0),
                amp_veltrack: spec.amp_veltrack.clamp(0.0, 100.0),
                loop_start,
                loop_end,
                start,
                end,
                loop_mode: spec.loop_mode,
                group: spec.group,
                off_by: spec.off_by,
                seq_position: spec.seq_position,
                seq_length: spec.seq_length,
                unison: spec.unison.max(1),
                unison_detune_cents: spec.unison_detune_cents.max(0.0),
                unison_width: spec.unison_width.clamp(0.0, 1.0),
                envelope: spec.ampeg,
            });
        }
        if regions.is_empty() {
            return Err("sampler patch has no playable regions".into());
        }
        Ok(Self {
            regions,
            voices: Vec::with_capacity(MAX_VOICES),
            gain: patch.gain.max(0.0),
            mono: patch.mono,
            velocity_curve: patch.velocity_curve,
            pan: patch.pan.clamp(-1.0, 1.0),
            sample_rate: 0,
            pitch_bend: [0.0; 16],
            sustain: [false; 16],
            round_robin: 0,
            age: 0,
        })
    }

    /// Build a sampler from preset parameters and an optional root.
    pub fn from_params(
        params: &[(String, ParamValue)],
        root: Option<&Path>,
    ) -> Result<Self, String> {
        Self::from_patch(&patch_from_params(params, root)?)
    }

    /// Whether any voice is still sounding.
    pub fn is_active(&self) -> bool {
        self.voices.iter().any(|voice| !voice.finished)
    }

    /// Start a note on a channel.
    pub fn note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        self.note_on_owned(channel, note, velocity, 0);
    }

    /// Start voices attributed to `owner` so the synth can filter them with the
    /// owning voice's filter and amp envelope.
    pub fn note_on_owned(&mut self, channel: u8, note: u8, velocity: u8, owner: u64) {
        if velocity == 0 {
            self.note_off(channel, note);
            return;
        }
        let channel = channel.min(15);
        let velocity_gain = velocity as f64 / 127.0;
        self.round_robin = self.round_robin.wrapping_add(1);
        let round_robin = self.round_robin;
        let mut candidates = [0usize; MAX_VOICES];
        let mut candidate_count = 0usize;
        let mut max_sequence = 0u32;
        for (index, region) in self.regions.iter().enumerate() {
            if note < region.lokey || note > region.hikey {
                continue;
            }
            if velocity < region.lovel || velocity > region.hivel {
                continue;
            }
            if let Some(sequence) = region.seq_position {
                max_sequence = max_sequence.max(sequence);
            }
            if let Some(length) = region.seq_length {
                max_sequence = max_sequence.max(length);
            }
            if candidate_count < candidates.len() {
                candidates[candidate_count] = index;
                candidate_count += 1;
            }
        }
        if candidate_count == 0 {
            return;
        }
        let selected_sequence = if max_sequence > 1 {
            Some((round_robin % u64::from(max_sequence)) as u32 + 1)
        } else {
            None
        };
        for &index in &candidates[..candidate_count] {
            let region = &self.regions[index];
            if let (Some(selected), Some(sequence)) = (selected_sequence, region.seq_position)
                && sequence != selected
            {
                continue;
            }
            self.start_voice(channel, note, velocity_gain, index, owner);
        }
    }

    fn start_voice(&mut self, channel: u8, note: u8, velocity_gain: f64, index: usize, owner: u64) {
        let region = &self.regions[index];
        let velocity = match self.velocity_curve {
            VelocityCurve::Linear => 1.0 - (1.0 - velocity_gain) * (region.amp_veltrack / 100.0),
            VelocityCurve::Preset => velocity_gain.powf(2.0 * region.amp_veltrack / 100.0),
        };
        let base_ratio =
            2f64.powf((f64::from(note) - region.root + region.tune_cents / 100.0) / 12.0);
        let envelope = region.envelope;
        let group = region.group;
        let off_by = region.off_by;
        let asset_rate = region.sample.rate;
        let start = region.start as f64;
        let voices = region.unison.clamp(1, MAX_UNISON_VOICES);
        let detune = region.unison_detune_cents;
        let width = region.unison_width;
        if let Some(off_by) = off_by {
            self.voices
                .retain(|voice| voice.group != Some(off_by) || voice.finished);
        }
        let unison_gain = sample_unison_voice_gain(voices);
        for voice_index in 0..voices {
            let position = unison_position(voice_index, voices);
            let note_ratio = base_ratio * 2f64.powf(position * detune / 1200.0);
            self.age = self.age.wrapping_add(1);
            let mut voice = SampleVoice {
                channel,
                note,
                region: index,
                owner,
                note_ratio,
                bend_ratio: 1.0,
                rate_scale: 1.0,
                position: start,
                reverse: false,
                unison_pan: unison_pan(position, width),
                velocity_gain: velocity * unison_gain,
                envelope,
                env_level: 0.0,
                env_stage: EnvStage::Attack,
                release_step: 0.0,
                key_down: true,
                group,
                age: self.age,
                finished: false,
            };
            voice.update_playback(
                asset_rate,
                self.sample_rate,
                self.pitch_bend[channel as usize],
            );
            if self.voices.len() < MAX_VOICES {
                self.voices.push(voice);
                continue;
            }
            // Steal the oldest voice without growing the preallocated vector.
            if let Some(oldest) = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, voice)| voice.age)
                .map(|(index, _)| index)
            {
                self.voices[oldest] = voice;
            }
        }
    }

    /// Release a note on a channel.
    pub fn note_off(&mut self, channel: u8, note: u8) {
        let channel = channel.min(15);
        let sustain = self.sustain[channel as usize];
        for voice in &mut self.voices {
            if voice.channel != channel || voice.note != note || !voice.key_down {
                continue;
            }
            let one_shot = self.regions[voice.region].loop_mode == LoopMode::OneShot;
            if one_shot {
                // SFZ one-shot regions ignore note-off and play to the end.
                continue;
            }
            if sustain {
                voice.key_down = false;
            } else {
                voice.release(f64::from(self.sample_rate.max(1)));
            }
        }
    }

    /// Set a channel pitch bend in the MIDI -8192..8191 range.
    pub fn set_pitch_bend(&mut self, channel: u8, value: i16) {
        let channel = channel.min(15);
        let channel_index = channel as usize;
        self.pitch_bend[channel_index] = value as f64 / 8192.0 * 2.0;
        let regions = &self.regions;
        let sample_rate = self.sample_rate;
        let bend = self.pitch_bend[channel_index];
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| voice.channel == channel && !voice.finished)
        {
            voice.update_playback(regions[voice.region].sample.rate, sample_rate, bend);
        }
    }

    /// Set a channel's sustain pedal.
    pub fn set_sustain(&mut self, channel: u8, enabled: bool) {
        let channel = channel.min(15);
        let was_enabled = self.sustain[channel as usize];
        self.sustain[channel as usize] = enabled;
        if was_enabled && !enabled {
            let sample_rate = f64::from(self.sample_rate.max(1));
            for voice in &mut self.voices {
                if voice.channel == channel && !voice.key_down {
                    voice.release(sample_rate);
                }
            }
        }
    }

    /// Return to the state a freshly constructed sampler has. The decoded
    /// samples stay loaded; only playback state and controllers reset.
    pub fn reset(&mut self) {
        self.voices.clear();
        self.sample_rate = 0;
        self.pitch_bend = [0.0; 16];
        self.sustain = [false; 16];
        self.round_robin = 0;
        self.age = 0;
    }

    /// Release every voice on a channel.
    pub fn all_notes_off(&mut self, channel: u8) {
        let channel = channel.min(15);
        let sample_rate = f64::from(self.sample_rate.max(1));
        for voice in &mut self.voices {
            if voice.channel == channel {
                voice.release(sample_rate);
            }
        }
    }

    /// Stop every voice on a channel immediately.
    pub fn all_sound_off(&mut self, channel: u8) {
        let channel = channel.min(15);
        self.voices.retain(|voice| voice.channel != channel);
    }

    /// Dispatch one MIDI message.
    pub fn send_midi(&mut self, event: &crate::MidiMessage) {
        use crate::MidiMessage;
        match event {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => self.note_on(*channel, *note, *velocity),
            MidiMessage::NoteOff { channel, note } => self.note_off(*channel, *note),
            MidiMessage::PitchBend { channel, value } => self.set_pitch_bend(*channel, *value),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => match controller {
                64 => self.set_sustain(*channel, *value >= 64),
                120 => self.all_sound_off(*channel),
                123 => self.all_notes_off(*channel),
                _ => {}
            },
            _ => {}
        }
    }

    fn ensure_sample_rate(&mut self, sample_rate: u32) {
        if sample_rate == 0 || sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        for voice in &mut self.voices {
            if voice.finished {
                continue;
            }
            let rate = self.regions[voice.region].sample.rate;
            voice.update_playback(rate, sample_rate, self.pitch_bend[voice.channel as usize]);
        }
    }

    /// Render the next stereo frame at the sample rate.
    pub fn next_stereo(&mut self, sample_rate: u32) -> [f32; 2] {
        self.ensure_sample_rate(sample_rate);
        let rate = f64::from(sample_rate.max(1));
        let gain = self.gain;
        let pan = self.pan;
        let mut left = 0.0;
        let mut right = 0.0;
        for voice in &mut self.voices {
            let Some((voice_left, voice_right)) =
                render_sample_voice(voice, &self.regions, rate, gain, pan, self.mono)
            else {
                continue;
            };
            left += voice_left;
            right += voice_right;
        }
        self.voices.retain(|voice| !voice.finished);
        [left as f32, right as f32]
    }

    /// Render one frame with voices grouped by owning synth voice. `output` is
    /// reserved by the caller, so this never allocates.
    pub fn next_owned_stereo(&mut self, sample_rate: u32, output: &mut Vec<OwnedSample>) -> usize {
        self.ensure_sample_rate(sample_rate);
        let rate = f64::from(sample_rate.max(1));
        let gain = self.gain;
        let pan = self.pan;
        output.clear();
        for voice in &mut self.voices {
            let Some((left, right)) =
                render_sample_voice(voice, &self.regions, rate, gain, pan, self.mono)
            else {
                continue;
            };
            let owner = voice.owner;
            match output.iter().position(|slot| slot.owner == owner) {
                Some(index) => {
                    output[index].left += left;
                    output[index].right += right;
                }
                None => {
                    if output.len() < output.capacity() {
                        output.push(OwnedSample { owner, left, right });
                    }
                }
            }
        }
        self.voices.retain(|voice| !voice.finished);
        output.len()
    }

    /// Drop owned voices whose synth voice is gone. Unowned voices are kept.
    pub fn retain_owners(&mut self, live: &[u64]) {
        self.voices
            .retain(|voice| voice.owner == 0 || live.contains(&voice.owner));
    }

    /// Render an interleaved stereo block. Only the output buffer is allocated.
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
}

impl SampleVoice {
    fn update_playback(&mut self, asset_rate: u32, output_rate: u32, bend_semitones: f64) {
        self.bend_ratio = 2f64.powf(bend_semitones / 12.0);
        self.rate_scale = f64::from(asset_rate) / f64::from(output_rate.max(1));
    }

    fn increment(&self) -> f64 {
        self.note_ratio * self.bend_ratio * self.rate_scale
    }
}

/// Advance and read one sample voice. `None` once the voice is finished.
fn render_sample_voice(
    voice: &mut SampleVoice,
    regions: &[RuntimeRegion],
    sample_rate: f64,
    gain: f64,
    pan: f64,
    mono: bool,
) -> Option<(f64, f64)> {
    if voice.finished || !voice.advance_envelope(sample_rate) {
        return None;
    }
    let region = &regions[voice.region];
    let loop_length = region.loop_end.saturating_sub(region.loop_start);
    let looping = loop_length > 0
        && match region.loop_mode {
            LoopMode::NoLoop | LoopMode::OneShot => false,
            LoopMode::LoopContinuous | LoopMode::LoopPingPong => true,
            LoopMode::LoopSustain => voice.key_down,
        };
    if voice.position >= region.end as f64 {
        voice.finished = true;
        return None;
    }
    if looping {
        if region.loop_mode == LoopMode::LoopPingPong {
            // Reflect at each edge instead of wrapping, so the scan keeps the
            // edge sample and reverses without a click.
            let start = region.loop_start as f64;
            let end = region.loop_end as f64;
            if voice.reverse && voice.position <= start {
                voice.position = start + (start - voice.position);
                voice.reverse = false;
            } else if !voice.reverse && voice.position >= end {
                voice.position = end - (voice.position - end);
                voice.reverse = true;
            }
        } else if voice.position >= region.loop_end as f64 {
            let over = voice.position - region.loop_end as f64;
            voice.position = region.loop_start as f64 + over % loop_length as f64;
        }
    }
    let (sample_left, sample_right) = read_stereo(&region.sample, voice.position);
    let (sample_left, sample_right) = if mono {
        let sum = sample_left + sample_right;
        (sum, sum)
    } else {
        (sample_left, sample_right)
    };
    let step = voice.increment();
    voice.position += if voice.reverse { -step } else { step };
    let gain = region.gain * voice.velocity_gain * voice.env_level * gain;
    // Region and patch pan use the balance law (centre is unity). The unison
    // spread uses the pan law instead, so a hard-panned stack keeps the mono
    // sum and the reference's measured unison level.
    let pan = (region.pan + pan).clamp(-1.0, 1.0);
    let (balance_left, balance_right) = (1.0 - pan.max(0.0), 1.0 + pan.min(0.0));
    let (unison_left, unison_right) = (1.0 - voice.unison_pan, 1.0 + voice.unison_pan);
    Some((
        sample_left * gain * balance_left * unison_left,
        sample_right * gain * balance_right * unison_right,
    ))
}

/// Linearly interpolate a decoded sample at `position`.
pub fn read_stereo(sample: &DecodedSample, position: f64) -> (f64, f64) {
    let position = position.max(0.0);
    let frame = position.floor() as usize;
    let next = frame + 1;
    let fraction = position - frame as f64;
    let last = sample.frame_count().saturating_sub(1);
    let frame = frame.min(last);
    let next = next.min(last);
    let channel = |index: usize| sample.at(index, 0) as f64;
    let left = channel(frame) * (1.0 - fraction) + channel(next) * fraction;
    if sample.channels <= 1 {
        (left, left)
    } else {
        let right =
            sample.at(frame, 1) as f64 * (1.0 - fraction) + sample.at(next, 1) as f64 * fraction;
        (left, right)
    }
}

// ============================================================================
// Offline rendering helpers
// ============================================================================

/// Render a `builtin.sampler` track from already-interpreted MIDI events.
pub fn try_render_track(
    params: &[(String, ParamValue)],
    events: &[&crate::MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    root: Option<&Path>,
) -> Result<Vec<f32>, String> {
    if sample_rate == 0 || total_samples.checked_mul(2).is_none() {
        return Err("invalid sampler render size".into());
    }
    let mut sampler = Sampler::from_params(params, root)?;
    let mut output = vec![0.0; total_samples * 2];
    let mut event_index = 0usize;
    for frame in 0..total_samples {
        while event_index < events.len() && events[event_index].time_samples as usize <= frame {
            sampler.send_midi(&events[event_index].event);
            event_index += 1;
        }
        let [left, right] = sampler.next_stereo(sample_rate);
        output[frame * 2] = left;
        output[frame * 2 + 1] = right;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "opencompose-sampler-{}-{label}",
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

    /// Write a 16-bit mono WAV with one sine cycle per second `frequency`.
    fn write_tone(path: &Path, frequency: f64, frames: usize, amplitude: f32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let amplitude = f64::from(amplitude);
        for frame in 0..frames {
            let value = (std::f64::consts::TAU * frequency * frame as f64 / 44_100.0).sin();
            writer
                .write_sample((value * amplitude * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn sampler_reads_padded_wav_without_changing_samples() {
        let directory = TempDir::new("padded-wav");
        let path = directory.join("tone.wav");
        write_tone(&path, 440.0, 32, 0.25);
        let expected = decode_sample(&path).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes.splice(12..12, *b"JUNK\x01\0\0\0A\0");
        let size = (bytes.len() as u32 - 8).to_le_bytes();
        bytes[4..8].copy_from_slice(&size);
        std::fs::write(&path, &bytes).unwrap();

        let actual = decode_sample(&path).unwrap();
        assert_eq!(actual.channels, expected.channels);
        assert_eq!(actual.rate, expected.rate);
        assert_eq!(actual.frames, expected.frames);
    }

    fn zero_crossing_hz(samples: &[f32], sample_rate: u32) -> f64 {
        let mut crossings = 0usize;
        for pair in samples.windows(2) {
            if pair[0] <= 0.0 && pair[1] > 0.0 {
                crossings += 1;
            }
        }
        crossings as f64 * sample_rate as f64 / samples.len() as f64
    }

    /// Write a 16-bit stereo WAV with a constant level on both channels.
    fn write_stereo_dc(path: &Path, frames: usize, amplitude: f32) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let value = (f64::from(amplitude) * f64::from(i16::MAX)) as i16;
        for _ in 0..frames {
            writer.write_sample(value).unwrap();
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn imported_mono_layers_sum_stereo_channels_at_unity() {
        let dir = TempDir::new("mono-layer");
        let path = dir.0.join("stereo.wav");
        write_stereo_dc(&path, 4_096, 0.25);
        let render = |mono: bool| {
            let mut patch = patch_with_regions(vec![region(path.clone(), 60, 60)]);
            patch.mono = mono;
            let mut sampler = Sampler::from_patch(&patch).unwrap();
            sampler.note_on(0, 60, 100);
            sampler.next_stereo(48_000)
        };
        let stereo = render(false);
        let mono = render(true);
        assert!((stereo[0] - stereo[1]).abs() < 1e-9);
        assert!((mono[0] - 2.0 * stereo[0]).abs() < 1e-4);
    }

    fn patch_with_regions(regions: Vec<SampleRegion>) -> SamplerPatch {
        SamplerPatch {
            regions,
            ..SamplerPatch::default()
        }
    }

    fn region(sample: PathBuf, lokey: u8, hikey: u8) -> SampleRegion {
        SampleRegion {
            sample,
            lokey,
            hikey,
            lovel: 0,
            hivel: 127,
            pitch_keycenter: 60.0,
            loop_start: None,
            loop_end: None,
            loop_mode: LoopMode::NoLoop,
            offset: None,
            end: None,
            tune_cents: 0.0,
            gain: 1.0,
            amp_veltrack: 0.0,
            pan: 0.0,
            seq_position: None,
            seq_length: None,
            group: None,
            off_by: None,
            unison: 1,
            unison_detune_cents: 0.0,
            unison_width: 0.0,
            ampeg: AmpEnvelope {
                attack: 0.0,
                decay: 0.0,
                sustain: 1.0,
                release: 0.0,
            },
        }
    }

    fn render_note(sampler: &mut Sampler, note: u8, velocity: u8, frames: usize) -> Vec<f32> {
        sampler.note_on(0, note, velocity);
        let mut output = Vec::with_capacity(frames);
        for _ in 0..frames {
            let [left, _] = sampler.next_stereo(44_100);
            output.push(left);
        }
        output
    }

    #[test]
    fn selects_key_zones_and_pitch_follows_the_root() {
        let dir = TempDir::new("zones");
        let low = dir.join("low.wav");
        let high = dir.join("high.wav");
        write_tone(&low, 220.0, 44_100, 0.8);
        write_tone(&high, 440.0, 44_100, 0.8);
        let mut low_region = region(low, 0, 59);
        low_region.pitch_keycenter = 48.0;
        let high_region = region(high, 60, 127);
        let patch = patch_with_regions(vec![low_region, high_region]);
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let low_output = render_note(&mut sampler, 48, 100, 22_050);
        let high_output = render_note(&mut sampler, 60, 100, 22_050);
        assert!((zero_crossing_hz(&low_output, 44_100) - 220.0).abs() < 3.0);
        assert!((zero_crossing_hz(&high_output, 44_100) - 440.0).abs() < 3.0);

        // One octave above the root doubles the playback frequency.
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let octave = render_note(&mut sampler, 72, 100, 22_050);
        assert!((zero_crossing_hz(&octave, 44_100) - 880.0).abs() < 4.0);
    }

    #[test]
    fn velocity_zones_select_the_matching_region() {
        let dir = TempDir::new("velocity");
        let soft = dir.join("soft.wav");
        let loud = dir.join("loud.wav");
        write_tone(&soft, 220.0, 44_100, 0.2);
        write_tone(&loud, 660.0, 44_100, 0.9);
        let mut soft_region = region(soft, 0, 127);
        soft_region.hivel = 63;
        let mut loud_region = region(loud, 0, 127);
        loud_region.lovel = 64;
        let patch = patch_with_regions(vec![soft_region, loud_region]);
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let soft_output = render_note(&mut sampler, 60, 32, 22_050);
        let loud_output = render_note(&mut sampler, 60, 100, 22_050);
        assert!((zero_crossing_hz(&soft_output, 44_100) - 220.0).abs() < 3.0);
        assert!((zero_crossing_hz(&loud_output, 44_100) - 660.0).abs() < 5.0);
        // The velocity zone gain is applied, not just the sample choice.
        let soft_peak = soft_output
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        let loud_peak = loud_output
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        assert!(loud_peak > soft_peak * 2.0);
    }

    #[test]
    fn continuous_loop_wraps_inside_the_loop_points() {
        let dir = TempDir::new("loop");
        let path = dir.join("loop.wav");
        write_tone(&path, 441.0, 4_410, 0.8);
        let mut region = region(path, 0, 127);
        region.loop_start = Some(1_000);
        region.loop_end = Some(3_000);
        region.loop_mode = LoopMode::LoopContinuous;
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region.clone()])).unwrap();
        sampler.note_on(0, 60, 100);
        let mut after_loop_end = Vec::new();
        for frame in 0..12_000 {
            let [left, _] = sampler.next_stereo(44_100);
            if frame >= 4_000 {
                after_loop_end.push(left);
            }
        }
        assert!(sampler.is_active());
        assert!(after_loop_end.iter().any(|sample| sample.abs() > 0.1));
        // A one-shot region stops at the end of the decoded sample.
        let mut one_shot = region;
        one_shot.sample = dir.join("loop.wav");
        one_shot.loop_mode = LoopMode::OneShot;
        one_shot.loop_start = None;
        one_shot.loop_end = None;
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![one_shot])).unwrap();
        sampler.note_on(0, 60, 100);
        for _ in 0..10_000 {
            sampler.next_stereo(44_100);
        }
        assert!(!sampler.is_active());
    }

    #[test]
    fn note_off_releases_and_looping_stops_with_loop_sustain() {
        let dir = TempDir::new("release");
        let path = dir.join("sustain.wav");
        write_tone(&path, 220.0, 8_820, 0.8);
        let mut region = region(path, 0, 127);
        region.loop_start = Some(100);
        region.loop_end = Some(8_000);
        region.loop_mode = LoopMode::LoopSustain;
        region.ampeg.release = 0.01;
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region])).unwrap();
        sampler.note_on(0, 60, 100);
        for _ in 0..1_000 {
            sampler.next_stereo(44_100);
        }
        assert!(sampler.is_active());
        sampler.note_off(0, 60);
        for _ in 0..1_000 {
            sampler.next_stereo(44_100);
        }
        assert!(!sampler.is_active());
    }

    #[test]
    fn flac_fixture_decodes_and_plays() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sampler/tone.flac");
        assert!(path.exists(), "missing FLAC fixture: {}", path.display());
        let decoded = decode_sample(&path).unwrap();
        assert!(decoded.frame_count() > 1_000);
        assert!(decoded.rate > 0);
        let mut region = region(path, 0, 127);
        region.pitch_keycenter = 60.0;
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region])).unwrap();
        let output = render_note(&mut sampler, 60, 100, 4_000);
        let peak = output
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        assert!(peak > 0.05, "FLAC fixture rendered silence");
    }

    #[test]
    fn sfz_control_default_path_and_doubled_separators_resolve() {
        let dir = TempDir::new("default-path");
        let samples = dir.0.join("Upright Piano Samples");
        std::fs::create_dir_all(&samples).unwrap();
        write_tone(&samples.join("C4.wav"), 220.0, 2_000, 0.5);
        // Imported SFZ files keep the directory in <control> and write
        // libSFZ-style doubled separators inside the sample path.
        let sfz = "// Hoffman 112 Upright\n\
<control>\n\
default_path=Upright Piano Samples/\n\
<group> amp_veltrack=25\n\
<region> lokey=0 hikey=127 pitch_keycenter=60 sample=C4.wav\n";
        let patch = patch_from_sfz_text(sfz, std::slice::from_ref(&dir.0));
        assert_eq!(patch.regions.len(), 1);
        assert!(patch.regions[0].sample.is_file());
        assert!((patch.regions[0].amp_veltrack - 25.0).abs() < 1e-9);
        // Both comment styles are ignored, not parsed as opcodes.
        assert!(patch.diagnostics.is_empty(), "{:?}", patch.diagnostics);

        let doubled = "// libSFZ\n\
<control> default_path=Upright Piano Samples\n\
<region> lokey=0 hikey=127 pitch_keycenter=60 sample=Upright Piano Samples//C4.wav\n";
        let patch = patch_from_sfz_text(doubled, std::slice::from_ref(&dir.0));
        assert_eq!(patch.regions.len(), 1);
        assert!(patch.regions[0].sample.is_file());

        // A trailing comment still strips; a path separator does not.
        let commented = "<region> lokey=0 hikey=127 pitch_keycenter=60 sample=Upright Piano Samples//C4.wav // tail\n".to_string();
        let patch = patch_from_sfz_text(&commented, std::slice::from_ref(&dir.0));
        assert_eq!(patch.regions.len(), 1);
        assert!(patch.regions[0].sample.is_file());
    }

    #[test]
    fn sfz_global_section_applies_to_every_region() {
        let dir = TempDir::new("global");
        let path = dir.join("tone.wav");
        write_tone(&path, 220.0, 1_000, 0.5);
        let sfz = format!(
            "<global>\namp_veltrack=50\nampeg_release=2\n\
<group> ampeg_release=1\n\
<region> lokey=0 hikey=59 pitch_keycenter=48 sample={}\n\
<region> key=72 amp_veltrack=10 sample={}\n",
            path.display(),
            path.display()
        );
        let patch = patch_from_sfz_text(&sfz, &[]);
        assert_eq!(patch.regions.len(), 2);
        assert!((patch.regions[0].amp_veltrack - 50.0).abs() < 1e-9);
        assert!((patch.regions[0].ampeg.release - 1.0).abs() < 1e-9);
        // A region-level opcode still wins over global and group defaults.
        assert!((patch.regions[1].amp_veltrack - 10.0).abs() < 1e-9);
    }

    #[test]
    fn imported_velocity_curve_squares_the_sfz_tracking() {
        let dir = TempDir::new("velocity-law");
        let path = dir.join("dc.wav");
        write_stereo_dc(&path, 4_096, 0.5);
        let mut region = region(path, 0, 127);
        region.amp_veltrack = 100.0;
        let peak_at = |curve: VelocityCurve, velocity: u8| {
            let mut patch = patch_with_regions(vec![region.clone()]);
            patch.velocity_curve = curve;
            let mut sampler = Sampler::from_patch(&patch).unwrap();
            sampler.note_on(0, 60, velocity);
            sampler.next_stereo(48_000)[0]
        };
        let linear = peak_at(VelocityCurve::Linear, 32) / peak_at(VelocityCurve::Linear, 127);
        let imported = peak_at(VelocityCurve::Preset, 32) / peak_at(VelocityCurve::Preset, 127);
        let velocity = 32.0 / 127.0;
        assert!((linear - velocity).abs() < 1e-6);
        assert!((imported - velocity * velocity).abs() < 1e-6);
    }

    #[test]
    fn zone_gain_follows_the_sample_level_without_rms_normalisation() {
        let dir = TempDir::new("no-normalisation");
        let quiet = dir.join("quiet.wav");
        let loud = dir.join("loud.wav");
        write_tone(&quiet, 220.0, 4_410, 0.1);
        write_tone(&loud, 220.0, 4_410, 0.4);
        let patch = patch_with_regions(vec![region(quiet, 0, 59), region(loud, 60, 127)]);
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let quiet_peak = render_note(&mut sampler, 48, 127, 2_000)
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let loud_peak = render_note(&mut sampler, 60, 127, 2_000)
            .iter()
            .fold(0.0f32, |peak, value| peak.max(value.abs()));
        // Zones keep the sample's own level: the quieter zone is 12 dB down,
        // not matched to the louder one by a per-region normalisation.
        assert!((loud_peak / quiet_peak - 4.0).abs() < 0.1);
    }

    #[test]
    fn velocity_layers_hold_a_constant_level_without_tracking() {
        let dir = TempDir::new("velocity-layers");
        let path = dir.join("layer.wav");
        write_tone(&path, 220.0, 4_410, 0.5);
        let mut soft = region(path.clone(), 0, 127);
        soft.hivel = 63;
        soft.amp_veltrack = 0.0;
        let mut loud = region(path, 0, 127);
        loud.lovel = 64;
        loud.amp_veltrack = 0.0;
        let patch = patch_with_regions(vec![soft, loud]);
        let peak_at = |velocity: u8| {
            let mut sampler = Sampler::from_patch(&patch).unwrap();
            render_note(&mut sampler, 60, velocity, 2_000)
                .iter()
                .fold(0.0f32, |peak, value| peak.max(value.abs()))
        };
        // amp_veltrack=0 is the factory layering convention: layers hard-switch
        // and the level stays constant inside each zone.
        for velocity in (1..=127).step_by(16) {
            assert!((peak_at(velocity) - peak_at(127)).abs() < 1e-4);
        }
    }

    #[test]
    fn sfz_parser_reports_unsupported_opcodes_and_parses_zones() {
        let dir = TempDir::new("sfz");
        let path = dir.join("sample.wav");
        write_tone(&path, 220.0, 1_000, 0.5);
        let sfz = format!(
            "<group> hivel=63 amp_veltrack=50 loop_mode=one_shot cutoff=1000\n\
             <region> lokey=0 hikey=59 pitch_keycenter=48 sample={}\n\
             <region> key=72 sample={} seq_position=2\n",
            path.display(),
            path.display()
        );
        let patch = patch_from_sfz_text(&sfz, &[]);
        assert_eq!(patch.regions.len(), 2);
        assert_eq!(patch.regions[0].pitch_keycenter, 48.0);
        assert_eq!(patch.regions[1].hikey, 72);
        assert_eq!(patch.regions[1].end, None);
        // The unsupported group opcode is reported, not silently dropped.
        assert!(patch.diagnostics.iter().any(|line| line.contains("cutoff")));
    }

    #[test]
    fn sample_rate_mismatch_resamples_without_allocating_in_next_stereo() {
        let dir = TempDir::new("rate");
        let path = dir.join("rate.wav");
        write_tone(&path, 441.0, 44_100, 0.8);
        let mut sampler =
            Sampler::from_patch(&patch_with_regions(vec![region(path, 0, 127)])).unwrap();
        sampler.note_on(0, 60, 100);
        // A 44.1 kHz asset played at 48 kHz would be 480 Hz at unity.
        let mut output = Vec::new();
        for _ in 0..24_000 {
            output.push(sampler.next_stereo(48_000)[0]);
        }
        let frequency = zero_crossing_hz(&output, 48_000);
        assert!((frequency - 441.0).abs() < 5.0, "frequency was {frequency}");
    }

    #[test]
    fn params_build_a_single_sample_region() {
        let dir = TempDir::new("params");
        let path = dir.join("single.wav");
        write_tone(&path, 330.0, 2_000, 0.5);
        let params = vec![
            (
                "sampler.sample_file".to_string(),
                ParamValue::String("single.wav".into()),
            ),
            ("sampler.root_note".to_string(), ParamValue::Number(60.0)),
        ];
        let mut sampler = Sampler::from_params(&params, Some(dir.0.as_path())).unwrap();
        let output = render_note(&mut sampler, 60, 100, 4_000);
        assert!(output.iter().any(|sample| sample.abs() > 0.05));
    }

    #[test]
    fn off_by_chokes_the_tagged_group() {
        let dir = TempDir::new("choke");
        let path = dir.join("choke.wav");
        write_tone(&path, 220.0, 44_100, 0.8);
        let mut region = region(path, 0, 127);
        region.group = Some(7);
        region.loop_mode = LoopMode::LoopContinuous;
        region.loop_start = Some(0);
        region.loop_end = Some(44_000);
        let mut choker = region.clone();
        choker.off_by = Some(7);
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region, choker])).unwrap();
        sampler.note_on(0, 60, 100);
        for _ in 0..100 {
            sampler.next_stereo(44_100);
        }
        sampler.note_on(0, 64, 100);
        // After the second note-on the first voice is choked and one remains.
        assert_eq!(sampler.voices.len(), 1);
    }

    /// Step WAV: 0.2 for the first half, 0.8 for the second.
    fn write_step(path: &Path, frames: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let value = if frame < frames / 2 { 0.2 } else { 0.8 };
            writer
                .write_sample((value * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn offset_starts_playback_at_the_declared_frame() {
        let dir = TempDir::new("offset");
        let path = dir.join("step.wav");
        write_step(&path, 2_000);
        let text = format!(
            "<region> sample={} pitch_keycenter=60 offset=1500 end=2000\n",
            path.display()
        );
        let patch = patch_from_sfz_text(&text, std::slice::from_ref(&dir.0));
        assert_eq!(patch.regions[0].offset, Some(1_500));
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        sampler.note_on(0, 60, 127);
        let [left, _] = sampler.next_stereo(44_100);
        // Frame 1500 is in the loud half, so the first output is ~0.8.
        assert!((left - 0.8).abs() < 0.05, "first sample {left}");
    }

    #[test]
    fn seq_length_extends_the_round_robin_cycle() {
        let dir = TempDir::new("seq-length");
        let low = dir.join("low.wav");
        let high = dir.join("high.wav");
        write_tone(&low, 220.0, 4_410, 0.5);
        write_tone(&high, 880.0, 4_410, 0.5);
        let text = format!(
            "<region> sample={} key=60 seq_position=1 seq_length=2\n\
             <region> sample={} key=60 seq_position=2\n",
            low.display(),
            high.display()
        );
        let patch = patch_from_sfz_text(&text, std::slice::from_ref(&dir.0));
        assert_eq!(patch.regions[0].seq_length, Some(2));
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        let mut notes = Vec::new();
        for _ in 0..3 {
            notes.push(zero_crossing_hz(
                &render_note(&mut sampler, 60, 100, 4_410),
                44_100,
            ));
        }
        // Three notes must alternate through a two-note cycle, so note 1 and 3
        // share a pitch and note 2 differs.
        assert!((notes[0] - notes[2]).abs() < 5.0, "{notes:?}");
        assert!((notes[0] - notes[1]).abs() > 200.0, "{notes:?}");
    }

    /// The sample-layer unison law: `1/sqrt(N)` keeps the stack's power flat,
    /// so 1/2/4/8 detuned voices sit within 0.5 dB of each other on a sine.
    /// The width stays zero so the linear pan law does not enter the reading.
    #[test]
    fn unison_law_keeps_the_mono_level_flat() {
        let dir = TempDir::new("unison");
        let path = dir.join("tone.wav");
        // Four seconds so the RMS window averages the beat between voices.
        write_tone(&path, 220.0, 176_400, 0.5);
        let rms = |unison: u32| {
            let mut region = region(path.clone(), 60, 60);
            region.unison = unison;
            // A wide detune makes the voices mutually incoherent, which is
            // the sum the normalisation is derived from.
            region.unison_detune_cents = 100.0;
            region.unison_width = 0.0;
            let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region])).unwrap();
            sampler.note_on(0, 60, 100);
            assert_eq!(sampler.voices.len() as u32, unison);
            let mut sum = 0.0;
            let mut count = 0.0;
            // Long enough to average the beat of a 100-cent detune.
            for _ in 0..176_400 {
                let [left, right] = sampler.next_stereo(44_100);
                sum += left * left + right * right;
                count += 2.0;
            }
            (sum / count).sqrt()
        };
        let single = rms(1);
        for count in [1, 2, 4, 8] {
            let level = 20.0 * (rms(count) / single).log10();
            assert!(
                level.abs() <= 0.5,
                "{count} voices: {level:+.2} dB from one voice"
            );
        }
    }

    /// Rewrite the RIFF data chunk length so the header claims far more samples
    /// than the file holds. Only the four header bytes change; no large buffer
    /// is ever written.
    fn wav_with_lying_data_len(path: &Path, declared_bytes: u32) {
        write_stereo_dc(path, 8, 0.25);
        let mut bytes = std::fs::read(path).unwrap();
        let data = bytes
            .windows(4)
            .position(|window| window == b"data")
            .expect("WAV data chunk");
        bytes[data + 4..data + 8].copy_from_slice(&declared_bytes.to_le_bytes());
        std::fs::write(path, &bytes).unwrap();
    }

    /// Minimal FLAC: one STREAMINFO block that declares `samples` frames per
    /// channel, followed by a truncated frame so decoding must fail.
    fn flac_with_declared_samples(path: &Path, samples: u64) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.push(0x80); // last metadata block, STREAMINFO
        bytes.extend_from_slice(&34u32.to_be_bytes()[1..]);
        bytes.extend_from_slice(&4_096u16.to_be_bytes()); // min block size
        bytes.extend_from_slice(&4_096u16.to_be_bytes()); // max block size
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // frame sizes unknown
        // 20-bit rate, 3-bit channels-1, 5-bit bit depth-1, 36-bit frames.
        let packed = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | samples;
        bytes.extend_from_slice(&packed.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 16]); // MD5
        bytes.extend_from_slice(&[0xff, 0xf8]); // truncated frame header
        std::fs::write(path, &bytes).unwrap();
    }

    /// A header count is a hint, not a reservation. The file stays tiny, so a
    /// regression that trusts the header tries to reserve gigabytes.
    #[test]
    fn lying_wav_header_count_is_rejected() {
        let dir = TempDir::new("wav-lying-count");
        let path = dir.join("huge.wav");
        wav_with_lying_data_len(&path, 0xffff_fff0);
        assert!(std::fs::metadata(&path).unwrap().len() < 1_024);
        let error = decode_sample(&path).unwrap_err();
        // The header parses; the failure must come from decoding the data.
        assert!(!error.contains("could not open"), "{error}");
        // The reservation itself stays bounded even for a saturated count.
        let mut frames = Vec::new();
        reserve_decoded(&mut frames, usize::MAX, &path).unwrap();
        assert!(frames.capacity() <= MAX_DECODE_PREALLOC);
    }

    /// The FLAC streaminfo field is 36 bits wide, so a malformed file can claim
    /// hundreds of gigabytes of samples before the first frame.
    #[test]
    fn lying_flac_header_count_is_rejected() {
        let dir = TempDir::new("flac-lying-count");
        let path = dir.join("huge.flac");
        flac_with_declared_samples(&path, 0x0f_ffff_ffff);
        assert!(std::fs::metadata(&path).unwrap().len() < 1_024);
        let error = decode_sample(&path).unwrap_err();
        // The header parses; the failure must come from decoding the data.
        assert!(!error.contains("could not open"), "{error}");
    }

    fn write_ramp(path: &Path, frames: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..frames {
            let value = (frame as f32 / frames as f32 * 0.9 * 32_767.0) as i16;
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn ping_pong_reads_the_loop_forward_then_backward() {
        let dir = TempDir::new("pingpong");
        let path = dir.join("ramp.wav");
        write_ramp(&path, 200);
        let mut region = region(path, 0, 127);
        region.loop_start = Some(0);
        region.loop_end = Some(100);
        region.loop_mode = LoopMode::LoopPingPong;
        region.ampeg.attack = 0.0;
        region.ampeg.decay = 0.0;
        region.ampeg.sustain = 1.0;
        let mut sampler = Sampler::from_patch(&patch_with_regions(vec![region])).unwrap();
        let output = render_note(&mut sampler, 60, 127, 320);
        let peak = output.iter().take(120).cloned().fold(f32::MIN, f32::max);
        let peak_at = output
            .iter()
            .take(120)
            .position(|value| *value == peak)
            .expect("a peak");
        assert!(
            (95..=105).contains(&peak_at),
            "the scan peaked at frame {peak_at} instead of the loop end"
        );
        assert!(
            output[150] < output[100] * 0.95,
            "the scan did not reverse: {} then {}",
            output[100],
            output[150]
        );
        assert!(
            output[250] > output[200] + 0.1 * peak,
            "the scan did not turn forward again: {} then {}",
            output[200],
            output[250]
        );
    }

    #[test]
    fn the_project_parameter_accepts_the_ping_pong_spelling() {
        let dir = TempDir::new("pingpong-param");
        let path = dir.join("ramp.wav");
        write_ramp(&path, 200);
        let params: Vec<(String, ParamValue)> = vec![
            (
                "sampler.sample_file".to_string(),
                ParamValue::String(path.display().to_string()),
            ),
            ("sampler.root_note".to_string(), ParamValue::Number(60.0)),
            ("sampler.loop_start".to_string(), ParamValue::Number(0.0)),
            ("sampler.loop_end".to_string(), ParamValue::Number(100.0)),
            (
                "sampler.loop_mode".to_string(),
                ParamValue::String("pingpong".to_string()),
            ),
        ];
        let patch = patch_from_params(&params, Some(dir.0.as_path())).expect("the patch builds");
        assert_eq!(patch.regions[0].loop_mode, LoopMode::LoopPingPong);
    }
}
