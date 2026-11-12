use oc_engine::preset::SoundPreset;

/// Insert sidechain sources name project tracks or buses, not portable detectors.
pub(super) fn validate(preset: &SoundPreset) -> Result<(), String> {
    for (index, effect) in preset.effects.iter().enumerate() {
        let Some(source) = &effect.sidechain_source else {
            continue;
        };
        let label = effect.effect_type.device_id().unwrap_or("plugin");
        return Err(format!(
            "Insert {} ({label}) in preset '{}' uses external sidechain '{}'. Preset bundles must be self-contained. Remove the sidechain source or use the insert's internal detector before sharing or loading this bundle.",
            index + 1,
            preset.name,
            source,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::{Effect, EffectType, ParamValue};
    use oc_engine::preset::{PresetBackend, PresetCategory, PresetTier};

    fn preset(effects: Vec<Effect>) -> SoundPreset {
        SoundPreset {
            id: "test:shared".into(),
            name: "Shared bass".into(),
            category: PresetCategory::Bass,
            tags: vec![],
            backend: PresetBackend::BuiltinPlugin {
                plugin_id: oc_synth::synth::PLUGIN_ID.into(),
            },
            tier: PresetTier::Project,
            patch: None,
            params: vec![],
            effects,
        }
    }

    fn compressor(source: Option<&str>) -> Effect {
        Effect {
            effect_type: EffectType::Compression,
            params: vec![],
            bypass: false,
            sidechain_source: source.map(str::to_owned),
        }
    }

    #[test]
    fn refuses_track_and_bus_keys_with_insert_context() {
        for source in ["kick", "drums_bus"] {
            let patch = preset(vec![compressor(None), compressor(Some(source))]);
            let error = validate(&patch).unwrap_err();
            assert!(error.contains("Insert 2 (compression)"), "{error}");
            assert!(error.contains("Shared bass"), "{error}");
            assert!(error.contains(source), "{error}");
            assert!(error.contains("internal detector"), "{error}");
        }
    }

    #[test]
    fn bypass_does_not_make_an_external_key_portable() {
        let mut effect = compressor(Some("kick"));
        effect.bypass = true;
        assert!(validate(&preset(vec![effect])).is_err());
    }

    #[test]
    fn allows_internal_detection_and_detector_filters() {
        let mut effect = compressor(None);
        effect.params = vec![
            ("detector".into(), ParamValue::String("rms".into())),
            ("detector_hpf".into(), ParamValue::Frequency(120.0)),
        ];
        assert!(validate(&preset(vec![effect])).is_ok());
        assert!(validate(&preset(vec![])).is_ok());
    }

    #[test]
    fn named_self_is_still_a_project_reference() {
        // An absent source selects internal detection; no name is a self sentinel.
        for source in ["self", "internal", "Shared bass"] {
            assert!(validate(&preset(vec![compressor(Some(source))])).is_err());
        }
    }
}
