//! 3-tier preset system.
//!
//! ```text
//! Resolution order (highest priority first):
//!   1. Project-specific (@preset blocks in .ocp file)
//!   2. User library    (~/.opencompose/presets/*.ocp-preset)
//!   3. Built-in        (GM instruments, built-in plugins)
//!
//! Track → preset name → resolve(project, library, builtins) → SoundPreset
//! ```
//!
//! A project preset can also *extend* a library or built-in preset:
//! ```ocp
//! @preset my_piano
//!   extends: "Acoustic Grand Piano"
//!   effects:
//!     - reverb: decay=2s, mix=30%
//! ```

#[cfg(feature = "plugin-host")]
pub mod hosted_parameters;

pub mod library;
mod library_assets;

use serde::{Deserialize, Serialize};

pub use library::{LibraryEntry, LibraryScan, PresetLibrary, SaveOutcome};

// ============================================================================
// Core types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// How a preset reaches its sound source.
pub enum PresetBackend {
    Gm {
        program: u8,
        bank: u16,
    },
    BuiltinPlugin {
        plugin_id: String,
    },
    ClapPlugin {
        path: String,
        plugin_id: String,
    },
    Vst3Plugin {
        path: String,
    },
    Vst2Plugin {
        path: String,
        plugin_id: String,
    },
    Sample {
        path: String,
        root_note: u8,
        looped: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One browsable preset entry.
pub struct SoundPreset {
    pub id: String,
    pub name: String,
    pub category: PresetCategory,
    pub tags: Vec<String>,
    pub backend: PresetBackend,
    /// Which tier this preset comes from
    pub tier: PresetTier,
    /// Patch this entry was derived from, kept as browsing provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Whole patch for builtin instruments; library entries carry their own
    /// parameters so loading one never inherits the previous patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<(String, oc_dsl::ast::ParamValue)>,
    /// Patch-level effect chain, applied after the instrument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<oc_dsl::ast::Effect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
/// Priority tier a preset comes from.
pub enum PresetTier {
    Builtin,
    UserLibrary,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Browsing category of a preset.
pub enum PresetCategory {
    Piano,
    Organ,
    Guitar,
    Bass,
    Strings,
    Brass,
    Woodwinds,
    Synth,
    Pad,
    Lead,
    Drums,
    Percussion,
    Vocal,
    Fx,
    Sample,
    Other,
}

impl PresetCategory {
    /// Map a lowercase browsing category onto the stored enum.
    pub fn from_slug(slug: &str) -> Self {
        match slug.trim().to_ascii_lowercase().as_str() {
            "piano" | "keys" => Self::Piano,
            "organ" => Self::Organ,
            "guitar" => Self::Guitar,
            "bass" => Self::Bass,
            "strings" => Self::Strings,
            "brass" => Self::Brass,
            "woodwinds" => Self::Woodwinds,
            "pad" => Self::Pad,
            "lead" => Self::Lead,
            "drums" => Self::Drums,
            "percussion" => Self::Percussion,
            "vocal" => Self::Vocal,
            "fx" => Self::Fx,
            "sample" => Self::Sample,
            "" | "synth" | "pluck" => Self::Synth,
            _ => Self::Other,
        }
    }

    /// Display label for the category.
    pub fn label(&self) -> &str {
        match self {
            Self::Piano => "Piano",
            Self::Organ => "Organ",
            Self::Guitar => "Guitar",
            Self::Bass => "Bass",
            Self::Strings => "Strings",
            Self::Brass => "Brass",
            Self::Woodwinds => "Woodwinds",
            Self::Synth => "Synth",
            Self::Pad => "Pad",
            Self::Lead => "Lead",
            Self::Drums => "Drums",
            Self::Percussion => "Percussion",
            Self::Vocal => "Vocal",
            Self::Fx => "FX",
            Self::Sample => "Sample",
            Self::Other => "Other",
        }
    }
}

// ============================================================================
// Preset Registry — holds all 3 tiers
// ============================================================================

/// Builtin, user-library and project presets.
pub struct PresetRegistry {
    /// Tier 3 (lowest priority): ships with the DAW
    builtins: Vec<SoundPreset>,
    /// Tier 2: user's global library (~/.opencompose/presets/)
    library: Vec<SoundPreset>,
    /// Tier 1 (highest priority): from the current project's @preset blocks
    project: Vec<SoundPreset>,
    /// Files the last library scan could not read.
    library_warnings: Vec<String>,
}

impl Default for PresetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetRegistry {
    /// Create a registry and load builtin/user presets.
    pub fn new() -> Self {
        let mut reg = Self {
            builtins: Vec::new(),
            library: Vec::new(),
            project: Vec::new(),
            library_warnings: Vec::new(),
        };
        reg.load_builtins();
        reg.load_user_library();
        reg
    }

    // === Resolution ===

    /// Resolve a preset name: project → library → builtin.
    pub fn resolve(&self, name: &str) -> Option<&SoundPreset> {
        self.project
            .iter()
            .find(|p| p.name == name)
            .or_else(|| self.library.iter().find(|p| p.name == name))
            .or_else(|| self.builtins.iter().find(|p| p.name == name))
    }

    /// Resolve a preset name to a GM program number (for the GM program map).
    pub fn resolve_program(&self, name: &str) -> Option<u8> {
        self.resolve(name).and_then(|p| match &p.backend {
            PresetBackend::Gm { program, .. } => Some(*program),
            _ => None,
        })
    }

    /// Find a CLAP plugin by ID or name from the cached registry.
    /// Returns a PluginInfo-like struct with the path and ID needed for loading.
    #[cfg(feature = "plugin-host")]
    pub fn find_clap_plugin(&self, id_or_name: &str) -> Option<oc_host::PluginInfo> {
        let all = self
            .builtins
            .iter()
            .chain(self.library.iter())
            .chain(self.project.iter());
        for preset in all {
            if let PresetBackend::ClapPlugin { path, plugin_id } = &preset.backend
                && (plugin_id == id_or_name || preset.name == id_or_name)
            {
                return Some(oc_host::PluginInfo {
                    id: plugin_id.clone(),
                    name: preset.name.clone(),
                    vendor: String::new(),
                    format: oc_host::PluginFormat::Clap,
                    path: path.clone(),
                    category: oc_host::PluginCategory::Instrument,
                    num_params: 0,
                    version: None,
                    architectures: vec![],
                    state_abi: None,
                });
            }
        }
        None
    }

    /// Get all presets from all tiers, flattened. Higher tiers shadow lower ones.
    pub fn all_merged(&self) -> Vec<&SoundPreset> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        // Project first (highest priority)
        for p in &self.project {
            if seen.insert(&p.name) {
                result.push(p);
            }
        }
        for p in &self.library {
            if seen.insert(&p.name) {
                result.push(p);
            }
        }
        for p in &self.builtins {
            if seen.insert(&p.name) {
                result.push(p);
            }
        }
        result
    }

    /// Get all presets as owned, serializable list (for Tauri IPC).
    pub fn all_serializable(&self) -> Vec<SoundPreset> {
        self.all_merged().into_iter().cloned().collect()
    }

    // === Tier access ===

    /// Builtin tier.
    pub fn builtins(&self) -> &[SoundPreset] {
        &self.builtins
    }
    /// User-library tier.
    pub fn library(&self) -> &[SoundPreset] {
        &self.library
    }
    /// Project tier.
    pub fn project_presets(&self) -> &[SoundPreset] {
        &self.project
    }

    // === Project presets (from @preset blocks) ===

    /// Sync project presets from AST PresetDef blocks.
    pub fn sync_project_presets(&mut self, defs: &[oc_dsl::ast::PresetDef]) {
        self.project = defs
            .iter()
            .map(|d| {
                let backend = preset_def_to_backend(d);
                let category = guess_category_from_backend(&backend, &d.source);
                SoundPreset {
                    id: format!("project:{}", d.name),
                    name: d.name.clone(),
                    category,
                    tags: vec!["project".into()],
                    backend,
                    tier: PresetTier::Project,
                    patch: d.patch.clone(),
                    params: d.params.clone(),
                    effects: d.effects.clone(),
                }
            })
            .collect();
    }

    // === User library ===

    /// Save a preset to the user library.
    pub fn save_to_library(&mut self, preset: &SoundPreset) -> Result<SaveOutcome, String> {
        let outcome = PresetLibrary::default().save(preset)?;
        self.library.retain(|entry| entry.name != preset.name);
        let mut stored = preset.clone();
        stored.tier = PresetTier::UserLibrary;
        stored.id = format!("library:{}", preset.name);
        self.library.push(stored);
        Ok(outcome)
    }

    /// Remove a preset from the user library.
    pub fn remove_from_library(&mut self, name: &str) -> Result<bool, String> {
        let removed = PresetLibrary::default().remove(name)?;
        self.library.retain(|preset| preset.name != name);
        Ok(removed)
    }

    /// Re-read the user library from disk; keeps the warnings for the GUI.
    pub fn reload_library(&mut self) {
        self.library.clear();
        self.library_warnings.clear();
        self.load_user_library();
    }

    /// Files skipped by the last library scan.
    pub fn library_warnings(&self) -> &[String] {
        &self.library_warnings
    }

    /// Load user library from disk.
    fn load_user_library(&mut self) {
        let scan = PresetLibrary::default().scan();
        self.library = scan.presets;
        self.library_warnings = scan.warnings;
        log::info!("Loaded {} presets from user library", self.library.len());
    }

    // === CLAP/VST3 registration ===

    #[cfg(feature = "plugin-host")]
    /// Register CLAP instrument plugins as builtin presets.
    pub fn register_clap_plugins(&mut self, plugins: &[oc_host::PluginInfo]) {
        for plugin in plugins {
            if plugin.format != oc_host::PluginFormat::Clap {
                continue;
            }
            if plugin.category != oc_host::PluginCategory::Instrument {
                continue;
            }
            self.builtins.push(SoundPreset {
                id: format!("clap:{}", plugin.id),
                name: plugin.name.clone(),
                category: PresetCategory::Synth,
                tags: vec!["clap".into(), plugin.vendor.to_lowercase()],
                backend: PresetBackend::ClapPlugin {
                    path: plugin.path.clone(),
                    plugin_id: plugin.id.clone(),
                },
                tier: PresetTier::Builtin,
                patch: None,
                params: Vec::new(),
                effects: Vec::new(),
            });
        }
    }

    // === Built-in presets ===

    fn load_builtins(&mut self) {
        // GM instruments
        let gm: &[(u8, &str, PresetCategory, &[&str])] = &[
            (
                0,
                "Acoustic Grand Piano",
                PresetCategory::Piano,
                &["piano", "acoustic", "grand"],
            ),
            (
                1,
                "Bright Acoustic Piano",
                PresetCategory::Piano,
                &["piano", "bright"],
            ),
            (
                4,
                "Electric Piano 1",
                PresetCategory::Piano,
                &["piano", "electric", "rhodes"],
            ),
            (
                5,
                "Electric Piano 2",
                PresetCategory::Piano,
                &["piano", "electric", "dx"],
            ),
            (6, "Harpsichord", PresetCategory::Piano, &["harpsichord"]),
            (
                16,
                "Drawbar Organ",
                PresetCategory::Organ,
                &["organ", "drawbar"],
            ),
            (
                19,
                "Church Organ",
                PresetCategory::Organ,
                &["organ", "church"],
            ),
            (
                24,
                "Nylon Guitar",
                PresetCategory::Guitar,
                &["guitar", "nylon"],
            ),
            (
                25,
                "Steel Guitar",
                PresetCategory::Guitar,
                &["guitar", "steel"],
            ),
            (
                27,
                "Clean Electric Guitar",
                PresetCategory::Guitar,
                &["guitar", "electric", "clean"],
            ),
            (
                29,
                "Overdriven Guitar",
                PresetCategory::Guitar,
                &["guitar", "overdrive"],
            ),
            (
                30,
                "Distortion Guitar",
                PresetCategory::Guitar,
                &["guitar", "distortion"],
            ),
            (
                32,
                "Acoustic Bass",
                PresetCategory::Bass,
                &["bass", "acoustic"],
            ),
            (33, "Finger Bass", PresetCategory::Bass, &["bass", "finger"]),
            (34, "Pick Bass", PresetCategory::Bass, &["bass", "pick"]),
            (
                35,
                "Fretless Bass",
                PresetCategory::Bass,
                &["bass", "fretless"],
            ),
            (36, "Slap Bass", PresetCategory::Bass, &["bass", "slap"]),
            (38, "Synth Bass", PresetCategory::Bass, &["bass", "synth"]),
            (40, "Violin", PresetCategory::Strings, &["violin", "solo"]),
            (41, "Viola", PresetCategory::Strings, &["viola", "solo"]),
            (42, "Cello", PresetCategory::Strings, &["cello", "solo"]),
            (43, "Contrabass", PresetCategory::Strings, &["contrabass"]),
            (
                44,
                "Tremolo Strings",
                PresetCategory::Strings,
                &["strings", "tremolo"],
            ),
            (
                45,
                "Pizzicato Strings",
                PresetCategory::Strings,
                &["strings", "pizzicato"],
            ),
            (46, "Orchestral Harp", PresetCategory::Strings, &["harp"]),
            (47, "Timpani", PresetCategory::Percussion, &["timpani"]),
            (
                48,
                "String Ensemble",
                PresetCategory::Strings,
                &["strings", "ensemble"],
            ),
            (56, "Trumpet", PresetCategory::Brass, &["trumpet"]),
            (57, "Trombone", PresetCategory::Brass, &["trombone"]),
            (58, "Tuba", PresetCategory::Brass, &["tuba"]),
            (60, "French Horn", PresetCategory::Brass, &["french horn"]),
            (
                61,
                "Brass Section",
                PresetCategory::Brass,
                &["brass", "section"],
            ),
            (
                64,
                "Soprano Sax",
                PresetCategory::Woodwinds,
                &["sax", "soprano"],
            ),
            (65, "Alto Sax", PresetCategory::Woodwinds, &["sax", "alto"]),
            (
                66,
                "Tenor Sax",
                PresetCategory::Woodwinds,
                &["sax", "tenor"],
            ),
            (68, "Oboe", PresetCategory::Woodwinds, &["oboe"]),
            (70, "Bassoon", PresetCategory::Woodwinds, &["bassoon"]),
            (71, "Clarinet", PresetCategory::Woodwinds, &["clarinet"]),
            (72, "Piccolo", PresetCategory::Woodwinds, &["piccolo"]),
            (73, "Flute", PresetCategory::Woodwinds, &["flute"]),
            (
                80,
                "Square Lead",
                PresetCategory::Lead,
                &["synth", "lead", "square"],
            ),
            (
                81,
                "Saw Lead",
                PresetCategory::Lead,
                &["synth", "lead", "saw"],
            ),
            (88, "New Age Pad", PresetCategory::Pad, &["synth", "pad"]),
            (
                89,
                "Warm Pad",
                PresetCategory::Pad,
                &["synth", "pad", "warm"],
            ),
            (
                91,
                "Choir Pad",
                PresetCategory::Pad,
                &["synth", "pad", "choir"],
            ),
        ];

        for &(program, name, ref cat, tags) in gm {
            self.builtins.push(SoundPreset {
                id: format!("gm:{}", program),
                name: name.to_string(),
                category: cat.clone(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                backend: PresetBackend::Gm { program, bank: 0 },
                tier: PresetTier::Builtin,
                patch: None,
                params: Vec::new(),
                effects: Vec::new(),
            });
        }

        // Built-in plugins
        self.builtins.push(SoundPreset {
            id: "builtin:test_tone".into(),
            name: "Test Tone Generator".into(),
            category: PresetCategory::Synth,
            tags: vec!["test".into(), "sine".into(), "generator".into()],
            backend: PresetBackend::BuiltinPlugin {
                plugin_id: "builtin.test_tone".into(),
            },
            tier: PresetTier::Builtin,
            patch: None,
            params: Vec::new(),
            effects: Vec::new(),
        });
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn preset_def_to_backend(def: &oc_dsl::ast::PresetDef) -> PresetBackend {
    match def.source.as_deref() {
        Some("soundfont") => PresetBackend::Gm {
            program: def.program.unwrap_or(0),
            bank: 0,
        },
        Some("clap") => PresetBackend::ClapPlugin {
            path: String::new(),
            plugin_id: def.plugin.clone().unwrap_or_default(),
        },
        Some("vst3") => PresetBackend::Vst3Plugin {
            path: def.plugin.clone().unwrap_or_default(),
        },
        Some("vst2") => PresetBackend::Vst2Plugin {
            path: String::new(),
            plugin_id: def.plugin.clone().unwrap_or_default(),
        },
        Some("builtin") => PresetBackend::BuiltinPlugin {
            plugin_id: def.plugin.clone().unwrap_or_default(),
        },
        Some("sample") => PresetBackend::Sample {
            path: def.file.clone().unwrap_or_default(),
            root_note: def.root_note.as_ref().map(|p| p.to_midi()).unwrap_or(60),
            looped: false,
        },
        _ => PresetBackend::Gm {
            program: 0,
            bank: 0,
        },
    }
}

fn guess_category_from_backend(
    backend: &PresetBackend,
    _source: &Option<String>,
) -> PresetCategory {
    match backend {
        PresetBackend::Gm { program, .. } => match program {
            0..=7 => PresetCategory::Piano,
            8..=15 => PresetCategory::Piano,
            16..=23 => PresetCategory::Organ,
            24..=31 => PresetCategory::Guitar,
            32..=39 => PresetCategory::Bass,
            40..=55 => PresetCategory::Strings,
            56..=63 => PresetCategory::Brass,
            64..=79 => PresetCategory::Woodwinds,
            80..=87 => PresetCategory::Lead,
            88..=95 => PresetCategory::Pad,
            _ => PresetCategory::Other,
        },
        PresetBackend::ClapPlugin { .. }
        | PresetBackend::Vst3Plugin { .. }
        | PresetBackend::Vst2Plugin { .. } => PresetCategory::Synth,
        PresetBackend::BuiltinPlugin { .. } => PresetCategory::Synth,
        PresetBackend::Sample { .. } => PresetCategory::Sample,
    }
}

/// Whether a SoundFont track crossfades velocity layers along a hairpin.
///
/// The track's `engine_params` win over the preset's `params`; the default is on.
pub(crate) fn dynamic_layers_enabled(
    preset: Option<&oc_dsl::ast::PresetDef>,
    track: &oc_dsl::ast::Track,
) -> bool {
    let mut enabled = true;
    let preset_params = preset.map(|preset| preset.params.as_slice()).unwrap_or(&[]);
    for (key, value) in preset_params
        .iter()
        .chain(track.engine_params.iter().flatten())
    {
        if key != "dynamic_layers" {
            continue;
        }
        enabled = match value {
            oc_dsl::ast::ParamValue::Bool(flag) => *flag,
            oc_dsl::ast::ParamValue::Number(number) => *number != 0.0,
            oc_dsl::ast::ParamValue::String(text) => !matches!(
                text.to_ascii_lowercase().as_str(),
                "off" | "false" | "no" | "0"
            ),
            _ => enabled,
        };
    }
    enabled
}

/// Effective parameters for a built-in track: preset params plus track overrides.
///
/// Every OpenSynth instantiation reads this list, so the per-patch loudness
/// trim it carries reaches offline renders, streaming playback and previews
/// alike.
pub fn builtin_track_params(
    project: &oc_dsl::ast::Project,
    preset: Option<&oc_dsl::ast::PresetDef>,
    track: &oc_dsl::ast::Track,
) -> Vec<(String, oc_dsl::ast::ParamValue)> {
    builtin_track_params_at(project, preset, track, None)
}

/// Effective track parameters with the same asset root as the instrument.
pub fn builtin_track_params_at(
    project: &oc_dsl::ast::Project,
    preset: Option<&oc_dsl::ast::PresetDef>,
    track: &oc_dsl::ast::Track,
    resource_root: Option<&std::path::Path>,
) -> Vec<(String, oc_dsl::ast::ParamValue)> {
    let params = builtin_track_params_untrimmed(project, preset, track);
    #[cfg(feature = "plugin-host")]
    let params = {
        let mut params = params;
        if let Some(preset) = preset {
            crate::loudness::append_trim(&mut params, preset, resource_root);
        }
        params
    };
    #[cfg(not(feature = "plugin-host"))]
    let _ = resource_root;
    params
}

/// Project metadata lookups do not need to measure the instrument.
pub(crate) fn builtin_track_params_untrimmed(
    project: &oc_dsl::ast::Project,
    preset: Option<&oc_dsl::ast::PresetDef>,
    track: &oc_dsl::ast::Track,
) -> Vec<(String, oc_dsl::ast::ParamValue)> {
    let mut params = preset.map(|p| p.params.clone()).unwrap_or_default();
    params.extend(crate::mpe::patch_params(project));
    if let Some(overrides) = &track.engine_params {
        params.extend(overrides.iter().cloned());
    }
    params
}

#[cfg(test)]
mod dynamic_layers_tests {
    use super::dynamic_layers_enabled;

    fn project(source: &str) -> oc_dsl::ast::Project {
        oc_dsl::parse(source).expect("the flag fixture parses")
    }

    #[test]
    fn the_layer_crossfade_defaults_on_and_the_track_override_wins() {
        let plain = project(
            "@project\n@preset p\n  source: soundfont\n@track t | preset: p, instrument: piano/acoustic_grand\n",
        );
        assert!(dynamic_layers_enabled(
            plain.presets.first(),
            &plain.tracks[0]
        ));

        let preset_off = project(
            "@project\n@preset p\n  source: soundfont\n  params: {dynamic_layers: off}\n@track t | preset: p, instrument: piano/acoustic_grand\n",
        );
        assert!(!dynamic_layers_enabled(
            preset_off.presets.first(),
            &preset_off.tracks[0]
        ));

        let track_wins = project(
            "@project\n@preset p\n  source: soundfont\n  params: {dynamic_layers: off}\n@track t | preset: p, instrument: piano/acoustic_grand\n  engine_params: {dynamic_layers: on}\n",
        );
        assert!(dynamic_layers_enabled(
            track_wins.presets.first(),
            &track_wins.tracks[0]
        ));
    }
}
