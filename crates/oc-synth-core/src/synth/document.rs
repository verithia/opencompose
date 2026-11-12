//! Portable voice-state documents for PGen. Effects and MIDI belong to the containing project.
//! Use the checked import/export methods, not raw serde, at trust boundaries.
use std::path::{Component, Path, PathBuf};

use super::SynthPatch;
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Portable patch document with metadata and sample paths.
pub struct PatchDocument {
    pub version: u32,
    pub name: String,
    pub author: String,
    pub category: String,
    pub use_policy: String,
    /// Full state, never overrides against mutable defaults. Wavetable samples are embedded.
    pub patch: SynthPatch,
}

fn check_portable_sample_name(path: &Path) -> Result<(), String> {
    let valid = path
        .to_str()
        .is_some_and(|name| !name.contains(['/', '\\', ':']))
        && matches!(path.components().next(), Some(Component::Normal(_)));
    if !valid {
        return Err(format!(
            "Portable patch samples must be sibling filenames: {}",
            path.display()
        ));
    }
    Ok(())
}

impl PatchDocument {
    /// Compile a self-contained numeric voice into editable OCP physical parameters.
    /// Custom-table packaging must be explicit; never silently substitute a factory table.
    pub fn to_ocp_params(&self) -> Result<Vec<(String, oc_dsl::ast::ParamValue)>, String> {
        self.to_params(true)
    }

    /// Numeric export for a project preset that keeps its source file. State the
    /// numeric ABI cannot carry (custom tables, arpeggiator) is rebuilt from that
    /// file at load, so it is not a reason to refuse the export.
    pub fn to_project_params(&self) -> Result<Vec<(String, oc_dsl::ast::ParamValue)>, String> {
        self.to_params(false)
    }

    fn to_params(
        &self,
        require_packaging: bool,
    ) -> Result<Vec<(String, oc_dsl::ast::ParamValue)>, String> {
        self.validate()?;
        let p = &self.patch;
        if require_packaging {
            if p.arpeggiator.is_some() {
                return Err(
                    "arpeggiator requires a native state document, not numeric export".into(),
                );
            }
            if [
                &p.custom_wavetable,
                &p.oscillator_b_wavetable,
                &p.oscillator_c_wavetable,
            ]
            .into_iter()
            .any(Option::is_some)
            {
                return Err(
                    "custom wavetable requires project asset packaging before OCP compilation"
                        .into(),
                );
            }
        }
        let mut values = Vec::new();
        macro_rules! push { ($($key:literal => $field:ident),+ $(,)?) => { $(
            values.push(($key.to_string(), oc_dsl::ast::ParamValue::Number(p.$field as f64)));
        )+ }; }
        push!(
            "osc.a.level" => oscillator_a_level,
            "osc.a.pulse_width" => oscillator_a_pulse_width, "osc.a.wavetable_position" => oscillator_a_wavetable_position,
            "osc.a.warp_amount" => oscillator_a_warp_amount,
            "osc.a.warp_var" => oscillator_a_warp_var,
            "osc.a.warp2_amount" => oscillator_a_warp2_amount,
            "osc.a.warp2_var" => oscillator_a_warp2_var,
            "osc.a.pan" => oscillator_a_pan, "osc.a.semitones" => oscillator_a_semitones,
            "osc.b.level" => oscillator_b_level,
            "osc.b.semitones" => oscillator_b_semitones, "osc.b.pulse_width" => oscillator_b_pulse_width,
            "osc.b.wavetable_position" => oscillator_b_wavetable_position,
            "osc.b.warp_amount" => oscillator_b_warp_amount,
            "osc.b.warp_var" => oscillator_b_warp_var,
            "osc.b.warp2_amount" => oscillator_b_warp2_amount,
            "osc.b.warp2_var" => oscillator_b_warp2_var,
            "osc.b.pan" => oscillator_b_pan,
            "osc.a.unison" => unison_voices,
            "osc.a.detune" => detune_cents,
            "osc.a.width" => stereo_width,
            "osc.b.unison" => oscillator_b_unison, "osc.b.detune" => oscillator_b_detune_cents,
            "osc.b.width" => oscillator_b_stereo_width,
            "osc.c.level" => oscillator_c_level,
            "osc.c.semitones" => oscillator_c_semitones,
            "osc.c.pulse_width" => oscillator_c_pulse_width,
            "osc.c.wavetable_position" => oscillator_c_wavetable_position,
            "osc.c.warp_amount" => oscillator_c_warp_amount,
            "osc.c.warp_var" => oscillator_c_warp_var,
            "osc.c.warp2_amount" => oscillator_c_warp2_amount,
            "osc.c.warp2_var" => oscillator_c_warp2_var,
            "osc.c.pan" => oscillator_c_pan,
            "osc.c.unison" => oscillator_c_unison, "osc.c.detune" => oscillator_c_detune_cents,
            "osc.c.width" => oscillator_c_stereo_width,
            "ring.level" => ring_mod_level,
            "sub.level" => sub_level, "sub.semitones" => sub_semitones,
            "noise.level" => noise_level, "output.gain" => gain,
            "velocity.to_amp" => velocity_to_amp,
            "amp.attack" => attack, "amp.decay" => decay, "amp.sustain" => sustain,
            "amp.release" => release, "amp.curve" => amp_curve, "amp.damping" => amp_damping,
            "unison.detune" => detune_cents,
            "unison.voices" => unison_voices, "unison.width" => stereo_width,
            "filter.cutoff" => filter_cutoff, "filter.resonance" => filter_resonance,
            "filter.drive" => filter_drive,
            "filter.wet" => filter_wet,
            "filter2.cutoff" => filter2_cutoff, "filter2.resonance" => filter2_resonance,
            "filter2.drive" => filter2_drive,
            "filter2.wet" => filter2_wet,
            "filter.keytrack" => filter_keytrack,
            "filter.keytrack.origin" => filter_keytrack_origin,
            "filter.env.amount" => filter_envelope_amount,
            "filter_env.attack" => filter_attack, "filter_env.decay" => filter_decay,
            "filter_env.sustain" => filter_sustain, "filter_env.release" => filter_release,
            "filter_env.curve" => filter_curve,
            "env3.attack" => env3_attack, "env3.decay" => env3_decay,
            "env3.sustain" => env3_sustain, "env3.release" => env3_release,
            "env3.curve" => env3_curve,
            "env4.attack" => env4_attack, "env4.decay" => env4_decay,
            "env4.sustain" => env4_sustain, "env4.release" => env4_release,
            "env4.curve" => env4_curve,
            "lfo.rate" => lfo_rate,
            "lfo.pitch" => lfo_pitch_cents,
            "lfo.division" => lfo_division, "step.rate" => step_rate,
            "step.division" => step_division, "step.smoothing" => step_smoothing,
            "voice.polyphony" => polyphony, "voice.glide" => glide_time,
            "quality.oversampling" => oscillator_quality, "osc.start_phase" => oscillator_start_phase,
            "sub.initial_phase" => sub_initial_phase,
            "sub.fine_cents" => sub_fine_cents, "sub.pan" => sub_pan,
            "osc.random_phase" => oscillator_random_phase, "tuning.reference_hz" => tuning_reference_hz,
            "tuning.transpose" => tuning_transpose, "tuning.fine" => tuning_fine_cents
        );
        for (name, value) in [
            ("osc.a.waveform", p.oscillator_a_waveform as u8),
            ("osc.a.warp_mode", p.oscillator_a_warp_mode as u8),
            ("osc.a.warp2_mode", p.oscillator_a_warp2_mode as u8),
            ("osc.b.waveform", p.oscillator_b_waveform as u8),
            ("osc.b.warp_mode", p.oscillator_b_warp_mode as u8),
            ("osc.b.warp2_mode", p.oscillator_b_warp2_mode as u8),
            ("osc.c.waveform", p.oscillator_c_waveform as u8),
            ("sub.waveform", p.sub_waveform as u8),
            ("osc.c.warp_mode", p.oscillator_c_warp_mode as u8),
            ("osc.c.warp2_mode", p.oscillator_c_warp2_mode as u8),
            ("filter.mode", p.filter_mode as u8),
            ("filter.model", p.filter_model as u8),
            ("filter2.mode", p.filter2_mode as u8),
            ("filter2.model", p.filter2_model as u8),
            ("filter.routing", p.filter_routing as u8),
        ] {
            values.push((name.into(), oc_dsl::ast::ParamValue::Number(value as f64)));
        }
        for (name, value) in [
            ("lfo.retrigger", p.lfo_retrigger),
            ("lfo.sync", p.lfo_sync),
            ("step.sync", p.step_sync),
            ("voice.mono", p.monophonic),
            ("voice.legato", p.legato),
            ("sub.contiguous_phase", p.sub_contiguous_phase),
            ("filter2.enabled", p.filter2_enabled),
        ] {
            values.push((
                name.into(),
                oc_dsl::ast::ParamValue::Number(u8::from(value) as f64),
            ));
        }
        for (i, value) in p.extra_macros.iter().enumerate() {
            values.push((
                format!("macro.{}", i + 5),
                oc_dsl::ast::ParamValue::Number(*value),
            ));
        }
        for (prefix, array) in [("macro", &p.macros[..]), ("step", &p.step_values[..])] {
            for (i, value) in array.iter().enumerate() {
                values.push((
                    format!("{prefix}.{}", i + 1),
                    oc_dsl::ast::ParamValue::Number(*value),
                ));
            }
        }
        push_lfo_shape(
            &mut values,
            "lfo",
            p.lfo_shape_type,
            &p.lfo_shape,
            p.lfo_phase,
            p.lfo_rate_x10,
            p.lfo_sync_div,
            p.lfo_smooth,
            p.lfo_rise,
            p.lfo_delay,
        );
        for (index, lfo) in p.extra_lfos.iter().enumerate() {
            let prefix = format!("lfo{}", index + 2);
            values.push((
                format!("{prefix}.rate"),
                oc_dsl::ast::ParamValue::Number(lfo.rate),
            ));
            values.push((
                format!("{prefix}.division"),
                oc_dsl::ast::ParamValue::Number(lfo.division as f64),
            ));
            for (field, value) in [("sync", lfo.sync), ("retrigger", lfo.retrigger)] {
                values.push((
                    format!("{prefix}.{field}"),
                    oc_dsl::ast::ParamValue::Number(u8::from(value) as f64),
                ));
            }
            push_lfo_shape(
                &mut values,
                &prefix,
                lfo.shape_type,
                &lfo.shape,
                lfo.phase,
                lfo.rate_x10,
                lfo.sync_div,
                lfo.smooth,
                lfo.rise,
                lfo.delay,
            );
        }
        // Always export the complete 64-slot matrix so consumers can address
        // every slot even when a patch only materialized the legacy 16.
        for index in 0..super::MAX_MOD_ROUTES {
            let route = p.mod_routes.get(index).copied().unwrap_or_default();
            let destination = super::mod_destination_value(route.destination);
            if destination.is_none() && route.amount != 0.0 {
                if require_packaging {
                    return Err(format!(
                        "mod route {} destination has no numeric id",
                        index + 1
                    ));
                }
                // The source file rebuilds this route; a numeric id would be a
                // different destination.
                continue;
            }
            for (field, value) in [
                ("source", route.source as u8 as f64),
                ("destination", destination.unwrap_or_default() as f64),
                ("amount", route.amount),
            ] {
                values.push((
                    format!("mod.{}.{field}", index + 1),
                    oc_dsl::ast::ParamValue::Number(value),
                ));
            }
        }
        // The FX rack is part of the portable state. Export it as the same
        // `fx.N.*` keys the project preset ABI uses so a document round-trip
        // through numeric OCP parameters keeps the chain.
        for (i, fx) in p.fx.iter().enumerate() {
            let prefix = format!("fx.{}", i + 1);
            values.push((
                format!("{prefix}.type"),
                oc_dsl::ast::ParamValue::Number(fx.fx_type.index() as f64),
            ));
            values.push((
                format!("{prefix}.enabled"),
                oc_dsl::ast::ParamValue::Number(u8::from(fx.enabled) as f64),
            ));
            for (name, value) in &fx.params {
                values.push((
                    format!("{prefix}.{name}"),
                    oc_dsl::ast::ParamValue::Number(*value),
                ));
            }
        }
        // FX-parameter modulation is part of the portable state too. `fxmod.N`
        // is 1-based like `mod.N`; `fx_index` stays zero-based into the rack.
        for (index, route) in p.fx_mod_routes.iter().enumerate() {
            let prefix = format!("fxmod.{}", index + 1);
            for (field, value) in [
                ("source", route.source as u8 as f64),
                ("fx_index", route.fx_index as f64),
                ("parameter", route.parameter as u8 as f64),
                ("amount", route.amount),
                ("bipolar", u8::from(route.bipolar) as f64),
            ] {
                values.push((
                    format!("{prefix}.{field}"),
                    oc_dsl::ast::ParamValue::Number(value),
                ));
            }
        }
        Ok(values)
    }
    /// Portable documents store their sample files beside the document.
    pub const PORTABLE_VERSION: u32 = 2;

    /// Sample paths referenced by the patch.
    pub fn sample_paths(&self) -> impl Iterator<Item = &Path> {
        let patch = &self.patch;
        patch
            .sampler
            .iter()
            .flat_map(|sampler| sampler.regions.iter().map(|region| &region.sample))
            .chain(patch.granular.iter().flatten().map(|source| &source.sample))
            .chain(patch.spectral.iter().flatten().map(|source| &source.sample))
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::as_path)
    }

    /// Mutable sample paths for relocation.
    pub fn sample_paths_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        let patch = &mut self.patch;
        patch
            .sampler
            .iter_mut()
            .flat_map(|sampler| sampler.regions.iter_mut().map(|region| &mut region.sample))
            .chain(
                patch
                    .granular
                    .iter_mut()
                    .flatten()
                    .map(|source| &mut source.sample),
            )
            .chain(
                patch
                    .spectral
                    .iter_mut()
                    .flatten()
                    .map(|source| &mut source.sample),
            )
            .filter(|path| !path.as_os_str().is_empty())
    }

    /// Convert validated file-relative sample names into runtime paths.
    pub fn into_patch_at(mut self, document_path: &Path) -> Result<SynthPatch, String> {
        if self.version == Self::PORTABLE_VERSION {
            let parent = document_path
                .parent()
                .ok_or("Patch document has no directory")?;
            for sample in self.sample_paths_mut() {
                check_portable_sample_name(sample)?;
                let path = parent.join(&*sample);
                if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                    return Err(format!(
                        "Portable patch sample is missing or invalid: {}",
                        path.display()
                    ));
                }
                *sample = path;
            }
        }
        Ok(self.patch)
    }

    /// Parse a document within the size limit and normalize legacy LFO data.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("preset exceeds 32 MiB".into());
        }
        let mut document: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        document.patch.normalize_legacy_lfo();
        document.validate()?;
        Ok(document)
    }

    /// Validate and serialize the document within the size limit.
    pub fn to_json(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("preset exceeds 32 MiB".into());
        }
        Ok(bytes)
    }

    /// Check arpeggiator settings, version and patch invariants.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(settings) = self.patch.arpeggiator {
            settings.validate().map_err(str::to_string)?;
        }
        match self.version {
            1 => {}
            Self::PORTABLE_VERSION => {
                for path in self.sample_paths() {
                    check_portable_sample_name(path)?;
                }
            }
            _ => return Err("unsupported OpenSynth document version".into()),
        }
        for text in [&self.name, &self.author, &self.category, &self.use_policy] {
            if text.trim().is_empty() || text.len() > 1024 {
                return Err("invalid preset metadata".into());
            }
        }
        let p = &self.patch;
        macro_rules! range {
            ($lo:expr, $hi:expr; $($field:ident),+ ) => { $(
                if !p.$field.is_finite() || !(($lo)..=($hi)).contains(&p.$field) {
                    return Err(format!("out-of-range {}", stringify!($field)));
                }
            )+ };
        }
        range!(0.0, 1.0; oscillator_a_level, oscillator_b_level, oscillator_c_level,
            oscillator_a_wavetable_position, oscillator_b_wavetable_position,
            oscillator_c_wavetable_position, oscillator_a_warp_amount,
            oscillator_b_warp_amount, oscillator_c_warp_amount,
            oscillator_a_warp_var, oscillator_b_warp_var, oscillator_c_warp_var,
            oscillator_a_warp2_amount, oscillator_b_warp2_amount, oscillator_c_warp2_amount,
            oscillator_a_warp2_var, oscillator_b_warp2_var, oscillator_c_warp2_var,
            ring_mod_level, sub_level, noise_level, gain,
            sustain, stereo_width, filter_resonance, filter_drive, filter2_resonance,
            filter2_drive, filter_wet, filter2_wet, filter_keytrack, filter_sustain, step_smoothing,
            oscillator_start_phase, oscillator_random_phase, amp_damping);
        range!(0.05, 0.95; oscillator_a_pulse_width, oscillator_b_pulse_width,
            oscillator_c_pulse_width);
        range!(-24.0, 24.0; oscillator_b_semitones, oscillator_c_semitones, tuning_transpose);
        range!(0.0, 10.0; attack, decay, filter_attack, filter_decay);
        range!(0.0, 20.0; release, filter_release);
        range!(-1.0, 1.0; amp_curve, filter_curve);
        range!(0.0, 10.0; env3_attack, env3_decay, env4_attack, env4_decay);
        range!(0.0, 20.0; env3_release, env4_release);
        range!(0.0, 1.0; env3_sustain, env4_sustain);
        range!(-1.0, 1.0; env3_curve, env4_curve);
        range!(0.0, 100.0; detune_cents);
        range!(8.0, 22050.0; filter_cutoff, filter2_cutoff);
        range!(-8.0, 8.0; filter_envelope_amount);
        range!(0.0, 100.0; lfo_rate);
        range!(0.01, 30.0; step_rate);
        range!(0.0, 200.0; lfo_pitch_cents);
        range!(0.0, 360.0; lfo_phase);
        range!(0.0, 1.0; lfo_smooth);
        range!(0.0, 5.0; lfo_rise, lfo_delay);
        range!(-1.0, 1.0; lfo_bend);
        range!(0.0, 4.0; glide_time);
        range!(-48.0, 48.0; oscillator_a_semitones, sub_semitones);
        range!(0.0, 127.0; filter_keytrack_origin);
        range!(400.0, 480.0; tuning_reference_hz);
        range!(-100.0, 100.0; tuning_fine_cents);
        if !(1..=7).contains(&p.unison_voices)
            || !(1..=32).contains(&p.polyphony)
            || p.oscillator_quality > 2
            || p.lfo_division > 5
            || p.lfo_sync_div
                .is_some_and(|index| index >= super::LFO_SYNC_DIVISIONS.len())
            || p.step_division > 5
        {
            return Err("invalid discrete control".into());
        }
        for (values, lo) in [(&p.macros[..], 0.0), (&p.step_values[..], -1.0)] {
            if values
                .iter()
                .any(|v| !v.is_finite() || !(lo..=1.0).contains(v))
            {
                return Err("invalid macro/step value".into());
            }
        }
        if p.extra_lfos.iter().any(|lfo| {
            !lfo.rate.is_finite()
                || !(0.0..=100.0).contains(&lfo.rate)
                || lfo.division > 5
                || lfo
                    .sync_div
                    .is_some_and(|index| index >= super::LFO_SYNC_DIVISIONS.len())
                || !(0.0..=360.0).contains(&lfo.phase)
                || !(0.0..=1.0).contains(&lfo.smooth)
                || !(0.0..=5.0).contains(&lfo.rise)
                || !(0.0..=5.0).contains(&lfo.delay)
                || !(-1.0..=1.0).contains(&lfo.bend)
        }) {
            return Err("invalid extra LFO settings".into());
        }
        if p.mod_routes.len() > super::MAX_MOD_ROUTES {
            return Err("invalid modulation matrix size".into());
        }
        if p.mod_routes
            .iter()
            .any(|r| !r.amount.is_finite() || r.amount.abs() > 1.0)
        {
            return Err("invalid modulation depth".into());
        }
        if p.fx.len() > super::fx::MAX_SYNTH_FX {
            return Err("too many synth effects".into());
        }
        if p.fx.iter().any(|fx| {
            fx.params
                .iter()
                .any(|(name, value)| name.trim().is_empty() || !value.is_finite())
        }) {
            return Err("invalid synth effect parameter".into());
        }
        if p.fx_mod_routes.len() > super::MAX_FX_MOD_ROUTES {
            return Err("invalid FX modulation matrix size".into());
        }
        if p.fx_mod_routes.iter().any(|route| {
            !route.amount.is_finite()
                || route.amount.abs() > 1.0
                || route.fx_index >= super::fx::MAX_SYNTH_FX
        }) {
            return Err("invalid FX modulation route".into());
        }
        for table in [
            &p.custom_wavetable,
            &p.oscillator_b_wavetable,
            &p.oscillator_c_wavetable,
        ]
        .into_iter()
        .flatten()
        {
            if table.samples.len() > super::MAX_CUSTOM_WAVETABLE_SAMPLES
                || table.frame_size < 32
                || table.frame_count == 0
                || table.frame_count > 256
                || table.frame_size.checked_mul(table.frame_count) != Some(table.samples.len())
                || table
                    .samples
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 1.0)
                || !table.samples.iter().any(|v| v.abs() > f64::EPSILON)
            {
                return Err("invalid embedded wavetable".into());
            }
        }
        Ok(())
    }
}

/// Export a shape as the same array params the DSL preset importer reads.
// Export helper mirrors the patch's LFO field layout.
#[allow(clippy::too_many_arguments)]
fn push_lfo_shape(
    values: &mut Vec<(String, oc_dsl::ast::ParamValue)>,
    prefix: &str,
    shape_type: super::LfoShapeType,
    shape: &super::LfoShape,
    phase: f64,
    rate_x10: bool,
    sync_div: Option<usize>,
    smooth: f64,
    rise: f64,
    delay: f64,
) {
    use oc_dsl::ast::ParamValue;
    let points = shape.points.clamp(2, super::LFO_SHAPE_POINTS);
    values.push((
        format!("{prefix}.shape"),
        ParamValue::Number(shape_type as u8 as f64),
    ));
    values.push((
        format!("{prefix}.shape.points"),
        ParamValue::Number(points as f64),
    ));
    for (field, array) in [
        ("x", &shape.x[..points]),
        ("y", &shape.y[..points]),
        ("curve", &shape.curve[..points]),
    ] {
        values.push((
            format!("{prefix}.shape.{field}"),
            ParamValue::Array(array.iter().map(|value| *value as f64).collect()),
        ));
    }
    for (field, value) in [
        ("phase", phase),
        ("smooth", smooth),
        ("rise", rise),
        ("delay", delay),
    ] {
        values.push((format!("{prefix}.{field}"), ParamValue::Number(value)));
    }
    values.push((
        format!("{prefix}.rate_x10"),
        ParamValue::Number(u8::from(rate_x10) as f64),
    ));
    if let Some(index) = sync_div {
        values.push((
            format!("{prefix}.sync_div"),
            ParamValue::Number(index as f64),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> PatchDocument {
        PatchDocument {
            version: 1,
            name: "test".into(),
            author: "OpenCompose".into(),
            category: "test".into(),
            use_policy: "synthetic diagnostic".into(),
            patch: SynthPatch::default(),
        }
    }
    #[test]
    fn default_patch_numeric_export_matches_concrete_schema_defaults() {
        let params = document().to_project_params().unwrap();
        let mut checked = std::collections::HashSet::new();
        for (key, value) in &params {
            let Some(value) = value.as_f64() else {
                continue;
            };
            let expected = oc_device_schema::synth::find_param(key)
                .map(|spec| spec.default)
                .or_else(|| {
                    oc_device_schema::synth::find_family_field(key).map(|(_, field)| field.default)
                });
            let Some(expected) = expected else {
                continue;
            };
            assert_eq!(value, expected, "default of {key}");
            checked.insert(key.as_str());
        }
        for key in [
            "osc.a.level",
            "osc.b.level",
            "osc.c.level",
            "osc.a.waveform",
            "osc.b.waveform",
            "osc.c.waveform",
            "env3.attack",
            "env4.release",
            "lfo.rate",
            "lfo2.rate",
            "lfo10.rate",
            "macro.8",
            "step.16",
        ] {
            assert!(
                checked.contains(key),
                "missing default parity coverage for {key}"
            );
        }
    }

    #[test]
    fn mod_destinations_keep_their_project_ids() {
        let mut d = document();
        for (index, destination) in [
            super::super::ModDestination::WarpAmountA,
            super::super::ModDestination::WavetablePositionB,
            super::super::ModDestination::SourceBendC,
        ]
        .into_iter()
        .enumerate()
        {
            d.patch.mod_routes[index].destination = destination;
            d.patch.mod_routes[index].amount = 0.5;
        }
        let params = d.to_ocp_params().unwrap();
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(
            restored.mod_routes[0].destination,
            super::super::ModDestination::WarpAmountA
        );
        assert_eq!(
            restored.mod_routes[1].destination,
            super::super::ModDestination::WavetablePositionB
        );
        assert_eq!(
            restored.mod_routes[2].destination,
            super::super::ModDestination::SourceBendC
        );
    }

    #[test]
    fn project_export_accepts_state_the_source_file_rebuilds() {
        let mut d = document();
        d.patch.custom_wavetable = Some(super::super::CustomWavetable {
            samples: std::sync::Arc::new((0..64).map(|n| (n as f64 * 0.098).sin()).collect()),
            frame_count: 1,
            frame_size: 64,
            imported_readout: false,
            mips: Default::default(),
        });
        d.patch.oscillator_a_waveform = super::super::Waveform::Wavetable;
        assert!(d.to_ocp_params().is_err());
        let params = d.to_project_params().expect("project export");
        assert!(params.iter().any(|(key, value)| key == "osc.a.waveform"
            && *value == oc_dsl::ast::ParamValue::Number(4.0)));
    }

    #[test]
    fn portable_state_preserves_samples_with_embedded_table() {
        let mut d = document();
        d.patch.custom_wavetable = Some(super::super::CustomWavetable {
            samples: std::sync::Arc::new((0..64).map(|n| (n as f64 * 0.098).sin()).collect()),
            frame_count: 1,
            frame_size: 64,
            imported_readout: false,
            mips: Default::default(),
        });
        d.patch.oscillator_a_waveform = super::super::Waveform::Wavetable;
        let restored = PatchDocument::from_json(&d.to_json().unwrap()).unwrap();
        assert_eq!(d.to_json().unwrap(), restored.to_json().unwrap());
        let mut a = super::super::OpenSynth::new(d.patch);
        let mut b = super::super::OpenSynth::new(restored.patch);
        a.note_on(0, 60, 100);
        b.note_on(0, 60, 100);
        for _ in 0..4800 {
            assert_eq!(a.next_stereo(48000), b.next_stereo(48000));
        }
    }
    #[test]
    fn rejects_invalid_and_incomplete_state() {
        let mut d = document();
        d.patch.gain = f64::NAN;
        assert!(d.to_json().is_err());
        d = document();
        d.version = 3;
        assert!(d.to_json().is_err());
        let mut v = serde_json::to_value(document()).unwrap();
        v["patch"]["polyphony"] = serde_json::json!(0);
        assert!(PatchDocument::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
        v["patch"]["polyphony"] = serde_json::json!(16);
        v["patch"].as_object_mut().unwrap().remove("gain");
        assert!(PatchDocument::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    #[test]
    fn portable_sample_paths_cannot_escape_the_document_folder() {
        use crate::sampler::SamplerPatch;
        let mut d = document();
        d.version = PatchDocument::PORTABLE_VERSION;
        d.patch.sampler = Some(SamplerPatch {
            regions: vec![serde_json::from_value(serde_json::json!({"sample":"kit.wav"})).unwrap()],
            ..Default::default()
        });
        assert!(d.to_json().is_ok());
        for invalid in [
            "../outside.wav",
            "/tmp/outside.wav",
            "folder\\outside.wav",
            "C:outside.wav",
        ] {
            *d.sample_paths_mut().next().unwrap() = invalid.into();
            assert!(d.to_json().is_err(), "{invalid}");
        }
    }

    #[test]
    fn rejects_unknown_fields_and_malformed_wavetable() {
        let mut v = serde_json::to_value(document()).unwrap();
        v["unexpected"] = serde_json::json!(true);
        assert!(PatchDocument::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
        v.as_object_mut().unwrap().remove("unexpected");
        v["patch"]["custom_wavetable"] = serde_json::json!({
            "frame_size":64,"frame_count":2,"samples":vec![0.5; 64]
        });
        assert!(PatchDocument::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    #[test]
    fn ocp_params_preserve_complete_voice_state() {
        let mut d = document();
        d.patch.oscillator_a_waveform = super::super::Waveform::Wavetable;
        d.patch.filter_mode = super::super::FilterMode::Notch;
        d.patch.filter_model = super::super::FilterModel::Ladder;
        d.patch.oscillator_c_waveform = super::super::Waveform::Triangle;
        d.patch.oscillator_c_level = 0.42;
        d.patch.oscillator_c_semitones = -12.0;
        d.patch.oscillator_c_pulse_width = 0.31;
        d.patch.oscillator_c_wavetable_position = 0.55;
        d.patch.oscillator_a_warp_mode = super::super::WarpMode::AsymPos;
        d.patch.oscillator_a_warp_amount = 0.37;
        d.patch.oscillator_b_warp_mode = super::super::WarpMode::RmOsc;
        d.patch.oscillator_b_warp_amount = 0.9;
        d.patch.oscillator_c_warp_mode = super::super::WarpMode::Pwm;
        d.patch.oscillator_c_warp_amount = 0.12;
        d.patch.filter2_enabled = true;
        d.patch.filter2_cutoff = 1_100.0;
        d.patch.filter2_resonance = 0.55;
        d.patch.filter2_drive = 0.35;
        d.patch.filter2_mode = super::super::FilterMode::HighPass;
        d.patch.filter2_model = super::super::FilterModel::DirtyLadder;
        d.patch.filter_routing = super::super::FilterRouting::Parallel;
        d.patch.mod_routes[15].source = super::super::ModSource::Step;
        d.patch.mod_routes[15].amount = -0.6;
        d.patch.step_values[15] = 0.75;
        d.patch.fx = vec![
            super::super::SynthFx::new(super::super::SynthFxType::Drive),
            super::super::SynthFx::new(super::super::SynthFxType::Delay),
        ];
        d.patch.fx[1].enabled = false;
        d.patch.fx[1].set_param("time", 0.4);
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&d.to_ocp_params().unwrap()));
        // Export pads the matrix to all 64 slots; normalize the expectation.
        let mut expected = d.patch.clone();
        expected.mod_routes.resize(
            super::super::MAX_MOD_ROUTES,
            super::super::ModRoute::default(),
        );
        assert_eq!(
            serde_json::to_value(expected).unwrap(),
            serde_json::to_value(restored).unwrap()
        );
    }

    #[test]
    fn warp_params_round_trip_through_ocp_params() {
        let mut d = document();
        d.patch.oscillator_a_warp_mode = super::super::WarpMode::Sync;
        d.patch.oscillator_a_warp_amount = 0.42;
        let params = d.to_ocp_params().unwrap();
        assert!(params.iter().any(|(name, value)| name == "osc.a.warp_mode"
            && *value == oc_dsl::ast::ParamValue::Number(1.0)));
        assert!(
            params
                .iter()
                .any(|(name, value)| name == "osc.a.warp_amount"
                    && *value == oc_dsl::ast::ParamValue::Number(0.42))
        );
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(
            restored.oscillator_a_warp_mode,
            super::super::WarpMode::Sync
        );
        assert_eq!(restored.oscillator_a_warp_amount, 0.42);
    }

    #[test]
    fn per_oscillator_pan_and_cross_warp_modes_round_trip() {
        let mut d = document();
        d.patch.oscillator_a_pan = -0.5;
        d.patch.oscillator_b_pan = 1.0;
        d.patch.oscillator_c_warp_mode = super::super::WarpMode::AmOsc;
        d.patch.oscillator_c_warp_amount = 0.75;
        let params = d.to_ocp_params().unwrap();
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(restored.oscillator_a_pan, -0.5);
        assert_eq!(restored.oscillator_b_pan, 1.0);
        assert_eq!(restored.oscillator_c_pan, 0.0);
        assert_eq!(
            restored.oscillator_c_warp_mode,
            super::super::WarpMode::AmOsc
        );
        assert_eq!(restored.oscillator_c_warp_amount, 0.75);
    }

    #[test]
    fn exports_filter_models_through_ocp_params() {
        let mut d = document();
        d.patch.filter_model = super::super::FilterModel::Ladder;
        d.patch.filter2_enabled = true;
        d.patch.filter2_model = super::super::FilterModel::DirtyLadder;
        let params = d.to_ocp_params().unwrap();
        assert!(params.iter().any(|(name, value)| name == "filter.model"
            && *value == oc_dsl::ast::ParamValue::Number(1.0)));
        assert!(params.iter().any(|(name, value)| name == "filter2.model"
            && *value == oc_dsl::ast::ParamValue::Number(2.0)));
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(restored.filter_model, super::super::FilterModel::Ladder);
        assert_eq!(
            restored.filter2_model,
            super::super::FilterModel::DirtyLadder
        );
    }

    #[test]
    fn legacy_patch_json_without_oscillator_c_and_filter2_fields_loads() {
        let mut value = serde_json::to_value(document()).unwrap();
        let patch = value["patch"].as_object_mut().unwrap();
        for key in [
            "oscillator_c_waveform",
            "oscillator_c_level",
            "oscillator_c_semitones",
            "oscillator_c_pulse_width",
            "oscillator_c_wavetable_position",
            "oscillator_c_warp_mode",
            "oscillator_c_warp_amount",
            "oscillator_a_warp_mode",
            "oscillator_a_warp_amount",
            "oscillator_b_warp_mode",
            "oscillator_b_warp_amount",
            "oscillator_a_pan",
            "oscillator_b_pan",
            "oscillator_c_pan",
            "oscillator_b_wavetable",
            "oscillator_c_wavetable",
            "filter2_enabled",
            "filter2_cutoff",
            "filter2_resonance",
            "filter2_drive",
            "filter2_mode",
            "filter2_model",
            "filter_model",
            "filter_routing",
            "env3_attack",
            "env3_decay",
            "env3_sustain",
            "env3_release",
            "env3_curve",
            "env4_attack",
            "env4_decay",
            "env4_sustain",
            "env4_release",
            "env4_curve",
            "extra_lfos",
            "fx",
        ] {
            patch.remove(key);
        }
        let restored = PatchDocument::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        let default = SynthPatch::default();
        assert_eq!(
            restored.patch.oscillator_c_waveform,
            default.oscillator_c_waveform
        );
        assert_eq!(restored.patch.oscillator_c_level, 0.0);
        assert_eq!(restored.patch.oscillator_c_semitones, 0.0);
        assert_eq!(restored.patch.oscillator_c_pulse_width, 0.5);
        assert_eq!(restored.patch.oscillator_c_wavetable_position, 0.0);
        assert_eq!(
            restored.patch.oscillator_a_warp_mode,
            super::super::WarpMode::Off
        );
        assert_eq!(
            restored.patch.oscillator_b_warp_mode,
            super::super::WarpMode::Off
        );
        assert_eq!(
            restored.patch.oscillator_c_warp_mode,
            super::super::WarpMode::Off
        );
        assert_eq!(restored.patch.oscillator_a_warp_amount, 0.0);
        assert_eq!(restored.patch.oscillator_b_warp_amount, 0.0);
        assert_eq!(restored.patch.oscillator_c_warp_amount, 0.0);
        assert_eq!(restored.patch.oscillator_a_pan, 0.0);
        assert_eq!(restored.patch.oscillator_b_pan, 0.0);
        assert_eq!(restored.patch.oscillator_c_pan, 0.0);
        assert!(restored.patch.oscillator_b_wavetable.is_none());
        assert!(restored.patch.oscillator_c_wavetable.is_none());
        assert!(!restored.patch.filter2_enabled);
        assert_eq!(restored.patch.filter2_cutoff, 12_000.0);
        assert_eq!(restored.patch.filter2_resonance, 0.15);
        assert_eq!(restored.patch.filter2_drive, 0.0);
        assert_eq!(restored.patch.filter2_mode, default.filter2_mode);
        assert_eq!(restored.patch.filter_model, default.filter_model);
        assert_eq!(restored.patch.filter2_model, default.filter2_model);
        assert_eq!(restored.patch.filter_routing, default.filter_routing);
        assert_eq!(restored.patch.env3_attack, default.env3_attack);
        assert_eq!(restored.patch.env3_sustain, default.env3_sustain);
        assert_eq!(restored.patch.env4_release, default.env4_release);
        assert_eq!(restored.patch.extra_lfos, default.extra_lfos);
        assert_eq!(restored.patch.mod_routes.len(), 16);
        assert!(restored.patch.fx.is_empty());
        assert!(restored.validate().is_ok());
    }

    #[test]
    fn exports_env3_env4_lfo2_through_10_and_all_64_routes() {
        let mut d = document();
        d.patch.env3_attack = 0.21;
        d.patch.env4_sustain = 0.33;
        d.patch.extra_lfos[8].rate = 7.5;
        d.patch.extra_lfos[8].shape_type = super::super::LfoShapeType::Curve;
        d.patch.extra_lfos[8].shape = std::sync::Arc::new(super::super::LfoShape::square());
        d.patch.mod_routes.resize(
            super::super::MAX_MOD_ROUTES,
            super::super::ModRoute::default(),
        );
        d.patch.mod_routes[63] = super::super::ModRoute {
            source: super::super::ModSource::Env3,
            destination: super::super::ModDestination::Filter2Drive,
            amount: 0.625,
            ..super::super::ModRoute::default()
        };
        let params = d.to_ocp_params().unwrap();
        let names: Vec<&str> = params.iter().map(|(name, _)| name.as_str()).collect();
        for expected in [
            "env3.attack",
            "env4.sustain",
            "lfo2.rate",
            "lfo10.shape",
            "lfo10.shape.x",
            "lfo10.division",
            "mod.1.amount",
            "mod.64.source",
            "mod.64.destination",
            "mod.64.amount",
        ] {
            assert!(names.contains(&expected), "missing exported {expected}");
        }
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(restored.mod_routes.len(), 64);
        assert_eq!(restored.mod_routes[63].amount, 0.625);
        assert_eq!(
            restored.mod_routes[63].destination,
            super::super::ModDestination::Filter2Drive
        );
        assert_eq!(restored.env3_attack, 0.21);
        assert_eq!(restored.extra_lfos[8], d.patch.extra_lfos[8]);
        assert_eq!(
            serde_json::to_value(&d.patch).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        let mut validated = document();
        validated.patch = restored;
        assert!(validated.validate().is_ok());
    }

    #[test]
    fn rejects_out_of_range_env_lfo_and_oversized_matrix() {
        let mut d = document();
        d.patch.env3_attack = 11.0;
        assert!(d.to_json().is_err());
        d = document();
        d.patch.extra_lfos[0].rate = -0.01;
        assert!(d.to_json().is_err());
        d = document();
        d.patch.extra_lfos[0].division = 6;
        assert!(d.to_json().is_err());
        d = document();
        d.patch.mod_routes = vec![super::super::ModRoute::default(); 65];
        assert!(d.to_json().is_err());
        d = document();
        d.patch.mod_routes.clear();
        assert!(d.to_json().is_ok());
    }

    #[test]
    fn document_validates_and_exports_fx_rack() {
        let mut d = document();
        d.patch.fx = vec![
            super::super::SynthFx::new(super::super::SynthFxType::Compressor),
            super::super::SynthFx::new(super::super::SynthFxType::Reverb),
        ];
        // `to_ocp_params` always exports the complete matrix, so match its
        // materialized route count before comparing the whole patch.
        d.patch.mod_routes.resize(
            super::super::MAX_MOD_ROUTES,
            super::super::ModRoute::default(),
        );
        assert!(d.validate().is_ok());
        let params = d.to_ocp_params().unwrap();
        for key in ["fx.1.type", "fx.1.enabled", "fx.1.threshold", "fx.2.size"] {
            assert!(params.iter().any(|(name, _)| name == key), "missing {key}");
        }
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(
            serde_json::to_value(&d.patch).unwrap(),
            serde_json::to_value(restored).unwrap()
        );

        let mut too_many = document();
        too_many.patch.fx = (0..super::super::fx::MAX_SYNTH_FX + 1)
            .map(|_| super::super::SynthFx::new(super::super::SynthFxType::Drive))
            .collect();
        assert!(too_many.validate().is_err());

        let mut non_finite = document();
        let mut fx = super::super::SynthFx::new(super::super::SynthFxType::Drive);
        fx.params.push(("drive".into(), f64::NAN));
        non_finite.patch.fx = vec![fx];
        assert!(non_finite.validate().is_err());
    }

    #[test]
    fn document_validates_and_exports_fx_modulation_routes() {
        let mut d = document();
        d.patch.fx = vec![super::super::SynthFx::new(
            super::super::SynthFxType::Distortion,
        )];
        d.patch.fx_mod_routes = vec![super::super::FxModRoute {
            source: super::super::ModSource::Macro2,
            fx_index: 0,
            parameter: super::super::FxParam::Wet,
            amount: 0.75,
            bipolar: true,
        }];
        d.patch.mod_routes.resize(
            super::super::MAX_MOD_ROUTES,
            super::super::ModRoute::default(),
        );
        assert!(d.validate().is_ok());
        let params = d.to_ocp_params().unwrap();
        for key in [
            "fxmod.1.source",
            "fxmod.1.fx_index",
            "fxmod.1.parameter",
            "fxmod.1.amount",
            "fxmod.1.bipolar",
        ] {
            assert!(params.iter().any(|(name, _)| name == key), "missing {key}");
        }
        let mut restored = SynthPatch::default();
        restored.apply_params(Some(&params));
        assert_eq!(restored.fx_mod_routes.len(), 1);
        assert_eq!(
            restored.fx_mod_routes[0].source,
            super::super::ModSource::Macro2
        );
        assert_eq!(
            restored.fx_mod_routes[0].parameter,
            super::super::FxParam::Wet
        );
        assert!(restored.fx_mod_routes[0].bipolar);
        assert!((restored.fx_mod_routes[0].amount - 0.75).abs() < 1e-9);
        assert_eq!(
            serde_json::to_value(&d.patch).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );

        let mut too_many = document();
        too_many.patch.fx_mod_routes =
            vec![super::super::FxModRoute::default(); super::super::MAX_FX_MOD_ROUTES + 1];
        assert!(too_many.validate().is_err());

        let mut bad_slot = document();
        bad_slot.patch.fx_mod_routes = vec![super::super::FxModRoute {
            fx_index: super::super::fx::MAX_SYNTH_FX,
            amount: 0.5,
            ..Default::default()
        }];
        assert!(bad_slot.validate().is_err());

        let mut bad_amount = document();
        bad_amount.patch.fx_mod_routes = vec![super::super::FxModRoute {
            amount: 1.5,
            ..Default::default()
        }];
        assert!(bad_amount.validate().is_err());
    }
}
