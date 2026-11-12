//! Controls a device preset writes must survive the render path: the declared
//! unit reaches the DSP, and every post-EQ shape label the schema offers decodes.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast::Project;
use oc_engine::dsp::try_create_effect;
use oc_engine::render_cache::RenderCache;

const RATE: u32 = 48_000;

/// One track, one built-in tone, one effect line; a bar is two seconds.
fn project_with(effect: &str) -> Project {
    let source = format!(
        r#"@ocp "0.2.0"

@project "device preset controls"
  tempo: 120
  time_signature: 4/4

@preset probe
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}

@track tone | preset: probe, instrument: synth/lead_square
  effects:
    - {effect}
  @phrase phrase [1:1 - 1:2]
    C4 q mf
"#
    );
    let project = oc_dsl::parse(&source).expect("the probe project parses");
    oc_dsl::validate(&project).expect("the probe project validates");
    project
}

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache
        .render_full(
            project,
            Path::new("/nonexistent/device-preset-probe.sf2"),
            RATE,
            &mut HashMap::new(),
        )
        .expect("the project renders")
}

fn peak(audio: &[f32]) -> f32 {
    audio
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

fn effect_param(project: &Project, name: &str) -> f64 {
    let effect = &project.tracks[0].effects[0];
    let instance = try_create_effect(&effect.effect_type, &effect.params, RATE)
        .expect("the effect is created");
    instance
        .get_param(name)
        .expect("the device exposes the control")
}

#[test]
fn compression_stereo_link_at_hundred_renders_from_the_dsl() {
    // Both spellings the schema declares: the typed percentage and the number a
    // preset or the GUI writes. Both must render.
    for line in [
        "compression: threshold=-20dB, ratio=3:1, stereo_link=100%",
        "compression: threshold=-20dB, ratio=3:1, stereo_link=100",
    ] {
        let project = project_with(line);
        assert!(
            (effect_param(&project, "stereo_link") - 1.0).abs() < 1e-12,
            "{line} must reach the DSP as 1.0"
        );
        assert!(peak(&render(&project)) > 0.0, "{line} must produce audio");
    }
}

#[test]
fn reverb_fdn_low_shelf_post_eq_renders() {
    // The bank's small-room preset: a low shelf on the wet output, a label the
    // decoder only accepted after normalisation.
    let project = project_with(
        "reverb_fdn: space=30, mix=18, post_eq1_freq=120Hz, post_eq1_gain=-2.5dB, \
         post_eq1_q=0.5, post_eq1_shape=low_shelf",
    );
    assert!(
        (effect_param(&project, "post_eq1_shape") - 1.0).abs() < 1e-12,
        "low_shelf must decode to the low-shelf filter"
    );
    assert!(peak(&render(&project)) > 0.0);
}
