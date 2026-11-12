//! Per-module FX level neutrality harness for `docs/fx-laws.md`.
//!
//! Isolates each module of the bass patch's rack on the demo phrase by
//! disabling the other modules in the imported patch, then renders stems.
//!
//! Run with:
//! `cargo test -p oc-engine --test all fx_neutrality:: -- --ignored --nocapture`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::{ParamValue, Project};
use oc_engine::render::render_with_stems;
use oc_synth::synth::document::PatchDocument;
use oc_synth::synth::{SynthFxType, SynthPatch, load_project_patch};

const RATE: u32 = 48_000;
fn demo_path() -> PathBuf {
    std::env::var_os("OCP_DEMO_PROJECT")
        .map(PathBuf::from)
        .expect("set OCP_DEMO_PROJECT to a demo .ocp")
}
const TEMP: &str = "/tmp/fx-neutral/patches";

fn demo_project() -> Project {
    let demo = demo_path();
    let text = std::fs::read_to_string(&demo).expect("demo project");
    oc_dsl::parse(&text).expect("demo parses")
}

fn bass_preset_index(project: &Project) -> usize {
    project
        .presets
        .iter()
        .position(|preset| preset.name == "demo_bass")
        .expect("demo_bass preset")
}

fn bass_patch(project: &Project) -> SynthPatch {
    let preset = &project.presets[bass_preset_index(project)];
    let demo = demo_path();
    load_project_patch(&preset.params, Some(demo.parent().unwrap())).expect("bass patch")
}

fn patch_file(patch: &SynthPatch, tag: &str) -> PathBuf {
    std::fs::create_dir_all(TEMP).expect("temp dir");
    let path = Path::new(TEMP).join(format!("{tag}.json"));
    let document = PatchDocument {
        version: 1,
        name: format!("fx-neutrality-{tag}"),
        author: "OpenCompose".into(),
        category: "probe".into(),
        use_policy: "measurement".into(),
        patch: patch.clone(),
    };
    std::fs::write(&path, document.to_json().expect("patch json")).expect("write patch");
    path
}

/// Render the demo with the bass preset replaced by `patch`, return the bass
/// stem after trimming its PDC lead-in.
fn render_bass(project: &Project, patch: &SynthPatch, tag: &str) -> Vec<f32> {
    let mut variant = project.clone();
    variant.tracks.retain(|track| track.name == "bass");
    let index = bass_preset_index(&variant);
    let path = patch_file(patch, tag);
    let output_gain = variant.presets[index]
        .params
        .iter()
        .find(|(name, _)| name == "output.gain")
        .and_then(|(_, value)| value.as_f64())
        .expect("output.gain");
    variant.presets[index].params = vec![
        (
            "open_synth_state_file".to_string(),
            ParamValue::String(path.to_string_lossy().into_owned()),
        ),
        ("output.gain".to_string(), ParamValue::Number(output_gain)),
    ];
    let mut plugins = HashMap::new();
    let export = render_with_stems(
        &variant,
        Path::new("/nonexistent/soundfont.sf2"),
        RATE,
        &mut plugins,
        Some(demo_path().parent().unwrap()),
        0.0,
        0.0,
    )
    .expect("render");
    let stem = export
        .stems
        .iter()
        .find(|stem| stem.name == "bass")
        .expect("bass stem");
    let first = stem
        .samples
        .chunks_exact(2)
        .position(|frame| frame[0] != 0.0 || frame[1] != 0.0)
        .unwrap_or(0);
    stem.samples[first * 2..].to_vec()
}

#[derive(Debug, Clone, Copy)]
struct Level {
    peak: f64,
    rms: f64,
}

impl Level {
    fn measure(samples: &[f32], frames: usize) -> Self {
        let mut peak = 0.0_f64;
        let mut power = 0.0_f64;
        for frame in samples.chunks_exact(2).take(frames) {
            for sample in frame {
                let value = f64::from(*sample);
                peak = peak.max(value.abs());
                power += value * value;
            }
        }
        let count = (frames * 2) as f64;
        Self {
            peak,
            rms: (power / count).sqrt(),
        }
    }

    fn crest_db(self) -> f64 {
        20.0 * (self.peak / self.rms).log10()
    }

    fn peak_db(self) -> f64 {
        20.0 * self.peak.max(1e-12).log10()
    }

    fn rms_db(self) -> f64 {
        20.0 * self.rms.max(1e-12).log10()
    }
}

fn label(fx_type: SynthFxType) -> &'static str {
    match fx_type {
        SynthFxType::Distortion => "distortion",
        SynthFxType::Delay => "delay",
        SynthFxType::Reverb => "reverb",
        SynthFxType::Flanger => "flanger",
        SynthFxType::Eq3 => "eq3",
        SynthFxType::Compressor => "compressor",
        SynthFxType::Multiband => "multiband",
        other => other.label(),
    }
}

fn enabled_set(patch: &SynthPatch, keep: &[usize]) -> SynthPatch {
    let mut variant = patch.clone();
    for (index, module) in variant.fx.iter_mut().enumerate() {
        module.enabled = keep.contains(&index);
    }
    variant
}

#[test]
#[ignore = "measurement harness for docs/fx-laws.md"]
fn fx_module_level_neutrality() {
    let project = demo_project();
    let patch = bass_patch(&project);
    println!("=== imported bass rack ===");
    for (index, module) in patch.fx.iter().enumerate() {
        let params: Vec<String> = module
            .params
            .iter()
            .map(|(name, value)| format!("{name}={value:.4}"))
            .collect();
        println!(
            "{index}: {:<11} enabled={} {}",
            label(module.fx_type),
            module.enabled,
            params.join(" ")
        );
    }
    for route in &patch.fx_mod_routes {
        println!(
            "route {:?} -> fx[{}] {:?} amount {:.4}",
            route.source, route.fx_index, route.parameter, route.amount
        );
    }

    let variants: Vec<(String, Vec<usize>)> = vec![
        ("off".into(), vec![]),
        ("distortion".into(), vec![0]),
        ("delay1".into(), vec![2]),
        ("reverb".into(), vec![3]),
        ("delay2".into(), vec![6]),
        ("flanger".into(), vec![5]),
        ("eq".into(), vec![1, 4]),
        ("delays".into(), vec![2, 6]),
        ("time_fx".into(), vec![2, 3, 5, 6]),
        ("all".into(), (0..patch.fx.len()).collect()),
    ];
    let mut rendered = Vec::new();
    for (tag, keep) in variants {
        let variant = enabled_set(&patch, &keep);
        let samples = render_bass(&project, &variant, &tag);
        rendered.push((tag, samples));
    }
    let frames = rendered
        .iter()
        .map(|(_, samples)| samples.len() / 2)
        .min()
        .expect("variants");
    let base = Level::measure(&rendered[0].1, frames);
    println!(
        "\n=== bass stem, {:.1} s window ===",
        frames as f64 / f64::from(RATE)
    );
    println!(
        "{:<11} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "variant", "peak", "rms", "crest", "dPeak", "dRMS"
    );
    for (tag, samples) in &rendered {
        let level = Level::measure(samples, frames);
        println!(
            "{:<11} {:>9.4} {:>9.4} {:>9.2} {:>+9.2} {:>+9.2}",
            tag,
            level.peak,
            level.rms,
            level.crest_db(),
            level.peak_db() - base.peak_db(),
            level.rms_db() - base.rms_db()
        );
    }
    println!(
        "\nfx-off: peak {:.4} ({:.2} dBFS) rms {:.5} ({:.2} dBFS) crest {:.2} dB",
        base.peak,
        base.peak_db(),
        base.rms,
        base.rms_db(),
        base.crest_db()
    );
}
