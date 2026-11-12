//! Foreign wavetable-synth preset reader and best-effort OpenSynth mapper.
//!
//! Container format (little-endian):
//! ```text
//! 0   9   "XferJson\0"
//! 9   4   metadata JSON length
//! 13  4   metadata flags
//! 17  N   metadata JSON
//! 17+N 4  uncompressed CBOR length
//! 21+N 4  payload flags
//! 25+N .. Zstandard-compressed CBOR
//! ```
//!
//! The full state tree is large (three oscillator families, sample, granular
//! and spectral modes, 64 modulation slots, and FX racks). OpenSynth has a
//! smaller engine, so this mapper transfers the audible core (three wavetable
//! oscillators with warp, sub and noise, two voice filters, four envelopes,
//! ten LFOs, macros, and the 64-slot modulation matrix) and leaves unsupported
//! features in the file.
//!
//! The modulation source IDs below are the preset format's identifiers:
//! `1` Mod Wheel, `2..=5` Env 1..4, `6..=15` LFO 1..10, `16` Velocity, `17`
//! Note, `18/19` channel/poly aftertouch, `20` Noise, `21..=24` note random /
//! alternating, `25..=32` Macro 1..8, `33` Pitch Bend, `34..=36` MPE, `37`
//! release velocity, `38` Fixed, `49..=54` oscillator/filter audio, and
//! `55..=59` voice bookkeeping. `0` means unused. Unknown IDs are skipped.

use std::path::{Path, PathBuf};

#[path = "foreign_preset_limits.rs"]
mod limits;

use md5::{Digest, Md5};
use oc_dsp::compander::SINE_CREST_DB;
use serde_json::{Value, json};

use oc_synth_core::sampler::{
    AmpEnvelope, LoopMode, SampleRegion, SamplerPatch, VelocityCurve, patch_from_sfz_file,
    patch_from_sfz_text, resolve_sample_path,
};
use oc_synth_core::synth::filter_id::{mode as filter_mode, model as filter_model};
use oc_synth_core::synth::granular::{GrainWindow, GranularPatch};
use oc_synth_core::synth::spectral::{SpectralPatch, SpectralWarp};
use oc_synth_core::synth::{
    CustomWavetable, FilterMode, FilterRouting, FxModRoute, FxParam, LfoSettings, LfoShape,
    LfoShapeType, ModDestination, ModRoute, ModSource, SourceFilterRoute, SourceFilterTarget,
    SubShape, SynthFx, SynthFxType, SynthPatch, WarpMode, Waveform,
};
use oc_synth_fx::MAX_SYNTH_FX;

/// Container marker that identifies a foreign preset file.
pub const MAGIC: &[u8] = b"XferJson\0";
const MAX_PRESET_BYTES: usize = limits::MAX_INPUT;
/// The preset format exposes exactly 64 modulation matrix slots.
const MOD_SLOT_COUNT: usize = 64;
// Module IDs reserve one hundred positions per rack. This also bounds each
// splitter's scan and the aggregate rack traversal to ten thousand steps.
const MAX_RACK_ITEMS: usize = 100;

/// One deduplicated import warning. `class` is a stable key (module class,
/// parameter class or route class) so a preset scan can count residuals and a
/// preset reports each unmapped class once instead of once per instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportWarning {
    pub class: String,
    pub message: String,
    /// How many instances of this class the preset contains.
    pub count: usize,
}

/// Warnings collected while mapping one preset.
#[derive(Debug, Clone, Default)]
pub struct ImportDiagnostics {
    warnings: Vec<ImportWarning>,
    index: std::collections::HashMap<String, usize>,
}

impl ImportDiagnostics {
    /// Record one instance of `class`; the message is kept from the first hit.
    pub fn warn(&mut self, class: impl Into<String>, message: impl Into<String>) {
        let class = class.into();
        if let Some(&index) = self.index.get(&class) {
            self.warnings[index].count += 1;
            return;
        }
        self.index.insert(class.clone(), self.warnings.len());
        self.warnings.push(ImportWarning {
            class,
            message: message.into(),
            count: 1,
        });
    }

    /// The deduplicated warnings recorded so far.
    pub fn warnings(&self) -> &[ImportWarning] {
        &self.warnings
    }

    /// Whether no warning was recorded.
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Emit every deduplicated warning through `log`, once per class.
    pub fn log(&self) {
        for warning in &self.warnings {
            log::warn!("Preset import: {}", warning.message);
        }
    }
}

/// A decoded foreign preset: its metadata header and CBOR state tree.
#[derive(Debug, Clone)]
pub struct ForeignPreset {
    /// `metadata` object from the container header.
    pub metadata: Value,
    /// Decoded CBOR state tree.
    pub data: Value,
    cbor: ciborium::value::Value,
}

/// The metadata summary reported to the importer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PresetMetadata {
    pub name: String,
    pub author: String,
    pub description: String,
    pub tags: Vec<String>,
    pub product_version: String,
}

impl ForeignPreset {
    /// Read and decode a preset from disk, bounded by the maximum input size.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|error| format!("could not read preset '{}': {error}", path.display()))?;
        let bytes = limits::read_limited(file, MAX_PRESET_BYTES)?;
        Self::parse(&bytes)
    }

    /// Decode a preset container; the first bytes must carry the magic marker.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_PRESET_BYTES {
            return Err("Preset exceeds 64 MiB".into());
        }
        if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
            return Err("not a supported preset (missing XferJson header)".into());
        }
        let mut offset = MAGIC.len();
        let metadata_len = usize::try_from(read_u32(bytes, &mut offset)?)
            .map_err(|_| "preset metadata length overflow")?;
        if metadata_len > limits::MAX_METADATA {
            return Err("preset metadata exceeds 1 MiB".into());
        }
        let _metadata_flags = read_u32(bytes, &mut offset)?;
        if metadata_len > bytes.len().saturating_sub(offset) {
            return Err("Preset metadata is truncated".into());
        }
        let metadata: Value = serde_json::from_slice(&bytes[offset..offset + metadata_len])
            .map_err(|error| format!("invalid preset metadata JSON: {error}"))?;
        offset += metadata_len;

        let cbor_len = usize::try_from(read_u32(bytes, &mut offset)?)
            .map_err(|_| "preset payload length overflow")?;
        if cbor_len > limits::MAX_EXPANDED {
            return Err("preset expanded payload exceeds 32 MiB".into());
        }
        let _payload_flags = read_u32(bytes, &mut offset)?;
        if offset >= bytes.len() {
            return Err("Preset has no CBOR payload".into());
        }
        let mut decoder = zstd::stream::read::Decoder::new(&bytes[offset..])
            .map_err(|error| format!("could not decompress preset payload: {error}"))?;
        decoder
            .window_log_max(25)
            .map_err(|error| format!("invalid preset compression window: {error}"))?;
        let cbor = limits::read_limited(decoder, cbor_len)?;
        if cbor.len() != cbor_len {
            return Err(format!(
                "Preset payload length mismatch: header {cbor_len}, decoded {}",
                cbor.len()
            ));
        }
        limits::validate_cbor(&cbor)?;
        let value = ciborium::de::from_reader_with_recursion_limit::<ciborium::value::Value, _>(
            &cbor[..],
            limits::MAX_DEPTH + 1,
        )
        .map_err(|error| format!("invalid preset CBOR: {error}"))?;
        Ok(Self {
            metadata,
            data: cbor_to_json(&value),
            cbor: value,
        })
    }

    /// Copy the header fields into the flat metadata summary.
    pub fn metadata_summary(&self) -> PresetMetadata {
        let text = |key: &str| {
            self.metadata
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let tags = self
            .metadata
            .get("tags")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        PresetMetadata {
            name: text("presetName"),
            author: text("presetAuthor"),
            description: text("presetDescription"),
            tags,
            product_version: text("productVersion"),
        }
    }

    /// Build a VST3 component-state blob from this preset.
    ///
    /// Presets are merged processor/editor states; `IComponent::setState`
    /// dispatches on the CBOR root `component` key and requires `"processor"`.
    pub fn to_vst3_state(&self) -> Result<Vec<u8>, String> {
        let map = self.cbor.as_map().ok_or("Preset root is not a CBOR map")?;
        struct ProcessorState<'a>(&'a [(ciborium::value::Value, ciborium::value::Value)]);
        impl serde::Serialize for ProcessorState<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(None)?;
                for (key, value) in self.0 {
                    if key.as_text() != Some("component") {
                        map.serialize_entry(key, value)?;
                    }
                }
                map.serialize_entry("component", "processor")?;
                map.end()
            }
        }
        let mut cbor = limits::BoundedWriter {
            bytes: Vec::new(),
            limit: limits::MAX_EXPANDED,
        };
        ciborium::ser::into_writer(&ProcessorState(map), &mut cbor)
            .map_err(|error| format!("could not encode preset state: {error}"))?;
        limits::validate_cbor(&cbor.bytes)?;
        let mut payload = limits::BoundedWriter {
            bytes: Vec::new(),
            limit: MAX_PRESET_BYTES,
        };
        zstd::stream::copy_encode(&cbor.bytes[..], &mut payload, 3)
            .map_err(|error| format!("could not compress preset state: {error}"))?;
        let cbor = cbor.bytes;
        let payload = payload.bytes;
        let hash = Md5::digest(&payload)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let product_version = self
            .metadata
            .get("productVersion")
            .and_then(Value::as_str)
            .unwrap_or("2.0.19");
        let metadata = json!({
            "component": "processor",
            "hash": hash,
            "product": "Serum2",
            "productVersion": product_version,
            "url": "https://xferrecords.com/",
            "vendor": "Xfer Records",
            "version": 7.0,
        });
        let mut metadata_bytes = limits::BoundedWriter {
            bytes: Vec::new(),
            limit: limits::MAX_METADATA,
        };
        serde_json::to_writer(&mut metadata_bytes, &metadata)
            .map_err(|error| format!("could not encode preset metadata: {error}"))?;
        let metadata = metadata_bytes.bytes;
        let output_len = MAGIC
            .len()
            .checked_add(16)
            .and_then(|n| n.checked_add(metadata.len()))
            .and_then(|n| n.checked_add(payload.len()))
            .filter(|n| *n <= MAX_PRESET_BYTES)
            .ok_or("preset output exceeds 64 MiB")?;
        let metadata_len = u32::try_from(metadata.len()).map_err(|_| "metadata length overflow")?;
        let cbor_len = u32::try_from(cbor.len()).map_err(|_| "payload length overflow")?;
        let mut output = Vec::with_capacity(output_len);
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&metadata_len.to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&metadata);
        output.extend_from_slice(&cbor_len.to_le_bytes());
        output.extend_from_slice(&2u32.to_le_bytes());
        output.extend_from_slice(&payload);
        Ok(output)
    }

    /// Best-effort transfer into the OpenSynth patch model.
    ///
    /// `content_root` is the preset's `Tables` directory (or the directory that
    /// contains it); it is only used to load the referenced `.wav` wavetable. Without
    /// it the patch uses OpenSynth's built-in table. OpenSynth has a single
    /// `custom_wavetable` slot, so oscillator C reuses the table loaded for A
    /// (or the built-in table) rather than loading a second table of its own.
    pub fn to_synth_patch(&self, content_root: Option<&Path>) -> SynthPatch {
        let mut diagnostics = ImportDiagnostics::default();
        let patch = self.to_synth_patch_diagnosed(content_root, &mut diagnostics);
        diagnostics.log();
        patch
    }

    /// Stable per-preset seed so a granular source renders identically on every
    /// import. FNV-1a keeps it independent of the process hash seed.
    fn source_seed(&self) -> u64 {
        let name = self
            .metadata
            .get("presetName")
            .or_else(|| self.metadata.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in name.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash | 1
    }

    /// Map the preset and collect one warning per unmapped module, parameter
    /// or route class instead of dropping it silently.
    pub fn to_synth_patch_diagnosed(
        &self,
        content_root: Option<&Path>,
        diagnostics: &mut ImportDiagnostics,
    ) -> SynthPatch {
        let mut patch = SynthPatch::default();
        let data = &self.data;
        let source_seed = self.source_seed();

        // ---- oscillators -------------------------------------------------
        // A sample/multisample oscillator is not a wavetable. It becomes a
        // decoded sampler layer, and the wavetable oscillator for that slot is
        // left at zero level (never substituted with a default table).
        let mut sample_patch: Option<SamplerPatch> = None;
        let osc_a = data.get("Oscillator0");
        let mode_a = osc_mode(osc_a, 0);
        let osc_b = data.get("Oscillator1");
        let mode_b = osc_mode(osc_b, 1);

        let a_level = osc_level(osc_a, 0.5625);
        let mut a_sample = false;
        if let Some((kind, mode)) = mode_a {
            match kind {
                "WTOsc" | "Oscillator" => {
                    patch.oscillator_a_waveform = Waveform::Wavetable;
                    patch.oscillator_a_wavetable_position =
                        table_position(mode).unwrap_or(patch.oscillator_a_wavetable_position);
                    patch.oscillator_start_phase =
                        degrees(plain_num(Some(mode), "kParamInitialPhase").unwrap_or(0.0));
                    patch.oscillator_a_warp_mode = warp_mode(mode);
                    patch.oscillator_a_warp_amount = warp_amount(mode, "");
                    patch.oscillator_a_warp_var = warp_var(mode, "");
                    patch.oscillator_a_warp2_mode = warp_mode2(mode);
                    patch.oscillator_a_warp2_amount = warp_amount(mode, "2");
                    patch.oscillator_a_warp2_var = warp_var(mode, "2");
                }
                "SampleOsc" | "MultiSampleOsc" => {
                    a_sample = true;
                    add_sample_layer(
                        &mut sample_patch,
                        kind,
                        mode,
                        osc_a,
                        a_level,
                        "oscillator A",
                        content_root,
                        diagnostics,
                    );
                }
                "GranularOsc" => {
                    if let Some(settings) = granular_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator A",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.granular[0] = Some(settings);
                        patch.oscillator_a_waveform = Waveform::Granular;
                    } else {
                        a_sample = true;
                    }
                }
                "SpectralOsc" => {
                    if let Some(settings) = spectral_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator A",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.spectral[0] = Some(settings);
                        patch.oscillator_a_waveform = Waveform::Spectral;
                    } else {
                        a_sample = true;
                    }
                }
                other => {
                    a_sample = true;
                    log_unsupported_oscillator(other, 0, a_level, diagnostics);
                }
            }
        }
        patch.oscillator_a_level = if a_sample { 0.0 } else { a_level };
        patch.oscillator_a_pan = oscillator_pan(osc_a);
        patch.oscillator_a_pulse_width = 0.5;
        // A's octave/pitch/fine are independent of B/C. The engine keeps a
        // separate semitone offset so A can sit two octaves down (Sweet Bass)
        // without dragging the other oscillators with it.
        patch.oscillator_a_semitones = oscillator_semitones(osc_a);

        let b_level = osc_level(osc_b, 0.0);
        let mut b_sample = false;
        if let Some((kind, mode)) = mode_b {
            match kind {
                "WTOsc" | "Oscillator" => {
                    patch.oscillator_b_waveform = Waveform::Wavetable;
                    patch.oscillator_b_wavetable_position =
                        table_position(mode).unwrap_or(patch.oscillator_b_wavetable_position);
                    patch.oscillator_b_warp_mode = warp_mode(mode);
                    patch.oscillator_b_warp_amount = warp_amount(mode, "");
                    patch.oscillator_b_warp_var = warp_var(mode, "");
                    patch.oscillator_b_warp2_mode = warp_mode2(mode);
                    patch.oscillator_b_warp2_amount = warp_amount(mode, "2");
                    patch.oscillator_b_warp2_var = warp_var(mode, "2");
                }
                "SampleOsc" | "MultiSampleOsc" => {
                    b_sample = true;
                    add_sample_layer(
                        &mut sample_patch,
                        kind,
                        mode,
                        osc_b,
                        b_level,
                        "oscillator B",
                        content_root,
                        diagnostics,
                    );
                }
                "GranularOsc" => {
                    if let Some(settings) = granular_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator B",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.granular[1] = Some(settings);
                        patch.oscillator_b_waveform = Waveform::Granular;
                    } else {
                        b_sample = true;
                    }
                }
                "SpectralOsc" => {
                    if let Some(settings) = spectral_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator B",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.spectral[1] = Some(settings);
                        patch.oscillator_b_waveform = Waveform::Spectral;
                    } else {
                        b_sample = true;
                    }
                }
                other => {
                    b_sample = true;
                    log_unsupported_oscillator(other, 1, b_level, diagnostics);
                }
            }
        } else {
            patch.oscillator_b_waveform = Waveform::Square;
        }
        patch.oscillator_b_level = if b_sample { 0.0 } else { b_level };
        patch.oscillator_b_pan = oscillator_pan(osc_b);
        patch.oscillator_b_semitones = oscillator_tune_cents(osc_b).clamp(-2400.0, 2400.0) / 100.0;

        // ---- oscillator C ------------------------------------------------
        // the preset's third wavetable oscillator is `Oscillator2`; the sample and
        // noise slots are `Oscillator3`/`Oscillator4` below. C is disabled by
        // default, so an absent `kParamEnable` must not resurrect it at full
        // level.
        let osc_c = data.get("Oscillator2");
        let mode_c = osc_mode(osc_c, 2);
        let c_level = osc_level_enabled(osc_c, false, 0.5625);
        let mut c_sample = false;
        if let Some((kind, mode)) = mode_c {
            match kind {
                "WTOsc" | "Oscillator" => {
                    patch.oscillator_c_waveform = Waveform::Wavetable;
                    patch.oscillator_c_wavetable_position =
                        table_position(mode).unwrap_or(patch.oscillator_c_wavetable_position);
                    patch.oscillator_c_warp_mode = warp_mode(mode);
                    patch.oscillator_c_warp_amount = warp_amount(mode, "");
                    patch.oscillator_c_warp_var = warp_var(mode, "");
                    patch.oscillator_c_warp2_mode = warp_mode2(mode);
                    patch.oscillator_c_warp2_amount = warp_amount(mode, "2");
                    patch.oscillator_c_warp2_var = warp_var(mode, "2");
                }
                "SampleOsc" | "MultiSampleOsc" => {
                    c_sample = true;
                    add_sample_layer(
                        &mut sample_patch,
                        kind,
                        mode,
                        osc_c,
                        c_level,
                        "oscillator C",
                        content_root,
                        diagnostics,
                    );
                }
                "GranularOsc" => {
                    if let Some(settings) = granular_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator C",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.granular[2] = Some(settings);
                        patch.oscillator_c_waveform = Waveform::Granular;
                    } else {
                        c_sample = true;
                    }
                }
                "SpectralOsc" => {
                    if let Some(settings) = spectral_patch(
                        kind,
                        mode,
                        content_root,
                        "oscillator C",
                        source_seed,
                        diagnostics,
                    ) {
                        patch.spectral[2] = Some(settings);
                        patch.oscillator_c_waveform = Waveform::Spectral;
                    } else {
                        c_sample = true;
                    }
                }
                other => {
                    c_sample = true;
                    log_unsupported_oscillator(other, 2, c_level, diagnostics);
                }
            }
        }
        patch.oscillator_c_level = if c_sample { 0.0 } else { c_level };
        patch.oscillator_c_pan = oscillator_pan(osc_c);
        patch.oscillator_c_pulse_width = 0.5;
        patch.oscillator_c_semitones = oscillator_tune_cents(osc_c).clamp(-2400.0, 2400.0) / 100.0;
        // Wavetable oscillators default to 180 deg initial phase in the preset.
        // The engine already staggers every unison voice, so the preset's 100 %
        // random phase is satisfied by that stagger instead of another offset.
        let phase_mode = osc_mode(data.get("Oscillator0"), 0).map(|(_, mode)| mode);
        patch.oscillator_start_phase =
            plain_num(phase_mode, "kParamInitialPhase").unwrap_or(180.0) / 360.0;

        // Sub and noise share the same slot list as the preset. The sub's octave
        // knob is absolute (0 plays at the note); the engine legacy default is
        // one octave down, so always transfer it when the module is present.
        let sub = data.get("Oscillator4");
        if enabled(sub, false) {
            let volume = plain_num(sub, "kParamVolume").unwrap_or(0.5625);
            patch.sub_level = volume.clamp(0.0, 1.0);
        }
        if sub.is_some() {
            patch.sub_semitones = plain_num(sub, "kParamOctave").unwrap_or(0.0) * 12.0;
            // Fine is in cents; kParamPitch is inert for the sub.
            patch.sub_fine_cents = plain_num(sub, "kParamFine")
                .unwrap_or(0.0)
                .clamp(-100.0, 100.0);
            patch.sub_waveform = sub_waveform(sub);
            patch.sub_pan = oscillator_pan(sub);
            let mode = sub.and_then(|module| module.get("SubOsc4"));
            // kParamInitialPhase is in degrees; the engine stores a phase fraction.
            patch.sub_initial_phase =
                (plain_num(mode, "kParamInitialPhase").unwrap_or(0.0) / 360.0).clamp(0.0, 1.0);
            patch.sub_contiguous_phase = flag(mode, "kParamContiguousPhase", false);
        }
        let noise = data.get("Oscillator3");
        if enabled(noise, false) {
            let volume = plain_num(noise, "kParamVolume").unwrap_or(0.0625);
            patch.noise_level = volume.clamp(0.0, 1.0);
        }

        // Unison and detune come from oscillator A; the preset keeps them per
        // oscillator, OpenSynth has one global unison stack.
        let unison = plain_num(osc_a, "kParamUnison")
            .unwrap_or(1.0)
            .round()
            .clamp(1.0, 16.0) as usize;
        patch.unison_voices = unison;
        // The preset readout is 1x; the engine's default is a 2x box average.
        patch.oscillator_quality = 0;
        let detune = plain_num(osc_a, "kParamDetune").unwrap_or(0.0);
        patch.detune_cents = preset_detune_cents(detune);
        // `kParamUnisonStereo` is the per-oscillator stereo spread. Oscillator
        // A's spread feeds the legacy global field; B/C have their own.
        patch.stereo_width = osc_width(osc_a);
        patch.oscillator_b_unison = osc_unison(osc_b);
        patch.oscillator_b_detune_cents = osc_detune(osc_b);
        patch.oscillator_b_stereo_width = osc_width(osc_b);
        patch.oscillator_c_unison = osc_unison(osc_c);
        patch.oscillator_c_detune_cents = osc_detune(osc_c);
        patch.oscillator_c_stereo_width = osc_width(osc_c);

        // ---- filter ------------------------------------------------------
        // the preset stores `kParamEnable` only for an engaged filter, so a missing key
        // is a bypass; the stale cutoff and resonance beside it must not apply.
        let filter = data.get("VoiceFilter0");
        if enabled(filter, false) && !filter_type_bypassed(filter) {
            let frequency = plain_num(filter, "kParamFreq").unwrap_or(0.5);
            patch.filter_cutoff = cutoff_hz(frequency);
            let resonance = plain_num(filter, "kParamReso").unwrap_or(0.0);
            patch.filter_resonance = normalize_percent(resonance);
            let drive = plain_num(filter, "kParamDrive").unwrap_or(0.0);
            patch.filter_drive = normalize_percent(drive);
            patch.filter_wet = plain_num(filter, "kParamWet")
                .map(|wet| (wet / 100.0).clamp(0.0, 1.0))
                .unwrap_or(1.0);
            let filter_type = plain_str(filter, "kParamType").unwrap_or("L12");
            patch.filter_mode = filter_mode(filter_type);
            patch.filter_model = filter_model(filter_type);
            // the preset's keytrack control is a 0/1 flag (every factory preset
            // writes exactly 1.0), and its origin is C1; the engine's legacy
            // origin is C4, so the origin is transferred explicitly.
            patch.filter_keytrack = plain_num(filter, "kParamKeyTrack")
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            patch.filter_keytrack_origin = 32.0;
        } else {
            // A missing or disabled VoiceFilter must not fall through to the
            // engine's default 12 kHz low-pass: that silently smooths every edge.
            patch.filter_wet = 0.0;
        }

        // ---- filter 2 ----------------------------------------------------
        // `VoiceFilter1` is the preset's second voice filter. It is disabled by
        // default (`plainParams` is frequently the literal "default"), so only
        // transfer its controls when explicitly enabled; otherwise the inert
        // engine defaults are left in place.
        let filter2 = data.get("VoiceFilter1");
        patch.filter2_enabled = enabled(filter2, false) && !filter_type_bypassed(filter2);
        if patch.filter2_enabled {
            let frequency = plain_num(filter2, "kParamFreq").unwrap_or(0.5);
            patch.filter2_cutoff = cutoff_hz(frequency);
            let resonance = plain_num(filter2, "kParamReso").unwrap_or(0.0);
            patch.filter2_resonance = normalize_percent(resonance);
            let drive = plain_num(filter2, "kParamDrive").unwrap_or(0.0);
            patch.filter2_drive = normalize_percent(drive);
            patch.filter2_wet = plain_num(filter2, "kParamWet")
                .map(|wet| (wet / 100.0).clamp(0.0, 1.0))
                .unwrap_or(1.0);
            let filter_type = plain_str(filter2, "kParamType").unwrap_or("L12");
            patch.filter2_mode = filter_mode(filter_type);
            patch.filter2_model = filter_model(filter_type);
        }

        // ---- filter routing ----------------------------------------------
        patch.filter_routing = detect_filter_routing(data);
        patch.source_filter_routes = source_filter_routes(data);

        // ---- envelopes ---------------------------------------------------
        let (a_attack, a_decay, a_sustain, a_release, _) = envelope(data, 0);
        patch.attack = a_attack;
        patch.decay = a_decay;
        // the preset Env 1 stores the control value; its signal is amplitude gain.
        patch.sustain = (a_sustain * a_sustain).clamp(0.0, 1.0);
        patch.release = a_release;
        // The amp shape is dominated by its decay stage. The engine has one
        // shared curve, so use Curve2 (decay) rather than Curve1 (attack).
        let amp_curve = (plain_num(data.get("Env0"), "kParamCurve2").unwrap_or(50.0) - 50.0) / 50.0;
        patch.amp_curve = amp_curve.clamp(-1.0, 1.0);

        let (f_attack, f_decay, f_sustain, f_release, f_curve) = envelope(data, 1);
        patch.filter_attack = f_attack;
        patch.filter_decay = f_decay;
        // Like the amplitude envelope, the filter envelope's signal is the
        // square of the stored sustain control.
        patch.filter_sustain = (f_sustain * f_sustain).clamp(0.0, 1.0);
        patch.filter_release = f_release;
        patch.filter_curve = f_curve;

        // the preset Env 2 and Env 3 are the two free modulation envelopes. They
        // keep the raw control sustain (only Env 1 has the squared amplitude
        // signal) and use `kParamCurve1` for the normalized curve amount.
        let (e3_attack, e3_decay, e3_sustain, e3_release, e3_curve) = envelope(data, 2);
        patch.env3_attack = e3_attack;
        patch.env3_decay = e3_decay;
        patch.env3_sustain = e3_sustain;
        patch.env3_release = e3_release;
        patch.env3_curve = e3_curve;

        let (e4_attack, e4_decay, e4_sustain, e4_release, e4_curve) = envelope(data, 3);
        patch.env4_attack = e4_attack;
        patch.env4_decay = e4_decay;
        patch.env4_sustain = e4_sustain;
        patch.env4_release = e4_release;
        patch.env4_curve = e4_curve;

        // ---- LFO 1 --------------------------------------------------------
        // LFO0 keeps the original flat `lfo_*` fields; LFO1..LFO9 are stored in
        // `extra_lfos[0..8]` (LFO2..LFO10 in the preset numbering).
        let lfo0 = data.get("LFO0");
        patch.lfo_rate = lfo_rate(lfo0);
        patch.lfo_sync = lfo_sync(lfo0);
        patch.lfo_rate_x10 = flag(lfo0, "kParamRate10x", false);
        patch.lfo_retrigger = lfo_retrigger(lfo0);
        patch.lfo_division = lfo_division(lfo0);
        patch.lfo_sync_div = lfo_sync_div(lfo0);
        patch.lfo_phase = lfo_phase(lfo0);
        patch.lfo_smooth = lfo_percent(lfo0, "kParamSmooth");
        patch.lfo_rise = lfo_seconds(lfo0, "kParamRise");
        patch.lfo_delay = lfo_seconds(lfo0, "kParamDelay");
        let (shape_type, shape) = lfo_shape_diagnosed(lfo0, diagnostics);
        patch.lfo_shape_type = shape_type;
        patch.lfo_shape = std::sync::Arc::new(shape);

        for (offset, settings) in patch.extra_lfos.iter_mut().enumerate() {
            let module = data.get(format!("LFO{}", offset + 1));
            let (shape_type, shape) = lfo_shape_diagnosed(module, diagnostics);
            *settings = LfoSettings {
                rate: lfo_rate(module),
                shape_type,
                shape: std::sync::Arc::new(shape),
                sync: lfo_sync(module),
                sync_div: lfo_sync_div(module),
                retrigger: lfo_retrigger(module),
                division: lfo_division(module),
                phase: lfo_phase(module),
                smooth: lfo_percent(module, "kParamSmooth"),
                rise: lfo_seconds(module, "kParamRise"),
                delay: lfo_seconds(module, "kParamDelay"),
                rate_x10: flag(module, "kParamRate10x", false),
                ..LfoSettings::default()
            };
        }

        // ---- macros / voice / global -------------------------------------
        for index in 0..8 {
            if let Some(module) = data.get(format!("Macro{index}")) {
                let value =
                    (plain_num(Some(module), "kParamValue").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
                if index < 4 {
                    patch.macros[index] = value;
                } else {
                    patch.extra_macros[index - 4] = value;
                }
            }
        }
        let global = data.get("Global0");
        patch.monophonic = flag(global, "kParamMonoToggle", false);
        // Portamento time is stored in seconds, like the envelope times.
        patch.glide_time = plain_num(global, "kParamPortamentoTime")
            .unwrap_or(0.0)
            .clamp(0.0, 4.0);
        // the preset's master volume is a linear gain. The controller default is
        // 50 % = -9 dB, so an omitted key must not keep the 0.22 patch default.
        let master = plain_num(global, "kParamMasterVolume").unwrap_or(MASTER_VOLUME_DEFAULT);
        patch.gain = master.clamp(0.0, 1.0);
        // Voice Amp is centred at 0.5 in the preset; 1.0 is the engine's unity.
        let voice_amp = plain_num(global, "kParamVoiceAmp").unwrap_or(0.5) / 0.5;
        patch.gain = (patch.gain * voice_amp).clamp(0.0, 1.0);
        // the preset has no implicit velocity gain; `Velo -> Amp` is a matrix route.
        patch.velocity_to_amp = 0.0;
        patch.polyphony = plain_num(global, "kParamPolyCount")
            .unwrap_or(16.0)
            .round()
            .clamp(1.0, 32.0) as usize;
        if patch.monophonic {
            patch.legato = true;
        }

        // ---- modulation matrix (all 64 slots) ----------------------------
        // `SynthPatch::default` pre-fills 64 inert routes so old documents keep
        // their slot count; replace them with only the routes this preset
        // actually populates. the preset's `kParamAmount` is a percentage, so it is
        // always divided by 100 here (values such as 0.2 mean 0.2%, not 20%).
        patch.mod_routes.clear();
        patch.mod_routes.reserve(MOD_SLOT_COUNT);
        for slot in 0..MOD_SLOT_COUNT {
            if patch.mod_routes.len() >= MOD_SLOT_COUNT {
                break;
            }
            let Some(module) = data.get(format!("ModSlot{slot}")) else {
                continue;
            };
            let Some(source_values) = module.get("source").and_then(Value::as_array) else {
                continue;
            };
            let Some(source_id) = source_values.first().and_then(number_as_i64) else {
                continue;
            };
            // Rows that are bypassed or have no amount are inert by design;
            // only a live row that cannot be mapped is a silent-drop risk.
            let live = !flag(Some(module), "kParamBypass", false)
                && plain_num(Some(module), "kParamAmount").unwrap_or(0.0) != 0.0;
            // `source[1]` is the preset's auxiliary scaling source (for example
            // LFO 5 x Macro 6). It is kept as a live source rather than folded
            // into the amount, so a macro at 0 gates the route now and a later
            // macro change brings it back without re-importing.
            let auxiliary_id = source_values.get(1).and_then(number_as_i64);
            let auxiliary = match auxiliary_id {
                None | Some(0) => None,
                // `Fixed` (38) is a constant source; treating it as unity
                // leaves the primary source unchanged, which is what an aux
                // constant of 1 would do.
                Some(38) => None,
                Some(id) => match mod_source(id) {
                    Some(source) => Some(source),
                    // An unresolved auxiliary would silently become unity and
                    // over-modulate, so drop the row until the source is known.
                    None => {
                        if live {
                            diagnostics.warn(
                                format!("mod-aux-source:{id}"),
                                format!("modulation auxiliary source id {id} has no engine source"),
                            );
                        }
                        continue;
                    }
                },
            };
            let Some(source) = mod_source(source_id) else {
                if live {
                    diagnostics.warn(
                        format!("mod-source:{source_id}"),
                        format!("modulation source id {source_id} has no engine source"),
                    );
                }
                continue;
            };
            // A bypassed row must not contribute anything.
            if flag(Some(module), "kParamBypass", false) {
                continue;
            }
            let destination_module = module
                .get("destModuleTypeString")
                .and_then(Value::as_str)
                .unwrap_or("");
            let destination_param = module
                .get("destModuleParamName")
                .and_then(Value::as_str)
                .unwrap_or("");
            let destination_instance = module
                .get("destModuleID")
                .and_then(number_as_i64)
                .unwrap_or(0);
            let Some(destination) =
                mod_destination(destination_module, destination_instance, destination_param)
            else {
                // FX destinations are imported by the FX-route path, which has
                // its own residual reporting; this matrix only holds voices.
                if live && !destination_module.starts_with("FX") {
                    diagnostics.warn(
                        format!("mod-dest:{destination_module}.{destination_param}"),
                        format!(
                            "modulation destination '{destination_module}.{destination_param}' has no engine parameter"
                        ),
                    );
                }
                continue;
            };
            let amount = plain_num(Some(module), "kParamAmount").unwrap_or(0.0);
            let amount = (amount / 100.0).clamp(-1.0, 1.0);
            if amount == 0.0 {
                continue;
            }
            patch.mod_routes.push(ModRoute {
                source,
                destination,
                amount,
                bipolar: flag(Some(module), "kParamBipolar", false),
                auxiliary,
                auxiliary_inverted: auxiliary.is_some()
                    && flag(Some(module), "kParamAuxInverted", false),
                ..ModRoute::default()
            });
        }
        // Deliberately not mapped yet: per-route curves (`flex`,
        // `kParamMainCurveData`/`kParamAuxCurveData`), `kParamCurveIn/Out`,
        // slew (`kParamSmoothRise/Fall`), `kParamDelayOffset` and
        // `kParamOut`. Their scaling is not modelled here, so the patch keeps the
        // neutral defaults and the raw the preset file retains them.

        // ---- FX racks ----------------------------------------------------
        // the preset's Main rack is the only serial chain; Bus 1/2 are parallel
        // sends that the flat OpenSynth rack cannot represent. Unknown modules
        // are skipped level-neutrally (the dry path passes unchanged) with a
        // warning instead of being guessed into an unrelated effect.
        let imported_fx = import_fx(data, diagnostics);
        patch.fx_mod_routes = import_fx_mod_routes(data, &imported_fx, diagnostics);
        patch.fx = imported_fx.modules;

        // ---- optional wavetable assets -----------------------------------
        // A keeps the legacy `custom_wavetable` slot; B and C get their own
        // tables. Sharing a path with A leaves the field empty, which makes the
        // engine fall back to A's table without duplicating samples.
        patch.sampler = sample_patch;

        if let Some(root) = content_root {
            patch.custom_wavetable = load_wavetable(root, mode_a, "oscillator A", diagnostics);
            let a_relative = if patch.custom_wavetable.is_some() {
                wavetable_relative(mode_a)
            } else {
                None
            };
            if wavetable_relative(mode_b) != a_relative {
                patch.oscillator_b_wavetable =
                    load_wavetable(root, mode_b, "oscillator B", diagnostics);
            }
            if wavetable_relative(mode_c) != a_relative {
                patch.oscillator_c_wavetable =
                    load_wavetable(root, mode_c, "oscillator C", diagnostics);
            }
        }

        patch
    }
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "Preset offset overflow".to_string())?;
    let slice = bytes
        .get(*offset..end)
        .ok_or_else(|| "Preset is truncated".to_string())?;
    *offset = end;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
fn json_to_cbor(value: Value) -> ciborium::value::Value {
    use ciborium::value::Value as C;
    match value {
        Value::Null => C::Null,
        Value::Bool(value) => C::Bool(value),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                C::Integer(value.into())
            } else if let Some(value) = value.as_u64() {
                C::Integer(value.into())
            } else {
                C::Float(value.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(value) => C::Text(value),
        Value::Array(values) => C::Array(values.into_iter().map(json_to_cbor).collect()),
        Value::Object(object) => C::Map(
            object
                .into_iter()
                .map(|(key, value)| (C::Text(key), json_to_cbor(value)))
                .collect(),
        ),
    }
}

fn cbor_to_json(value: &ciborium::value::Value) -> Value {
    use ciborium::value::Value as C;
    match value {
        C::Null => Value::Null,
        C::Bool(value) => Value::Bool(*value),
        C::Integer(value) => {
            let number = i128::from(*value);
            if number >= i64::MIN as i128 && number <= i64::MAX as i128 {
                json!(number as i64)
            } else {
                json!(number.to_string())
            }
        }
        C::Float(value) => json!(value),
        C::Text(value) => json!(value),
        C::Bytes(value) => Value::Array(value.iter().map(|byte| json!(byte)).collect()),
        C::Array(values) => Value::Array(values.iter().map(cbor_to_json).collect()),
        C::Map(entries) => {
            let mut object = serde_json::Map::new();
            for (key, value) in entries {
                let key = match key {
                    C::Text(text) => text.clone(),
                    other => format!("{other:?}"),
                };
                object.insert(key, cbor_to_json(value));
            }
            Value::Object(object)
        }
        C::Tag(_, value) => cbor_to_json(value),
        _ => Value::Null,
    }
}

fn plain_num(module: Option<&Value>, key: &str) -> Option<f64> {
    module
        .and_then(|module| module.get("plainParams"))
        .and_then(|params| params.get(key))
        .and_then(Value::as_f64)
}

/// CBOR integers become JSON integers, but a hand-written or reserialized tree
/// may store IDs as floats; accept both.
fn number_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number.round() as i64))
}

fn plain_str<'a>(module: Option<&'a Value>, key: &str) -> Option<&'a str> {
    module
        .and_then(|module| module.get("plainParams"))
        .and_then(|params| params.get(key))
        .and_then(Value::as_str)
}

fn flag(module: Option<&Value>, key: &str, fallback: bool) -> bool {
    plain_num(module, key)
        .map(|value| value >= 0.5)
        .unwrap_or(fallback)
}

fn enabled(module: Option<&Value>, fallback: bool) -> bool {
    flag(module, "kParamEnable", fallback)
}

fn osc_mode<'a>(oscillator: Option<&'a Value>, index: usize) -> Option<(&'static str, &'a Value)> {
    const KINDS: [&str; 7] = [
        "WTOsc",
        "SampleOsc",
        "MultiSampleOsc",
        "GranularOsc",
        "SpectralOsc",
        "NoiseOsc",
        "SubOsc",
    ];
    let oscillator = oscillator?;
    // the preset keeps an inactive mode object for every oscillator family
    // (`plainParams: "default"`). The active family is recorded in the
    // oscillator's top-level `kParamType`; when that key is absent, score the
    // individual mode objects instead of assuming WTOsc comes first.
    let declared = plain_str(Some(oscillator), "kParamType").and_then(|kind| {
        let kind = match kind {
            "kOsc_Wavetable" => "WTOsc",
            "kOsc_Sample" => "SampleOsc",
            "kOsc_MultiSample" => "MultiSampleOsc",
            "kOsc_Granular" => "GranularOsc",
            "kOsc_Spectral" => "SpectralOsc",
            "kOsc_Noise" => "NoiseOsc",
            "kOsc_Sub" => "SubOsc",
            _ => return None,
        };
        KINDS.iter().copied().find(|candidate| *candidate == kind)
    });
    if let Some(declared) = declared {
        let key = format!("{declared}{index}");
        if let Some(module) = oscillator.get(&key)
            && mode_score(module) > 0
        {
            return Some((declared, module));
        }
    }
    let mut best: Option<(&'static str, &'a Value, u8)> = None;
    for kind in KINDS {
        let key = format!("{kind}{index}");
        let Some(module) = oscillator.get(&key) else {
            continue;
        };
        let score = mode_score(module);
        let replace = match best {
            None => true,
            Some((_, _, best_score)) => score > best_score,
        };
        if replace {
            best = Some((kind, module, score));
        }
    }
    // Preserve the old "first present mode" behavior when every object is
    // marked default; the caller maps it at zero level anyway.
    let (kind, module, _) = best?;
    Some((kind, module))
}

fn mode_score(mode: &Value) -> u8 {
    let Some(object) = mode.as_object() else {
        return 0;
    };
    let has_content = object.iter().any(|(key, value)| {
        key != "plainParams" && !value.is_null() && value.as_str() != Some("default")
    });
    if has_content {
        return 3;
    }
    match object.get("plainParams") {
        Some(Value::Object(params)) if !params.is_empty() => 2,
        Some(Value::Object(_)) => 1,
        Some(Value::String(text)) if text != "default" => 2,
        Some(Value::Null) | None => 1,
        _ => 0,
    }
}

fn osc_level(module: Option<&Value>, fallback: f64) -> f64 {
    osc_level_enabled(module, true, fallback)
}

/// Level for an oscillator whose enable flag has its own the preset default. Slot 0
/// defaults to enabled; every other slot defaults to disabled.
fn osc_level_enabled(module: Option<&Value>, enabled_by_default: bool, fallback: f64) -> f64 {
    if !enabled(module, enabled_by_default) {
        return 0.0;
    }
    plain_num(module, "kParamVolume")
        .unwrap_or(fallback)
        .clamp(0.0, 1.0)
}

/// the preset octave/coarse/fine controls in cents. Sample layers use this because
/// they do not go through the wavetable oscillator's semitone fields.
fn oscillator_tune_cents(module: Option<&Value>) -> f64 {
    let octave = plain_num(module, "kParamOctave").unwrap_or(0.0);
    let pitch = plain_num(module, "kParamPitch")
        .or_else(|| plain_num(module, "kParamCoarsePit"))
        .or_else(|| plain_num(module, "kParamCoarse"))
        .unwrap_or(0.0);
    let fine = plain_num(module, "kParamFine").unwrap_or(0.0);
    octave * 1200.0 + pitch * 100.0 + fine
}

/// Append a the preset sample/multisample slot to the patch's sampler layer.
/// Returns whether the slot was mapped. A slot that cannot be mapped is left
/// silent by the caller; it is never replaced by a default wavetable.
#[allow(clippy::too_many_arguments)]
fn add_sample_layer(
    target: &mut Option<SamplerPatch>,
    kind: &str,
    mode: &Value,
    oscillator: Option<&Value>,
    level: f64,
    label: &str,
    content_root: Option<&Path>,
    diagnostics: &mut ImportDiagnostics,
) -> bool {
    if level <= 0.0 {
        return true;
    }
    let Some(mut layer) = sample_layer_patch(kind, mode, content_root, label, diagnostics) else {
        return false;
    };
    // the preset's sample and multisample oscillators are mono layers.
    layer.mono = true;
    // the preset's `amp_veltrack` is an exponent, not linear SFZ tracking.
    layer.velocity_curve = VelocityCurve::Preset;
    if layer.regions.is_empty() {
        diagnostics.warn(
            format!("sample:{label}:regions"),
            format!("{label} {kind} resolved no playable regions; oscillator level set to 0"),
        );
        return false;
    }
    let pan = oscillator_pan(oscillator);
    let tune = oscillator_tune_cents(oscillator);
    let unison = osc_unison(oscillator);
    let unison_detune = osc_detune(oscillator);
    let unison_width = osc_width(oscillator);
    // the preset's sample Level knob is unity at 0.5, i.e. twice the wavetable law.
    let level = level * SAMPLE_OSCILLATOR_LEVEL_GAIN;
    for region in &mut layer.regions {
        region.gain *= level;
        region.pan = (region.pan + pan).clamp(-1.0, 1.0);
        region.tune_cents += tune;
        region.unison = unison.max(1) as u32;
        region.unison_detune_cents = unison_detune;
        region.unison_width = unison_width;
    }
    for diagnostic in &layer.diagnostics {
        diagnostics.warn(
            sample_diagnostic_class(label, diagnostic),
            diagnostic.clone(),
        );
    }
    let target = target.get_or_insert_with(SamplerPatch::default);
    target.mono |= layer.mono;
    target.velocity_curve = layer.velocity_curve;
    if target.sfz.is_none() {
        target.sfz = layer.sfz.take();
    }
    target.regions.extend(layer.regions);
    target.diagnostics.extend(layer.diagnostics);
    true
}

/// Group SFZ diagnostics by opcode ("unsupported opcode 'x'") or by source, so
/// a preset scan counts opcode classes rather than line numbers.
fn sample_diagnostic_class(label: &str, diagnostic: &str) -> String {
    if let Some(rest) = diagnostic.split_once("unsupported opcode '") {
        let opcode = rest.1.split('\'').next().unwrap_or("unknown");
        return format!("sample-sfz-opcode:{opcode}");
    }
    format!("sample:{label}:sfz")
}

/// Resolve the file-backed source shared by the granular and spectral modes.
/// Returns the sample path, root note and static start position in seconds.
fn source_asset(
    kind: &str,
    mode: &Value,
    content_root: Option<&Path>,
    label: &str,
    diagnostics: &mut ImportDiagnostics,
) -> Option<(PathBuf, f64, f64)> {
    let raw = mode
        .get("samplePath")
        .or_else(|| mode.get("samplePathRelative"))
        .and_then(Value::as_str);
    let Some(raw) = raw else {
        if mode.get("embedded_audio").is_some() {
            diagnostics.warn(
                format!("oscillator:{kind}"),
                format!(
                    "{label} {kind} uses embedded_audio; only file-backed sources are supported"
                ),
            );
        } else {
            diagnostics.warn(
                format!("oscillator:{kind}"),
                format!("{label} {kind} has no sample path"),
            );
        }
        return None;
    };
    let roots = sample_search_roots(content_root, None);
    let Some(sample) = resolve_sample_path(raw, &roots) else {
        diagnostics.warn(
            format!("oscillator:{kind}"),
            format!("{label} {kind} sample '{raw}' was not found under the preset content root"),
        );
        return None;
    };
    let root_note = mode.get("baseNote").and_then(number_as_i64).unwrap_or(60) as f64;
    let rate = mode
        .get("sampleRate")
        .and_then(number_as_i64)
        .unwrap_or(44_100)
        .max(1) as f64;
    let position = mode
        .get("trimHead")
        .and_then(number_as_i64)
        .unwrap_or(0)
        .max(0) as f64
        / rate;
    Some((sample, root_note, position))
}

fn grain_window(name: Option<&str>) -> GrainWindow {
    match name {
        Some("kWindowExpDec") => GrainWindow::ExpDec,
        Some("kWindowTriangle") => GrainWindow::Triangle,
        Some("kWindowBlackmanHarris") => GrainWindow::BlackmanHarris,
        Some("kWindowGaussian") => GrainWindow::Gaussian,
        Some("kWindowTukey") => GrainWindow::Tukey,
        _ => GrainWindow::Hann,
    }
}

fn percent_param(params: Option<&Value>, key: &str) -> f64 {
    (plain_num(params, key).unwrap_or(0.0) / 100.0).clamp(0.0, 1.0)
}

fn granular_patch(
    kind: &str,
    mode: &Value,
    content_root: Option<&Path>,
    label: &str,
    seed: u64,
    diagnostics: &mut ImportDiagnostics,
) -> Option<GranularPatch> {
    let (sample, root_note, position) = source_asset(kind, mode, content_root, label, diagnostics)?;
    let params = Some(mode);
    let present = |key: &str| {
        mode.get("plainParams")
            .and_then(|params| params.get(key))
            .is_some()
    };
    if present("kParamWarpMenu")
        || present("kParamWarpMenu2")
        || present("kParamRandomWarp")
        || present("kParamRandomWarp2")
    {
        diagnostics.warn(
            "granular:warp",
            format!("{label} GranularOsc grain warp has no engine equivalent"),
        );
    }
    if present("kParamDensityMode")
        && plain_str(params, "kParamDensityMode") != Some("kDensityFree")
    {
        diagnostics.warn(
            "granular:density-mode",
            format!("{label} GranularOsc tempo-synced density renders as free grains/second"),
        );
    }
    if present("kParamLengthMode") {
        diagnostics.warn(
            "granular:length-mode",
            format!("{label} GranularOsc grain-length mode renders as a fixed length"),
        );
    }
    if present("kParamUnisonTrigPattern") {
        diagnostics.warn(
            "granular:unison-trigger",
            format!("{label} GranularOsc unison trigger pattern is not modelled"),
        );
    }
    if present("kParamRandomWindowSkew") || present("kParamRandomWindowAmount") {
        diagnostics.warn(
            "granular:random-window",
            format!("{label} GranularOsc per-grain window randomisation is not modelled"),
        );
    }
    if present("kParamYAxisAssignment") {
        diagnostics.warn(
            "granular:y-axis",
            format!("{label} GranularOsc Y-axis assignment is UI state only"),
        );
    }
    if mode.get("edgeFadeMs").is_some() {
        diagnostics.warn(
            "granular:edge-fade",
            format!("{label} GranularOsc edge fade is applied by the grain window"),
        );
    }
    if mode.get("gain").is_some() {
        diagnostics.warn(
            "granular:gain",
            format!("{label} GranularOsc source gain is not applied"),
        );
    }
    // Defaults when the preset leaves the control at its factory value.
    let density = plain_num(params, "kParamDensity")
        .unwrap_or(20.0)
        .clamp(0.0, 4096.0);
    let grain_length = plain_num(params, "kParamGrainLength")
        .unwrap_or(0.1)
        .clamp(0.002, 10.0);
    Some(GranularPatch {
        sample,
        root_note,
        density,
        grain_length,
        position,
        loop_start: 0.0,
        loop_end: 0.0,
        ping_pong: false,
        scan_rate: 0.0,
        window: grain_window(plain_str(params, "kParamWindowShape")),
        window_param: plain_num(params, "kParamWindowParam")
            .map(|value| (value / 100.0).clamp(0.0, 1.0))
            .unwrap_or(0.5),
        window_skew: (plain_num(params, "kParamWindowSkew").unwrap_or(0.0) / 100.0)
            .clamp(-1.0, 1.0),
        random_offset: percent_param(params, "kParamRandomOffset"),
        random_length: percent_param(params, "kParamRandomGrainLength"),
        random_pitch: plain_num(params, "kParamRandomPitch")
            .unwrap_or(0.0)
            .clamp(0.0, 48.0),
        random_direction: percent_param(params, "kParamRandomDir"),
        random_pan: percent_param(params, "kParamRandomPan"),
        random_gain: percent_param(params, "kParamRandomGain"),
        reverse: false,
        pitch_track: true,
        seed,
    })
}

fn spectral_warp(name: &str) -> Option<SpectralWarp> {
    Some(match name {
        "kSpectralComb" => SpectralWarp::Comb,
        "kGate" => SpectralWarp::Gate,
        "kSmear" => SpectralWarp::Smear,
        "kSpread" => SpectralWarp::Spread,
        "kDetune" => SpectralWarp::Detune,
        // Both names scale the frequency axis, so both use the pitch warp.
        "kSpectralPitchShift" | "kSpectralShift" => SpectralWarp::PitchShift,
        "kAddharmonics" => SpectralWarp::AddHarmonics,
        "kAddsubharmonics" => SpectralWarp::AddSubharmonics,
        "kShepardFilter" | "kShepardNarrow" => SpectralWarp::Shepard,
        "kPeakOctaveUp" | "kPeakOctaveDown" | "kPeakHarmUp" | "kPeakHarmDown" => {
            SpectralWarp::PeakShift
        }
        "kMirror" => SpectralWarp::Mirror,
        "kSpectralPhaseTwist" => SpectralWarp::PhaseTwist,
        _ => return None,
    })
}

fn spectral_patch(
    kind: &str,
    mode: &Value,
    content_root: Option<&Path>,
    label: &str,
    seed: u64,
    diagnostics: &mut ImportDiagnostics,
) -> Option<SpectralPatch> {
    let (sample, root_note, position) = source_asset(kind, mode, content_root, label, diagnostics)?;
    let params = Some(mode);
    let mut warp_slot = |menu: &str, amount: &str| -> (SpectralWarp, f64) {
        let name = plain_str(params, menu);
        let Some(name) = name else {
            return (SpectralWarp::Off, 0.0);
        };
        match spectral_warp(name) {
            Some(warp) => {
                let amount = plain_num(params, amount).unwrap_or(0.0).clamp(0.0, 1.0);
                (warp, amount)
            }
            None => {
                diagnostics.warn(
                    format!("spectral:warp:{name}"),
                    format!("{label} SpectralOsc warp '{name}' has no spectral-engine mapping"),
                );
                (SpectralWarp::Off, 0.0)
            }
        }
    };
    let (warp, morph) = warp_slot("kParamWarpMenu", "kParamWarp");
    let (warp2, morph2) = warp_slot("kParamWarpMenu2", "kParamWarp2");
    if plain_num(params, "kParamWarpVar").is_some() || plain_num(params, "kParamWarpVar2").is_some()
    {
        diagnostics.warn(
            "spectral:warp-var",
            format!("{label} SpectralOsc warp variation is folded out"),
        );
    }
    if plain_num(params, "kParamSpecFltWetDry").is_some_and(|wet| wet < 100.0) {
        diagnostics.warn(
            "spectral:filter-mix",
            format!("{label} SpectralOsc spectral-filter mix applies the shift at full depth"),
        );
    }
    if mode
        .get("flex")
        .and_then(Value::as_object)
        .is_some_and(|flex| !flex.is_empty())
    {
        diagnostics.warn(
            "spectral:flex",
            format!("{label} SpectralOsc custom source-position curve is not modelled"),
        );
    }
    // The spectral filter mix scales the shift; at the default 0 the shift is
    // inaudible, so folding the mix into `bend` keeps the preset's depth.
    let wet = plain_num(params, "kParamSpecFltWetDry")
        .unwrap_or(0.0)
        .clamp(0.0, 100.0)
        / 100.0;
    let bend = (plain_num(params, "kParamSpecFltShift").unwrap_or(0.0)
        + plain_num(Some(mode), "baseDetune").unwrap_or(0.0) / 100.0)
        * wet;
    Some(SpectralPatch {
        sample,
        root_note,
        position,
        loop_start: 0.0,
        loop_end: 0.0,
        ping_pong: false,
        scan_rate: 0.0,
        pitch_track: true,
        warp,
        morph,
        warp2,
        morph2,
        bend: bend.clamp(-48.0, 48.0),
        phase_lock: if flag(params, "kParamPhaseLock", false) {
            1.0
        } else {
            0.0
        },
        transients: flag(params, "kParamTransients", false),
        low_hz: plain_num(params, "kParamFreqLo")
            .unwrap_or(20.0)
            .clamp(0.0, 24_000.0),
        high_hz: plain_num(params, "kParamFreqHi")
            .unwrap_or(20_000.0)
            .clamp(0.0, 24_000.0),
        band_smooth: flag(params, "kParamLoHiIsSmooth", false),
        seed,
    })
}

fn sample_layer_patch(
    kind: &str,
    mode: &Value,
    content_root: Option<&Path>,
    label: &str,
    diagnostics: &mut ImportDiagnostics,
) -> Option<SamplerPatch> {
    match kind {
        "MultiSampleOsc" => {
            let relative = mode.get("sfzPathRelative").and_then(Value::as_str);
            let roots = sample_search_roots(content_root, relative);
            if let Some(text) = mode.get("embedded_sfz").and_then(Value::as_str) {
                return Some(patch_from_sfz_text(text, &roots));
            }
            let Some(relative) = relative else {
                diagnostics.warn(
                    format!("sample:{label}:path"),
                    format!("{label} MultiSampleOsc has neither embedded_sfz nor sfzPathRelative"),
                );
                return None;
            };
            let relative = relative.replace('\\', "/");
            for root in &roots {
                let path = root.join(&relative);
                if path.is_file() {
                    return patch_from_sfz_file(&path, &roots).ok();
                }
            }
            diagnostics.warn(
                format!("sample:{label}:path"),
                format!(
                    "{label} multisample SFZ '{relative}' was not found under the preset content root"
                ),
            );
            None
        }
        "SampleOsc" => {
            if let Some(text) = mode.get("embedded_sfz").and_then(Value::as_str) {
                let roots = sample_search_roots(content_root, None);
                return Some(patch_from_sfz_text(text, &roots));
            }
            // the preset stores file-backed sample slots as `samplePathRelative`
            // (relative to the preset root); the older `samplePath` is still
            // accepted when it appears.
            let raw = mode
                .get("samplePath")
                .or_else(|| mode.get("samplePathRelative"))
                .and_then(Value::as_str);
            let Some(raw) = raw else {
                if mode.get("embedded_audio").is_some() {
                    diagnostics.warn(
                        format!("sample:{label}:embedded-audio"),
                        format!(
                            "{label} SampleOsc uses embedded_audio; only file-backed samples are supported"
                        ),
                    );
                } else {
                    diagnostics.warn(
                        format!("sample:{label}:path"),
                        format!("{label} SampleOsc has no sample path"),
                    );
                }
                return None;
            };
            let roots = sample_search_roots(content_root, None);
            let Some(sample) = resolve_sample_path(raw, &roots) else {
                diagnostics.warn(
                    format!("sample:{label}:path"),
                    format!(
                        "{label} SampleOsc sample '{raw}' was not found under the preset content root"
                    ),
                );
                return None;
            };
            let root_note = mode.get("baseNote").and_then(number_as_i64).unwrap_or(60) as f64;
            Some(SamplerPatch {
                regions: vec![SampleRegion {
                    sample,
                    lokey: 0,
                    hikey: 127,
                    lovel: 0,
                    hivel: 127,
                    pitch_keycenter: root_note,
                    loop_start: None,
                    loop_end: None,
                    loop_mode: LoopMode::NoLoop,
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
            })
        }
        _ => None,
    }
}

fn log_unsupported_oscillator(
    kind: &str,
    slot: usize,
    level: f64,
    diagnostics: &mut ImportDiagnostics,
) {
    diagnostics.warn(
        format!("oscillator:{kind}"),
        format!("{kind}{slot} has no sampler mapping; oscillator level set to 0 (was {level:.3})"),
    );
}

/// Candidate roots for the preset sample files. Embedded SFZ paths are relative to
/// the directory of the corresponding sidecar SFZ, so the parent of
/// `sfzPathRelative` is included before generic content roots.
fn sample_search_roots(content_root: Option<&Path>, sfz_relative: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Some(content_root) = content_root else {
        return roots;
    };
    let sfz_parent = sfz_relative
        .map(|relative| relative.replace('\\', "/"))
        .and_then(|relative| {
            Path::new(&relative)
                .parent()
                .map(std::path::Path::to_path_buf)
        });
    let mut current = Some(content_root.to_path_buf());
    for _ in 0..6 {
        let Some(path) = current else { break };
        push_unique(&mut roots, path.clone());
        push_unique(&mut roots, path.join("Multisamples"));
        push_unique(&mut roots, path.join("Multisamples").join("Factory"));
        push_unique(&mut roots, path.join("Factory"));
        // File-backed sample oscillators store paths relative to `Samples`.
        push_unique(&mut roots, path.join("Samples"));
        push_unique(&mut roots, path.join("Samples").join("Factory"));
        if let Some(parent) = &sfz_parent {
            push_unique(&mut roots, path.join(parent));
            push_unique(&mut roots, path.join("Multisamples").join(parent));
        }
        current = path.parent().map(Path::to_path_buf);
    }
    roots
}

fn push_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.contains(&path) {
        roots.push(path);
    }
}

/// Find the preset content root from a preset path. Used when a project omits
/// `serum_root` but the preset sits inside a normal installation.
pub fn infer_content_root(preset: &Path) -> Option<PathBuf> {
    for ancestor in preset.ancestors().take(8) {
        if ancestor.join("Tables").is_dir() || ancestor.join("Multisamples").is_dir() {
            return Some(ancestor.to_path_buf());
        }
        if ancestor
            .file_name()
            .is_some_and(|name| name == "Tables" || name == "Multisamples")
        {
            return ancestor.parent().map(Path::to_path_buf);
        }
    }
    None
}

fn osc_unison(module: Option<&Value>) -> usize {
    plain_num(module, "kParamUnison")
        .unwrap_or(1.0)
        .round()
        .clamp(1.0, 16.0) as usize
}

fn osc_detune(module: Option<&Value>) -> f64 {
    preset_detune_cents(plain_num(module, "kParamDetune").unwrap_or(0.0))
}

/// Half-span of a unison stack from the preset's stored detune. A stored value
/// is the square of the control: 0.16 renders the same pair as the 0.4 control,
/// and the stored law puts that control at plus/minus `200 * knob^2.5` cents
/// (0.04 -> +/-3.58, 0.16 -> +/-20.2, 0.25 -> +/-35.4, 0.5 -> +/-84.1), so the
/// stored value maps to `200 * value^1.25` cents either side. The same cents at
/// C4 and C6 make it a frequency ratio. Legacy values above 1.0 are cents.
fn preset_detune_cents(value: f64) -> f64 {
    if value <= 1.0 {
        value.clamp(0.0, 1.0).powf(1.25) * 200.0
    } else {
        value.clamp(0.0, 100.0)
    }
}

/// the preset's per-oscillator unison stereo knob is `kParamUnisonStereo` and
/// defaults to 100; `kParamDetuneWid` is a detune-curve control it ignores.
fn osc_width(module: Option<&Value>) -> f64 {
    (plain_num(module, "kParamUnisonStereo").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0)
}

fn table_position(module: &Value) -> Option<f64> {
    let raw = plain_num(Some(module), "kParamTablePos")?;
    Some(((raw - 1.0) / 255.0).clamp(0.0, 1.0))
}

/// the preset pans each oscillator in `-50..=50`; the engine uses `-1.0..=1.0`.
fn oscillator_pan(module: Option<&Value>) -> f64 {
    (plain_num(module, "kParamPan").unwrap_or(0.0) / 50.0).clamp(-1.0, 1.0)
}

/// The 70 imported warp enum names, in enum order. Entries 33..69
/// are class 0 in the format: selectable but inert (66..69 are a flat gain),
/// so they are imported with their real identity rather than collapsed.
const WARP_MODE_NAMES: [(&str, WarpMode); 70] = [
    ("kNoWarp", WarpMode::Off),
    ("kSync", WarpMode::Sync),
    ("kBendPos", WarpMode::BendPos),
    ("kBendNeg", WarpMode::BendNeg),
    ("kBendPosNeg", WarpMode::BendPosNeg),
    ("kPWM", WarpMode::Pwm),
    ("kASYMPos", WarpMode::AsymPos),
    ("kASYMNeg", WarpMode::AsymNeg),
    ("kASYMPosNeg", WarpMode::AsymPosNeg),
    ("kFlip", WarpMode::Flip),
    ("kDLM", WarpMode::Dlm),
    ("kRemap_1", WarpMode::Remap1),
    ("kRemap_2", WarpMode::Remap2),
    ("kRemap_3", WarpMode::Remap3),
    ("kRemap_4", WarpMode::Remap4),
    ("kQuantize", WarpMode::Quantize),
    ("kEvenOdd", WarpMode::EvenOdd),
    ("kFilterLPF", WarpMode::FilterLpf),
    ("kFilterHPF", WarpMode::FilterHpf),
    ("kDistTube", WarpMode::DistTube),
    ("kDistSoftClip", WarpMode::DistSoftClip),
    ("kDistHardClip", WarpMode::DistHardClip),
    ("kDistDiode1", WarpMode::DistDiode1),
    ("kDistDiode2", WarpMode::DistDiode2),
    ("kDistLinFold", WarpMode::DistLinFold),
    ("kDistSinFold", WarpMode::DistSinFold),
    ("kDistZeroSquare", WarpMode::DistZeroSquare),
    ("kDistAsym", WarpMode::DistAsym),
    ("kDistRectify", WarpMode::DistRectify),
    ("kDistSineShaper", WarpMode::DistSineShaper),
    ("kDistStompBox", WarpMode::DistStompBox),
    ("kDistTapeSat", WarpMode::DistTapeSat),
    ("kDistSoftSat", WarpMode::DistSoftSat),
    ("kFM_OSC", WarpMode::FmOsc),
    ("kFM_OSC2", WarpMode::FmOsc2),
    ("kFM_NOISE", WarpMode::FmNoise),
    ("kFM_SUB", WarpMode::FmSub),
    ("kFM_FILT1", WarpMode::FmFilt1),
    ("kFM_FILT2", WarpMode::FmFilt2),
    ("kFMX_OSC", WarpMode::FmxOsc),
    ("kFMX_OSC2", WarpMode::FmxOsc2),
    ("kFMX_NOISE", WarpMode::FmxNoise),
    ("kFMX_SUB", WarpMode::FmxSub),
    ("kFMX_FILT1", WarpMode::FmxFilt1),
    ("kFMX_FILT2", WarpMode::FmxFilt2),
    ("kFMP_OSC", WarpMode::FmpOsc),
    ("kFMP_OSC2", WarpMode::FmpOsc2),
    ("kFMP_NOISE", WarpMode::FmpNoise),
    ("kFMP_SUB", WarpMode::FmpSub),
    ("kFMP_FILT1", WarpMode::FmpFilt1),
    ("kFMP_FILT2", WarpMode::FmpFilt2),
    ("kPD_OSC", WarpMode::PdOsc),
    ("kPD_OSC2", WarpMode::PdOsc2),
    ("kPD_NOISE", WarpMode::PdNoise),
    ("kPD_SUB", WarpMode::PdSub),
    ("kPD_FILT1", WarpMode::PdFilt1),
    ("kPD_FILT2", WarpMode::PdFilt2),
    ("kSelfPD", WarpMode::SelfPd),
    ("kAM_OSC", WarpMode::AmOsc),
    ("kAM_OSC2", WarpMode::AmOsc2),
    ("kAM_NOISE", WarpMode::AmNoise),
    ("kAM_SUB", WarpMode::AmSub),
    ("kAM_FILT1", WarpMode::AmFilt1),
    ("kAM_FILT2", WarpMode::AmFilt2),
    ("kRM_OSC", WarpMode::RmOsc),
    ("kRM_OSC2", WarpMode::RmOsc2),
    ("kRM_NOISE", WarpMode::RmNoise),
    ("kRM_SUB", WarpMode::RmSub),
    ("kRM_FILT1", WarpMode::RmFilt1),
    ("kRM_FILT2", WarpMode::RmFilt2),
];

/// Map the preset's `kParamWarpMenu` name to the engine mode. The menu is
/// serialised by name, so the whole enum is mapped (including the inert
/// families); unknown names fall back to `Off`.
fn warp_mode(module: &Value) -> WarpMode {
    plain_str(Some(module), "kParamWarpMenu")
        .and_then(|name| {
            WARP_MODE_NAMES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, mode)| *mode)
        })
        .unwrap_or(WarpMode::Off)
}

fn warp_amount(module: &Value, suffix: &str) -> f64 {
    plain_num(Some(module), &format!("kParamWarp{suffix}"))
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

fn warp_var(module: &Value, suffix: &str) -> f64 {
    plain_num(Some(module), &format!("kParamWarpVar{suffix}"))
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

fn warp_mode2(module: &Value) -> WarpMode {
    plain_str(Some(module), "kParamWarpMenu2")
        .and_then(|name| {
            WARP_MODE_NAMES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, mode)| *mode)
        })
        .unwrap_or(WarpMode::Off)
}

/// Map the preset's sub-oscillator shape IDs. The shape lives under
/// `Oscillator4.SubOsc4.plainParams`, not on the slot itself.
fn sub_waveform(module: Option<&Value>) -> SubShape {
    let shape = module
        .and_then(|module| module.get("SubOsc4"))
        .and_then(|mode| mode.get("plainParams"))
        .and_then(|params| params.get("kParamShape"))
        .and_then(Value::as_str);
    match shape {
        Some("kRoundRect") => SubShape::RoundRect,
        Some("kTriangle") => SubShape::Triangle,
        Some("kSaw") => SubShape::Saw,
        Some("kSquare") => SubShape::Square,
        Some("kPulse") => SubShape::Pulse,
        _ => SubShape::Sine,
    }
}

fn degrees(value: f64) -> f64 {
    (value / 360.0).clamp(0.0, 1.0)
}

/// the preset stores oscillator transpose as octave + semitone + fine controls.
/// `kParamPitch` is the live semitone knob; `kParamCoarsePit` is the older
/// wide control and is only a fallback.
fn oscillator_semitones(module: Option<&Value>) -> f64 {
    let octave = plain_num(module, "kParamOctave").unwrap_or(0.0);
    let pitch = plain_num(module, "kParamPitch")
        .or_else(|| plain_num(module, "kParamCoarsePit"))
        .or_else(|| plain_num(module, "kParamCoarse"))
        .unwrap_or(0.0);
    let fine = plain_num(module, "kParamFine").unwrap_or(0.0);
    (octave * 12.0 + pitch + fine / 100.0).clamp(-48.0, 48.0)
}

/// the preset's `Main Vol` controller default: 50 % on the UI, -9 dB of gain.
/// the preset's sample-oscillator Level is 0 dB at 0.5, so it is 2x the wavetable Level.
const SAMPLE_OSCILLATOR_LEVEL_GAIN: f64 = 2.0;
const MASTER_VOLUME_DEFAULT: f64 = 0.354_813_389_233_575_47;

fn cutoff_hz(normalized: f64) -> f64 {
    // MIDI note 0 (8.1758 Hz) at x=0, 22050 Hz at x=1. The minimum is
    // deliberately below the engine's old 20 Hz floor: a low-pitched preset
    // such as Sweet Bass stores x=0 and relies on modulation to open the
    // filter, and clamping the base to 20 Hz shifts every modulated cutoff up
    // by ~1.3 octaves.
    let min_hz = 8.175_798_915_643_707;
    (min_hz * 2f64.powf(11.4 * normalized.clamp(0.0, 1.0))).clamp(min_hz, 22_050.0)
}

/// the preset stores `kParamReso`/`kParamDrive` on a 0..100 scale, so 0.5 means
/// 0.5% (0.005), not 50%. Always divide by 100; do not guess from magnitude.
fn normalize_percent(value: f64) -> f64 {
    (value / 100.0).clamp(0.0, 1.0)
}

/// `Bypass` is a real filter type (registry id −1), not a response family.
fn filter_type_bypassed(module: Option<&Value>) -> bool {
    plain_str(module, "kParamType").is_some_and(|id| id.eq_ignore_ascii_case("Bypass"))
}

/// Map the serial/parallel dual-filter topology of the preset.
///
/// `RoutingSlot0..4` are the five sound sources (A, B, C, noise, sub) and
/// `RoutingSlot5..6` are the Filter 1 and Filter 2 outputs. A filter output
/// routed to `kRoutingDestFilter` feeds the other filter; absent output slots
/// default to Main. An explicit filter destination on
/// either output is therefore a serial (possibly feedback) chain, while an
/// enabled Filter 2 with both outputs going elsewhere runs in parallel.
///
/// `kParamFilterBalance` is deliberately not used as topology evidence: it is
/// a per-source blend between the two filters (stored default `-100` =
/// Filter 1 only) and means the same thing inside either topology.
fn detect_filter_routing(data: &Value) -> FilterRouting {
    let serial_chain = ["RoutingSlot5", "RoutingSlot6"].iter().any(|key| {
        matches!(
            plain_str(data.get(*key), "kParamRoutingDest"),
            Some("kRoutingDestFilter")
        )
    });
    if serial_chain {
        return FilterRouting::Serial;
    }
    if enabled(data.get("VoiceFilter1"), false) {
        FilterRouting::Parallel
    } else {
        FilterRouting::Serial
    }
}

/// Per-source filter routing in the preset. `RoutingSlot0..4` are oscillator A, B, C,
/// noise and sub; slots 5/6 only carry the serial edge. A source's destination
/// is Filter, Master, Direct or None. `kParamFilterBalance` is bipolar:
/// -100 = filter 1, +100 = filter 2 (default -100), split as
/// `(balance + 100) / 200`.
///
/// A slot without an explicit destination keeps the legacy filter-1 path: the
/// defaults for B..sub are not modelled here (the preset writes Filter explicitly when a
/// preset wants it, and the format also writes redundant Filter rows). So does
/// `kRoutingDestNone`, which maps to Bypass rather than mute; reverse serial
/// (slot 6 feeding slot 5) still renders forward.
fn source_filter_routes(data: &Value) -> Option<[SourceFilterRoute; 5]> {
    let explicit = (0..5).any(|index| {
        data.get(format!("RoutingSlot{index}"))
            .and_then(|module| module.get("plainParams"))
            .is_some_and(Value::is_object)
    });
    if !explicit {
        return None;
    }
    let mut routes = [SourceFilterRoute::default(); 5];
    for (index, route) in routes.iter_mut().enumerate() {
        let module = data.get(format!("RoutingSlot{index}"));
        let Some(destination) = plain_str(module, "kParamRoutingDest") else {
            continue;
        };
        if destination != "kRoutingDestFilter" {
            route.target = SourceFilterTarget::Bypass;
            continue;
        }
        let balance = plain_num(module, "kParamFilterBalance").unwrap_or(-100.0);
        let share = ((balance + 100.0) / 200.0).clamp(0.0, 1.0);
        route.target = if share <= 0.0 {
            SourceFilterTarget::Filter1
        } else if share >= 1.0 {
            SourceFilterTarget::Filter2
        } else {
            SourceFilterTarget::Both
        };
        route.balance = share;
    }
    Some(routes)
}

fn envelope(data: &Value, index: usize) -> (f64, f64, f64, f64, f64) {
    let module = data.get(format!("Env{index}"));
    let attack = plain_num(module, "kParamAttack")
        .unwrap_or(if index == 0 { 0.001 } else { 0.0005 })
        .clamp(0.0, 10.0);
    let decay = plain_num(module, "kParamDecay")
        .unwrap_or(1.0)
        .clamp(0.0, 10.0);
    let sustain = plain_num(module, "kParamSustain")
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let release = plain_num(module, "kParamRelease")
        .unwrap_or(0.015)
        .clamp(0.0, 20.0);
    let curve = (plain_num(module, "kParamCurve1").unwrap_or(50.0) - 50.0) / 50.0;
    (attack, decay, sustain, release, curve.clamp(-1.0, 1.0))
}

/// The preset stores the free rate in real Hz (1:1); `kParamRate10x` is the
/// x10 switch, and synced rates select one of the 43 division steps.
fn lfo_rate(module: Option<&Value>) -> f64 {
    plain_num(module, "kParamRate")
        .unwrap_or(6.25)
        .clamp(0.0, 100.0)
}

/// `kParamBeatSync` defaults to on when the key is absent.
fn lfo_sync(module: Option<&Value>) -> bool {
    flag(module, "kParamBeatSync", true)
}

/// `kParamMode` is `Free`, `Trigger`, `Envelope`, or `OneShot`. The engine has
/// a single retrigger flag, so every non-Free mode retriggers.
fn lfo_retrigger(module: Option<&Value>) -> bool {
    plain_str(module, "kParamMode")
        .map(|mode| mode != "Free")
        .unwrap_or(true)
}

/// Retired six-step selector, kept so exported patches keep a readable field.
fn lfo_division(module: Option<&Value>) -> usize {
    let beats = lfo_division_beats(module);
    let mut best = 2;
    let mut best_distance = f64::INFINITY;
    for (index, candidate) in [0.25, 0.5, 1.0, 2.0, 4.0, 8.0].iter().enumerate() {
        let distance = (*candidate - beats).abs();
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best
}

/// Index into the engine's 43-step ladder; picks the nearest step.
fn lfo_sync_div(module: Option<&Value>) -> Option<usize> {
    let beats = lfo_division_beats(module);
    let mut best = 21;
    let mut best_distance = f64::INFINITY;
    for (index, candidate) in oc_synth_core::synth::LFO_SYNC_DIVISIONS.iter().enumerate() {
        let distance = (*candidate - beats).abs();
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    Some(best)
}

/// The synced rate knob is 43 positions over 32 bars .. 1/256; the quarter
/// note sits at the centre, and dotted/triplet flags shift the step.
fn lfo_division_beats(module: Option<&Value>) -> f64 {
    let rate = plain_num(module, "kParamRate").unwrap_or(6.25);
    let normalized = (rate / 100.0).clamp(0.0, 1.0);
    let step = (42.0 * normalized.powf(0.25)).round().clamp(0.0, 42.0) as i32;
    let mut beats = 2f64.powi(7 - step / 3);
    if step % 3 == 1 && flag(module, "kParamDotted", false) {
        beats *= 0.75;
    } else if step % 3 == 2 {
        if flag(module, "kParamTriplets", false) {
            beats *= 2.0 / 3.0;
        } else if flag(module, "kParamDotted", false) {
            beats *= 0.75;
        }
    }
    beats
}

fn lfo_phase(module: Option<&Value>) -> f64 {
    plain_num(module, "kParamPhase")
        .unwrap_or(0.0)
        .rem_euclid(360.0)
}

fn lfo_percent(module: Option<&Value>, key: &str) -> f64 {
    (plain_num(module, key).unwrap_or(0.0) / 100.0).clamp(0.0, 1.0)
}

fn lfo_seconds(module: Option<&Value>, key: &str) -> f64 {
    plain_num(module, key).unwrap_or(0.0).clamp(0.0, 5.0)
}

/// Map the preset's generator to a shape. `yVals` are drawn downward, so the
/// engine value is `1 - y`; `numPoints` counts segments.
fn lfo_shape_diagnosed(
    module: Option<&Value>,
    diagnostics: &mut ImportDiagnostics,
) -> (LfoShapeType, LfoShape) {
    if let Some(module) = module {
        for key in ["pathData", "curveData"] {
            if let Some(data) = module.get(key)
                && data.as_object().is_some_and(|object| !object.is_empty())
                && shape_from_data(data).is_none()
            {
                diagnostics.warn(
                    "invalid-lfo-shape",
                    "Malformed LFO shape uses the default triangle.",
                );
            }
        }
    }
    lfo_shape(module)
}

fn lfo_shape(module: Option<&Value>) -> (LfoShapeType, LfoShape) {
    let Some(module) = module else {
        return (LfoShapeType::Curve, LfoShape::triangle());
    };
    let kind = plain_str(Some(module), "kParamType").unwrap_or("Curve");
    match kind {
        "Path" => match module.get("pathData").and_then(shape_from_data) {
            Some(shape) => (LfoShapeType::Path, shape),
            None => (LfoShapeType::Path, LfoShape::triangle()),
        },
        "Lorenz" => (LfoShapeType::ChaosA, LfoShape::triangle()),
        "Rossler" => (LfoShapeType::ChaosB, LfoShape::triangle()),
        "RandomSH" | "RandomS&H" | "Random" => (LfoShapeType::RandomHold, LfoShape::triangle()),
        "Sine" => (LfoShapeType::Curve, LfoShape::sine()),
        "Triangle" => (LfoShapeType::Curve, LfoShape::triangle()),
        "Saw" | "Sawtooth" | "Ramp" => (LfoShapeType::Curve, LfoShape::saw()),
        "Square" | "Pulse" => (LfoShapeType::Curve, LfoShape::square()),
        _ => match module.get("curveData").and_then(shape_from_data) {
            Some(shape) => (LfoShapeType::Curve, shape),
            None => (LfoShapeType::Curve, LfoShape::triangle()),
        },
    }
}

fn shape_from_data(data: &Value) -> Option<LfoShape> {
    let xs = data.get("xVals")?.as_array()?;
    let ys = data.get("yVals")?.as_array()?;
    let curves = data.get("curveVals").and_then(Value::as_array);
    let available = xs.len().min(ys.len()).min(480);
    if available < 2 {
        return None;
    }
    let segments = match data.get("numPoints").and_then(Value::as_f64) {
        Some(value) if value.is_finite() && value >= 1.0 && value < available as f64 => {
            value.floor() as usize
        }
        Some(_) => return None,
        None => available - 1,
    };
    let points = segments.checked_add(1)?;
    let x: Vec<f64> = xs[..points]
        .iter()
        .map(Value::as_f64)
        .collect::<Option<_>>()?;
    let y: Vec<f64> = ys[..points]
        .iter()
        .map(|value| value.as_f64().map(|value| 1.0 - value))
        .collect::<Option<_>>()?;
    let curve: Vec<f64> = curves
        .map(|values| {
            values[..points.min(values.len())]
                .iter()
                .filter_map(Value::as_f64)
                .collect()
        })
        .unwrap_or_default();
    if x.len() < 2 || y.len() < x.len() {
        return None;
    }
    // The default triangle is stored with 0.49999997 bends; keep it canonical
    // so it stays numerically identical to the engine default.
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    if x.len() == 3
        && near(x[0], 0.0)
        && near(x[1], 0.5)
        && near(x[2], 1.0)
        && near(y[0], 0.0)
        && near(y[1], 1.0)
        && near(y[2], 0.0)
        && (curve.is_empty() || curve.iter().all(|bend| near(*bend, 0.5)))
    {
        return Some(LfoShape::triangle());
    }
    Some(LfoShape::from_points(&x, &y, &curve))
}

fn mod_source(id: i64) -> Option<ModSource> {
    match id {
        1 => Some(ModSource::ModWheel),
        // 2 is Env 1, the amplitude envelope.
        2 => Some(ModSource::AmpEnvelope),
        3 => Some(ModSource::FilterEnvelope), // Env 2, the filter envelope
        4 => Some(ModSource::Env3),
        5 => Some(ModSource::Env4),
        6 => Some(ModSource::Lfo), // LFO 1
        7 => Some(ModSource::Lfo2),
        8 => Some(ModSource::Lfo3),
        9 => Some(ModSource::Lfo4),
        10 => Some(ModSource::Lfo5),
        11 => Some(ModSource::Lfo6),
        12 => Some(ModSource::Lfo7),
        13 => Some(ModSource::Lfo8),
        14 => Some(ModSource::Lfo9),
        15 => Some(ModSource::Lfo10),
        16 => Some(ModSource::Velocity),
        // the preset's "Note" source is the MIDI note number; `Keytrack` is the
        // engine's nearest note-derived source ((note - 60) / 60).
        17 => Some(ModSource::Keytrack),
        // Channel and polyphonic aftertouch collapse into one engine source.
        18 | 19 => Some(ModSource::Aftertouch),
        25 => Some(ModSource::Macro1),
        26 => Some(ModSource::Macro2),
        27 => Some(ModSource::Macro3),
        28 => Some(ModSource::Macro4),
        29 => Some(ModSource::Macro5),
        30 => Some(ModSource::Macro6),
        31 => Some(ModSource::Macro7),
        32 => Some(ModSource::Macro8),
        // Skipped because the engine has no counterpart:
        //   0 unused, 20 noise audio, 21..24 note random/alternating,
        //   33 pitch bend, 34..36 MPE X/Y/Z, 37 release velocity, 38 fixed,
        //   39..44/47/48 unidentified, 49..54 oscillator/filter audio, 55..59 bookkeeping.
        _ => None,
    }
}

fn mod_destination(module: &str, instance: i64, parameter: &str) -> Option<ModDestination> {
    let oscillator_level = |instance: i64| match instance {
        0 => Some(ModDestination::OscillatorALevel),
        1 => Some(ModDestination::OscillatorMix),
        2 => Some(ModDestination::OscillatorCLevel),
        _ => None,
    };
    match module {
        "GranularOsc" => match parameter {
            "kParamDensity" => match instance {
                0 => Some(ModDestination::SourceDensityA),
                1 => Some(ModDestination::SourceDensityB),
                2 => Some(ModDestination::SourceDensityC),
                _ => None,
            },
            "kParamVolume" => oscillator_level(instance),
            "kParamPitch" | "kParamCoarsePit" | "kParamOctave" => Some(ModDestination::Pitch),
            "kParamPan" => Some(ModDestination::StereoWidth),
            _ => None,
        },
        "SpectralOsc" => match parameter {
            "kParamWarp" | "kParamWarp2" => match instance {
                0 => Some(ModDestination::SourceMorphA),
                1 => Some(ModDestination::SourceMorphB),
                2 => Some(ModDestination::SourceMorphC),
                _ => None,
            },
            "kParamSpecFltShift" => match instance {
                0 => Some(ModDestination::SourceBendA),
                1 => Some(ModDestination::SourceBendB),
                2 => Some(ModDestination::SourceBendC),
                _ => None,
            },
            "kParamVolume" => oscillator_level(instance),
            "kParamPitch" | "kParamCoarsePit" | "kParamOctave" => Some(ModDestination::Pitch),
            "kParamPan" => Some(ModDestination::StereoWidth),
            _ => None,
        },
        "WTOsc" | "Oscillator" | "SampleOsc" | "MultiSampleOsc" | "NoiseOsc" => match parameter {
            // the preset's destination instance is 0 = A, 1 = B, 2 = C, 3 = noise,
            // 4 = sub. B keeps the legacy `OscillatorMix` destination so old
            // patches stay compatible.
            "kParamTablePos" => match instance {
                2 => Some(ModDestination::WavetablePositionC),
                1 => Some(ModDestination::WavetablePositionB),
                0 => Some(ModDestination::WavetablePositionA),
                _ => None,
            },
            "kParamVolume" => match instance {
                0 => Some(ModDestination::OscillatorALevel),
                1 => Some(ModDestination::OscillatorMix),
                2 => Some(ModDestination::OscillatorCLevel),
                3 => Some(ModDestination::NoiseLevel),
                4 => Some(ModDestination::SubLevel),
                _ => None,
            },
            "kParamPitch" | "kParamCoarsePit" | "kParamOctave" => Some(ModDestination::Pitch),
            // Warp amount is a 0..1 control; the engine exposes it for A/B/C.
            "kParamWarp" => Some(match instance {
                0 => ModDestination::WarpAmountA,
                1 => ModDestination::WarpAmountB,
                2 => ModDestination::WarpAmountC,
                _ => return None,
            }),
            "kParamWarp2" => Some(match instance {
                0 => ModDestination::Warp2AmountA,
                1 => ModDestination::Warp2AmountB,
                2 => ModDestination::Warp2AmountC,
                _ => return None,
            }),
            // The patch ABI has no per-oscillator pan destination yet; global
            // stereo width is a deliberate approximation, not a true pan.
            "kParamPan" => Some(ModDestination::StereoWidth),
            // `kParamFine` also has no per-oscillator destination yet.
            _ => None,
        },
        "VoiceFilter" => match (instance, parameter) {
            (1, "kParamWet") => Some(ModDestination::Filter2Wet),
            (0, "kParamWet") => Some(ModDestination::FilterWet),
            (1, "kParamFreq") => Some(ModDestination::Filter2Cutoff),
            (1, "kParamReso") => Some(ModDestination::Filter2Resonance),
            (1, "kParamDrive") => Some(ModDestination::Filter2Drive),
            (0, "kParamFreq") => Some(ModDestination::FilterCutoff),
            (0, "kParamReso") => Some(ModDestination::Resonance),
            (0, "kParamDrive") => Some(ModDestination::FilterDrive),
            _ => None,
        },
        "Global" => match parameter {
            "kParamVoiceAmp" => Some(ModDestination::VoiceAmp),
            _ => None,
        },
        // Remaining the preset destinations (Env/LFO parameters, warp, macros,
        // routing slots, FX, arp/clip, ...) have no engine member and are
        // skipped rather than approximated.
        _ => None,
    }
}

fn wavetable_relative<'a>(mode: Option<(&'a str, &'a Value)>) -> Option<&'a str> {
    let (kind, mode) = mode?;
    if kind != "WTOsc" {
        return None;
    }
    // `relativePathToWT` sits beside `plainParams` on the mode object, not
    // inside it (unlike the `kParam*` controls).
    mode.get("relativePathToWT").and_then(Value::as_str)
}

fn load_wavetable(
    root: &Path,
    mode: Option<(&str, &Value)>,
    label: &str,
    diagnostics: &mut ImportDiagnostics,
) -> Option<CustomWavetable> {
    let relative = wavetable_relative(mode)?;
    let path = resolve_asset(root, relative);
    let text = path.to_str()?;
    match CustomWavetable::load_imported(text) {
        Ok(table) => Some(table),
        Err(error) => {
            diagnostics.warn(
                format!("wavetable:{label}"),
                format!(
                    "wavetable for {label} '{}' could not be loaded: {error}",
                    path.display()
                ),
            );
            None
        }
    }
}

fn resolve_asset(root: &Path, relative: &str) -> PathBuf {
    let relative = relative.trim_start_matches(['/', '\\']);
    let path = Path::new(relative);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Imported FX rack plus the preset module-ID lookup needed by ModSlots.
struct ImportedFx {
    modules: Vec<SynthFx>,
    /// the preset `destModuleID` (`rack * 100 + position`) -> (class, imported index).
    /// The lookup is a Vec sized for the three racks and is built only on the
    /// load path; the audio thread only sees the flat `modules` chain.
    by_id: Vec<Option<(SynthFxType, usize)>>,
}

impl ImportedFx {
    fn insert(&mut self, id: usize, fx_type: SynthFxType, index: usize) {
        if self.by_id.len() <= id {
            self.by_id.resize(id + 1, None);
        }
        self.by_id[id] = Some((fx_type, index));
    }

    fn get(&self, id: i64) -> Option<(SynthFxType, usize)> {
        usize::try_from(id)
            .ok()
            .and_then(|id| self.by_id.get(id).copied().flatten())
    }
}

/// Collect one rack's modules in import order; `preset_id` is `rack * 100 +
/// position`, the id ModSlots address.
fn collect_rack_modules(
    data: &Value,
    rack: usize,
    diagnostics: &mut ImportDiagnostics,
) -> Vec<(usize, SynthFx)> {
    let Some(items) = data
        .get(format!("FXRack{rack}"))
        .and_then(|rack| rack.get("FX"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    if items.len() > MAX_RACK_ITEMS {
        diagnostics.warn(
            "fx-rack-item-budget",
            "FX rack exceeds 100 modules and was not imported.",
        );
        return Vec::new();
    }
    let mut modules = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let preset_id = rack * 100 + position;
        let Some((module_name, module)) = preset_fx_module(item) else {
            continue;
        };
        let mut fx_type = preset_fx_type(module_name, item.get("type").and_then(Value::as_f64));
        if fx_type == Some(SynthFxType::Compressor)
            && plain_num(Some(module), "kParamMultiband")
                .map(|value| value >= 0.5)
                .unwrap_or(false)
        {
            fx_type = Some(SynthFxType::Multiband);
        }
        let Some(fx_type) = fx_type else {
            diagnostics.warn(
                format!("fx-module:{module_name}"),
                format!(
                    "FX module '{module_name}' has no OpenSynth FX equivalent; skipped level-neutrally"
                ),
            );
            continue;
        };
        let enabled = plain_num(Some(module), "kParamEnable")
            .map(|value| value >= 0.5)
            .unwrap_or(true)
            && !(fx_type == SynthFxType::Filter && filter_type_bypassed(Some(module)));
        let mut fx = SynthFx {
            fx_type,
            enabled,
            params: preset_fx_params(fx_type, module, diagnostics),
        };
        // Splitter counts address the preset modules; skipped children shift the
        // branches, so rewrite the counts in imported units.
        if let Some(counts) = splitter_imported_counts(module_name, module, items, position) {
            for (name, value) in counts {
                fx.set_param(name, value);
            }
        } else if matches!(module_name, "FXSplit" | "FXSplit3" | "FXSplitMS") {
            diagnostics.warn(
                "invalid-fx-split-count",
                "Malformed FX splitter child count was skipped.",
            );
            continue;
        }
        modules.push((preset_id, fx));
    }
    modules
}

/// Loudest per-source send to each bus. The engine has one post-voice sum, so
/// the loudest send is used rather than collapsing the bus to silence.
fn bus_send_levels(data: &Value) -> [f64; 2] {
    let mut levels: [f64; 2] = [0.0; 2];
    for slot in 0..7 {
        let Some(params) = data
            .get(format!("RoutingSlot{slot}"))
            .and_then(|slot| slot.get("plainParams"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (index, key) in ["kParamFXBus1Level", "kParamFXBus2Level"]
            .into_iter()
            .enumerate()
        {
            if let Some(value) = params.get(key).and_then(Value::as_f64) {
                levels[index] = levels[index].max((value / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    levels
}

/// Build the imported rack tree. A bus rack with a live send becomes a parallel
/// branch around the voice sum; the main chain runs after the branch sum.
fn import_fx(data: &Value, diagnostics: &mut ImportDiagnostics) -> ImportedFx {
    let mut imported = ImportedFx {
        modules: Vec::new(),
        by_id: Vec::new(),
    };
    let mut main = collect_rack_modules(data, 0, diagnostics);
    if main.len() > MAX_SYNTH_FX {
        diagnostics.warn(
            "fx-module-budget",
            format!(
                "Preset has {} FX modules; OpenSynth keeps the first {MAX_SYNTH_FX}",
                main.len()
            ),
        );
        main.truncate(MAX_SYNTH_FX);
    }
    let sends = bus_send_levels(data);
    let mut buses: [Vec<(usize, SynthFx)>; 2] = [Vec::new(), Vec::new()];
    for rack in 1..=2 {
        if sends[rack - 1] <= 0.0 {
            continue;
        }
        buses[rack - 1] = collect_rack_modules(data, rack, diagnostics);
    }
    let bus_total: usize = buses.iter().map(Vec::len).sum();
    if bus_total > 0 {
        let budget = MAX_SYNTH_FX.saturating_sub(1 + main.len());
        if bus_total > budget {
            diagnostics.warn(
                "fx-bus-budget",
                format!(
                    "Preset has {bus_total} bus FX modules; OpenSynth keeps {budget} after the main chain"
                ),
            );
            let keep = buses[0].len().min(budget);
            buses[0].truncate(keep);
            buses[1].truncate(budget - keep);
        }
    }
    if buses[0].is_empty() && buses[1].is_empty() {
        push_modules(&mut imported, main);
        return imported;
    }
    // Branch 1 is the dry path; branches 2 and 3 are the two bus sends.
    let mut parallel = SynthFx::new(SynthFxType::ParallelRack);
    parallel.set_param("count2", buses[0].len() as f64);
    parallel.set_param("count3", buses[1].len() as f64);
    parallel.set_param("gain2", if buses[0].is_empty() { 0.0 } else { sends[0] });
    parallel.set_param("gain3", if buses[1].is_empty() { 0.0 } else { sends[1] });
    imported.modules.push(parallel);
    let [bus1, bus2] = buses;
    push_modules(&mut imported, bus1);
    push_modules(&mut imported, bus2);
    push_modules(&mut imported, main);
    imported
}

fn push_modules(imported: &mut ImportedFx, modules: Vec<(usize, SynthFx)>) {
    for (id, fx) in modules {
        let index = imported.modules.len();
        imported.insert(id, fx.fx_type, index);
        imported.modules.push(fx);
    }
}

/// Count the children this importer keeps. An unsupported child would otherwise
/// make a branch consume the next branch's modules; nested splitters count once.
fn splitter_imported_counts(
    module_name: &str,
    module: &Value,
    items: &[Value],
    position: usize,
) -> Option<Vec<(&'static str, f64)>> {
    let (keys, names): ([&str; 3], [&'static str; 3]) = match module_name {
        "FXSplit" => (
            ["kParamModuleCount1", "kParamModuleCount2", ""],
            ["count1", "count2", ""],
        ),
        "FXSplit3" => (
            [
                "kParamModuleCount1",
                "kParamModuleCount2",
                "kParamModuleCount3",
            ],
            ["count1", "count2", "count3"],
        ),
        "FXSplitMS" => (
            ["kParamModuleCount1", "kParamModuleCount2", ""],
            ["count1", "count2", ""],
        ),
        _ => return None,
    };
    let mut cursor = position.checked_add(1)?;
    if items.len() > MAX_RACK_ITEMS {
        return None;
    }
    let mut counts = Vec::new();
    for (key, name) in keys.iter().zip(names.iter()) {
        if key.is_empty() {
            continue;
        }
        let raw = plain_num(Some(module), key).unwrap_or(0.0).round();
        let remaining = items.len().saturating_sub(cursor);
        if !raw.is_finite() || raw < 0.0 || raw > remaining as f64 {
            return None;
        }
        let count = raw as usize;
        let mut imported = 0;
        for _ in 0..count {
            if let Some((child_name, _)) = items.get(cursor).and_then(preset_fx_module)
                && preset_fx_type(
                    child_name,
                    items[cursor].get("type").and_then(Value::as_f64),
                )
                .is_some()
            {
                imported += 1;
            }
            cursor += 1;
        }
        counts.push((*name, imported as f64));
    }
    Some(counts)
}

fn preset_fx_module(item: &Value) -> Option<(&str, &Value)> {
    item.as_object()?
        .iter()
        .find(|(key, value)| key.starts_with("FX") && value.is_object())
        .map(|(key, value)| (key.as_str(), value))
}

/// Map a the preset FX `destModuleParamName` onto the small engine FX palette.
///
/// Unsupported FX destinations are documented instead of guessed: reverb
/// size/damp/predelay/width, delay time, rate/resonance and splitter/Bode/
/// HyperD/convolution parameters have no [`FxParam`] counterpart and are
/// dropped. The raw the preset file still keeps them.
fn fx_mod_param(fx_type: SynthFxType, parameter: &str) -> Option<FxParam> {
    let param = match (fx_type, parameter) {
        (_, "kParamWet") => FxParam::Wet,
        (
            SynthFxType::Delay | SynthFxType::Flanger | SynthFxType::Phaser | SynthFxType::Chorus,
            "kParamFeedback",
        ) => FxParam::Feedback,
        (SynthFxType::Flanger | SynthFxType::Phaser | SynthFxType::Chorus, "kParamDepth") => {
            FxParam::Depth
        }
        (SynthFxType::Eq3, "kParamGain1") => FxParam::Gain1,
        (SynthFxType::Eq3, "kParamGain2") => FxParam::Gain2,
        (SynthFxType::Eq3, "kParamFreq1") => FxParam::Freq1,
        (SynthFxType::Eq3, "kParamFreq2") => FxParam::Freq2,
        (SynthFxType::Distortion | SynthFxType::Drive, "kParamDrive") => FxParam::Drive,
        (SynthFxType::Distortion, "kParamBW") => FxParam::Bandwidth,
        (SynthFxType::Distortion | SynthFxType::Delay, "kParamFreq") => FxParam::Tone,
        (SynthFxType::Filter, "kParamFreq") => FxParam::Cutoff,
        (SynthFxType::Filter, "kParamReso") => FxParam::Resonance,
        (SynthFxType::Chorus | SynthFxType::Phaser | SynthFxType::Flanger, "kParamRate") => {
            FxParam::Rate
        }
        (SynthFxType::HyperD, "kParamRate") => FxParam::Rate,
        (SynthFxType::Bode, "kParamShift") => FxParam::Shift,
        (SynthFxType::Bode, "kParamFeedback") => FxParam::Feedback,
        (SynthFxType::Bode, "kParamOutputMix") => FxParam::OutputMix,
        (SynthFxType::Bode, "kParamOutputWidth") => FxParam::OutputWidth,
        (SynthFxType::Bode, "kParamDelayTime") => FxParam::DelayTime,
        (SynthFxType::Bode, "kParamRange") => FxParam::Range,
        (SynthFxType::Utils, "kParamWidth") => FxParam::Width,
        (SynthFxType::Utils, "kParamHPF") => FxParam::Hpf,
        (SynthFxType::Utils, "kParamLPF") => FxParam::Lpf,
        (SynthFxType::Utils, "kParamBalance") => FxParam::Balance,
        (SynthFxType::Eq3, "kParamReso1") => FxParam::Reso1,
        (SynthFxType::Eq3, "kParamReso2") => FxParam::Reso2,
        (SynthFxType::Delay | SynthFxType::Flanger | SynthFxType::Chorus, "kParamTimeL") => {
            FxParam::Time
        }
        (SynthFxType::Delay, "kParamTimeR") => FxParam::Time,
        (SynthFxType::Compressor | SynthFxType::Multiband, "kParamMakeup") => FxParam::Makeup,
        (_, "kParamLevelOut") => FxParam::LevelOut,
        _ => return None,
    };
    // Only accept parameters the imported module actually exposes. This keeps
    // e.g. `kParamFeedback` on FXEQ from becoming an inert route.
    fx_type
        .default_params()
        .iter()
        .any(|(name, _)| *name == param.native_name())
        .then_some(param)
}

/// Translate the preset ModSlots whose `destModuleTypeString` starts with `FX` into
/// real patch routes. The lookup goes through [`ImportedFx::by_id`], so skipped
/// modules and rack offsets cannot mis-address the flattened chain.
fn import_fx_mod_routes(
    data: &Value,
    imported: &ImportedFx,
    diagnostics: &mut ImportDiagnostics,
) -> Vec<FxModRoute> {
    let mut routes = Vec::new();
    for slot in 0..MOD_SLOT_COUNT {
        let Some(module) = data.get(format!("ModSlot{slot}")) else {
            continue;
        };
        let Some(destination_module) = module.get("destModuleTypeString").and_then(Value::as_str)
        else {
            continue;
        };
        if !destination_module.starts_with("FX") {
            continue;
        }
        let Some(destination_param) = module.get("destModuleParamName").and_then(Value::as_str)
        else {
            continue;
        };
        let Some(destination_id) = module.get("destModuleID").and_then(number_as_i64) else {
            continue;
        };
        let Some((fx_type, fx_index)) = imported.get(destination_id) else {
            continue;
        };
        // The ID is authoritative, but require the class string to agree so a
        // malformed slot cannot silently modulate a different module.
        if preset_fx_type(destination_module, None) != Some(fx_type) {
            continue;
        }
        let Some(parameter) = fx_mod_param(fx_type, destination_param) else {
            diagnostics.warn(
                format!("fx-route:{destination_module}.{destination_param}"),
                format!(
                    "FX route destination '{destination_module}.{destination_param}' has no engine parameter"
                ),
            );
            continue;
        };
        let Some(source_values) = module.get("source").and_then(Value::as_array) else {
            continue;
        };
        let Some(source_id) = source_values.first().and_then(number_as_i64) else {
            continue;
        };
        let Some(source) = mod_source(source_id) else {
            continue;
        };
        let auxiliary_scale = match source_values.get(1).and_then(number_as_i64) {
            Some(0) | None => 1.0,
            Some(id) => macro_source_value(data, id).unwrap_or(1.0),
        };
        let amount =
            plain_num(Some(module), "kParamAmount").unwrap_or(0.0) / 100.0 * auxiliary_scale;
        let amount = amount.clamp(-1.0, 1.0);
        if amount == 0.0 {
            continue;
        }
        routes.push(FxModRoute {
            source,
            fx_index,
            parameter,
            amount,
            bipolar: plain_num(Some(module), "kParamBipolar").unwrap_or(0.0) >= 0.5,
        });
    }
    routes
}

/// the preset's serialized FX `type` enum, in preset order. Ids 9 (HyperD) and 11
/// (Conv) use the engine's own widener and convolver; see `docs/fx-hyperd-conv.md`.
const PRESET_FX_TYPES: [SynthFxType; 16] = [
    SynthFxType::Distortion, // 0
    SynthFxType::Flanger,    // 1
    SynthFxType::Phaser,     // 2
    SynthFxType::Chorus,     // 3
    SynthFxType::Delay,      // 4
    SynthFxType::Compressor, // 5
    SynthFxType::Reverb,     // 6
    SynthFxType::Eq3,        // 7
    SynthFxType::Filter,     // 8
    SynthFxType::HyperD,     // 9 HyperD
    SynthFxType::Bode,       // 10
    SynthFxType::Conv,       // 11 Conv
    SynthFxType::Utils,      // 12
    SynthFxType::Split,      // 13
    SynthFxType::Split3,     // 14
    SynthFxType::SplitMs,    // 15
];

/// the preset serializes the FX class both as a `FX*` key and a numeric `type`.
/// Module names are authoritative; the numeric table is a fallback.
fn preset_fx_type(module_name: &str, type_index: Option<f64>) -> Option<SynthFxType> {
    let named = match module_name {
        "FXDistortion" => Some(SynthFxType::Distortion),
        "FXComp" => Some(SynthFxType::Compressor),
        "FXEQ" => Some(SynthFxType::Eq3),
        "FXDelay" => Some(SynthFxType::Delay),
        "FXReverb" => Some(SynthFxType::Reverb),
        "FXChorus" => Some(SynthFxType::Chorus),
        "FXFlanger" => Some(SynthFxType::Flanger),
        "FXPhaser" => Some(SynthFxType::Phaser),
        "FXFilter" => Some(SynthFxType::Filter),
        "FXUtils" => Some(SynthFxType::Utils),
        "FXSplit" => Some(SynthFxType::Split),
        "FXSplit3" => Some(SynthFxType::Split3),
        "FXSplitMS" => Some(SynthFxType::SplitMs),
        "FXBode" => Some(SynthFxType::Bode),
        "FXHyperD" => Some(SynthFxType::HyperD),
        "FXConv" => Some(SynthFxType::Conv),
        _ => None,
    };
    let inferred = type_index.and_then(preset_fx_type_from_index);
    if let (Some(named), Some(inferred)) = (named, inferred)
        && named != inferred
    {
        log::warn!(
            "Preset FX module '{module_name}' disagrees with its numeric type; using the module name"
        );
    }
    named.or(inferred)
}

fn preset_fx_type_from_index(index: f64) -> Option<SynthFxType> {
    let index = index.round();
    if !(0.0..PRESET_FX_TYPES.len() as f64).contains(&index) {
        return None;
    }
    PRESET_FX_TYPES.get(index as usize).copied()
}

/// the preset FXEQ band type: 0 = shelf, 1 = bell, 2 = cut. Band 1 is the low band
/// (low shelf / low cut) and band 2 the high band (high shelf / high cut).
fn eq_band_type(value: Option<f64>) -> Option<f64> {
    value.map(|value| value.round().clamp(0.0, 2.0))
}

fn preset_fx_params(
    fx_type: SynthFxType,
    module: &Value,
    diagnostics: &mut ImportDiagnostics,
) -> Vec<(String, f64)> {
    let mut fx = SynthFx::new(fx_type);
    let mut set = |name: &str, value: Option<f64>| {
        if let Some(value) = value.filter(|value| value.is_finite()) {
            fx.set_param(name, value);
        }
    };
    // Every the preset key read by the mapping is recorded, so the parameter scan
    // below can report the ones this module class cannot represent.
    let consumed = std::cell::RefCell::new(std::collections::HashSet::new());
    let number = |key: &str| {
        consumed.borrow_mut().insert(key.to_string());
        plain_num(Some(module), key)
    };
    match fx_type {
        SynthFxType::Drive => {
            set("drive", number("kParamDrive").map(|value| value / 100.0));
            set("tone", number("kParamFreq").map(frequency_to_tone));
            // `level` is the native wet trim; the preset has no such knob, so the
            // imported module keeps it neutral and uses `level_out` instead.
            set("level", Some(1.0));
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Distortion => {
            set("drive", number("kParamDrive").map(|value| value / 100.0));
            set("tone", number("kParamFreq").map(frequency_to_tone));
            set("level", Some(1.0));
            set("mix", number("kParamWet").map(|value| value / 100.0));
            // Bandwidth in octaves (`kParamBW`); 0 keeps the one-pole tone.
            set("bw", number("kParamBW").map(|value| value.clamp(0.0, 8.0)));
            // 2..16 shaper stages. The mode is not mapped: the stored value is
            // a family name with no established correspondence to our palette.
            set(
                "stages",
                number("kParamNumStages").map(|value| value.round().clamp(2.0, 16.0)),
            );
        }
        SynthFxType::Compressor => {
            // Crossovers default to the stored 120 Hz / 2.5 kHz pair; a
            // module that stores its own pair overrides them.
            set(
                "crossover_low",
                number("kParamXoverLow").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set(
                "crossover_high",
                number("kParamXoverHi").map(|value| value.clamp(20.0, 20_000.0)),
            );
            // The stored taper is peak-referenced, so add the sine crest
            // for the RMS detector; older dB values pass through.
            set(
                "threshold",
                number("kParamThreshold").or_else(|| {
                    number("kParamThresh")
                        .map(|value| preset_compressor_threshold(value) + SINE_CREST_DB)
                }),
            );
            set(
                "ratio",
                number("kParamRatio").map(|value| value.clamp(1.0, 1_000.0)),
            );
            set(
                "attack",
                number("kParamAttack").map(|value| value.clamp(0.1, 200.0)),
            );
            set(
                "release",
                number("kParamRelease").map(|value| value.clamp(5.0, 2_000.0)),
            );
            set(
                "ratio_below",
                number("kParamRatioBelow").map(preset_multiband_ratio_below),
            );
            // the preset stores the make-up as a linear amplitude; the device takes dB.
            set(
                "makeup",
                number("kParamMakeup").map(preset_compressor_makeup),
            );
            set(
                "mix",
                number("kParamWet").map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            for (index, band) in [0, 1, 2].into_iter().enumerate() {
                set(
                    &format!("band{}_gain", index + 1),
                    number(&format!("kParamGain{band}")).map(|value| value.clamp(-24.0, 24.0)),
                );
            }
        }
        SynthFxType::Eq3 => {
            // Gains are dB, frequencies are Hz and `kParamReso` is a 0..100 Q
            // control; all three already match the engine's units.
            set(
                "low",
                number("kParamGain1").map(|value| value.clamp(-24.0, 24.0)),
            );
            set(
                "high",
                number("kParamGain2").map(|value| value.clamp(-24.0, 24.0)),
            );
            set(
                "low_freq",
                number("kParamFreq1").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set(
                "high_freq",
                number("kParamFreq2").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set("type1", eq_band_type(number("kParamType1")));
            set("type2", eq_band_type(number("kParamType2")));
            set(
                "reso1",
                number("kParamReso1").map(|value| value.clamp(0.0, 100.0)),
            );
            set(
                "reso2",
                number("kParamReso2").map(|value| value.clamp(0.0, 100.0)),
            );
        }
        SynthFxType::Delay => {
            // `kParamTimeL/R` is a stored control, not seconds: the DSP maps it
            // through `t = 2^(20x - 2)` (0.05 -> 0.5 s).
            set(
                "time",
                number("kParamTimeL").or_else(|| number("kParamTimeR")),
            );
            set(
                "feedback",
                number("kParamFeedback").map(|value| value / 100.0),
            );
            set("tone", number("kParamFreq").map(frequency_to_tone));
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Reverb => {
            // `kParamSize` owns the decay; older presets without it fall back
            // to the feedback control.
            set(
                "size",
                number("kParamSize")
                    .or_else(|| number("kParamFeedback"))
                    .map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            set(
                "damp",
                number("kParamDamp")
                    .map(percent_or_normalized)
                    .or_else(|| number("kParamFreq").map(percent_or_normalized))
                    .or_else(|| number("kParamFreqB").map(|value| value / 100.0))
                    .map(|value| value.clamp(0.0, 1.0)),
            );
            set("predelay", number("kParamPreDelay").map(seconds));
            set(
                "width",
                number("kParamWidth").map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            set(
                "mix",
                number("kParamWet").map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            // The type selector is a string, so the numeric scan cannot report
            // it; record it here instead.
            if module
                .get("plainParams")
                .and_then(Value::as_object)
                .and_then(|params| params.get("kParamType"))
                .is_some()
            {
                diagnostics.warn(
                    "fx-param:reverb:kParamType",
                    "Preset reverb type has no engine counterpart; the FDN tank is used",
                );
            }
        }
        SynthFxType::Chorus => {
            // the preset's rate is Hz and depth/feedback/wet are 0..100 percentages.
            set("rate", number("kParamRate"));
            set("depth", number("kParamDepth").map(percent_or_normalized));
            set(
                "feedback",
                number("kParamFeedback").map(percent_or_normalized),
            );
            set("mix", number("kParamWet").map(|value| value / 100.0));
            // Two voice delay times: presets store milliseconds, 0 means the
            // engine's default tap spacing.
            set("delay", number("kParamDelay").filter(|value| *value > 0.0));
            set(
                "delay2",
                number("kParamDelay2").filter(|value| *value > 0.0),
            );
        }
        SynthFxType::Phaser => {
            set("rate", number("kParamRate"));
            set("depth", number("kParamDepth").map(percent_or_normalized));
            set(
                "feedback",
                number("kParamFeedback").map(percent_or_normalized),
            );
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Flanger => {
            // the preset stores rate in Hz and depth/feedback/wet as 0..100
            // percentages; `percent_or_normalized` also accepts normalized
            // reserialized values. Width has no engine counterpart.
            set("rate", number("kParamRate"));
            set("depth", number("kParamDepth").map(percent_or_normalized));
            set(
                "feedback",
                number("kParamFeedback").map(percent_or_normalized),
            );
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Filter => {
            set("cutoff", number("kParamFreq").map(fx_cutoff));
            set("resonance", number("kParamReso").map(normalize_percent));
            set("mode", preset_fx_filter_mode(module));
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Utils => {
            // Utils HPF is 24 dB/oct and tuned 1.3..400 Hz; the LPF is a
            // 2-pole 50 Hz..20 kHz control. Width 100 is neutral.
            set(
                "hpf",
                number("kParamHPF").map(|value| value.clamp(1.0, 400.0)),
            );
            set(
                "lpf",
                number("kParamLPF").map(|value| value.clamp(50.0, 20_000.0)),
            );
            set("lf_mono", number("kParamLFMono").map(flag_value));
            set(
                "lf_xover",
                number("kParamLFXover").map(|value| value.clamp(20.0, 400.0)),
            );
            set(
                "width",
                number("kParamWidth").map(|value| value.clamp(0.0, 800.0)),
            );
            set(
                "balance",
                number("kParamBalance").map(|value| value.clamp(-100.0, 100.0)),
            );
            set("polarity_l", number("kParamPolarityL").map(flag_value));
            set("polarity_r", number("kParamPolarityR").map(flag_value));
            // Utils is fully wet when `kParamWet` is absent.
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        SynthFxType::Split => {
            set(
                "freq",
                number("kParamFreq").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set("count1", number("kParamModuleCount1"));
            set("count2", number("kParamModuleCount2"));
            set("bypass1", number("kParamBypass1").map(flag_value));
            set("bypass2", number("kParamBypass2").map(flag_value));
        }
        SynthFxType::Split3 => {
            set(
                "freq",
                number("kParamFreq").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set(
                "freq2",
                number("kParamFreq2").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set("count1", number("kParamModuleCount1"));
            set("count2", number("kParamModuleCount2"));
            set("count3", number("kParamModuleCount3"));
            set("bypass1", number("kParamBypass1").map(flag_value));
            set("bypass2", number("kParamBypass2").map(flag_value));
            set("bypass3", number("kParamBypass3").map(flag_value));
        }
        SynthFxType::SplitMs => {
            set("count1", number("kParamModuleCount1"));
            set("count2", number("kParamModuleCount2"));
            set("bypass1", number("kParamBypass1").map(flag_value));
            set("bypass2", number("kParamBypass2").map(flag_value));
        }
        SynthFxType::Bode => {
            // `kParamShift` is a -100..100 percentage of `kParamRange` (Hz).
            set(
                "shift",
                number("kParamShift").map(|value| value.clamp(-100.0, 100.0)),
            );
            set(
                "range",
                number("kParamRange").map(|value| value.clamp(0.1, 5_000.0)),
            );
            set(
                "feedback",
                number("kParamFeedback").map(|value| value.clamp(0.0, 95.0) / 100.0),
            );
            set(
                "delay",
                number("kParamDelayTime").map(|value| value.clamp(0.0, 1.2)),
            );
            set(
                "output_mix",
                number("kParamOutputMix").map(|value| value.clamp(-100.0, 100.0)),
            );
            set(
                "output_width",
                number("kParamOutputWidth").map(|value| value.clamp(0.0, 100.0)),
            );
            set("mono_input", number("kParamMonoInput").map(flag_value));
            set("mix", number("kParamWet").map(|value| value / 100.0));
            // SwapAB exchanges the A (down) and B (up) sidebands.
            set("swap_ab", number("kParamSwapAB").map(flag_value));
        }
        // the preset's FXComp turns into a three-band compander when
        // `kParamMultiband` is set. Thresholds, ratios, per-band gains and the
        // global make-up use the laws documented in docs/fx-laws.md section 3.
        SynthFxType::Multiband => {
            set("band_count", Some(1.0));
            set("slope", Some(1.0));
            set(
                "crossover_1",
                number("kParamXoverLow").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set(
                "crossover_2",
                number("kParamXoverHi").map(|value| value.clamp(20.0, 20_000.0)),
            );
            set(
                "mix",
                number("kParamWet").map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            let attack = number("kParamAttack").map(|value| value.clamp(0.1, 200.0));
            let release = number("kParamRelease").map(|value| value.clamp(10.0, 2_000.0));
            let ratio = number("kParamRatio").map(|value| value.clamp(1.0, 20.0));
            let ratio_below = number("kParamRatioBelow").map(preset_multiband_ratio_below);
            set(
                "makeup",
                number("kParamMakeup").map(preset_compressor_makeup),
            );
            for band in 1..=3 {
                let index = band - 1;
                // The device's zero range disables dynamics; the module has no
                // such clamp, so imported bands get the full +-60 dB window.
                set(&format!("band{band}_range"), Some(60.0));
                let threshold = number(&format!("kParamThreshUD{index}"))
                    .map(preset_multiband_threshold)
                    .unwrap_or(MULTIBAND_DEFAULT_THRESHOLD_DB);
                set(&format!("band{band}_threshold"), Some(threshold));
                set(
                    &format!("band{band}_threshold_below"),
                    Some(threshold - MULTIBAND_DEADBAND_DB),
                );
                set(
                    &format!("band{band}_ratio"),
                    Some(ratio.unwrap_or(MULTIBAND_DEFAULT_RATIO)),
                );
                set(
                    &format!("band{band}_ratio_below"),
                    number(&format!("kParamRatioBelow{index}"))
                        .map(preset_multiband_ratio_below)
                        .or(ratio_below)
                        .or(Some(1.0)),
                );
                set(
                    &format!("band{band}_makeup"),
                    number(&format!("kParamGain{index}")).map(|value| value.clamp(-24.0, 24.0)),
                );
                set(&format!("band{band}_attack"), attack);
                set(&format!("band{band}_release"), release);
            }
        }
        // A parallel rack is our routing node; it has no preset controls.
        SynthFxType::ParallelRack => {}
        // Rate is Hz and detune is a percentage mapped onto modulation depth;
        // the network size and unison controls have no engine counterpart.
        SynthFxType::HyperD => {
            set(
                "rate",
                number("kParamRate").map(|value| value.clamp(0.01, 5.0)),
            );
            set("depth", number("kParamDetune").map(percent_or_normalized));
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
        // Size selects one of the generated rooms/plates; the rest shape the
        // wet path. Only the wet and output trims were mapped before.
        SynthFxType::Conv => {
            set(
                "size",
                number("kParamSize").map(|value| (value / 100.0).clamp(0.0, 1.0)),
            );
            set(
                "damp",
                number("kParamDamping")
                    .or_else(|| number("kParamDamp"))
                    .map(percent_or_normalized),
            );
            set(
                "predelay",
                number("kParamPredelay")
                    .or_else(|| number("kParamPreDelay"))
                    .map(seconds),
            );
            set("width", number("kParamWidth").map(|value| value / 100.0));
            set("mix", number("kParamWet").map(|value| value / 100.0));
        }
    }
    // Every the preset FX module exposes the same post-mix output trim; an omitted
    // key means the controller default 0.5, which is unity under the law.
    set(
        "level_out",
        Some(preset_level_out(number("kParamLevelOut"))),
    );
    // the preset's controller default for delay feedback is 40%, not "no repeats".
    // Keeping the engine's 35% handbook default would still be wrong, and 0%
    // changes a preset that simply omitted the key.
    if fx_type == SynthFxType::Delay && number("kParamFeedback").is_none() {
        fx.set_param("feedback", 0.4);
    }
    // Preserve every remaining numeric the preset control (for example
    // `kParamMode`, `kParamBeatSync`, multiband compressor gains) so re-import
    // analysis can still see it. Names that collide with a native parameter
    // were already transferred above.
    if let Some(params) = module.get("plainParams").and_then(Value::as_object) {
        for (key, value) in params {
            let Some(value) = value.as_f64() else {
                continue;
            };
            if let Some(entry) = fx.params.iter_mut().find(|(name, _)| name == key) {
                entry.1 = value;
                continue;
            }
            if !consumed.borrow().contains(key) && !structural_fx_param(key) {
                diagnostics.warn(
                    format!("fx-param:{}:{key}", fx_type.label()),
                    format!(
                        "Preset FX parameter '{key}' on {} has no engine counterpart; kept in the raw parameter list",
                        fx_type.label()
                    ),
                );
            }
            fx.params.push((key.clone(), value));
        }
    }
    fx.params
}

/// Rack-structure controls the importer reads outside the parameter palette.
fn structural_fx_param(key: &str) -> bool {
    key == "kParamEnable"
        || key == "kParamMultiband"
        || key.starts_with("kParamModuleCount")
        || key.starts_with("kParamBypass")
}

/// the preset's `kParamLevelOut` output trim is `4 * LO^2`: 0.5 (the controller
/// default) is unity and 1.0 is +12 dB. Missing keys keep unity.
fn preset_level_out(value: Option<f64>) -> f64 {
    let lo = value.unwrap_or(0.5).clamp(0.0, 1.0);
    4.0 * lo * lo
}

/// Macro 1..8 are source ids 25..32 and store 0..100.
fn macro_source_value(data: &Value, source: i64) -> Option<f64> {
    let index = source.checked_sub(25)?;
    if !(0..8).contains(&index) {
        return None;
    }
    let module = data.get(format!("Macro{index}"))?;
    Some((plain_num(Some(module), "kParamValue").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0))
}

fn frequency_to_tone(value: f64) -> f64 {
    if value <= 1.0 {
        return value.clamp(0.0, 1.0);
    }
    ((value / 250.0).ln() / (18_000.0_f64 / 250.0).ln()).clamp(0.0, 1.0)
}

/// the preset filter frequency is normalized 0..1 in the preset container; accept a
/// direct Hz value too so reserialized trees still import.
fn fx_cutoff(value: f64) -> f64 {
    if value <= 1.0 {
        cutoff_hz(value)
    } else {
        value.clamp(20.0, 20_000.0)
    }
}

/// FXFilter serializes its response as a string (`kParamType`, e.g. `L12`).
/// Numeric mode ids are not proven, so only the string form is mapped.
fn preset_fx_filter_mode(module: &Value) -> Option<f64> {
    let id =
        plain_str(Some(module), "kParamType").or_else(|| plain_str(Some(module), "kParamMode"))?;
    Some(match filter_mode(id) {
        FilterMode::HighPass => 1.0,
        FilterMode::BandPass => 2.0,
        _ => 0.0,
    })
}

/// Reverb damping is serialized as a normalized 0..1 control in some presets
/// and as a 0..100 percentage in others. Accept both without guessing from
/// the key name alone.
fn flag_value(value: f64) -> f64 {
    if value >= 0.5 { 1.0 } else { 0.0 }
}

fn percent_or_normalized(value: f64) -> f64 {
    if value <= 1.0 {
        value.clamp(0.0, 1.0)
    } else {
        (value / 100.0).clamp(0.0, 1.0)
    }
}

/// the preset's compressor threshold is a normalized 0..1 control; the engine's
/// compact compressor takes dB at the module input. Negative values are treated
/// as already-dB so older reserialized trees keep importing.
fn preset_compressor_threshold(value: f64) -> f64 {
    if value < 0.0 {
        return value.clamp(-60.0, oc_synth_fx::COMPRESSOR_KNEE_OFFSET_DB);
    }
    let amount = if value <= 1.0 { value } else { value / 100.0 };
    let amount = amount.clamp(0.0, 1.0);
    // Stored taper: 60*log10(1-a) + 13.50 dB, where 13.50 is the module's
    // detector offset; see docs/fx-laws.md section 3. The knob mutes at 1, so
    // the engine uses its own -60 dB floor.
    let knee = 60.0 * (1.0 - amount).log10() + oc_synth_fx::COMPRESSOR_KNEE_OFFSET_DB;
    knee.clamp(-60.0, oc_synth_fx::COMPRESSOR_KNEE_OFFSET_DB)
}

/// the preset stores the compressor make-up as a linear amplitude, not dB.
fn preset_compressor_makeup(value: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    20.0 * value.log10()
}

/// Multiband threshold knobs (0..200) to module-input dB; linear fit of five
/// probed knees (0/50/100/150/200) is `0.240*value - 37.9`.
fn preset_multiband_threshold(value: f64) -> f64 {
    0.240 * value.clamp(0.0, 200.0) - 38.4
}

/// Stored deadband between the down and up thresholds, in dB.
const MULTIBAND_DEADBAND_DB: f64 = 13.0;

/// Stored down ratio when the module stores no ratio control.
const MULTIBAND_DEFAULT_RATIO: f64 = 3.17;

/// Default band threshold when the module stores no `ThreshUD` for it.
const MULTIBAND_DEFAULT_THRESHOLD_DB: f64 = -15.0;

/// `RatioBelow` (0..1) to the stored upward slope `0.78*value`, then to a
/// ratio; 0 is neutral.
fn preset_multiband_ratio_below(value: f64) -> f64 {
    let slope = 0.78 * value.clamp(0.0, 1.0);
    if slope <= 0.0 {
        1.0
    } else {
        1.0 / (1.0 - slope)
    }
}

fn seconds(value: f64) -> f64 {
    if value > 1.0 { value / 1000.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_synth_core::synth::FilterModel;

    fn malformed_container(payload: &[u8], declared: u32) -> Vec<u8> {
        let compressed = zstd::stream::encode_all(payload, 1).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"{}");
        bytes.extend_from_slice(&declared.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&compressed);
        bytes
    }

    #[test]
    fn malformed_preset_rejects_expansion_and_declared_budget_overflow() {
        let bytes = malformed_container(&vec![0; 4096], 1);
        assert!(
            ForeignPreset::parse(&bytes)
                .unwrap_err()
                .contains("exceeds")
        );
        let bytes = malformed_container(&[0xa0], u32::MAX);
        assert!(ForeignPreset::parse(&bytes).unwrap_err().contains("32 MiB"));
        let mut nested = vec![0x81; limits::MAX_DEPTH + 2];
        nested.push(0xf6);
        let bytes = malformed_container(&nested, nested.len() as u32);
        assert!(ForeignPreset::parse(&bytes).unwrap_err().contains("depth"));
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&((limits::MAX_METADATA + 1) as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert!(ForeignPreset::parse(&bytes).unwrap_err().contains("1 MiB"));
    }

    #[test]
    fn malformed_lfo_dimensions_and_segment_overflow_use_diagnosed_fallback() {
        for data in [
            json!({"xVals": [], "yVals": []}),
            json!({"xVals": [0], "yVals": [0]}),
            json!({"xVals": [0,1], "yVals": [0,1], "numPoints": 1e300}),
            json!({"xVals": [0,1], "yVals": [0,1], "numPoints": -1}),
            json!({"xVals": [0,"bad"], "yVals": [0,1]}),
        ] {
            assert!(shape_from_data(&data).is_none());
            let module = json!({"curveData": data});
            let mut diagnostics = ImportDiagnostics::default();
            lfo_shape_diagnosed(Some(&module), &mut diagnostics);
            assert!(!diagnostics.is_empty());
        }
        assert!(
            shape_from_data(&json!({"xVals": [0,1], "yVals": [0,1], "numPoints": 1})).is_some()
        );
    }

    #[test]
    fn bounded_processor_state_roundtrips_without_cloning_the_tree() {
        let preset = synthetic_preset(json!({"component": "editor", "settings": [1,2,3]}));
        let bytes = preset.to_vst3_state().unwrap();
        let parsed = ForeignPreset::parse(&bytes).unwrap();
        assert_eq!(parsed.data["component"], "processor");
        assert_eq!(parsed.data["settings"], json!([1, 2, 3]));
        assert_eq!(preset.data["component"], "editor");
    }

    #[test]
    fn oversized_fx_rack_is_refused_with_diagnostics() {
        let data = json!({"FXRack0": {"FX": vec![json!({}); MAX_RACK_ITEMS + 1]}});
        let mut diagnostics = ImportDiagnostics::default();
        assert!(collect_rack_modules(&data, 0, &mut diagnostics).is_empty());
        assert_eq!(diagnostics.warnings()[0].class, "fx-rack-item-budget");
        let module = json!({});
        assert!(splitter_imported_counts("FXSplit", &module, &[], usize::MAX).is_none());
    }

    #[test]
    fn malformed_splitter_counts_do_not_drive_file_sized_loops() {
        for count in [1e300, -1.0, 2.0] {
            let module = json!({"plainParams": {"kParamModuleCount1": count}});
            assert!(splitter_imported_counts("FXSplit", &module, &[json!({})], 0).is_none());
        }
        let module = json!({"plainParams": {"kParamModuleCount1": 0}});
        assert!(splitter_imported_counts("FXSplit", &module, &[json!({})], 0).is_some());
    }

    #[test]
    fn routing_slots_map_to_per_source_filter_targets() {
        let data = serde_json::json!({
            "RoutingSlot0": {
                "plainParams": {
                    "kParamRoutingDest": "kRoutingDestFilter",
                    "kParamFilterBalance": -100.0
                }
            },
            "RoutingSlot1": {
                "plainParams": {
                    "kParamRoutingDest": "kRoutingDestFilter",
                    "kParamFilterBalance": 0.0
                }
            },
            "RoutingSlot2": {
                "plainParams": {
                    "kParamRoutingDest": "kRoutingDestFilter",
                    "kParamFilterBalance": 100.0
                }
            },
            "RoutingSlot3": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestDirect" }
            },
            "RoutingSlot4": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestMaster" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let routes = patch.source_filter_routes.expect("routing slots present");
        assert_eq!(routes[0].target, SourceFilterTarget::Filter1);
        assert_eq!(routes[0].balance, 0.0);
        assert_eq!(routes[1].target, SourceFilterTarget::Both);
        assert!((routes[1].balance - 0.5).abs() < 1e-12);
        assert_eq!(routes[2].target, SourceFilterTarget::Filter2);
        assert!((routes[2].balance - 1.0).abs() < 1e-12);
        assert_eq!(routes[3].target, SourceFilterTarget::Bypass);
        assert_eq!(routes[4].target, SourceFilterTarget::Bypass);
    }

    #[test]
    fn routing_slots_without_a_destination_keep_filter_one() {
        let data = serde_json::json!({
            "RoutingSlot1": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestFilter" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let routes = patch.source_filter_routes.expect("routing slots present");
        for route in routes {
            assert_eq!(route.target, SourceFilterTarget::Filter1);
        }
    }

    #[test]
    fn presets_without_source_routing_keep_the_legacy_path() {
        let data = serde_json::json!({
            "VoiceFilter0": { "plainParams": { "kParamFreq": 0.4 } },
            "RoutingSlot5": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestFilter" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!(patch.source_filter_routes.is_none());
    }

    fn synthetic_preset(data: Value) -> ForeignPreset {
        ForeignPreset {
            metadata: serde_json::json!({}),
            data: data.clone(),
            cbor: json_to_cbor(data),
        }
    }

    #[test]
    fn maps_enabled_oscillator_c_and_second_filter() {
        let data = serde_json::json!({
            "Oscillator2": {
                "WTOsc2": {
                    "plainParams": { "kParamTablePos": 128.0 }
                },
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamVolume": 0.25,
                    "kParamOctave": -1.0,
                    "kParamCoarsePit": 7.0
                }
            },
            "VoiceFilter1": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamFreq": 0.5,
                    "kParamReso": 50.0,
                    "kParamDrive": 50.0,
                    "kParamType": "H12"
                }
            },
            "RoutingSlot5": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestMaster" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.oscillator_c_waveform, Waveform::Wavetable);
        assert!((patch.oscillator_c_level - 0.25).abs() < 1e-9);
        assert!((patch.oscillator_c_semitones - (-5.0)).abs() < 1e-9);
        assert!((patch.oscillator_c_wavetable_position - 127.0 / 255.0).abs() < 1e-9);
        assert!(patch.filter2_enabled);
        assert!((patch.filter2_cutoff - cutoff_hz(0.5)).abs() < 1e-6);
        assert!((patch.filter2_resonance - 0.5).abs() < 1e-9);
        assert!((patch.filter2_drive - 0.5).abs() < 1e-9);
        assert_eq!(patch.filter2_mode, FilterMode::HighPass);
        assert_eq!(patch.filter_routing, FilterRouting::Parallel);
    }

    #[test]
    fn serial_routing_when_filter_one_feeds_filter_two() {
        let data = serde_json::json!({
            "VoiceFilter1": {
                "plainParams": { "kParamEnable": 1.0 }
            },
            "RoutingSlot5": {
                "plainParams": { "kParamRoutingDest": "kRoutingDestFilter" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!(patch.filter2_enabled);
        assert_eq!(patch.filter_routing, FilterRouting::Serial);
    }

    #[test]
    fn filter_id_prefixes_map_to_modes() {
        assert_eq!(filter_mode("H6"), FilterMode::HighPass);
        assert_eq!(filter_mode("H12"), FilterMode::HighPass);
        assert_eq!(filter_mode("H24"), FilterMode::HighPass);
        assert_eq!(filter_mode("B12"), FilterMode::BandPass);
        assert_eq!(filter_mode("B24"), FilterMode::BandPass);
        assert_eq!(filter_mode("BP12"), FilterMode::BandPass);
        assert_eq!(filter_mode("N12"), FilterMode::Notch);
        assert_eq!(filter_mode("N24"), FilterMode::Notch);
        assert_eq!(filter_mode("L12"), FilterMode::LowPass);
        assert_eq!(filter_mode("LBH12"), FilterMode::LowPass);
        // `HEQ12` is a high-shelf EQ band, not a highpass. Mapping it to HP cut
        // PL - Sublime Spark's whole low end and made the preset near-silent.
        assert_eq!(filter_mode("HEQ12"), FilterMode::LowPass);
        assert_eq!(filter_mode("BEQ12"), FilterMode::LowPass);
        // Stored `LEQ6` as a ~6 dB/oct low-shelf cut, i.e. high-pass shaped.
        assert_eq!(filter_mode("LEQ6"), FilterMode::HighPass);
        assert_eq!(filter_mode("LEQ12"), FilterMode::HighPass);
        assert_eq!(filter_mode("BandReject"), FilterMode::Notch);
        // `Bypass` starts with B but is not a band-pass; the importer disables
        // the filter for it.
        assert_eq!(filter_mode("Bypass"), FilterMode::LowPass);
        assert_eq!(filter_mode("Comb+"), FilterMode::LowPass);
        assert_eq!(filter_mode("Phase"), FilterMode::LowPass);
        assert_eq!(filter_mode("Formant"), FilterMode::LowPass);
    }

    #[test]
    fn filter_ids_route_by_algorithm_class() {
        // Moog class: the four ladder taps, and nothing else.
        for (id, model) in [
            ("MgL6", FilterModel::Moog6),
            ("MgL12", FilterModel::Moog12),
            ("MgL18", FilterModel::Moog18),
            ("MgL24", FilterModel::Moog24),
        ] {
            assert_eq!(filter_model(id), model, "{id}");
        }
        // Ladder class: exactly the three named ladders plus the dirty loop.
        for id in ["LadderMg", "LadderAcid", "LadderEMS"] {
            assert_eq!(filter_model(id), FilterModel::Ladder, "{id}");
        }
        assert_eq!(filter_model("DirtyMg"), FilterModel::DirtyLadder);

        // One state-variable class. 14 plain ids + 13 pair morphs + 8 triple
        // morphs; the suffix picks the 12/18/24 dB slope of that one class.
        let svf: [(&str, FilterModel); 35] = [
            ("L6", FilterModel::Svf1),
            ("L12", FilterModel::Svf),
            ("L18", FilterModel::Svf3),
            ("L24", FilterModel::Svf4),
            ("H6", FilterModel::Svf1),
            ("H12", FilterModel::Svf),
            ("H18", FilterModel::Svf3),
            ("H24", FilterModel::Svf4),
            ("B12", FilterModel::Svf),
            ("B24", FilterModel::Svf4),
            ("P12", FilterModel::Svf),
            ("P24", FilterModel::Svf4),
            ("N12", FilterModel::Svf),
            ("N24", FilterModel::Svf4),
            ("LH6", FilterModel::Svf1),
            ("LH12", FilterModel::Svf),
            ("LB12", FilterModel::Svf),
            ("LP12", FilterModel::Svf),
            ("LN12", FilterModel::Svf),
            ("HB12", FilterModel::Svf),
            ("HP12", FilterModel::Svf),
            ("HN12", FilterModel::Svf),
            ("BP12", FilterModel::Svf),
            ("BN12", FilterModel::Svf),
            ("PP12", FilterModel::Svf),
            ("PN12", FilterModel::Svf),
            ("NN12", FilterModel::Svf),
            ("LBH12", FilterModel::Svf),
            ("LBH24", FilterModel::Svf4),
            ("LPH12", FilterModel::Svf),
            ("LPH24", FilterModel::Svf4),
            ("LNH12", FilterModel::Svf),
            ("LNH24", FilterModel::Svf4),
            ("BPN12", FilterModel::Svf),
            ("BPN24", FilterModel::Svf4),
        ];
        assert_eq!(svf.len(), 35);
        for (id, model) in svf {
            assert_eq!(filter_model(id), model, "{id}");
        }
        // Families the engine approximates still use the SVF, never a ladder.
        for id in [
            "Comb+",
            "CombP",
            "FlangeP",
            "Phase",
            "Phase12P",
            "Formant",
            "FormantONE",
            "HEQ12",
            "BEQ12",
            "Scream",
            "Wsp",
            "ZDF_A",
            "Bypass",
        ] {
            assert!(
                matches!(
                    filter_model(id),
                    FilterModel::Svf | FilterModel::Svf1 | FilterModel::Svf3 | FilterModel::Svf4
                ),
                "{id} routed to {:?}",
                filter_model(id)
            );
        }
        // The enum discriminants are the serialized ABI.
        assert_eq!(FilterModel::Svf as u8, 0);
        assert_eq!(FilterModel::Ladder as u8, 1);
        assert_eq!(FilterModel::DirtyLadder as u8, 2);
        assert_eq!(FilterModel::Ladder3 as u8, 3);
        assert_eq!(FilterModel::Ladder1 as u8, 4);
        assert_eq!(FilterModel::Svf1 as u8, 5);
        assert_eq!(FilterModel::Svf3 as u8, 6);
        assert_eq!(FilterModel::Svf4 as u8, 7);
        assert_eq!(FilterModel::Moog6 as u8, 8);
        assert_eq!(FilterModel::Moog12 as u8, 9);
        assert_eq!(FilterModel::Moog18 as u8, 10);
        assert_eq!(FilterModel::Moog24 as u8, 11);
    }

    #[test]
    fn preset_filter_types_select_models_for_both_filters() {
        let data = serde_json::json!({
            "VoiceFilter0": {
                "plainParams": { "kParamEnable": 1.0, "kParamType": "MgL24" }
            },
            "VoiceFilter1": {
                "plainParams": { "kParamEnable": 1.0, "kParamType": "DirtyMg" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.filter_model, FilterModel::Moog24);
        assert_eq!(patch.filter_mode, FilterMode::LowPass);
        assert!(patch.filter2_enabled);
        assert_eq!(patch.filter2_model, FilterModel::DirtyLadder);
        assert_eq!(patch.filter2_mode, FilterMode::LowPass);
    }

    #[test]
    fn pad_lfo5_x_macro6_is_a_live_auxiliary_route() {
        // `PD - Analog Soft Cotton` ModSlot 19: LFO 5 x Macro 6 -> Filter 1
        // cutoff. Macro 6 is 0 in the factory preset, so the route must be
        // silent, but the gate has to stay live so raising Macro 6 works.
        let data = serde_json::json!({
            "Macro5": { "plainParams": { "kParamValue": 0.0 } },
            "ModSlot19": {
                "source": [10, 30],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 60.2443665266037 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.mod_routes.len(), 1);
        let route = &patch.mod_routes[0];
        assert_eq!(route.source, ModSource::Lfo5);
        // Source IDs are 1-based: 30 is Macro 6 (`Macro5` in the JSON).
        assert_eq!(route.auxiliary, Some(ModSource::Macro6));
        assert!(!route.bipolar);
        assert!(!route.auxiliary_inverted);
        assert!((route.amount - 0.602443665266037).abs() < 1e-12);
        assert_eq!(patch.extra_macros[1], 0.0);
    }

    #[test]
    fn amp_envelope_source_id_maps_to_amp_envelope() {
        // the preset source 2 is Env 1. `PL - Sublime Spark` routes it to Filter 1
        // cutoff at +72.87%; dropping it made the imported patch nearly silent.
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [2, 0],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 72.87 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.mod_routes.len(), 1);
        assert_eq!(patch.mod_routes[0].source, ModSource::AmpEnvelope);
        assert!((patch.mod_routes[0].amount - 0.7287).abs() < 1e-12);
    }

    #[test]
    fn filter_envelope_signal_is_the_squared_sustain_control() {
        // The imported filter envelope follows the square of the stored
        // control, like the amplitude envelope. The static cutoff of the demo
        // bass reads 6.56/6.91/7.49/8.93/10.92 octaves at 0/25/50/75/100 %
        // sustain; a linear control reads 1.1 octaves high at 50 %.
        let data = serde_json::json!({
            "Env1": { "plainParams": { "kParamSustain": 0.5 } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.filter_sustain - 0.25).abs() < 1e-12);
    }

    #[test]
    fn amp_curve_uses_the_decay_stage_not_the_attack_stage() {
        let data = serde_json::json!({
            "Env0": {
                "plainParams": {
                    "kParamAttack": 0.003,
                    "kParamDecay": 1.2,
                    "kParamSustain": 0.15,
                    "kParamRelease": 1.46,
                    "kParamCurve1": 50.0,
                    "kParamCurve2": 67.8,
                    "kParamCurve3": 66.6
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        // (67.8 - 50) / 50 = 0.356: linear attack curves were leaving a 1.2 s
        // decay almost flat, so the render stayed loud between plucks.
        assert!((patch.amp_curve - 0.356).abs() < 1e-12);
    }

    #[test]
    fn nonzero_auxiliary_macro_is_not_folded_into_the_amount() {
        let data = serde_json::json!({
            "Macro5": { "plainParams": { "kParamValue": 50.0 } },
            "ModSlot19": {
                "source": [10, 30],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 60.2443665266037 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let route = &patch.mod_routes[0];
        // The imported depth is the row amount; Macro 6 scales it per sample.
        assert!((route.amount - 0.602443665266037).abs() < 1e-12);
        assert_eq!(route.auxiliary, Some(ModSource::Macro6));
        assert_eq!(patch.extra_macros[1], 0.5);
    }

    #[test]
    fn mod_slot_polarity_aux_inversion_bypass_and_zero_amount() {
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [25, 30],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": {
                    "kParamAmount": -61.40352487564087,
                    "kParamBipolar": 1.0,
                    "kParamAuxInverted": 1.0
                }
            },
            "ModSlot1": {
                "source": [7, 0],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 50.0, "kParamBypass": 1.0 }
            },
            "ModSlot2": {
                "source": [7, 0],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 0.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        // The bypassed and zero-amount rows contribute nothing.
        assert_eq!(patch.mod_routes.len(), 1);
        let route = &patch.mod_routes[0];
        assert_eq!(route.source, ModSource::Macro1);
        assert_eq!(route.auxiliary, Some(ModSource::Macro6));
        assert!(route.bipolar);
        assert!(route.auxiliary_inverted);
        assert!((route.amount + 0.6140352487564087).abs() < 1e-12);
    }

    #[test]
    fn preset_detune_matches_stored_curve() {
        // Each pair is (stored value, half-span cents) from the stored mapping
        // `200 * sqrt(stored)^2.5` law; 0.16 is the point checked against the
        // plug-in's own 0.4 control.
        for (raw, cents) in [
            (0.04, 3.578),
            (0.16, 20.239),
            (0.25, 35.355),
            (0.5, 84.090),
            (1.0, 200.0),
        ] {
            let mapped = preset_detune_cents(raw);
            assert!(
                (mapped - cents).abs() < 0.2,
                "detune {raw} -> {mapped}, expected {cents}"
            );
        }
        // Legacy values above 1.0 were already cents.
        assert!((preset_detune_cents(20.0) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn fx_macro_routes_are_real_routes_not_folded_parameters() {
        let data = serde_json::json!({
            "Macro1": { "plainParams": { "kParamValue": 20.0 } },
            "ModSlot0": {
                "source": [26, 0],
                "destModuleTypeString": "FXDistortion",
                "destModuleID": 0,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "FXRack0": {
                "FX": [{
                    "FXDistortion": {
                        "plainParams": { "kParamDrive": 50.0, "kParamWet": 0.0 }
                    },
                    "type": 0
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let distortion = patch
            .fx
            .iter()
            .find(|fx| fx.fx_type == SynthFxType::Distortion)
            .expect("distortion must import");
        // The importer keeps the macro value out of the base mix.
        assert_eq!(distortion.param("mix"), Some(0.0));
        assert_eq!(patch.fx_mod_routes.len(), 1);
        let route = patch.fx_mod_routes[0];
        assert_eq!(route.source, ModSource::Macro2);
        assert_eq!(route.fx_index, 0);
        assert_eq!(route.parameter, FxParam::Wet);
        assert!((route.amount - 1.0).abs() < 1e-9);
        assert!(!route.bipolar);
    }

    #[test]
    fn imported_delay_without_feedback_uses_preset_default() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "FXDelay": {
                        "plainParams": { "kParamTimeL": 0.05, "kParamWet": 40.0 }
                    },
                    "type": 4
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let delay = patch
            .fx
            .iter()
            .find(|fx| fx.fx_type == SynthFxType::Delay)
            .expect("delay must import");
        assert_eq!(delay.param("feedback"), Some(0.4));
        // The stored time is the control, not seconds; the DSP curve maps it
        // to 0.5 s. Pinning the raw value keeps the curve the only mapping.
        assert_eq!(delay.param("time"), Some(0.05));
    }

    #[test]
    fn imported_distortion_stage_count_is_clamped_to_the_palette() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "FXDistortion": {
                        "plainParams": { "kParamDrive": 50.0, "kParamNumStages": 9.0 }
                    },
                    "type": 0
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let distortion = patch
            .fx
            .iter()
            .find(|fx| fx.fx_type == SynthFxType::Distortion)
            .expect("distortion must import");
        assert_eq!(distortion.param("stages"), Some(9.0));
        assert_eq!(distortion.param("mode"), Some(0.0));
    }

    #[test]
    fn bass_style_fx_routes_are_imported_to_the_right_slots_and_parameters() {
        // Mirrors BA - Sweet Bass: Macro2 -> distortion wet, Macro4 -> EQ high
        // gain, Macro8 (serialized Macro7) -> flanger wet, plus the LFO
        // depth/feedback and EQ-frequency routes from that preset.
        let data = serde_json::json!({
            "ModSlot3": {
                "source": [26, 0],
                "destModuleTypeString": "FXDistortion",
                "destModuleID": 0,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "ModSlot6": {
                "source": [28, 0],
                "destModuleTypeString": "FXEQ",
                "destModuleID": 1,
                "destModuleParamName": "kParamGain2",
                "plainParams": { "kParamAmount": 39.473676681518555 }
            },
            "ModSlot9": {
                "source": [31, 0],
                "destModuleTypeString": "FXEQ",
                "destModuleID": 1,
                "destModuleParamName": "kParamFreq1",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "ModSlot10": {
                "source": [8, 0],
                "destModuleTypeString": "FXFlanger",
                "destModuleID": 2,
                "destModuleParamName": "kParamFeedback",
                "plainParams": { "kParamAmount": -11.32138967514038 }
            },
            "ModSlot11": {
                "source": [9, 0],
                "destModuleTypeString": "FXFlanger",
                "destModuleID": 2,
                "destModuleParamName": "kParamDepth",
                "plainParams": { "kParamAmount": 47.51026630401611 }
            },
            "ModSlot12": {
                "source": [32, 0],
                "destModuleTypeString": "FXFlanger",
                "destModuleID": 2,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "FXRack0": {
                "FX": [
                    { "type": 0, "FXDistortion": { "plainParams": { "kParamWet": 0.0 } } },
                    { "type": 6, "FXEQ": { "plainParams": { "kParamGain2": 5.0 } } },
                    { "type": 3, "FXFlanger": { "plainParams": { "kParamWet": 0.0 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 3);
        assert_eq!(patch.fx_mod_routes.len(), 6);
        let find = |source, parameter| {
            patch
                .fx_mod_routes
                .iter()
                .find(|route| route.source == source && route.parameter == parameter)
                .copied()
                .unwrap_or_else(|| panic!("missing {source:?} -> {parameter:?} route"))
        };
        assert_eq!(find(ModSource::Macro2, FxParam::Wet).fx_index, 0);
        let gain = find(ModSource::Macro4, FxParam::Gain2);
        assert_eq!(gain.fx_index, 1);
        assert!((gain.amount - 0.394_736_766_815_185_55).abs() < 1e-9);
        assert_eq!(find(ModSource::Macro7, FxParam::Freq1).fx_index, 1);
        let feedback = find(ModSource::Lfo3, FxParam::Feedback);
        assert_eq!(feedback.fx_index, 2);
        assert!(feedback.amount < 0.0);
        assert_eq!(find(ModSource::Lfo4, FxParam::Depth).fx_index, 2);
        assert_eq!(find(ModSource::Macro8, FxParam::Wet).fx_index, 2);
    }

    #[test]
    fn fx_routes_to_sent_bus_modules_resolve() {
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [26, 0],
                "destModuleTypeString": "FXDelay",
                "destModuleID": 100,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "FXRack1": {
                "FX": [{ "type": 4, "FXDelay": { "plainParams": { "kParamTimeL": 0.2 } } }]
            },
            "RoutingSlot0": { "plainParams": { "kParamFXBus1Level": 100.0 } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::ParallelRack);
        assert_eq!(patch.fx[1].fx_type, SynthFxType::Delay);
        assert_eq!(patch.fx_mod_routes.len(), 1);
        assert_eq!(patch.fx_mod_routes[0].fx_index, 1);
        assert_eq!(patch.fx_mod_routes[0].parameter, FxParam::Wet);
    }

    #[test]
    fn fx_routes_to_skipped_bus_racks_are_dropped() {
        // the preset IDs are rack*100 + position. This bus rack has no routing send,
        // so it is not imported and a route that addresses it has no target.
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [26, 0],
                "destModuleTypeString": "FXDistortion",
                "destModuleID": 103,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "ModSlot1": {
                "source": [26, 0],
                "destModuleTypeString": "FXDistortion",
                "destModuleID": 0,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "FXRack0": {
                "FX": [
                    { "type": 0, "FXDistortion": { "plainParams": { "kParamWet": 0.0 } } }
                ]
            },
            "FXRack1": {
                "FX": [
                    { "type": 0, "FXDistortion": { "plainParams": { "kParamWet": 0.0 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Distortion);
        assert_eq!(patch.fx_mod_routes.len(), 1);
        assert_eq!(patch.fx_mod_routes[0].fx_index, 0);
        assert_eq!(patch.fx_mod_routes[0].parameter, FxParam::Wet);
    }

    #[test]
    fn skipped_fx_mod_targets_are_dropped_without_misaddressing() {
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [26, 0],
                "destModuleTypeString": "FXFuture",
                "destModuleID": 0,
                "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "ModSlot1": {
                "source": [26, 0],
                "destModuleTypeString": "FXDelay",
                "destModuleID": 1,
                "destModuleParamName": "kParamOffsetL",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "FXRack0": {
                "FX": [
                    { "type": 99, "FXFuture": { "plainParams": { "kParamWet": 30.0 } } },
                    { "type": 4, "FXDelay": { "plainParams": { "kParamWet": 20.0 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Delay);
        assert!(patch.fx_mod_routes.is_empty());
    }

    #[test]
    fn imported_distortion_level_and_eq_units_match_the_engine() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    {
                        "type": 0,
                        "FXDistortion": {
                            "plainParams": {
                                "kParamDrive": 50.0,
                                "kParamWet": 80.0,
                                "kParamLevelOut": 0.5
                            }
                        }
                    },
                    {
                        "type": 6,
                        "FXEQ": {
                            "plainParams": {
                                "kParamFreq1": 291.48,
                                "kParamFreq2": 5486.7,
                                "kParamGain1": -5.26,
                                "kParamGain2": 5.05
                            }
                        }
                    }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let distortion = &patch.fx[0];
        assert!((distortion.param("drive").unwrap() - 0.5).abs() < 1e-9);
        assert!((distortion.param("mix").unwrap() - 0.8).abs() < 1e-9);
        // The native wet trim stays neutral; the preset's trim is `level_out`.
        assert!((distortion.param("level").unwrap() - 1.0).abs() < 1e-9);
        // kParamLevelOut 0.5 is unity under the stored `4 * LO^2` law.
        assert!((distortion.param("level_out").unwrap() - 1.0).abs() < 1e-9);
        let eq = &patch.fx[1];
        assert!((eq.param("low").unwrap() - -5.26).abs() < 1e-9);
        assert!((eq.param("high").unwrap() - 5.05).abs() < 1e-9);
        assert!((eq.param("low_freq").unwrap() - 291.48).abs() < 1e-9);
        assert!((eq.param("high_freq").unwrap() - 5486.7).abs() < 1e-9);
        // No type keys: the legacy shelves stay in place.
        assert_eq!(eq.param("type1"), Some(-1.0));
        assert_eq!(eq.param("type2"), Some(-1.0));
        assert_eq!(eq.param("reso1"), Some(60.0));
    }

    #[test]
    fn imported_compressor_threshold_follows_the_stored_taper() {
        // Fit values of 60*log10(1-a) + 13.50 dB; the stored knees at
        // 0.30/0.45/0.60/0.75/0.90 are within 0.2 dB of these.
        for (knob, expected) in [
            (0.0, 13.50),
            (0.30, 4.21),
            (0.45, -2.08),
            (0.60, -10.38),
            (0.75, -22.62),
            (0.90, -46.50),
        ] {
            let value = preset_compressor_threshold(knob);
            assert!(
                (value - expected).abs() < 0.05,
                "knob {knob}: {value:.2} dB vs {expected:.2} dB"
            );
        }
        // The knob mutes at 1; the device floor is -60 dB.
        assert!((preset_compressor_threshold(1.0) + 60.0).abs() < 1e-9);
        // Older reserialized trees may already carry a dB value.
        assert_eq!(preset_compressor_threshold(-24.0), -24.0);
    }

    #[test]
    fn imported_compressor_makeup_is_a_linear_amplitude() {
        assert!((preset_compressor_makeup(1.0)).abs() < 1e-9);
        assert!((preset_compressor_makeup(2.0) - 6.0206).abs() < 0.01);
        assert!((preset_compressor_makeup(3.7475763) - 11.48).abs() < 0.02);
        assert_eq!(preset_compressor_makeup(0.0), 0.0);
    }

    #[test]
    fn imported_level_out_follows_the_preset_square_law() {
        assert!((preset_level_out(None) - 1.0).abs() < 1e-9);
        assert!((preset_level_out(Some(0.5)) - 1.0).abs() < 1e-9);
        assert!((preset_level_out(Some(0.25)) - 0.25).abs() < 1e-9);
        assert!((preset_level_out(Some(1.0)) - 4.0).abs() < 1e-9);
        assert!((preset_level_out(Some(0.0))).abs() < 1e-9);

        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    {
                        "type": 0,
                        "FXDistortion": {
                            "plainParams": { "kParamDrive": 20.0, "kParamLevelOut": 1.0 }
                        }
                    },
                    {
                        "type": 6,
                        "FXEQ": { "plainParams": { "kParamLevelOut": 0.5 } }
                    }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.fx[0].param("level_out").unwrap() - 4.0).abs() < 1e-9);
        assert!((patch.fx[1].param("level_out").unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn imported_compressor_makeup_passes_through() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 5,
                    "FXComp": { "plainParams": { "kParamMakeup": 4.0 } }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let expected = 20.0 * 4.0_f64.log10();
        assert!((patch.fx[0].param("makeup").unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn imported_chorus_reads_preset_parameters() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 2,
                    "FXChorus": {
                        "plainParams": {
                            "kParamRate": 0.5,
                            "kParamDepth": 50.0,
                            "kParamFeedback": 20.0,
                            "kParamWet": 40.0
                        }
                    }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let chorus = &patch.fx[0];
        assert!((chorus.param("rate").unwrap() - 0.5).abs() < 1e-9);
        assert!((chorus.param("depth").unwrap() - 0.5).abs() < 1e-9);
        assert!((chorus.param("feedback").unwrap() - 0.2).abs() < 1e-9);
        assert!((chorus.param("mix").unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn hyperd_and_conv_import_as_the_real_engine_types() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    { "type": 11, "FXConv": { "plainParams": { "kParamWet": 100.0 } } },
                    { "type": 9, "FXHyperD": { "plainParams": { "kParamWet": 50.0 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 2);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Conv);
        assert_eq!(patch.fx[1].fx_type, SynthFxType::HyperD);
        assert!((patch.fx[0].param("mix").unwrap() - 1.0).abs() < 1e-9);
        assert!((patch.fx[1].param("mix").unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn utils_and_bode_params_follow_the_preset_encoding() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    { "type": 12, "FXUtils": { "plainParams": {
                        "kParamHPF": 400.0, "kParamLPF": 500.0, "kParamLFMono": 1.0,
                        "kParamLFXover": 150.0, "kParamWidth": 200.0,
                        "kParamBalance": -50.0, "kParamPolarityL": 1.0,
                        "kParamPolarityR": 0.0, "kParamWet": 50.0
                    }}},
                    { "type": 8, "FXBode": { "plainParams": {
                        "kParamShift": 50.0, "kParamRange": 1000.0,
                        "kParamFeedback": 50.0, "kParamDelayTime": 0.1,
                        "kParamOutputMix": -100.0, "kParamOutputWidth": 25.0,
                        "kParamMonoInput": 1.0, "kParamWet": 100.0
                    }}}
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 2);
        let utils = &patch.fx[0];
        assert_eq!(utils.fx_type, SynthFxType::Utils);
        assert_eq!(utils.param("hpf"), Some(400.0));
        assert_eq!(utils.param("lpf"), Some(500.0));
        assert_eq!(utils.param("lf_mono"), Some(1.0));
        assert_eq!(utils.param("lf_xover"), Some(150.0));
        assert_eq!(utils.param("width"), Some(200.0));
        assert_eq!(utils.param("balance"), Some(-50.0));
        assert_eq!(utils.param("polarity_l"), Some(1.0));
        assert_eq!(utils.param("polarity_r"), Some(0.0));
        assert_eq!(utils.param("mix"), Some(0.5));
        // Omitted LevelOut keeps the unity 4 * 0.5^2 default.
        assert_eq!(utils.param("level_out"), Some(1.0));

        let bode = &patch.fx[1];
        assert_eq!(bode.fx_type, SynthFxType::Bode);
        assert_eq!(bode.param("shift"), Some(50.0));
        assert_eq!(bode.param("range"), Some(1000.0));
        assert_eq!(bode.param("feedback"), Some(0.5));
        assert_eq!(bode.param("delay"), Some(0.1));
        assert_eq!(bode.param("output_mix"), Some(-100.0));
        assert_eq!(bode.param("output_width"), Some(25.0));
        assert_eq!(bode.param("mono_input"), Some(1.0));
    }

    #[test]
    fn splitter_counts_are_rewritten_for_skipped_children() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    { "type": 13, "FXSplit": { "plainParams": {
                        "kParamFreq": 400.0, "kParamModuleCount1": 1.0,
                        "kParamModuleCount2": 1.0
                    }}},
                    // Unknown classes are skipped, so branch 1 must consume nothing.
                    { "type": 99, "FXFuture": { "plainParams": { "kParamWet": 100.0 } } },
                    { "type": 0, "FXDistortion": { "plainParams": { "kParamDrive": 50.0 } } },
                    { "type": 7, "FXEQ": { "plainParams": { "kParamFreq1": 400.0 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 3);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Split);
        assert_eq!(patch.fx[0].param("count1"), Some(0.0));
        assert_eq!(patch.fx[0].param("count2"), Some(1.0));
        assert_eq!(patch.fx[1].fx_type, SynthFxType::Distortion);
        assert_eq!(patch.fx[2].fx_type, SynthFxType::Eq3);
    }

    #[test]
    fn split3_and_split_ms_import_their_branch_counts() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    { "type": 14, "FXSplit3": { "plainParams": {
                        "kParamFreq": 200.0, "kParamFreq2": 2_000.0,
                        "kParamModuleCount1": 1.0, "kParamModuleCount2": 0.0,
                        "kParamModuleCount3": 2.0
                    }}},
                    { "type": 0, "FXDistortion": { "plainParams": {} } },
                    { "type": 7, "FXEQ": { "plainParams": {} } },
                    { "type": 5, "FXComp": { "plainParams": {} } },
                    { "type": 15, "FXSplitMS": { "plainParams": {
                        "kParamModuleCount1": 1.0, "kParamModuleCount2": 1.0
                    }}},
                    { "type": 1, "FXFilter": { "plainParams": { "kParamFreq": 0.5 } } },
                    { "type": 1, "FXFilter": { "plainParams": { "kParamFreq": 0.5 } } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 7);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Split3);
        assert_eq!(patch.fx[0].param("count1"), Some(1.0));
        assert_eq!(patch.fx[0].param("count2"), Some(0.0));
        assert_eq!(patch.fx[0].param("count3"), Some(2.0));
        assert_eq!(patch.fx[4].fx_type, SynthFxType::SplitMs);
        assert_eq!(patch.fx[4].param("count1"), Some(1.0));
        assert_eq!(patch.fx[4].param("count2"), Some(1.0));
        assert_eq!(patch.fx[5].fx_type, SynthFxType::Filter);
        assert_eq!(patch.fx[6].fx_type, SynthFxType::Filter);
    }

    #[test]
    fn nested_split_child_chain_is_imported_in_place() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    // Branch 1 of the outer split is the nested split plus its
                    // own child; branch 2 is a single module.
                    { "type": 13, "FXSplit": { "plainParams": {
                        "kParamFreq": 800.0,
                        "kParamModuleCount1": 2.0, "kParamModuleCount2": 1.0
                    }}},
                    { "type": 15, "FXSplitMS": { "plainParams": {
                        "kParamModuleCount1": 1.0, "kParamModuleCount2": 0.0
                    }}},
                    { "type": 4, "FXDelay": { "plainParams": { "kParamTimeL": 0.2 } } },
                    { "type": 3, "FXChorus": { "plainParams": {} } },
                    { "type": 6, "FXReverb": { "plainParams": {} } }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 5);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Split);
        assert_eq!(patch.fx[0].param("count1"), Some(2.0));
        assert_eq!(patch.fx[0].param("count2"), Some(1.0));
        assert_eq!(patch.fx[1].fx_type, SynthFxType::SplitMs);
        assert_eq!(patch.fx[1].param("count1"), Some(1.0));
        assert_eq!(patch.fx[1].param("count2"), Some(0.0));
        assert_eq!(patch.fx[2].fx_type, SynthFxType::Delay);
        assert_eq!(patch.fx[3].fx_type, SynthFxType::Chorus);
        assert_eq!(patch.fx[4].fx_type, SynthFxType::Reverb);
    }

    #[test]
    fn sent_bus_racks_become_parallel_branches() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    { "type": 0, "FXDistortion": { "plainParams": {} } }
                ]
            },
            "FXRack1": {
                "FX": [
                    { "type": 4, "FXDelay": { "plainParams": { "kParamTimeL": 0.2 } } }
                ]
            },
            "FXRack2": {
                "FX": [
                    { "type": 12, "FXUtils": { "plainParams": { "kParamHPF": 200.0 } } }
                ]
            },
            "RoutingSlot0": { "plainParams": { "kParamFXBus1Level": 100.0 } },
            "RoutingSlot3": { "plainParams": { "kParamFXBus2Level": 50.0 } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 4);
        let parallel = &patch.fx[0];
        assert_eq!(parallel.fx_type, SynthFxType::ParallelRack);
        assert_eq!(parallel.param("count1"), Some(0.0));
        assert_eq!(parallel.param("count2"), Some(1.0));
        assert_eq!(parallel.param("count3"), Some(1.0));
        assert_eq!(parallel.param("gain1"), Some(1.0));
        assert_eq!(parallel.param("gain2"), Some(1.0));
        assert_eq!(parallel.param("gain3"), Some(0.5));
        assert_eq!(patch.fx[1].fx_type, SynthFxType::Delay);
        assert_eq!(patch.fx[2].fx_type, SynthFxType::Utils);
        assert_eq!(patch.fx[3].fx_type, SynthFxType::Distortion);
    }

    #[test]
    fn split_inside_a_bus_rack_keeps_its_branch_counts() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{ "type": 0, "FXDistortion": { "plainParams": {} } }]
            },
            "FXRack1": {
                "FX": [
                    { "type": 13, "FXSplit": { "plainParams": {
                        "kParamFreq": 600.0,
                        "kParamModuleCount1": 0.0, "kParamModuleCount2": 1.0
                    }}},
                    { "type": 9, "FXFilter": { "plainParams": { "kParamFreq": 0.5 } } }
                ]
            },
            "RoutingSlot2": { "plainParams": { "kParamFXBus1Level": 100.0 } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 4);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::ParallelRack);
        assert_eq!(patch.fx[0].param("count2"), Some(2.0));
        assert_eq!(patch.fx[1].fx_type, SynthFxType::Split);
        assert_eq!(patch.fx[1].param("count1"), Some(0.0));
        assert_eq!(patch.fx[1].param("count2"), Some(1.0));
        assert_eq!(patch.fx[2].fx_type, SynthFxType::Filter);
        assert_eq!(patch.fx[3].fx_type, SynthFxType::Distortion);
    }

    #[test]
    fn bus_racks_without_a_send_do_not_enter_the_chain() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{ "type": 0, "FXDistortion": { "plainParams": {} } }]
            },
            "FXRack1": {
                "FX": [{ "type": 4, "FXDelay": { "plainParams": {} } }]
            },
            "RoutingSlot0": { "plainParams": { "kParamRoutingDest": "kRoutingDestMaster" } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Distortion);
    }

    #[test]
    fn sent_bus_with_only_unmapped_modules_adds_no_branch() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{ "type": 0, "FXDistortion": { "plainParams": {} } }]
            },
            "FXRack1": {
                "FX": [{ "type": 99, "FXFuture": { "plainParams": { "kParamWet": 100.0 } } }]
            },
            "RoutingSlot0": { "plainParams": { "kParamFXBus1Level": 100.0 } }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Distortion);
    }

    #[test]
    #[ignore = "requires an installed preset library (OCP_PRESET_LIBRARY)"]
    fn factory_pluck_imports_its_sent_bus_rack() {
        let Some(root) = std::env::var_os("OCP_PRESET_LIBRARY").map(PathBuf::from) else {
            eprintln!("set OCP_PRESET_LIBRARY to the preset library root; skipping");
            return;
        };
        let path = root.join("Factory/Pluck/PL - Sublime Spark.SerumPreset");
        if !path.is_file() {
            eprintln!("{} is missing; skipping", path.display());
            return;
        }
        let patch = ForeignPreset::load(&path)
            .expect("PL - Sublime Spark must load")
            .to_synth_patch(None);
        let parallel = &patch.fx[0];
        assert_eq!(parallel.fx_type, SynthFxType::ParallelRack);
        assert_eq!(parallel.param("count2"), Some(2.0));
        assert_eq!(parallel.param("count3"), Some(0.0));
        assert_eq!(parallel.param("gain2"), Some(1.0));
        // The Delay lives only on bus 1; the silent bus 2 chain is omitted.
        assert_eq!(
            patch
                .fx
                .iter()
                .filter(|fx| fx.fx_type == SynthFxType::Delay)
                .count(),
            1
        );
    }

    #[test]
    fn imported_eq_band_types_and_resonance_follow_the_preset_encoding() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "FXEQ": {
                        "plainParams": {
                            "kParamFreq1": 291.48,
                            "kParamFreq2": 5486.7,
                            "kParamGain1": -5.26,
                            "kParamGain2": 5.05,
                            "kParamType1": 1.0,
                            "kParamType2": 2.0,
                            "kParamReso1": 39.82,
                            "kParamReso2": 28.86
                        }
                    }
                }]
            }
        });
        let eq = &synthetic_preset(data).to_synth_patch(None).fx[0];
        assert_eq!(eq.param("type1"), Some(1.0));
        assert_eq!(eq.param("type2"), Some(2.0));
        assert!((eq.param("reso1").unwrap() - 39.82).abs() < 1e-9);
        assert!((eq.param("reso2").unwrap() - 28.86).abs() < 1e-9);
    }

    #[test]
    fn fx_filter_heq_is_not_imported_as_highpass() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 8,
                    "FXFilter": {
                        "plainParams": {
                            "kParamType": "HEQ12",
                            "kParamFreq": 0.7853618860244751
                        }
                    }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Filter);
        // 0 = lowpass fallback (the engine has no shelf response). The old
        // leading-H rule selected 1 = highpass and removed the low end.
        assert_eq!(patch.fx[0].param("mode"), Some(0.0));
    }

    #[test]
    fn compressor_reads_preset_threshold() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 5,
                    "FXComp": {
                        "plainParams": {
                            "kParamThresh": 0.5,
                            "kParamRatio": 4.0,
                            "kParamAttack": 10.0,
                            "kParamRelease": 100.0
                        }
                    }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Compressor);
        let expected = preset_compressor_threshold(0.5) + SINE_CREST_DB;
        assert!((patch.fx[0].param("threshold").unwrap() - expected).abs() < 1e-9);
        // The raw preset key is preserved for diagnostics.
        assert!(
            patch.fx[0]
                .params
                .iter()
                .any(|(key, _)| key == "kParamThresh")
        );
    }

    #[test]
    fn single_band_compressor_maps_onto_the_three_band_compander() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    {
                        "type": 5,
                        "FXComp": {
                            "plainParams": {
                                "kParamThresh": 0.5,
                                "kParamRatio": 4.0,
                                "kParamAttack": 10.0,
                                "kParamRelease": 100.0,
                                "kParamMakeup": 2.0,
                                "kParamWet": 100.0,
                                "kParamGain1": 3.5,
                                "kParamCompensatedWetDry": 1.0
                            }
                        }
                    },
                    {
                        "type": 5,
                        "FXComp": {
                            "plainParams": {
                                "kParamThresh": 0.25,
                                "kParamCompensatedWetDry": 0.0
                            }
                        }
                    }
                ]
            }
        });
        let mut diagnostics = ImportDiagnostics::default();
        let patch = synthetic_preset(data).to_synth_patch_diagnosed(None, &mut diagnostics);
        assert_eq!(patch.fx.len(), 2);
        let fx = &patch.fx[0];
        assert_eq!(fx.fx_type, SynthFxType::Compressor);
        assert_eq!(fx.param("crossover_low"), Some(120.0));
        assert_eq!(fx.param("crossover_high"), Some(2_500.0));
        let expected = preset_compressor_threshold(0.5) + SINE_CREST_DB;
        assert!((fx.param("threshold").unwrap() - expected).abs() < 1e-9);
        assert_eq!(fx.param("ratio"), Some(4.0));
        assert_eq!(fx.param("attack"), Some(10.0));
        assert_eq!(fx.param("release"), Some(100.0));
        assert!((fx.param("makeup").unwrap() - 6.0206).abs() < 0.01);
        assert_eq!(fx.param("mix"), Some(1.0));
        assert_eq!(fx.param("band2_gain"), Some(3.5));
        assert_eq!(fx.param("limiter"), Some(1.0));
        // One deduplicated warning for the two unmapped compensation flags.
        let warning = diagnostics
            .warnings()
            .iter()
            .find(|warning| warning.class == "fx-param:compressor:kParamCompensatedWetDry");
        let warning = warning.expect("unmapped compensation flag must warn");
        assert_eq!(warning.count, 2);
    }

    #[test]
    fn compressor_threshold_is_highest_at_zero_and_mutes_at_one() {
        assert!((preset_compressor_threshold(0.0) - 13.5).abs() < 1e-9);
        assert!((preset_compressor_threshold(0.5) - (-4.58)).abs() < 0.02);
        assert!((preset_compressor_threshold(1.0) + 60.0).abs() < 1e-9);
        // A dB value written by an older importer is passed through.
        assert_eq!(preset_compressor_threshold(-12.0), -12.0);
    }

    #[test]
    fn fx_bus_racks_are_not_appended_to_the_serial_chain() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{ "type": 3, "FXDelay": { "plainParams": { "kParamWet": 25.0 } } }]
            },
            "FXRack1": {
                "FX": [{
                    "type": 3,
                    "FXDelay": { "plainParams": { "kParamWet": 100.0 } }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Delay);
    }

    #[test]
    fn percent_controls_use_0_100_scale() {
        assert!((normalize_percent(50.0) - 0.5).abs() < 1e-9);
        assert!((normalize_percent(0.5) - 0.005).abs() < 1e-9);
    }

    #[test]
    fn maps_c_pitch_and_fine() {
        let data = serde_json::json!({
            "Oscillator2": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamPitch": -7.0,
                    "kParamFine": -50.0
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.oscillator_c_semitones + 7.5).abs() < 1e-9);
    }

    #[test]
    fn maps_third_and_fourth_envelopes() {
        let data = serde_json::json!({
            "Env2": {
                "plainParams": {
                    "kParamAttack": 0.25,
                    "kParamDecay": 0.5,
                    "kParamSustain": 0.75,
                    "kParamRelease": 1.5,
                    "kParamCurve1": 25.0
                }
            },
            "Env3": {
                "plainParams": {
                    "kParamAttack": 2.0,
                    "kParamDecay": 3.0,
                    "kParamSustain": 0.25,
                    "kParamRelease": 4.0,
                    "kParamCurve1": 75.0
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.env3_attack - 0.25).abs() < 1e-9);
        assert!((patch.env3_decay - 0.5).abs() < 1e-9);
        assert!((patch.env3_sustain - 0.75).abs() < 1e-9);
        assert!((patch.env3_release - 1.5).abs() < 1e-9);
        assert!((patch.env3_curve + 0.5).abs() < 1e-9);
        assert!((patch.env4_attack - 2.0).abs() < 1e-9);
        assert!((patch.env4_decay - 3.0).abs() < 1e-9);
        assert!((patch.env4_sustain - 0.25).abs() < 1e-9);
        assert!((patch.env4_release - 4.0).abs() < 1e-9);
        assert!((patch.env4_curve - 0.5).abs() < 1e-9);
    }

    #[test]
    fn maps_lfo2_through_lfo10_into_extra_lfos() {
        let data = serde_json::json!({
            "LFO1": {
                "plainParams": {
                    "kParamRate": 6.25,
                    "kParamBeatSync": 1.0,
                    "kParamMode": "Trigger",
                    "kParamType": "RandomSH"
                },
                "curveData": {}
            },
            "LFO2": {
                "plainParams": {
                    "kParamRate": 2.0,
                    "kParamBeatSync": 0.0,
                    "kParamMode": "Free"
                },
                "curveData": {
                    "numPoints": 2,
                    "xVals": [0.0, 0.5, 1.0],
                    "yVals": [1.0, 0.0, 1.0],
                    "curveVals": [0.49999997, 0.49999997, 0.49999997]
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        let lfo2 = patch.extra_lfos[0].clone();
        assert!((lfo2.rate - 6.25).abs() < 1e-9);
        assert_eq!(lfo2.shape_type, LfoShapeType::RandomHold);
        assert!(lfo2.sync);
        assert!(lfo2.retrigger);
        assert_eq!(lfo2.division, 2);
        let lfo3 = patch.extra_lfos[1].clone();
        assert!((lfo3.rate - 2.0).abs() < 1e-9);
        assert_eq!(lfo3.shape_type, LfoShapeType::Curve);
        assert_eq!(*lfo3.shape, LfoShape::triangle());
        assert!(!lfo3.sync);
        assert!(!lfo3.retrigger);
    }

    #[test]
    fn maps_last_mod_slot_and_filter_two_destination() {
        let data = serde_json::json!({
            "ModSlot63": {
                "source": [4, 0],
                "destModuleTypeString": "VoiceFilter",
                "destModuleID": 1,
                "destModuleParamName": "kParamFreq",
                "plainParams": { "kParamAmount": 25.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.mod_routes.len(), 1);
        assert_eq!(patch.mod_routes[0].source, ModSource::Env3);
        assert_eq!(
            patch.mod_routes[0].destination,
            ModDestination::Filter2Cutoff
        );
        assert!((patch.mod_routes[0].amount - 0.25).abs() < 1e-9);
    }

    #[test]
    fn maps_oscillator_a_noise_sub_levels_and_filter_wet_routes() {
        let data = serde_json::json!({
            "Oscillator0": { "plainParams": { "kParamEnable": 1.0, "kParamVolume": 0.0 } },
            "Oscillator3": { "plainParams": { "kParamEnable": 1.0, "kParamVolume": 0.0 } },
            "Oscillator4": { "plainParams": { "kParamVolume": 0.75 } },
            "VoiceFilter0": {
                "plainParams": {
                    "kParamEnable": 1.0, "kParamType": "HN12",
                    "kParamFreq": 0.23245613276958466, "kParamWet": 35.52631735801697
                }
            },
            "ModSlot2": {
                "source": [6, 0], "destModuleTypeString": "Oscillator",
                "destModuleID": 0, "destModuleParamName": "kParamVolume",
                "plainParams": { "kParamAmount": 65.0 }
            },
            "ModSlot6": {
                "source": [6, 0], "destModuleTypeString": "Oscillator",
                "destModuleID": 3, "destModuleParamName": "kParamVolume",
                "plainParams": { "kParamAmount": 25.44 }
            },
            "ModSlot7": {
                "source": [25, 0], "destModuleTypeString": "VoiceFilter",
                "destModuleID": 0, "destModuleParamName": "kParamWet",
                "plainParams": { "kParamAmount": 100.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.filter_wet - 0.355_263_173_580_169_7).abs() < 1e-9);
        let destinations: Vec<_> = patch.mod_routes.iter().map(|r| r.destination).collect();
        assert!(destinations.contains(&ModDestination::OscillatorALevel));
        assert!(destinations.contains(&ModDestination::NoiseLevel));
        assert!(destinations.contains(&ModDestination::FilterWet));
        // The sub volume is stored even though the slot has no enable key; the
        // slot is off until the preset's sub tab is selected.
        assert!((patch.sub_level - 0.0).abs() < 1e-9);
        assert!((patch.sub_semitones - 0.0).abs() < 1e-9);
    }

    #[test]
    fn disabled_voice_filter_bypasses_the_engine_default_lowpass() {
        let data = serde_json::json!({
            "VoiceFilter0": {
                "plainParams": { "kParamEnable": 0.0, "kParamFreq": 0.0, "kParamType": "MgL24" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.filter_wet, 0.0);
    }

    #[test]
    fn a_stored_filter_without_an_enable_key_is_bypassed() {
        // The format writes `kParamEnable` only on an engaged filter; the
        // controls left beside a disabled one are stale UI values, not a sound.
        let data = serde_json::json!({
            "VoiceFilter0": {
                "plainParams": { "kParamFreq": 0.43168577551841736, "kParamReso": 10.0 }
            },
            "VoiceFilter1": {
                "plainParams": { "kParamReso": 10.0, "kParamType": "MgL24" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.filter_wet, 0.0);
        assert!(!patch.filter2_enabled);

        // The module is optional; a preset without one is unfiltered as well.
        let patch = synthetic_preset(serde_json::json!({})).to_synth_patch(None);
        assert_eq!(patch.filter_wet, 0.0);
    }

    #[test]
    fn bypass_filter_type_is_passthrough_for_both_voice_filters() {
        let data = serde_json::json!({
            "VoiceFilter0": {
                "plainParams": { "kParamEnable": 1.0, "kParamType": "Bypass", "kParamWet": 100.0 }
            },
            "VoiceFilter1": {
                "plainParams": { "kParamEnable": 1.0, "kParamType": "Bypass", "kParamWet": 100.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.filter_wet, 0.0);
        assert!(!patch.filter2_enabled);
        assert_eq!(patch.filter_mode, FilterMode::LowPass);

        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 8,
                    "FXFilter": { "plainParams": { "kParamType": "Bypass", "kParamEnable": 1.0 } }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert!(!patch.fx[0].enabled);
    }

    #[test]
    fn unison_stereo_defaults_to_full_spread() {
        let data = serde_json::json!({
            "Oscillator0": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamVolume": 0.5,
                    "kParamUnison": 2.0,
                    "kParamDetune": 0.2,
                    "kParamDetuneWid": 0.0
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        // the preset's knob defaults to 100 and `kParamDetuneWid` is a detune curve.
        assert_eq!(patch.stereo_width, 1.0);
        assert_eq!(patch.oscillator_b_stereo_width, 1.0);
        assert_eq!(patch.oscillator_c_stereo_width, 1.0);
    }

    #[test]
    fn wavetable_oscillators_use_preset_phase_defaults() {
        let data = serde_json::json!({
            "Oscillator0": {
                "WTOsc0": { "plainParams": { "kParamTablePos": 1.0 } },
                "plainParams": { "kParamEnable": 1.0, "kParamVolume": 0.5 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.oscillator_start_phase - 0.5).abs() < 1e-9);
        // The engine's own unison stagger supplies the random-phase spread.
        assert!((patch.oscillator_random_phase - 0.0).abs() < 1e-9);
    }

    #[test]
    fn preset_mod_source_ids_and_new_destinations_match_the_contract() {
        assert_eq!(mod_source(1), Some(ModSource::ModWheel));
        assert_eq!(mod_source(3), Some(ModSource::FilterEnvelope));
        assert_eq!(mod_source(4), Some(ModSource::Env3));
        assert_eq!(mod_source(5), Some(ModSource::Env4));
        assert_eq!(mod_source(6), Some(ModSource::Lfo));
        assert_eq!(mod_source(7), Some(ModSource::Lfo2));
        assert_eq!(mod_source(15), Some(ModSource::Lfo10));
        assert_eq!(mod_source(16), Some(ModSource::Velocity));
        assert_eq!(mod_source(17), Some(ModSource::Keytrack));
        assert_eq!(mod_source(18), Some(ModSource::Aftertouch));
        assert_eq!(mod_source(25), Some(ModSource::Macro1));
        assert_eq!(mod_source(28), Some(ModSource::Macro4));
        assert_eq!(mod_source(29), Some(ModSource::Macro5));
        assert_eq!(mod_source(32), Some(ModSource::Macro8));
        assert_eq!(mod_source(2), Some(ModSource::AmpEnvelope));
        // Audio-rate and unidentified IDs are still skipped.
        assert_eq!(mod_source(39), None);
        assert_eq!(
            mod_destination("WTOsc", 2, "kParamTablePos"),
            Some(ModDestination::WavetablePositionC)
        );
        assert_eq!(
            mod_destination("Oscillator", 2, "kParamVolume"),
            Some(ModDestination::OscillatorCLevel)
        );
        // The legacy `OscillatorMix` slot is the engine's B level.
        assert_eq!(
            mod_destination("Oscillator", 1, "kParamVolume"),
            Some(ModDestination::OscillatorMix)
        );
        assert_eq!(
            mod_destination("Oscillator", 0, "kParamVolume"),
            Some(ModDestination::OscillatorALevel)
        );
        assert_eq!(
            mod_destination("Oscillator", 3, "kParamVolume"),
            Some(ModDestination::NoiseLevel)
        );
        assert_eq!(
            mod_destination("Oscillator", 4, "kParamVolume"),
            Some(ModDestination::SubLevel)
        );
        // Fine and the audio-rate destinations are still unsupported.
        assert_eq!(mod_destination("Oscillator", 0, "kParamFine"), None);
        assert_eq!(
            mod_destination("VoiceFilter", 1, "kParamReso"),
            Some(ModDestination::Filter2Resonance)
        );
        assert_eq!(
            mod_destination("VoiceFilter", 1, "kParamDrive"),
            Some(ModDestination::Filter2Drive)
        );
        assert_eq!(
            mod_destination("VoiceFilter", 0, "kParamFreq"),
            Some(ModDestination::FilterCutoff)
        );
        assert_eq!(
            mod_destination("Global", 0, "kParamVoiceAmp"),
            Some(ModDestination::VoiceAmp)
        );
    }

    #[test]
    fn velocity_is_a_matrix_source_not_an_implicit_gain() {
        let data = serde_json::json!({
            "Global0": {
                "plainParams": {
                    "kParamMasterVolume": 0.5,
                    "kParamVoiceAmp": 0.43
                }
            },
            "ModSlot7": {
                "source": [16, 0],
                "destModuleTypeString": "Global",
                "destModuleID": 0,
                "destModuleParamName": "kParamVoiceAmp",
                "plainParams": { "kParamAmount": 100.0, "kParamBipolar": 1.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.velocity_to_amp, 0.0);
        assert_eq!(patch.mod_routes.len(), 1);
        assert_eq!(patch.mod_routes[0].source, ModSource::Velocity);
        assert_eq!(patch.mod_routes[0].destination, ModDestination::VoiceAmp);
        assert!(patch.mod_routes[0].bipolar);
        assert!((patch.mod_routes[0].amount - 1.0).abs() < 1e-9);
        // Voice Amp is a 0.5-centred gain in the preset; 0.43 is 0.86 of unity.
        assert!((patch.gain - 0.43).abs() < 1e-9);
    }

    #[test]
    fn missing_master_volume_uses_the_preset_controller_default() {
        let patch = synthetic_preset(serde_json::json!({})).to_synth_patch(None);
        assert!((patch.gain - MASTER_VOLUME_DEFAULT).abs() < 1e-12);
        let patch = synthetic_preset(serde_json::json!({
            "Global0": { "plainParams": { "kParamMasterVolume": 0.2 } }
        }))
        .to_synth_patch(None);
        assert!((patch.gain - 0.2).abs() < 1e-12);
    }

    #[test]
    fn maps_warp_menu_and_amount_to_all_oscillators() {
        let data = serde_json::json!({
            "Oscillator0": {
                "WTOsc0": {
                    "plainParams": { "kParamWarpMenu": "kSync", "kParamWarp": 0.4 }
                }
            },
            "Oscillator1": {
                "WTOsc1": {
                    "plainParams": { "kParamWarpMenu": "kPWM", "kParamWarp": 0.6 }
                }
            },
            "Oscillator2": {
                "WTOsc2": {
                    "plainParams": {
                        "kParamWarpMenu": "kRM_OSC", "kParamWarp": 0.8,
                        "kParamWarpMenu2": "kSelfPD", "kParamWarp2": 0.3,
                        "kParamWarpVar": 0.2, "kParamWarpVar2": 0.4
                    }
                }
            },
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.oscillator_a_warp_mode, WarpMode::Sync);
        assert!((patch.oscillator_a_warp_amount - 0.4).abs() < 1e-9);
        assert_eq!(patch.oscillator_b_warp_mode, WarpMode::Pwm);
        assert!((patch.oscillator_b_warp_amount - 0.6).abs() < 1e-9);
        assert_eq!(patch.oscillator_c_warp_mode, WarpMode::RmOsc);
        assert!((patch.oscillator_c_warp_amount - 0.8).abs() < 1e-9);
        assert_eq!(patch.oscillator_c_warp2_mode, WarpMode::SelfPd);
        assert!((patch.oscillator_c_warp2_amount - 0.3).abs() < 1e-9);
        assert!((patch.oscillator_c_warp_var - 0.2).abs() < 1e-9);
        assert!((patch.oscillator_c_warp2_var - 0.4).abs() < 1e-9);
    }

    #[test]
    fn warp_menu_values_map_to_engine_modes() {
        // Every mapped enum name maps to its own mode, in enum order.
        assert_eq!(WARP_MODE_NAMES.len(), 70);
        for (index, (name, mode)) in WARP_MODE_NAMES.iter().enumerate() {
            assert_eq!(*mode as usize, index, "{name}");
            let data = serde_json::json!({
                "Oscillator0": {
                    "WTOsc0": {
                        "plainParams": { "kParamWarpMenu": name, "kParamWarp": 0.5 }
                    }
                }
            });
            let patch = synthetic_preset(data).to_synth_patch(None);
            assert_eq!(patch.oscillator_a_warp_mode, *mode, "menu {name}");
            assert!((patch.oscillator_a_warp_amount - 0.5).abs() < 1e-9);
        }
        // Names outside the preset format's warp menu are not warp modes.
        for invented in ["kBend", "kAsym", "kRing", "kPD", "kFM", "kAM"] {
            let data = serde_json::json!({
                "Oscillator0": {
                    "WTOsc0": {
                        "plainParams": { "kParamWarpMenu": invented, "kParamWarp": 0.5 }
                    }
                }
            });
            let patch = synthetic_preset(data).to_synth_patch(None);
            assert_eq!(patch.oscillator_a_warp_mode, WarpMode::Off, "{invented}");
        }
        // Absent warp keys leave the engine default (Off, zero intensity).
        let patch = synthetic_preset(serde_json::json!({
            "Oscillator0": { "WTOsc0": { "plainParams": {} } }
        }))
        .to_synth_patch(None);
        assert_eq!(patch.oscillator_a_warp_mode, WarpMode::Off);
        assert_eq!(patch.oscillator_a_warp_amount, 0.0);
        assert_eq!(patch.oscillator_a_warp_var, 0.0);
        assert_eq!(patch.oscillator_a_warp2_mode, WarpMode::Off);
    }

    #[test]
    fn maps_per_oscillator_pan_to_the_signed_engine_range() {
        let data = serde_json::json!({
            "Oscillator0": { "plainParams": { "kParamPan": -25.0 } },
            "Oscillator1": {
                "WTOsc1": { "plainParams": {} },
                "plainParams": { "kParamEnable": 1.0, "kParamPan": 50.0 }
            },
            "Oscillator2": {
                "WTOsc2": { "plainParams": {} },
                "plainParams": { "kParamEnable": 1.0, "kParamPan": -50.0 }
            },
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert!((patch.oscillator_a_pan + 0.5).abs() < 1e-12);
        assert!((patch.oscillator_b_pan - 1.0).abs() < 1e-12);
        assert!((patch.oscillator_c_pan + 1.0).abs() < 1e-12);

        let centred = synthetic_preset(serde_json::json!({
            "Oscillator0": { "plainParams": {} }
        }))
        .to_synth_patch(None);
        assert_eq!(centred.oscillator_a_pan, 0.0);
        assert_eq!(centred.oscillator_b_pan, 0.0);
        assert_eq!(centred.oscillator_c_pan, 0.0);
    }

    fn write_test_wav(path: &std::path::Path, shape: fn(f64) -> f64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for index in 0..64 {
            let phase = index as f64 / 64.0;
            writer
                .write_sample((shape(phase) * 30_000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn wavetable_preset(paths: [&str; 3]) -> Value {
        let mut oscillators = serde_json::Map::new();
        for (index, path) in paths.into_iter().enumerate() {
            let mut mode = serde_json::Map::new();
            mode.insert("plainParams".into(), serde_json::json!({}));
            mode.insert("relativePathToWT".into(), Value::String(path.into()));
            let mut oscillator = serde_json::Map::new();
            oscillator.insert(
                "plainParams".into(),
                serde_json::json!({
                    "kParamEnable": 1.0,
                    "kParamVolume": 0.8,
                    "kParamPan": 0.0
                }),
            );
            oscillator.insert(format!("WTOsc{index}"), Value::Object(mode));
            oscillators.insert(format!("Oscillator{index}"), Value::Object(oscillator));
        }
        Value::Object(oscillators)
    }

    #[test]
    fn imports_distinct_wavetables_per_oscillator() {
        let dir = std::env::temp_dir().join(format!(
            "opencompose-preset-wavetables-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sine = |phase: f64| (std::f64::consts::TAU * phase).sin();
        let square = |phase: f64| if phase < 0.5 { 1.0 } else { -1.0 };
        let saw = |phase: f64| phase * 2.0 - 1.0;
        write_test_wav(&dir.join("a.wav"), sine);
        write_test_wav(&dir.join("b.wav"), square);
        write_test_wav(&dir.join("c.wav"), saw);

        let preset = synthetic_preset(wavetable_preset(["a.wav", "b.wav", "c.wav"]));
        let patch = preset.to_synth_patch(Some(&dir));
        assert!(patch.custom_wavetable.is_some());
        assert!(patch.oscillator_b_wavetable.is_some());
        assert!(patch.oscillator_c_wavetable.is_some());

        let render = |mut patch: SynthPatch| {
            patch.oscillator_a_level = 0.0;
            patch.oscillator_c_level = 0.0;
            patch.oscillator_b_level = 0.9;
            patch.oscillator_b_waveform = Waveform::Wavetable;
            patch.oscillator_a_waveform = Waveform::Wavetable;
            patch.unison_voices = 1;
            let mut synth = oc_synth_core::synth::OpenSynth::new(patch);
            synth.note_on(0, 60, 100);
            (0..2_048)
                .flat_map(|_| synth.next_stereo(48_000))
                .collect::<Vec<_>>()
        };
        let imported = render(patch.clone());
        let mut fallback = patch.clone();
        fallback.oscillator_b_wavetable = None;
        assert_ne!(
            imported,
            render(fallback),
            "oscillator B must use its own wavetable instead of A's"
        );

        // Identical paths stay on the shared A table and must render the same
        // as an explicit copy of that table.
        let shared = synthetic_preset(wavetable_preset(["a.wav", "a.wav", "a.wav"]))
            .to_synth_patch(Some(&dir));
        assert!(shared.oscillator_b_wavetable.is_none());
        let table = shared.custom_wavetable.clone().unwrap();
        let mut explicit = shared.clone();
        explicit.oscillator_b_wavetable = Some(table);
        assert_eq!(render(shared), render(explicit));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fixture stores A and B wavetable paths at the mode-object root. This
    /// also guards the older bug where the importer looked inside `plainParams`
    /// and silently never loaded a table.
    #[test]
    fn multisample_osc_maps_to_a_real_sampler_layer() {
        let dir = std::env::temp_dir().join(format!(
            "opencompose-preset-multisample-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let samples = dir.join("Multisamples/Factory/Synth/Samples");
        std::fs::create_dir_all(&samples).unwrap();
        write_test_wav(&samples.join("tone.wav"), |phase| {
            (std::f64::consts::TAU * phase * 4.0).sin()
        });
        let sfz = "<group> amp_veltrack=50 ampeg_release=1.5\n\
<region> lokey=0 hikey=127 pitch_keycenter=60 loop_mode=loop_continuous loop_start=0 loop_end=32 sample=Samples/tone.wav\n";
        let data = serde_json::json!({
            "Oscillator1": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_MultiSample",
                    "kParamVolume": 0.4,
                    "kParamOctave": -1.0,
                    "kParamFine": 50.0
                },
                "MultiSampleOsc1": {
                    "embedded_sfz": sfz,
                    "sfzPathRelative": "Factory/Synth/Pad.sfz"
                },
                "WTOsc1": { "plainParams": "default" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(Some(&dir));
        // The sample slot is not a wavetable: B is silent and A stays at its
        // default level (the fixture has no Oscillator0 content).
        assert_eq!(patch.oscillator_b_level, 0.0);
        assert_eq!(patch.oscillator_b_waveform, Waveform::Square);
        let sampler = patch.sampler.as_ref().expect("sampler layer");
        assert!(sampler.mono, "preset sample layers are mono");
        assert_eq!(sampler.regions.len(), 1);
        let region = &sampler.regions[0];
        // the preset's sample Level is unity at 0.5, so 0.4 maps to 0.8.
        assert!((region.gain - 0.8).abs() < 1e-9);
        assert_eq!(sampler.velocity_curve, VelocityCurve::Preset);
        assert!((region.tune_cents - (-1150.0)).abs() < 1e-9);
        assert_eq!(region.loop_mode, LoopMode::LoopContinuous);
        assert!((region.ampeg.release - 1.5).abs() < 1e-9);
        assert!(region.sample.is_file());

        // The mapped layer really renders through OpenSynth.
        let mut synth = oc_synth_core::synth::OpenSynth::new_with_tempo(patch, 120.0);
        synth.note_on(0, 60, 100);
        let mut peak = 0.0f64;
        for _ in 0..512 {
            let [left, right] = synth.next_stereo(44_100);
            peak = peak.max(f64::from(left.abs()).max(f64::from(right.abs())));
        }
        assert!(peak > 0.001, "mapped sample layer produced silence");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn granular_oscillator_is_silent_without_a_file_backed_sample() {
        let data = serde_json::json!({
            "Oscillator0": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_Granular",
                    "kParamVolume": 0.8
                },
                "GranularOsc0": {
                    "plainParams": { "kParamGrainSize": 0.5 }
                },
                "WTOsc0": { "plainParams": "default" }
            }
        });
        let mut diagnostics = ImportDiagnostics::default();
        let patch = synthetic_preset(data).to_synth_patch_diagnosed(None, &mut diagnostics);
        assert_eq!(patch.oscillator_a_level, 0.0);
        assert!(patch.sampler.is_none());
        assert!(patch.granular[0].is_none());
        assert!(
            diagnostics
                .warnings()
                .iter()
                .any(|warning| warning.class == "oscillator:GranularOsc")
        );
    }

    fn source_content_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opencompose-preset-source-{label}-{}",
            std::process::id()
        ));
        let samples = dir.join("Samples").join("Factory").join("Granular");
        std::fs::create_dir_all(&samples).unwrap();
        write_test_wav(&samples.join("tone.wav"), |phase| {
            (std::f64::consts::TAU * phase).sin()
        });
        dir
    }

    #[test]
    fn maps_a_granular_module_onto_the_grain_engine() {
        let dir = source_content_root("granular");
        let data = serde_json::json!({
            "Oscillator0": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_Granular",
                    "kParamVolume": 0.8
                },
                "GranularOsc0": {
                    "samplePathRelative": "Factory/Granular/tone.wav",
                    "baseNote": 48,
                    "sampleRate": 44_100,
                    "trimHead": 22_050,
                    "numFrames": 44_100,
                    "plainParams": {
                        "kParamDensity": 120.0,
                        "kParamGrainLength": 0.05,
                        "kParamWindowShape": "kWindowBlackmanHarris",
                        "kParamWindowParam": 70.0,
                        "kParamWindowSkew": -50.0,
                        "kParamRandomOffset": 25.0,
                        "kParamRandomGrainLength": 40.0,
                        "kParamRandomPitch": 2.0,
                        "kParamRandomDir": 30.0,
                        "kParamRandomPan": 60.0,
                        "kParamRandomGain": 20.0,
                        "kParamWarpMenu": "kDistTube"
                    }
                },
                "WTOsc0": { "plainParams": "default" }
            }
        });
        let mut diagnostics = ImportDiagnostics::default();
        let patch = synthetic_preset(data).to_synth_patch_diagnosed(Some(&dir), &mut diagnostics);
        assert_eq!(patch.oscillator_a_waveform, Waveform::Granular);
        assert_eq!(patch.oscillator_a_level, 0.8);
        let granular = patch.granular[0].as_ref().expect("granular patch");
        assert!(granular.sample.ends_with("Factory/Granular/tone.wav"));
        assert_eq!(granular.root_note, 48.0);
        assert_eq!(granular.position, 0.5);
        assert_eq!(granular.density, 120.0);
        assert_eq!(granular.grain_length, 0.05);
        assert_eq!(granular.window, GrainWindow::BlackmanHarris);
        assert!((granular.window_param - 0.7).abs() < 1.0e-9);
        assert!((granular.window_skew + 0.5).abs() < 1.0e-9);
        assert!((granular.random_offset - 0.25).abs() < 1.0e-9);
        assert!((granular.random_length - 0.4).abs() < 1.0e-9);
        assert_eq!(granular.random_pitch, 2.0);
        assert!((granular.random_direction - 0.3).abs() < 1.0e-9);
        assert!((granular.random_pan - 0.6).abs() < 1.0e-9);
        assert!((granular.random_gain - 0.2).abs() < 1.0e-9);
        assert!(
            diagnostics
                .warnings()
                .iter()
                .any(|warning| warning.class == "granular:warp")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_a_spectral_module_onto_the_stft_engine() {
        let dir = source_content_root("spectral");
        let data = serde_json::json!({
            "Oscillator1": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_Spectral",
                    "kParamVolume": 0.6
                },
                "SpectralOsc1": {
                    "samplePathRelative": "Factory/Granular/tone.wav",
                    "baseNote": 60,
                    "plainParams": {
                        "kParamWarpMenu": "kSpectralComb",
                        "kParamWarp": 0.6,
                        "kParamWarpMenu2": "kMirror",
                        "kParamWarp2": 0.4,
                        "kParamSpecFltShift": -12.0,
                        "kParamSpecFltWetDry": 50.0,
                        "kParamFreqLo": 200.0,
                        "kParamFreqHi": 8_000.0,
                        "kParamPhaseLock": 1.0,
                        "kParamTransients": 1.0,
                        "kParamLoHiIsSmooth": 1.0
                    }
                },
                "WTOsc1": { "plainParams": "default" }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(Some(&dir));
        assert_eq!(patch.oscillator_b_waveform, Waveform::Spectral);
        assert_eq!(patch.oscillator_b_level, 0.6);
        let spectral = patch.spectral[1].as_ref().expect("spectral patch");
        assert!(spectral.sample.ends_with("Factory/Granular/tone.wav"));
        assert_eq!(spectral.root_note, 60.0);
        assert_eq!(spectral.warp, SpectralWarp::Comb);
        assert_eq!(spectral.morph, 0.6);
        assert_eq!(spectral.warp2, SpectralWarp::Mirror);
        assert_eq!(spectral.morph2, 0.4);
        assert!((spectral.bend + 6.0).abs() < 1.0e-9);
        assert_eq!(spectral.low_hz, 200.0);
        assert_eq!(spectral.high_hz, 8_000.0);
        assert_eq!(spectral.phase_lock, 1.0);
        assert!(spectral.transients);
        assert!(spectral.band_smooth);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmappable_spectral_warp_warns_once_and_stays_off() {
        let dir = source_content_root("spectral-warp");
        let data = serde_json::json!({
            "Oscillator0": {
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_Spectral",
                    "kParamVolume": 0.6
                },
                "SpectralOsc0": {
                    "samplePathRelative": "Factory/Granular/tone.wav",
                    "plainParams": { "kParamWarpMenu": "kDistTube", "kParamWarp": 0.9 }
                },
                "WTOsc0": { "plainParams": "default" }
            }
        });
        let mut diagnostics = ImportDiagnostics::default();
        let patch = synthetic_preset(data).to_synth_patch_diagnosed(Some(&dir), &mut diagnostics);
        let spectral = patch.spectral[0].as_ref().expect("spectral patch");
        assert_eq!(spectral.warp, SpectralWarp::Off);
        assert_eq!(spectral.morph, 0.0);
        let warnings: Vec<_> = diagnostics
            .warnings()
            .iter()
            .filter(|warning| warning.class == "spectral:warp:kDistTube")
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_modulation_destinations_map_by_oscillator() {
        assert_eq!(
            mod_destination("GranularOsc", 1, "kParamDensity"),
            Some(ModDestination::SourceDensityB)
        );
        assert_eq!(
            mod_destination("SpectralOsc", 2, "kParamWarp2"),
            Some(ModDestination::SourceMorphC)
        );
        assert_eq!(
            mod_destination("SpectralOsc", 0, "kParamSpecFltShift"),
            Some(ModDestination::SourceBendA)
        );
    }

    #[test]
    fn maps_the_main_fx_rack_and_ignores_unsent_bus_racks() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    {
                        "type": 0,
                        "FXDistortion": {
                            "plainParams": {
                                "kParamDrive": 50.0,
                                "kParamFreq": 0.5,
                                "kParamWet": 80.0,
                                "kParamMode": 2
                            }
                        }
                    },
                    {
                        "type": 5,
                        "FXComp": {
                            "plainParams": {
                                "kParamAttack": 12.0,
                                "kParamRelease": 200.0,
                                "kParamRatio": 3.0
                            }
                        }
                    },
                    {
                        "type": 1,
                        "FXFilter": {
                            "plainParams": {
                                "kParamFreq": 0.5,
                                "kParamReso": 40.0,
                                "kParamType": "H12",
                                "kParamWet": 100.0
                            }
                        }
                    },
                    {
                        "type": 9,
                        "FXBode": { "plainParams": { "kParamFreq": 0.5 } }
                    }
                ]
            },
            "FXRack1": {
                "FX": [
                    {
                        "type": 4,
                        "FXDelay": {
                            "plainParams": {
                                "kParamTimeL": 0.125,
                                "kParamFeedback": 40.0,
                                "kParamWet": 25.0
                            }
                        }
                    },
                    {
                        "type": 10,
                        "FXReverb": {
                            "plainParams": {
                                "kParamSize": 60.0,
                                "kParamDamp": 30.0,
                                "kParamWidth": 90.0,
                                "kParamWet": 20.0
                            }
                        }
                    }
                ]
            },
            "FXRack2": {
                "FX": [
                    {
                        "type": 7,
                        "FXPhaser": {
                            "plainParams": {
                                "kParamRate": 0.75,
                                "kParamDepth": 60.0,
                                "kParamFeedback": 50.0,
                                "kParamWet": 50.0,
                                "kParamMode": 4
                            }
                        }
                    },
                    {
                        "type": 3,
                        "FXFlanger": {
                            "plainParams": {
                                "kParamRate": 0.25,
                                "kParamDepth": 80.0,
                                "kParamFeedback": 40.0,
                                "kParamWet": 60.0
                            }
                        }
                    }
                ]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        // Without a routing send, the bus racks are silent and must not be
        // appended to the main serial chain.
        assert_eq!(patch.fx.len(), 4);
        assert!(
            patch
                .fx
                .iter()
                .all(|fx| !matches!(fx.fx_type, SynthFxType::Delay | SynthFxType::Reverb))
        );
        assert_eq!(patch.fx[3].fx_type, SynthFxType::Bode);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Distortion);
        assert!((patch.fx[0].param("drive").unwrap() - 0.5).abs() < 1e-9);
        assert!((patch.fx[0].param("mix").unwrap() - 0.8).abs() < 1e-9);
        assert_eq!(patch.fx[1].fx_type, SynthFxType::Compressor);
        assert!((patch.fx[1].param("attack").unwrap() - 12.0).abs() < 1e-9);
        assert_eq!(patch.fx[2].fx_type, SynthFxType::Filter);
        assert!((patch.fx[2].param("cutoff").unwrap() - cutoff_hz(0.5)).abs() < 1e-9);
        assert!((patch.fx[2].param("resonance").unwrap() - 0.4).abs() < 1e-9);
        assert!((patch.fx[2].param("mode").unwrap() - 1.0).abs() < 1e-9);
        // The Bode module has no equivalent and must not be mapped to a random
        // effect; its raw parameters stay in the preset file.
        assert!(patch.fx.iter().all(|fx| fx.fx_type != SynthFxType::Drive));
        // Unmapped numeric the preset controls are retained on the module.
        assert!(
            patch.fx[1]
                .params
                .iter()
                .any(|(key, _)| key == "kParamRelease")
        );
        assert!(
            patch.fx[2]
                .params
                .iter()
                .any(|(key, _)| key == "kParamFreq")
        );
    }

    #[test]
    fn reverb_import_maps_the_fdn_controls_and_warns_once() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [
                    {
                        "type": 6,
                        "FXReverb": {
                            "plainParams": {
                                "kParamSize": 60.0,
                                "kParamDamp": 30.0,
                                "kParamPreDelay": 0.05,
                                "kParamWidth": 90.0,
                                "kParamWet": 20.0,
                                "kParamType": "Hall",
                                "kParamFeedback": 50.0,
                                "kParamDelay": 40.0,
                                "kParamFreqB": 20.0,
                                "kParamFreqC": 30.0,
                                "kParamVintageScaleA": 10.0
                            }
                        }
                    },
                    {
                        "type": 6,
                        "FXReverb": {
                            "plainParams": {
                                "kParamSize": 40.0,
                                "kParamType": "Plate",
                                "kParamFeedback": 50.0
                            }
                        }
                    }
                ]
            }
        });
        let mut diagnostics = ImportDiagnostics::default();
        let patch = synthetic_preset(data).to_synth_patch_diagnosed(None, &mut diagnostics);
        assert_eq!(patch.fx.len(), 2);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Reverb);
        assert!((patch.fx[0].param("size").unwrap() - 0.6).abs() < 1e-9);
        assert!((patch.fx[0].param("damp").unwrap() - 0.3).abs() < 1e-9);
        assert!((patch.fx[0].param("predelay").unwrap() - 0.05).abs() < 1e-9);
        assert!((patch.fx[0].param("width").unwrap() - 0.9).abs() < 1e-9);
        assert!((patch.fx[0].param("mix").unwrap() - 0.2).abs() < 1e-9);
        assert!((patch.fx[1].param("size").unwrap() - 0.4).abs() < 1e-9);
        let warning = |class: &str| {
            diagnostics
                .warnings()
                .iter()
                .find(|warning| warning.class == class)
        };
        assert_eq!(warning("fx-param:reverb:kParamType").unwrap().count, 2);
        assert_eq!(warning("fx-param:reverb:kParamFeedback").unwrap().count, 2);
        assert_eq!(warning("fx-param:reverb:kParamDelay").unwrap().count, 1);
        assert_eq!(warning("fx-param:reverb:kParamFreqB").unwrap().count, 1);
        assert_eq!(warning("fx-param:reverb:kParamFreqC").unwrap().count, 1);
        assert_eq!(
            warning("fx-param:reverb:kParamVintageScaleA")
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            diagnostics
                .warnings()
                .iter()
                .filter(|warning| warning.class.starts_with("fx-param:reverb:"))
                .count(),
            6
        );
    }

    #[test]
    fn fx_numeric_type_is_a_fallback_for_unknown_module_names() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 4,
                    "FXSomethingNew": {
                        "plainParams": { "kParamTimeL": 0.5 }
                    }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        assert_eq!(patch.fx[0].fx_type, SynthFxType::Delay);
        assert!((patch.fx[0].param("time").unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn fx_numeric_types_follow_preset_order() {
        // Pinned against the factory switch and all 626 factory presets.
        assert_eq!(
            PRESET_FX_TYPES,
            [
                SynthFxType::Distortion,
                SynthFxType::Flanger,
                SynthFxType::Phaser,
                SynthFxType::Chorus,
                SynthFxType::Delay,
                SynthFxType::Compressor,
                SynthFxType::Reverb,
                SynthFxType::Eq3,
                SynthFxType::Filter,
                SynthFxType::HyperD,
                SynthFxType::Bode,
                SynthFxType::Conv,
                SynthFxType::Utils,
                SynthFxType::Split,
                SynthFxType::Split3,
                SynthFxType::SplitMs,
            ]
        );
        for (index, expected) in PRESET_FX_TYPES.iter().enumerate() {
            assert_eq!(
                preset_fx_type_from_index(index as f64),
                Some(*expected),
                "id {index}"
            );
        }
        assert_eq!(preset_fx_type_from_index(16.0), None);
        assert_eq!(preset_fx_type_from_index(-1.0), None);
    }

    #[test]
    fn rejects_non_preset_bytes() {
        assert!(ForeignPreset::parse(b"not a preset").is_err());
    }

    #[test]
    #[ignore = "requires a local preset installation"]
    fn factory_presets_import_without_errors() {
        fn collect(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "SerumPreset") {
                    files.push(path);
                }
            }
        }
        let Some(root) = std::env::var_os("OCP_PRESET_LIBRARY").map(PathBuf::from) else {
            eprintln!("set OCP_PRESET_LIBRARY to the preset library root; skipping");
            return;
        };
        if !root.exists() {
            eprintln!("factory presets not installed; skipping");
            return;
        }
        let mut files = Vec::new();
        collect(&root, &mut files);
        println!("factory presets: {}", files.len());
        let mut errors = Vec::new();
        let mut total_fx = 0usize;
        let mut total_routes = 0usize;
        let mut total_fx_routes = 0usize;
        for path in &files {
            match ForeignPreset::load(path) {
                Ok(preset) => {
                    let patch = preset.to_synth_patch(None);
                    total_fx += patch.fx.len();
                    total_routes += patch.mod_routes.iter().filter(|r| r.amount != 0.0).count();
                    total_fx_routes += patch.fx_mod_routes.len();
                    if !patch.gain.is_finite() || !patch.oscillator_a_level.is_finite() {
                        errors.push((path.clone(), "non-finite patch value".to_string()));
                    }
                    if patch
                        .fx_mod_routes
                        .iter()
                        .any(|route| route.fx_index >= patch.fx.len() || !route.amount.is_finite())
                    {
                        errors.push((path.clone(), "invalid FX modulation route".to_string()));
                    }
                }
                Err(error) => errors.push((path.clone(), error)),
            }
        }
        println!(
            "imported {} presets, {} FX modules, {} active routes, {} active FX routes",
            files.len() - errors.len(),
            total_fx,
            total_routes,
            total_fx_routes
        );
        for (path, error) in errors.iter().take(20) {
            eprintln!("FAIL {}: {error}", path.display());
        }
        assert!(errors.is_empty(), "{} presets failed", errors.len());
    }

    #[test]
    #[ignore = "requires a local preset installation"]
    fn factory_presets_report_unmapped_classes() {
        fn collect(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "SerumPreset") {
                    files.push(path);
                }
            }
        }
        fn is_module_class(class: &str) -> bool {
            class.starts_with("fx-module:")
                || class == "fx-module-budget"
                || class.starts_with("oscillator:")
                || class.starts_with("wavetable:")
                || (class.starts_with("sample:") && !class.starts_with("sample-sfz-opcode:"))
        }
        let Some(root) = std::env::var_os("OCP_PRESET_LIBRARY").map(PathBuf::from) else {
            eprintln!("set OCP_PRESET_LIBRARY to the preset library root; skipping");
            return;
        };
        let content = std::env::var_os("OCP_PRESET_TABLES")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("../Tables"));
        if !root.exists() {
            eprintln!("factory presets not installed; skipping");
            return;
        }
        let mut files = Vec::new();
        collect(&root, &mut files);
        let mut classes: std::collections::BTreeMap<String, (usize, usize)> =
            std::collections::BTreeMap::new();
        let mut clean = 0usize;
        let mut no_module = 0usize;
        let mut failed = 0usize;
        for path in &files {
            let Ok(preset) = ForeignPreset::load(path) else {
                failed += 1;
                continue;
            };
            let mut diagnostics = ImportDiagnostics::default();
            preset.to_synth_patch_diagnosed(Some(&content), &mut diagnostics);
            let warnings = diagnostics.warnings();
            if warnings.is_empty() {
                clean += 1;
            }
            if !warnings
                .iter()
                .any(|warning| is_module_class(&warning.class))
            {
                no_module += 1;
            }
            for warning in warnings {
                let entry = classes.entry(warning.class.clone()).or_default();
                entry.0 += 1;
                entry.1 += warning.count;
            }
        }
        println!("presets: {}", files.len());
        println!("parse failures: {failed}");
        println!("presets with no module-class warning: {no_module}");
        println!("presets with no warning at all: {clean}");
        println!("residual classes: {}", classes.len());
        for (class, (presets, instances)) in &classes {
            println!("  {class}\tpresets={presets}\tinstances={instances}");
        }
        assert!(files.len() > 500, "factory presets should be installed");
    }

    #[test]
    #[ignore = "requires an installed preset library (OCP_PRESET_LIBRARY)"]
    fn sweet_bass_factory_preset_routes_fx_parameters() {
        let Some(root) = std::env::var_os("OCP_PRESET_LIBRARY").map(PathBuf::from) else {
            eprintln!("set OCP_PRESET_LIBRARY to the preset library root; skipping");
            return;
        };
        let path = root.join("Factory/Bass/Misc/BA - Sweet Bass.SerumPreset");
        if !path.is_file() {
            eprintln!("{} is missing; skipping", path.display());
            return;
        }
        let preset = ForeignPreset::load(&path).expect("BA - Sweet Bass must load");
        let patch = preset.to_synth_patch(None);
        let position = |fx_type| {
            patch
                .fx
                .iter()
                .position(|fx| fx.fx_type == fx_type)
                .unwrap_or_else(|| panic!("missing {fx_type:?}"))
        };
        let distortion = position(SynthFxType::Distortion);
        let eq = position(SynthFxType::Eq3);
        let flanger = position(SynthFxType::Flanger);
        let find = |source, parameter| {
            patch
                .fx_mod_routes
                .iter()
                .find(|route| route.source == source && route.parameter == parameter)
                .copied()
                .unwrap_or_else(|| panic!("missing {source:?} -> {parameter:?}"))
        };
        // The acceptance routes from the task.
        assert_eq!(find(ModSource::Macro2, FxParam::Wet).fx_index, distortion);
        assert_eq!(find(ModSource::Macro4, FxParam::Gain2).fx_index, eq);
        // the preset serializes this macro as `Macro7` (0-based); engine source ids
        // label the same slot `Macro8`.
        assert_eq!(find(ModSource::Macro8, FxParam::Wet).fx_index, flanger);
        // Macro3 opens both time-based wet mixes.
        let delay = position(SynthFxType::Delay);
        let reverb = position(SynthFxType::Reverb);
        let macro3: Vec<_> = patch
            .fx_mod_routes
            .iter()
            .filter(|route| route.source == ModSource::Macro3 && route.parameter == FxParam::Wet)
            .collect();
        assert_eq!(macro3.len(), 2);
        assert!(macro3.iter().any(|route| route.fx_index == delay));
        assert!(macro3.iter().any(|route| route.fx_index == reverb));
        // The importer keeps the macro out of the base wet value.
        assert_eq!(patch.fx[distortion].param("mix"), Some(0.0));
        // the preset's delay feedback default is 40%.
        assert!((patch.fx[delay].param("feedback").unwrap() - 0.4).abs() < 1e-5);
    }

    fn write_probe_wav(path: &std::path::Path) -> usize {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..2_000 {
            let value = if frame < 1_000 { 0.2 } else { 0.8 };
            writer
                .write_sample((value * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
        2_000
    }

    #[test]
    fn sample_oscillator_imports_unison_slot_fields() {
        let dir = std::env::temp_dir().join(format!("opencompose-preset-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("probe.wav");
        write_probe_wav(&wav);
        let sfz = format!(
            "<region> sample={} pitch_keycenter=60 offset=1200 end=2000\n\
             <region> sample={} pitch_keycenter=72 seq_position=2 seq_length=2\n",
            wav.display(),
            wav.display()
        );
        let data = serde_json::json!({
            "Oscillator0": {
                "MultiSampleOsc0": { "embedded_sfz": sfz },
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamType": "kOsc_MultiSample",
                    "kParamVolume": 0.5,
                    "kParamUnison": 3.0,
                    "kParamDetune": 0.2,
                    "kParamUnisonStereo": 50.0
                }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(Some(&dir));
        let sampler = patch.sampler.expect("sample layer");
        assert_eq!(sampler.regions.len(), 2);
        assert_eq!(sampler.regions[0].offset, Some(1_200));
        assert_eq!(sampler.regions[0].seq_length, None);
        assert_eq!(sampler.regions[1].seq_position, Some(2));
        assert_eq!(sampler.regions[1].seq_length, Some(2));
        for region in &sampler.regions {
            assert_eq!(region.unison, 3);
            assert!((region.unison_detune_cents - 26.750).abs() < 0.01);
            assert!((region.unison_width - 0.5).abs() < 1e-9);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn warp_destinations_are_per_oscillator() {
        let data = serde_json::json!({
            "ModSlot0": {
                "source": [16, 0],
                "destModuleTypeString": "WTOsc",
                "destModuleID": 1,
                "destModuleParamName": "kParamWarp",
                "plainParams": { "kParamAmount": 100.0 }
            },
            "ModSlot1": {
                "source": [16, 0],
                "destModuleTypeString": "WTOsc",
                "destModuleID": 0,
                "destModuleParamName": "kParamWarp2",
                "plainParams": { "kParamAmount": 50.0 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.mod_routes.len(), 2);
        assert_eq!(patch.mod_routes[0].destination, ModDestination::WarpAmountB);
        assert_eq!(
            patch.mod_routes[1].destination,
            ModDestination::Warp2AmountA
        );
        assert_eq!(
            mod_destination("WTOsc", 2, "kParamWarp"),
            Some(ModDestination::WarpAmountC)
        );
    }

    #[test]
    fn multiband_compressor_maps_onto_the_three_band_device() {
        let data = serde_json::json!({
            "FXRack0": {
                "FX": [{
                    "type": 5,
                    "FXComp": {
                        "plainParams": {
                            "kParamMultiband": 1.0,
                            "kParamXoverLow": 150.0,
                            "kParamXoverHi": 3_000.0,
                            "kParamAttack": 90.0,
                            "kParamRelease": 251.0,
                            "kParamMakeup": 3.7475763196925396,
                            "kParamThreshUD0": 0.0,
                            "kParamThreshUD1": 101.58734321594238,
                            "kParamRatioBelow": 0.75,
                            "kParamGain0": 4.6,
                            "kParamGain1": 0.75,
                            "kParamGain2": -3.0,
                            "kParamWet": 100.0
                        }
                    }
                }]
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        assert_eq!(patch.fx.len(), 1);
        let fx = &patch.fx[0];
        assert_eq!(fx.fx_type, SynthFxType::Multiband);
        assert_eq!(fx.param("band_count"), Some(1.0));
        assert_eq!(fx.param("crossover_1"), Some(150.0));
        assert_eq!(fx.param("crossover_2"), Some(3_000.0));
        assert_eq!(fx.param("mix"), Some(1.0));
        // Linear make-up amplitude -> dB.
        assert!((fx.param("makeup").unwrap() - 11.475).abs() < 0.01);
        assert_eq!(fx.param("band1_attack"), Some(90.0));
        assert_eq!(fx.param("band1_release"), Some(251.0));
        assert_eq!(fx.param("band2_attack"), Some(90.0));
        // Thresholds use the stored 0.240*value - 38.4 dB fit.
        assert!((fx.param("band1_threshold").unwrap() + 38.4).abs() < 1e-9);
        assert!((fx.param("band2_threshold").unwrap() - (-14.019)).abs() < 0.01);
        assert!((fx.param("band2_threshold_below").unwrap() - (-27.019)).abs() < 0.01);
        // A band with no stored threshold uses the stored module default.
        assert_eq!(fx.param("band3_threshold"), Some(-15.0));
        assert_eq!(fx.param("band1_makeup"), Some(4.6));
        assert_eq!(fx.param("band2_makeup"), Some(0.75));
        assert_eq!(fx.param("band3_makeup"), Some(-3.0));
        // RatioBelow 0.75 -> upward slope 0.585 -> ratio 2.41.
        assert!((fx.param("band1_ratio_below").unwrap() - 2.4096).abs() < 0.01);
        assert_eq!(fx.param("band3_ratio"), Some(3.17));
        // The module has no range clamp; the device's zero range would disable
        // dynamics, so imported bands get the full window.
        assert_eq!(fx.param("band1_range"), Some(60.0));
    }

    #[test]
    fn maps_bass_transpose_keytrack_portamento_and_levels() {
        let data = serde_json::json!({
            "Oscillator0": {
                "WTOsc0": { "plainParams": { "kParamTablePos": 1.0 } },
                "plainParams": {
                    "kParamEnable": 1.0,
                    "kParamVolume": 0.48022,
                    "kParamOctave": -2.0,
                    "kParamUnison": 2.0,
                    "kParamDetune": 0.08895044317242551,
                    "kParamUnisonStereo": 23.4375
                }
            },
            "Oscillator4": {
                "SubOsc4": { "plainParams": { "kParamShape": "kSaw" } },
                "plainParams": { "kParamEnable": 1.0, "kParamOctave": -3.0,
                                 "kParamVolume": 0.5877192616462708 }
            },
            "VoiceFilter0": {
                "plainParams": { "kParamEnable": 1.0, "kParamType": "MgL24",
                                 "kParamFreq": 0.0, "kParamReso": 3.8671147,
                                 "kParamDrive": 14.0350878, "kParamKeyTrack": 1.0 }
            },
            "Global0": {
                "plainParams": { "kParamMonoToggle": 1.0, "kParamPortaAlways": 1.0,
                                 "kParamPortamentoTime": 0.07517805662784978,
                                 "kParamMasterVolume": 0.19551098293192926 }
            }
        });
        let patch = synthetic_preset(data).to_synth_patch(None);
        // A's octave is independent of B/C.
        assert!((patch.oscillator_a_semitones - (-24.0)).abs() < 1e-9);
        // Sub octave is absolute: -3 octaves, not the engine's legacy -1,
        // and the bass uses a saw sub, not the engine's legacy sine.
        assert!((patch.sub_semitones - (-36.0)).abs() < 1e-9);
        assert_eq!(patch.sub_waveform, SubShape::Saw);
        // the preset unison detune: +/-200 * stored^1.25 cents.
        assert!((patch.detune_cents - 9.7155).abs() < 0.001);
        assert!((patch.stereo_width - 0.234375).abs() < 1e-9);
        // Keytrack is a 0/1 flag and the stored origin is C1 (32).
        assert!((patch.filter_keytrack - 1.0).abs() < 1e-9);
        assert!((patch.filter_keytrack_origin - 32.0).abs() < 1e-9);
        // Base cutoff is MIDI note 0, not the engine's old 20 Hz floor.
        assert!((patch.filter_cutoff - 8.175_798_915_643_707).abs() < 1e-9);
        // Drive/resonance are percent.
        assert!((patch.filter_drive - 0.140350878).abs() < 1e-6);
        assert!((patch.filter_resonance - 0.038671147).abs() < 1e-6);
        // Portamento time and master volume are stored in DSP units.
        assert!((patch.glide_time - 0.07517805662784978).abs() < 1e-9);
        assert!((patch.gain - 0.19551098293192926).abs() < 1e-9);
    }

    #[test]
    fn master_volume_is_a_linear_amplitude() {
        // 0.354813 is the controller default (-9 dB) and 1.0 the engine ceiling.
        for value in [0.0, 0.152_814_798_532_009_63, 0.354_813_389_233_575_47, 1.0] {
            let data = serde_json::json!({
                "Global0": { "plainParams": { "kParamMasterVolume": value } }
            });
            let patch = synthetic_preset(data).to_synth_patch(None);
            assert!((patch.gain - value).abs() < 1e-12);
        }
    }

    #[test]
    fn cutoff_normalization_spans_midi_zero_to_22050() {
        assert!((cutoff_hz(0.0) - 8.175_798_915_643_707).abs() < 1e-9);
        assert!((cutoff_hz(1.0) - 22_050.0).abs() < 0.5);
        // The documented 11.4-octave exponent.
        let ratio = cutoff_hz(0.5) / cutoff_hz(0.0);
        assert!((ratio - 2f64.powf(5.7)).abs() < 1e-6);
    }

    #[test]
    fn cutoff_law_matches_preset_corners() {
        // The imported L12 cutoff curve spans these corner frequencies at
        // zero resonance. Keep the low and midrange control mapping stable;
        // the upper end approaches Nyquist and is rate dependent.
        for (normalized, stored_hz) in [
            (0.0, 8.2),
            (0.05, 12.1),
            (0.10, 18.0),
            (0.20, 39.7),
            (0.35, 132.5),
            (0.50, 425.7),
            (0.65, 1403.0),
        ] {
            let error = cutoff_hz(normalized) / stored_hz - 1.0;
            assert!(error.abs() < 0.03, "x={normalized}: {error}");
        }
        // 11.4 octaves per unit is the fitted slope of the same probe set.
        let decades = (cutoff_hz(0.6) / cutoff_hz(0.1)).log2() / (0.6 - 0.1);
        assert!((decades - 11.4).abs() < 0.25, "{decades}");
    }
}
