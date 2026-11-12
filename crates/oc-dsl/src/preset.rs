//! Project-local preset inheritance shared by rendering paths.
use crate::ast::{PresetDef, Project};

/// Resolved values and the original ancestor-to-child assignment layers.
/// The layers retain authored ordering across aliases; the resolved parameter
/// vector preserves exact-key in-place overlay semantics.
#[derive(Debug)]
pub struct PresetResolution<'a> {
    pub preset: PresetDef,
    pub layers: Vec<&'a PresetDef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionFailureKind {
    Unknown,
    Cycle,
}

/// The owner is the actual declaration whose extends edge failed. A request
/// for an absent root has no declaration from which to infer a source subject.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PresetResolutionError {
    kind: ResolutionFailureKind,
    name: String,
    reference_from: Option<usize>,
}
impl PresetResolutionError {
    pub(crate) fn parent_reference_owner(&self) -> Option<usize> {
        self.reference_from
    }
}
impl std::fmt::Display for PresetResolutionError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ResolutionFailureKind::Unknown => write!(output, "Unknown preset {}", self.name),
            ResolutionFailureKind::Cycle => {
                write!(output, "Preset inheritance cycle at {}", self.name)
            }
        }
    }
}

/// Resolve metadata and borrowed authored layers through one iterative walk.
pub fn resolve_preset_with_layers<'a>(
    definitions: &'a [PresetDef],
    name: &str,
) -> Result<PresetResolution<'a>, String> {
    resolve_preset_with_layers_checked(definitions, name).map_err(|error| error.to_string())
}

pub(crate) fn resolve_preset_with_layers_checked<'a>(
    definitions: &'a [PresetDef],
    name: &str,
) -> Result<PresetResolution<'a>, PresetResolutionError> {
    let mut layers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = name;
    let mut reference_from = None;
    loop {
        if !seen.insert(current) {
            return Err(PresetResolutionError {
                kind: ResolutionFailureKind::Cycle,
                name: current.to_owned(),
                reference_from,
            });
        }
        let (index, layer) = definitions
            .iter()
            .enumerate()
            .find(|(_, preset)| preset.name == current)
            .ok_or_else(|| PresetResolutionError {
                kind: ResolutionFailureKind::Unknown,
                name: current.to_owned(),
                reference_from,
            })?;
        layers.push(layer);
        match layer.extends.as_deref() {
            Some(parent) => {
                reference_from = Some(index);
                current = parent;
            }
            None => break,
        }
    }
    layers.reverse();
    let mut merged = layers[0].clone();
    for child in layers.iter().skip(1) {
        merged.name = child.name.clone();
        macro_rules! overlay { ($($field:ident),*) => { $(if child.$field.is_some() { merged.$field = child.$field.clone(); })* }; }
        overlay!(source, plugin, program, patch, file, root_note);
        for (name, value) in &child.params {
            if let Some((_, old)) = merged.params.iter_mut().find(|(key, _)| key == name) {
                *old = value.clone();
            } else {
                merged.params.push((name.clone(), value.clone()));
            }
        }
        merged.effects.extend(child.effects.clone());
        merged.extends = None;
    }
    Ok(PresetResolution {
        preset: merged,
        layers,
    })
}

/// Resolve one preset and its ancestors without cloning the project.
/// Resolve a preset and its ancestors into one definition.
pub fn resolve_preset(definitions: &[PresetDef], name: &str) -> Result<PresetDef, String> {
    resolve_preset_with_layers(definitions, name).map(|resolved| resolved.preset)
}

/// Resolve every preset reference and inherited layer.
pub fn resolve(project: &Project) -> Result<Project, String> {
    resolve_checked(project).map_err(|error| error.to_string())
}

pub(crate) fn resolve_checked(project: &Project) -> Result<Project, PresetResolutionError> {
    let mut resolved = project.clone();
    resolved.presets = project
        .presets
        .iter()
        .map(|preset| {
            resolve_preset_with_layers_checked(&project.presets, &preset.name)
                .map(|resolution| resolution.preset)
        })
        .collect::<Result<_, _>>()?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    #[test]
    fn preset_resolution_origin_tracks_actual_closing_and_dangling_edges() {
        use super::*;
        let base=crate::parse("@project\n@preset start\n  extends: a\n@preset a\n  extends: b\n@preset b\n  extends: a\n").unwrap();
        let cycle = resolve_checked(&base).unwrap_err();
        assert_eq!(cycle.kind, ResolutionFailureKind::Cycle);
        assert_eq!(cycle.parent_reference_owner(), Some(2));
        assert_eq!(cycle.to_string(), "Preset inheritance cycle at a");
        assert_eq!(resolve(&base).unwrap_err(), cycle.to_string());
        let mut dangling = base.clone();
        dangling.presets[2].extends = Some("absent".into());
        let error = resolve_checked(&dangling).unwrap_err();
        assert_eq!(error.kind, ResolutionFailureKind::Unknown);
        assert_eq!(error.parent_reference_owner(), Some(2));
        assert_eq!(error.to_string(), "Unknown preset absent");
        let absent = resolve_preset_with_layers_checked(&base.presets, "absent").unwrap_err();
        assert_eq!(absent.parent_reference_owner(), None);
        assert_eq!(
            resolve_preset_with_layers(&base.presets, "absent").unwrap_err(),
            absent.to_string()
        );
        let mut direct = base.clone();
        direct.presets[0].extends = Some("start".into());
        assert_eq!(
            resolve_checked(&direct)
                .unwrap_err()
                .parent_reference_owner(),
            Some(0)
        );
    }

    #[test]
    fn preset_resolution_origin_preserves_duplicate_first_match_and_empty_parent() {
        use super::*;
        let mut project =
            crate::parse("@project\n@preset same\n@preset same\n@preset child\n  extends: same\n")
                .unwrap();
        project.presets[0].extends = Some("".into());
        project.presets[1].extends = Some("different".into());
        let error = resolve_preset_with_layers_checked(&project.presets, "child").unwrap_err();
        assert_eq!(error.parent_reference_owner(), Some(0));
        assert_eq!(error.to_string(), "Unknown preset ");
        assert_eq!(
            resolve_preset(&project.presets, "child").unwrap_err(),
            error.to_string()
        );
        project.presets[0].extends = None;
        assert!(resolve_preset_with_layers_checked(&project.presets, "child").is_ok());
    }

    #[test]
    fn inheritance_overlays_values_and_appends_effects_without_mutation() {
        let p = crate::parse("@project\n@preset parent\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {output.gain: 0.5, osc.a.level: 1}\n  effects:\n    - gain: factor=0.5\n@preset child\n  extends: parent\n  params: {output.gain: 0.2}\n  effects:\n    - gain: factor=0.25\n").unwrap();
        let resolved = super::resolve(&p).unwrap();
        let child = &resolved.presets[1];
        assert_eq!(child.plugin.as_deref(), Some("builtin.open_synth"));
        assert_eq!(child.effects.len(), 2);
        assert_eq!(super::resolve_preset(&p.presets, "child").unwrap(), *child);
        assert_eq!(child.params[0].1, crate::ast::ParamValue::Number(0.2));
        assert_eq!(p.presets[1].effects.len(), 1);
        assert_eq!(super::resolve(&resolved).unwrap(), resolved);
        let mut cycle = p.clone();
        cycle.presets[0].extends = Some("child".into());
        assert!(super::resolve(&cycle).is_err());
        assert!(crate::validate(&cycle).is_err());
    }

    #[test]
    fn borrowed_inheritance_layers_keep_child_order_and_resolved_positions() {
        let project = crate::parse("@project\n@preset parent\n  source: clap\n  plugin: \"fixture\"\n  program: 25\n  params: {Gain: 0.1, gain_alias: 0.2}\n  effects:\n    - gain: factor=0.5\n@preset child\n  extends: parent\n  params: {Gain: 0.7}\n  effects:\n    - gain: factor=0.25\n").unwrap();
        let result = super::resolve_preset_with_layers(&project.presets, "child").unwrap();
        assert_eq!(
            result
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            ["parent", "child"]
        );
        assert!(std::ptr::eq(result.layers[0], &project.presets[0]));
        assert!(std::ptr::eq(result.layers[1], &project.presets[1]));
        assert_eq!(
            result
                .preset
                .params
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["Gain", "gain_alias"]
        );
        assert_eq!(
            result.preset.params[0].1,
            crate::ast::ParamValue::Number(0.7)
        );
        assert_eq!(result.preset.program, Some(25));
        assert_eq!(
            result.preset.effects[0].params[0].1,
            crate::ast::ParamValue::Number(0.5)
        );
        assert_eq!(
            result.preset.effects[1].params[0].1,
            crate::ast::ParamValue::Number(0.25)
        );
        assert_eq!(
            super::resolve_preset(&project.presets, "child").unwrap(),
            result.preset
        );
    }

    #[test]
    fn iterative_inheritance_preserves_missing_cycle_and_first_match_contracts() {
        let mut project = crate::parse("@project\n@preset first\n  extends: missing\n").unwrap();
        assert_eq!(
            super::resolve_preset_with_layers(&project.presets, "first").unwrap_err(),
            "Unknown preset missing"
        );
        assert_eq!(
            super::resolve_preset(&project.presets, "absent").unwrap_err(),
            "Unknown preset absent"
        );
        project.presets[0].extends = Some("first".into());
        assert_eq!(
            super::resolve_preset_with_layers(&project.presets, "first").unwrap_err(),
            "Preset inheritance cycle at first"
        );
        project.presets[0].extends = None;
        project.presets[0].program = Some(25);
        let mut duplicate = project.presets[0].clone();
        duplicate.program = Some(33);
        project.presets.push(duplicate);
        assert_eq!(
            super::resolve_preset(&project.presets, "first")
                .unwrap()
                .program,
            Some(25)
        );
    }

    #[test]
    fn iterative_inheritance_handles_a_deep_chain_without_recursive_frames() {
        let base = crate::parse(
            "@project\n@preset base\n  source: clap\n  plugin: \"fixture\"\n  program: 33\n",
        )
        .unwrap()
        .presets
        .remove(0);
        let mut definitions: Vec<_> = (0..4096)
            .map(|index| {
                let mut preset = base.clone();
                preset.name = format!("layer{index}");
                preset.extends = (index != 0).then(|| format!("layer{}", index - 1));
                preset
            })
            .collect();
        let resolved = super::resolve_preset_with_layers(&definitions, "layer4095").unwrap();
        assert_eq!(resolved.layers.len(), definitions.len());
        assert_eq!(resolved.layers[0].name, "layer0");
        assert_eq!(resolved.layers[4095].name, "layer4095");
        assert_eq!(resolved.preset.program, Some(33));
        assert!(resolved.preset.extends.is_none());
        drop(resolved);
        definitions[0].extends = Some("layer2048".into());
        let error =
            super::resolve_preset_with_layers_checked(&definitions, "layer4095").unwrap_err();
        assert_eq!(error.parent_reference_owner(), Some(0));
        assert_eq!(error.to_string(), "Preset inheritance cycle at layer2048");
    }
}
