//! Typed project adapters for the dependency-free General MIDI authority.
use crate::ast::InstrumentPath;

pub use oc_device_schema::gm::{Program, playback_program, program, programs};

/// Preserve the interpreter's existing family fallbacks and unknown-path default.
pub fn instrument_to_program(path: &InstrumentPath) -> u8 {
    oc_device_schema::gm::playback_program(&path.category, &path.name).unwrap_or(0)
}

/// A broad imported instrument suggestion, not an exact program identity.
pub fn suggested_instrument(program: u8) -> Option<InstrumentPath> {
    let (category, name) = oc_device_schema::gm::suggested_instrument(program)?;
    Some(InstrumentPath::new(category, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_adapter_preserves_program_identity_and_historical_playback_paths() {
        for id in 0..=127 {
            assert_eq!(program(id).unwrap().program, id);
            assert!(suggested_instrument(id).is_some());
        }
        for (category, name, expected) in [
            ("guitar", "acoustic_steel", 25),
            ("bass", "electric_finger", 33),
            ("guitar", "electric_jazz", 26),
            ("strings", "unknown", 48),
            ("unknown", "unknown", 0),
        ] {
            assert_eq!(
                instrument_to_program(&InstrumentPath::new(category, name)),
                expected
            );
        }
        assert!(suggested_instrument(128).is_none());
    }
}
