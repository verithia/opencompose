//! End-to-end render checks for the split-rack topology.

#[cfg(feature = "plugin-host")]
use std::path::Path;

#[cfg(feature = "plugin-host")]
use oc_engine::render::render_to_buffer_per_track;

fn project(fx: &str, note: &str) -> String {
    format!(
        r#"@ocp "0.2.0"

@project "Split rack render"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {{osc.a.level: 0.8, osc.b.level: 0, amp.attack: 0.004s, amp.decay: 0.08s, amp.sustain: 0.9, amp.release: 0.1s, output.gain: 0.5, {fx}}}

@track tone | preset: tone, instrument: synth/virtual_analog
  mix: vol=0dB
  @phrase note [1:1:0 - 2:1:0]
    {note} w f
"#
    )
}

#[cfg(feature = "plugin-host")]
fn render(fx: &str, note: &str) -> Vec<f32> {
    let text = project(fx, note);
    let project = oc_dsl::parse(&text).expect("project should parse");
    oc_dsl::validate(&project).expect("project should validate");
    render_to_buffer_per_track(&project, Path::new("unused.sf2"), 48_000)
        .expect("render should succeed")
}

#[cfg(feature = "plugin-host")]
fn rms(audio: &[f32]) -> f32 {
    let energy: f64 = audio.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (energy / audio.len() as f64).sqrt() as f32
}

#[test]
#[cfg(feature = "plugin-host")]
fn unity_split_branches_match_the_serial_signal() {
    let split = r#"fx.count: 1, fx.1.type: "split", fx.1.freq: 500, fx.1.count1: 0, fx.1.count2: 0, fx.1.gain1: 1, fx.1.gain2: 1"#;
    let wet = render(split, "C3");
    let dry = render("fx.count: 0", "C3");
    assert!(wet.iter().all(|sample| sample.is_finite()));
    let wet_rms = rms(&wet[4_800..]);
    let dry_rms = rms(&dry[4_800..]);
    assert!(dry_rms > 0.001, "dry render is silent");
    let delta_db = 20.0 * (wet_rms / dry_rms).log10();
    assert!(
        delta_db.abs() < 0.2,
        "unity split branches drifted by {delta_db:+.2} dB"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn muted_split_branch_removes_only_its_band() {
    let all = r#"fx.count: 1, fx.1.type: "split", fx.1.freq: 500, fx.1.count1: 0, fx.1.count2: 0, fx.1.gain1: 1, fx.1.gain2: 1"#;
    let low_only = r#"fx.count: 1, fx.1.type: "split", fx.1.freq: 500, fx.1.count1: 0, fx.1.count2: 0, fx.1.gain1: 1, fx.1.gain2: 0"#;
    let all = render(all, "B6");
    let low_only = render(low_only, "B6");
    let all_rms = rms(&all[4_800..]);
    let low_rms = rms(&low_only[4_800..]);
    assert!(all_rms > 0.001, "split render is silent");
    assert!(
        low_rms < all_rms * 0.25,
        "muted high band kept {:.2} of the signal",
        low_rms / all_rms
    );
}

#[test]
fn split_effects_round_trip_through_the_dsl() {
    let split = r#"fx.count: 2, fx.1.type: "split", fx.1.freq: 500, fx.1.count1: 1, fx.1.count2: 0, fx.1.gain2: 0.5, fx.2.type: "filter", fx.2.cutoff: 4000, fx.2.mix: 1"#;
    let parsed = oc_dsl::parse(&project(split, "C3")).expect("project should parse");
    let formatted = oc_dsl::serialize(&parsed);
    let reparsed = oc_dsl::parse(&formatted).expect("formatted project should parse");
    let preset = reparsed
        .presets
        .iter()
        .find(|preset| preset.name == "tone")
        .expect("preset should survive");
    let param = |key: &str| {
        preset
            .params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        param("fx.1.type"),
        Some(oc_dsl::ast::ParamValue::String("split".into()))
    );
    assert_eq!(
        param("fx.1.count2"),
        Some(oc_dsl::ast::ParamValue::Number(0.0))
    );
    assert_eq!(
        param("fx.1.gain2"),
        Some(oc_dsl::ast::ParamValue::Number(0.5))
    );
    assert_eq!(
        param("fx.2.type"),
        Some(oc_dsl::ast::ParamValue::String("filter".into()))
    );
}
