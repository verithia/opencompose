//! Built-in test plugins — ship with the DAW for testing and learning.
//!
//! These are small, self-contained plugins that implement AudioEffect.
//! They appear in the plugin browser as "Internal" format.

pub mod bitcrusher;
pub mod gain;
pub mod stereo_pan;
pub mod test_tone;
pub mod tremolo;

use super::{PluginCategory, PluginFormat, PluginInfo};
use oc_dsp::AudioEffect;

/// Create a built-in plugin instance by ID. Returns None if unknown.
pub fn create_builtin(id: &str) -> Option<Box<dyn AudioEffect>> {
    match id {
        "builtin.gain" => Some(Box::new(gain::Gain::new())),
        "builtin.test_tone" => Some(Box::new(test_tone::TestTone::new())),
        "builtin.bitcrusher" => Some(Box::new(bitcrusher::Bitcrusher::new())),
        "builtin.stereo_pan" => Some(Box::new(stereo_pan::StereoPan::new())),
        "builtin.tremolo" => Some(Box::new(tremolo::Tremolo::new())),
        _ => None,
    }
}

/// Get the list of all built-in plugins as PluginInfo.
pub fn builtin_plugin_list() -> Vec<PluginInfo> {
    vec![
        PluginInfo {
            id: oc_synth::audio_region::PLUGIN_ID.into(),
            name: "Audio Regions".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.audio_regions:document-v2".into()),
        },
        PluginInfo {
            id: oc_synth::sampler::PLUGIN_ID.into(),
            name: "Sampler".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.sampler:sfz-v1".into()),
        },
        PluginInfo {
            id: oc_synth::drumkit::PLUGIN_ID.into(),
            name: "Drum Kit".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.drumkit:pads-v1".into()),
        },
        PluginInfo {
            id: oc_synth::synth::PLUGIN_ID.into(),
            name: "OpenSynth".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.open_synth:params-v2".into()),
        },
        PluginInfo {
            id: "builtin.gain".into(),
            name: "Gain".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Effect,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.gain:params-v1".into()),
        },
        PluginInfo {
            id: "builtin.test_tone".into(),
            name: "Test Tone".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Instrument,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.test_tone:params-v1".into()),
        },
        PluginInfo {
            id: "builtin.bitcrusher".into(),
            name: "Bitcrusher".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Effect,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.bitcrusher:params-v1".into()),
        },
        PluginInfo {
            id: "builtin.stereo_pan".into(),
            name: "Stereo Pan".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Effect,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.stereo_pan:params-v1".into()),
        },
        PluginInfo {
            id: "builtin.tremolo".into(),
            name: "Tremolo".into(),
            vendor: "OpenCompose".into(),
            format: PluginFormat::Internal,
            path: String::new(),
            category: PluginCategory::Effect,
            num_params: 0,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            architectures: vec![std::env::consts::ARCH.into()],
            state_abi: Some("opencompose:builtin.tremolo:params-v1".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_region_source_has_one_internal_identity() {
        let plugins = super::builtin_plugin_list();
        let matches: Vec<_> = plugins
            .iter()
            .filter(|p| p.id == oc_synth::audio_region::PLUGIN_ID)
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].format, crate::PluginFormat::Internal);
        assert_eq!(
            matches[0].state_abi.as_deref(),
            Some("opencompose:builtin.audio_regions:document-v2")
        );
    }
}
