//! Duplicate must produce an independent patch: editing the copy leaves the
//! original track's rendered audio bit-identical, and moving the track to the
//! copy makes the edit audible.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast::{ParamValue, Project};
use oc_engine::analysis;
use oc_engine::render_cache::RenderCache;

const SOURCE: &str = r#"@ocp "0.2.0"

@project "Duplicate isolation"
  tempo: 120
  time_signature: 4/4

@preset pad
  source: builtin
  plugin: "builtin.open_synth"
  params: { osc.a.waveform: 1, osc.a.level: 0.7, filter.cutoff: 6000, filter.resonance: 0.2, amp.attack: 0.01, amp.decay: 0.3, amp.sustain: 0.8, amp.release: 0.3 }

@track lead | preset: pad, instrument: synth/lead_square
  @phrase lead [1:1 - 3:1]
    C4 q mf
    E4 q mf
    G4 q mf
    C5 q mf
"#;

const RATE: u32 = 48_000;

fn project() -> Project {
    oc_dsl::parse(SOURCE).expect("isolation fixture parses")
}

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(Path::new(".").to_path_buf()));
    let mut plugins = HashMap::new();
    cache
        .render_full(
            project,
            Path::new("/nonexistent/soundfont.sf2"),
            RATE,
            &mut plugins,
        )
        .expect("project renders")
}

fn set_param(project: &mut Project, preset: &str, key: &str, value: f64) {
    let preset = project
        .presets
        .iter_mut()
        .find(|entry| entry.name == preset)
        .expect("preset exists");
    if let Some(slot) = preset.params.iter_mut().find(|(name, _)| name == key) {
        slot.1 = ParamValue::Number(value);
    } else {
        preset
            .params
            .push((key.to_string(), ParamValue::Number(value)));
    }
}

#[test]
fn editing_a_duplicate_leaves_the_original_render_unchanged() {
    let mut project = project();
    let baseline = render(&project);

    let mut copy = project.presets[0].clone();
    copy.name = "pad copy".into();
    project.presets.push(copy);
    set_param(&mut project, "pad copy", "filter.cutoff", 400.0);
    set_param(&mut project, "pad copy", "osc.a.waveform", 2.0);

    let after_copy_edit = render(&project);
    assert_eq!(
        baseline, after_copy_edit,
        "editing the duplicate changed the original track's render"
    );

    // The copy really is different: pointing the track at it changes the sound.
    project.tracks[0].preset = Some("pad copy".into());
    let copy_render = render(&project);
    let baseline_level = analysis::measure_stereo_interleaved(&baseline, RATE);
    let copy_level = analysis::measure_stereo_interleaved(&copy_render, RATE);
    assert!(
        (baseline_level.rms_dbfs - copy_level.rms_dbfs).abs() > 0.5,
        "the duplicate edit was inaudible: {:.2} vs {:.2} dBFS",
        baseline_level.rms_dbfs,
        copy_level.rms_dbfs
    );
    assert_ne!(baseline, copy_render);
}
