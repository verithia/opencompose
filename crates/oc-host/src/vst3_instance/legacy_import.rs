//! Import an opaque VST2 bank through its VST3 processor's compatibility reader.

use super::compatibility;
use super::preset_file::{PresetFile, class_id_bytes};
use super::{Vst3ClassSelector, Vst3Instance, Vst3LoadTarget};
use crate::vst2::program_bank::migration_bank_id;
use std::path::Path;

fn vst2_processor_id(id: u32, name: &str) -> [u8; 16] {
    let mut class = [0; 16];
    class[..3].copy_from_slice(b"VST");
    class[3..7].copy_from_slice(&id.to_be_bytes());
    for (byte, name_byte) in class[7..].iter_mut().zip(name.bytes()) {
        *byte = name_byte.to_ascii_lowercase();
    }
    class
}

fn export_preset(instance: &Vst3Instance, component: &[u8]) -> Result<Vec<u8>, String> {
    let controller = instance.capture_controller_state()?;
    PresetFile {
        component,
        controller: controller.as_deref().filter(|bytes| !bytes.is_empty()),
    }
    .encode(instance.class_info().cid)
    .map_err(|error| error.to_string())
}

fn normalized_parameters(instance: &Vst3Instance) -> Vec<(u32, f64)> {
    let mut parameters: Vec<_> = instance
        .get_params_with_values()
        .into_iter()
        .map(|(parameter, value)| (parameter.id, value))
        .collect();
    parameters.sort_unstable_by_key(|(id, _)| *id);
    parameters
}

/// Export a VST2 bank as a native `.vstpreset`.
///
/// Select the standard VST2-derived class or a declared replacement class.
/// The processor must recall changed state and reopen its own exported preset on a
/// clean instance. Call on the native control owner outside realtime processing.
/// See the VST3 developer portal's compatibility guidance for the legacy ID
/// and for passing the complete FXB stream to the VST3 processor.
pub(super) fn import_vst2_bank(
    bundle: &Path,
    source_name: &str,
    bank: &[u8],
) -> Result<Vec<u8>, String> {
    let id = migration_bank_id(bank)?;
    if source_name.is_empty() || source_name.as_bytes().contains(&0) {
        return Err("the legacy VST2 plugin name is required".into());
    }
    let class = compatibility::resolve_legacy_class(bundle, vst2_processor_id(id, source_name))?;
    let open = || -> Result<Vst3Instance, String> {
        let instance = Vst3Instance::load_selected(
            Vst3LoadTarget {
                bundle,
                class: Vst3ClassSelector::Uid(class_id_bytes(class.cid)),
            },
            48_000,
            512,
        )?;
        if instance.class_info().cid != class.cid {
            return Err("loaded VST3 processor differs from the selected legacy class".into());
        }
        Ok(instance)
    };
    let mut instance = open()?;
    let default = instance.state_bytes()?;
    instance.set_state_bytes(bank)?;
    let component = instance.state_bytes()?;
    if component.is_empty() || component == default {
        return Err(
            "the VST3 processor did not produce distinct native state from the bank".into(),
        );
    }
    let preset = export_preset(&instance, &component)?;
    let parameters = normalized_parameters(&instance);
    drop(instance);
    let mut fresh = open()?;
    fresh.set_state_bytes(&preset)?;

    let reopened = fresh.state_bytes()?;
    let canonical = export_preset(&fresh, &reopened)?;
    if canonical == preset {
        return Ok(preset);
    }
    // Some compatibility readers reserialize migrated data in native form
    // on the first VST3 reopen. Require stable native state after this rewrite
    // and reject it if any exposed parameter changes value.
    let readback = normalized_parameters(&fresh);
    if readback.len() != parameters.len()
        || readback
            .iter()
            .zip(parameters)
            .any(|((id, value), (before_id, before))| {
                *id != before_id
                    || !value.is_finite()
                    || !before.is_finite()
                    || (value - before).abs() > 1e-6
            })
    {
        return Err("the VST3 processor changed parameter values on reopen".into());
    }
    drop(fresh);
    let mut stable = open()?;
    stable.set_state_bytes(&canonical)?;
    if export_preset(&stable, &stable.state_bytes()?)? != canonical {
        return Err("the VST3 processor did not recall the exported state on reopen".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_processor_id_matches_format_order_and_short_names() {
        assert_eq!(
            vst2_processor_id(u32::from_be_bytes(*b"Tst1"), "LeAd"),
            *b"VSTTst1lead\0\0\0\0\0"
        );
        assert_eq!(
            vst2_processor_id(u32::from_be_bytes(*b"Tst1"), "VeryLongName"),
            *b"VSTTst1verylongn"
        );
    }

    #[test]
    fn bank_preflight_checks_container_before_loading() {
        let mut bank = vec![0; 164];
        bank[..4].copy_from_slice(b"CcnK");
        bank[4..8].copy_from_slice(&156u32.to_be_bytes());
        bank[8..12].copy_from_slice(b"FBCh");
        bank[12..16].copy_from_slice(&2u32.to_be_bytes());
        bank[16..20].copy_from_slice(b"Tst1");
        bank[24..28].copy_from_slice(&1u32.to_be_bytes());
        bank[156..160].copy_from_slice(&4u32.to_be_bytes());
        assert_eq!(migration_bank_id(&bank), Ok(u32::from_be_bytes(*b"Tst1")));
        assert!(migration_bank_id(&bank[..163]).is_err());
        bank[31] = 1;
        assert!(migration_bank_id(&bank).is_err());
        assert!(import_vst2_bank(Path::new("missing.vst3"), "Lead", &bank).is_err());
    }
}
