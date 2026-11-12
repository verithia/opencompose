//! Static LV2 batches bind immutable native control ports rather than the
//! filtered UI parameter surface. Connected scalar writes have no native setter
//! callback; all validation precedes mutation of either PCM controls or readout.

use super::*;
use crate::preset_parameters::{
    AuthoredParameter, AuthoredParameterOwner, ParameterApplyError, ParameterErrorKind,
    ParameterRange, ParameterWriteDomain, PreparedParameterValue, ResolvedParameter,
};
use std::collections::HashSet;

fn validate_control_metadata(port: &metadata::Port) -> Result<(), ParameterErrorKind> {
    if !port.minimum.is_finite()
        || !port.maximum.is_finite()
        || port.minimum > port.maximum
        || !port.default.is_finite()
        || port.default < port.minimum
        || port.default > port.maximum
    {
        return Err(ParameterErrorKind::InvalidMetadata);
    }
    Ok(())
}

fn resolve_control_parameter(
    ports: &[metadata::Port],
    name: &str,
) -> Result<ResolvedParameter<u32>, ParameterErrorKind> {
    let target = normalize(name);
    let mut matching = ports.iter().filter(|port| {
        port.is_control()
            && (normalize(&port.name) == target
                || normalize(&port.symbol) == target
                || normalize(&format!("{}{}", port.symbol, port.name)) == target)
    });
    let port = matching.next().ok_or(ParameterErrorKind::Unknown)?;
    if matching.next().is_some() {
        return Err(ParameterErrorKind::Ambiguous);
    }
    validate_control_metadata(port)?;
    Ok(ResolvedParameter {
        key: port.index,
        writable: port.is_input()
            && !matches!(
                port.designation.as_deref(),
                Some(LV2__SAMPLE_RATE) | Some(LV2__LATENCY)
            ),
        plain_range: Some(ParameterRange {
            min: f64::from(port.minimum),
            max: f64::from(port.maximum),
        }),
    })
}

impl Lv2Instance {
    /// Apply one fully prevalidated native-unit batch. LV2 port metadata and
    /// connected scalar storage remain immutable for this instance's lifetime;
    /// exclusive access excludes a concurrent run or cleanup operation.
    pub fn apply_authored_parameters(
        &mut self,
        assignments: &[AuthoredParameter<'_>],
    ) -> Result<(), ParameterApplyError> {
        let fail = |kind| ParameterApplyError {
            format: crate::PluginFormat::Lv2,
            parameter: "<batch>".into(),
            kind,
        };
        let mut ids = HashSet::new();
        for port in &self.plugin.ports {
            if !ids.insert(port.index) {
                return Err(fail(ParameterErrorKind::InvalidMetadata));
            }
        }
        let inputs: Vec<_> = self
            .plugin
            .ports
            .iter()
            .filter(|port| port.is_control() && port.is_input())
            .collect();
        if inputs.len() != self.params.len() || inputs.len() != self.control_values.len() {
            return Err(fail(ParameterErrorKind::InvalidMetadata));
        }
        let mut slots = HashSet::new();
        let mut parameter_ports = HashSet::new();
        for parameter in &self.params {
            if parameter.slot >= self.control_values.len()
                || !slots.insert(parameter.slot)
                || !parameter_ports.insert(parameter.param.port)
                || parameter.param.id != parameter.param.port
                || !inputs.iter().any(|port| port.index == parameter.param.port)
            {
                return Err(fail(ParameterErrorKind::InvalidMetadata));
            }
        }
        CheckedLv2Parameters { instance: self }.apply_authored_parameters(assignments)
    }
}

struct CheckedLv2Parameters<'a> {
    instance: &'a mut Lv2Instance,
}
impl AuthoredParameterOwner for CheckedLv2Parameters<'_> {
    type Key = u32;
    fn parameter_format(&self) -> crate::PluginFormat {
        crate::PluginFormat::Lv2
    }
    fn check_parameter_authority(&mut self) -> Result<(), ParameterErrorKind> {
        if self.instance.cleaned_up || self.instance.handle.is_null() {
            return Err(ParameterErrorKind::Native {
                operation: "LV2 authored parameter preparation",
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "LV2 parameter owner is retired",
                )),
            });
        }
        Ok(())
    }
    fn resolve_authored_parameter(
        &mut self,
        name: &str,
    ) -> Result<ResolvedParameter<u32>, ParameterErrorKind> {
        resolve_control_parameter(&self.instance.plugin.ports, name)
    }
    fn prepare_bound_value(
        &mut self,
        _: &u32,
        value: f64,
        domain: ParameterWriteDomain,
    ) -> Result<PreparedParameterValue, ParameterErrorKind> {
        // The shared preflight checks range and finite f32 representability.
        // Return the actual connected scalar value for a matching readout.
        Ok(PreparedParameterValue {
            value: f64::from(value as f32),
            domain,
        })
    }
    fn write_bound_parameter(
        &mut self,
        key: &u32,
        value: f64,
        _: ParameterWriteDomain,
    ) -> Result<(), ParameterErrorKind> {
        let parameter = self
            .instance
            .params
            .iter_mut()
            .find(|slot| slot.param.port == *key)
            .ok_or(ParameterErrorKind::InventoryChanged)?;
        let destination = self
            .instance
            .control_values
            .get_mut(parameter.slot)
            .ok_or(ParameterErrorKind::InventoryChanged)?;
        *destination = value as f32;
        parameter.param.current = value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metadata::{Port, PortDirection, PortKind};

    fn port(index: u32, name: &str, symbol: &str, direction: PortDirection) -> Port {
        Port {
            index,
            symbol: symbol.into(),
            name: name.into(),
            kind: PortKind::Control,
            direction,
            minimum: -2.0,
            maximum: 4.0,
            default: 1.0,
            designation: None,
            supports_midi: false,
            atom_buffer_type: None,
            unit: None,
            group: None,
            steps: None,
        }
    }

    #[test]
    fn lv2_authored_binding_uses_native_control_ports_and_all_existing_aliases() {
        let input = port(4, "Cutoff Hz", "cutoff", PortDirection::Input);
        for name in ["Cutoff Hz", "cutoff", "cutoff_Cutoff Hz", "CUTOFF-HZ"] {
            let binding = resolve_control_parameter(std::slice::from_ref(&input), name).unwrap();
            assert_eq!(binding.key, 4);
            assert!(binding.writable);
            let range = binding.plain_range.unwrap();
            assert_eq!((range.min, range.max), (-2.0, 4.0));
        }
        let output = port(9, "Meter", "meter", PortDirection::Output);
        assert!(
            !resolve_control_parameter(std::slice::from_ref(&output), "meter")
                .unwrap()
                .writable
        );
        let mut host_rate = input.clone();
        host_rate.designation = Some(LV2__SAMPLE_RATE.into());
        assert!(
            !resolve_control_parameter(&[host_rate], "cutoff")
                .unwrap()
                .writable
        );
        assert!(matches!(
            resolve_control_parameter(&[input], "missing"),
            Err(ParameterErrorKind::Unknown)
        ));
    }

    #[test]
    fn lv2_authored_binding_rejects_ambiguous_names_and_malformed_metadata() {
        let one = port(4, "Shared", "one", PortDirection::Input);
        let two = port(8, "Shared", "two", PortDirection::Input);
        assert!(matches!(
            resolve_control_parameter(&[one.clone(), two.clone()], "Shared"),
            Err(ParameterErrorKind::Ambiguous)
        ));
        assert_eq!(
            resolve_control_parameter(&[one.clone(), two], "one")
                .unwrap()
                .key,
            4
        );
        for (min, max, default) in [
            (f32::NAN, 4.0, 1.0),
            (-2.0, f32::INFINITY, 1.0),
            (4.0, 2.0, 3.0),
            (-2.0, 4.0, f32::NAN),
            (-2.0, 4.0, 5.0),
        ] {
            let mut invalid = one.clone();
            invalid.minimum = min;
            invalid.maximum = max;
            invalid.default = default;
            assert!(matches!(
                resolve_control_parameter(&[invalid], "one"),
                Err(ParameterErrorKind::InvalidMetadata)
            ));
        }
        let mut constant = one;
        constant.minimum = 2.0;
        constant.maximum = 2.0;
        constant.default = 2.0;
        assert!(resolve_control_parameter(&[constant], "one").is_ok());
    }
}
