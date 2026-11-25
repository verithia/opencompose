use oc_dsl::ast::Project;

/// Offline capture choices; these never change which graph nodes process audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StemCaptureSelection {
    pub tracks: bool,
    /// `None` captures every bus; an empty list captures no buses.
    pub buses: Option<Vec<String>>,
    pub pre_master: bool,
}

impl Default for StemCaptureSelection {
    fn default() -> Self {
        Self {
            tracks: true,
            buses: None,
            pre_master: false,
        }
    }
}

impl StemCaptureSelection {
    pub fn captures_bus(&self, name: &str) -> bool {
        self.buses
            .as_ref()
            .is_none_or(|buses| buses.iter().any(|bus| bus == name))
    }

    /// Validate names before constructing any native instrument or effect.
    pub fn validate(&self, project: &Project) -> Result<(), String> {
        if let Some(buses) = &self.buses {
            for name in buses {
                if !project.buses.iter().any(|bus| bus.name == *name) {
                    return Err(format!("stem bus '{name}' does not exist"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AudioGraph, StemCapture};
    use oc_dsl::ast::{Effect, EffectType, ParamValue};

    #[test]
    fn selective_stems_preserve_routed_sidechain_pcm_and_capture_configuration() {
        let mut project = oc_dsl::parse("@project\n@track lead | instrument: keys/piano\n  mix: output=first\n@track key | instrument: keys/piano\n@bus first\n  routes: [second]\n  effects:\n    - limiter: lookahead=2ms\n@bus second\n  routes: [master]\n@bus unselected\n  routes: [master]\n").unwrap();
        project.buses[1].effects.push(Effect {
            effect_type: EffectType::Compression,
            params: vec![
                ("threshold".into(), ParamValue::Db(-20.0)),
                ("ratio".into(), ParamValue::String("4:1".into())),
            ],
            bypass: false,
            sidechain_source: Some("key".into()),
        });
        for workers in [1, 4] {
            let mut full_graph =
                AudioGraph::try_from_project_with_source_latencies(&project, 8000, &[3, 17])
                    .unwrap();
            let mut selected_graph =
                AudioGraph::try_from_project_with_source_latencies(&project, 8000, &[3, 17])
                    .unwrap();
            let mut full = StemCapture::default();
            let selection = StemCaptureSelection {
                tracks: false,
                buses: Some(vec!["second".into()]),
                pre_master: true,
            };
            selection.validate(&project).unwrap();
            let mut selected = StemCapture {
                selection: selection.clone(),
                capture_master_inputs: false,
                ..Default::default()
            };
            for _ in 0..2 {
                let input = vec![
                    (0..1024)
                        .map(|i| ((i as f32) * 0.071).sin() * 0.3)
                        .collect::<Vec<_>>(),
                    vec![0.5; 1024],
                ];
                let expected = full_graph.mix_processed_with_workers(
                    &mut input.clone(),
                    None,
                    Some(&mut full),
                    &Default::default(),
                    workers,
                );
                let actual = selected_graph.mix_processed_with_workers(
                    &mut input.clone(),
                    None,
                    Some(&mut selected),
                    &Default::default(),
                    workers,
                );
                assert_eq!(actual, expected);
                assert!(actual.iter().any(|sample| sample.abs() > 0.001));
                assert!(selected.tracks.is_empty() && selected.master_inputs.is_empty());
                assert_eq!(selected.buses.len(), 1);
                assert_eq!(
                    selected.buses[0],
                    *full
                        .buses
                        .iter()
                        .find(|(name, _)| name == "second")
                        .unwrap()
                );
                assert_eq!(selected.pre_master, full.pre_master);
                assert_eq!(selected.selection, selection);
            }
        }
    }
}
