//! Effect identity shared by project text, automation and command adapters.
use crate::ast::EffectType;

macro_rules! effect_identities {
    ($($variant:ident => ($device:literal, $token:literal, [$($alias:literal),*])),* $(,)?) => {
        impl EffectType {
            /// All built-in effect identities; external plugins carry their own ID.
            pub const BUILTINS: &'static [Self] = &[$(Self::$variant),*];

            /// Built-in device schema this effect instantiates; plugins have none.
            pub fn device_id(&self) -> Option<&'static str> {
                match self { $(Self::$variant => Some($device),)* Self::Plugin(_) => None }
            }

            /// Canonical project token, also used to address automation lanes.
            pub fn lane_name(&self) -> &'static str {
                match self { $(Self::$variant => $token,)* Self::Plugin(_) => "plugin" }
            }

            /// Accepted built-in spellings; project serialization uses lane_name.
            pub fn aliases(&self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$device, $token, $($alias),*],)*
                    Self::Plugin(_) => &[],
                }
            }

            /// Built-in names and compatibility aliases, with ASCII case ignored.
            pub fn builtin_from_name(name: &str) -> Option<Self> {
                $(if name.eq_ignore_ascii_case($device) || name.eq_ignore_ascii_case($token)
                    $(|| name.eq_ignore_ascii_case($alias))* {
                    return Some(Self::$variant);
                })*
                None
            }

            /// Command identity. Plugin IDs remain case-sensitive and opaque.
            pub fn from_name(name: &str) -> Option<Self> {
                if let Some(id) = name.strip_prefix("plugin:") {
                    return Some(Self::Plugin(id.to_owned()));
                }
                Self::builtin_from_name(name)
            }
        }
    };
}

effect_identities! {
    Gain => ("gain", "gain", []),
    Eq => ("eq", "eq", []),
    DynamicEq => ("dynamic-eq", "dynamic_eq", ["dynamiceq"]),
    Compression => ("compression", "compression", ["compressor"]),
    DeEsser => ("de-esser", "de_esser", ["deesser"]),
    BreathSplit => ("breath-split", "breath_split", []),
    Plosive => ("plosive", "plosive", []),
    Multiband => ("multiband", "multiband", ["multiband_compressor", "multiband-compressor"]),
    Leveler => ("leveler", "leveler", ["vocal_leveler"]),
    Opto => ("opto", "opto", ["opto_compressor"]),
    Fet => ("fet", "fet", ["fet_compressor"]),
    Iron => ("iron", "iron", []),
    PassiveEq => ("passive-eq", "passive_eq", []),
    Doubler => ("doubler", "doubler", []),
    Exciter => ("exciter", "exciter", []),
    DuckedSend => ("ducked-send", "ducked_send", []),
    Resonance => ("resonance", "resonance", []),
    Tuner => ("tuner", "tuner", []),
    Reverb => ("reverb", "reverb", []),
    ReverbFdn => ("reverb-fdn", "reverb_fdn", []),
    Delay => ("delay", "delay", []),
    Distortion => ("distortion", "distortion", []),
    Chorus => ("chorus", "chorus", []),
    Flanger => ("flanger", "flanger", []),
    Phaser => ("phaser", "phaser", []),
    Limiter => ("limiter", "limiter", []),
    Gate => ("gate", "gate", []),
    Saturator => ("saturator", "saturator", []),
    StereoWidth => ("stereo-width", "stereo_width", []),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_identity_roundtrips_every_builtin_through_project_text_and_json() {
        let mut devices = std::collections::BTreeSet::new();
        let mut tokens = std::collections::BTreeSet::new();
        for effect in EffectType::BUILTINS {
            let device = effect.device_id().unwrap();
            let token = effect.lane_name();
            assert!(devices.insert(device));
            assert!(tokens.insert(token));
            assert_eq!(EffectType::from_name(device), Some(effect.clone()));
            assert_eq!(EffectType::from_name(token), Some(effect.clone()));
            assert_eq!(
                EffectType::from_name(&device.to_ascii_uppercase()),
                Some(effect.clone())
            );
            let source = format!("@project\n@master\n  effects:\n    - {token}\n");
            let project = crate::parse(&source).unwrap();
            let text = crate::serialize(&project);
            assert!(text.contains(&format!("- {token}")), "{text}");
            assert_eq!(crate::parse(&text).unwrap(), project);
            for alias in effect.aliases() {
                let source = format!("@project\n@master\n  effects:\n    - {alias}\n");
                let parsed = crate::parse(&source).unwrap();
                assert_eq!(
                    parsed.master.as_ref().unwrap().effects[0].effect_type,
                    *effect
                );
                assert_eq!(crate::serialize(&parsed), text);
            }
            let json = serde_json::to_string(effect).unwrap();
            assert_eq!(serde_json::from_str::<EffectType>(&json).unwrap(), *effect);
        }
        assert_eq!(devices.len(), 29);
    }

    #[test]
    fn effect_identity_preserves_command_aliases_and_opaque_plugin_ids() {
        for (name, expected) in [
            ("compressor", EffectType::Compression),
            ("dynamiceq", EffectType::DynamicEq),
            ("deesser", EffectType::DeEsser),
            ("multiband_compressor", EffectType::Multiband),
            ("multiband-compressor", EffectType::Multiband),
            ("vocal_leveler", EffectType::Leveler),
            ("opto_compressor", EffectType::Opto),
            ("fet_compressor", EffectType::Fet),
            ("passive-eq", EffectType::PassiveEq),
            ("stereo_width", EffectType::StereoWidth),
        ] {
            assert_eq!(EffectType::from_name(name), Some(expected));
        }
        for id in ["org.example.MixedCase", "with spaces:and/slashes", ""] {
            let effect = EffectType::from_name(&format!("plugin:{id}")).unwrap();
            assert_eq!(effect, EffectType::Plugin(id.into()));
            assert_eq!(effect.device_id(), None);
        }
        for name in ["unknown", " gain", "gain ", "", "PLUGin:example"] {
            assert_eq!(EffectType::from_name(name), None);
        }
        assert!(crate::parse("@project\n@master\n  effects:\n    - unknown\n").is_err());
    }
}
