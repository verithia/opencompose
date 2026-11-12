//! Wet/dry law probe: one rack module on the demo bass phrase, bass only.
//!
//! Run with:
//! `cargo test -p oc-engine --test all fx_law_probe:: -- --ignored --nocapture`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::{ParamValue, Project};
use oc_engine::render::render_with_stems;
use oc_synth::synth::document::PatchDocument;
use oc_synth::synth::{SynthFx, SynthFxType, SynthPatch, load_project_patch};

const RATE: u32 = 48_000;
fn demo_path() -> PathBuf {
    std::env::var_os("OCP_DEMO_PROJECT")
        .map(PathBuf::from)
        .expect("set OCP_DEMO_PROJECT to a demo .ocp")
}
const TEMP: &str = "/tmp/fx-neutral/law-patches";

fn project() -> Project {
    let demo = demo_path();
    let text = std::fs::read_to_string(&demo).expect("demo");
    oc_dsl::parse(&text).expect("parse")
}

fn bass_patch(project: &Project) -> SynthPatch {
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "demo_bass")
        .expect("demo_bass");
    let demo = demo_path();
    load_project_patch(&preset.params, Some(demo.parent().unwrap())).expect("patch")
}

fn render(project: &Project, patch: &SynthPatch, tag: &str) -> Vec<f32> {
    std::fs::create_dir_all(TEMP).expect("temp");
    let path = Path::new(TEMP).join(format!("{tag}.json"));
    let document = PatchDocument {
        version: 1,
        name: tag.into(),
        author: "OpenCompose".into(),
        category: "probe".into(),
        use_policy: "measurement".into(),
        patch: patch.clone(),
    };
    std::fs::write(&path, document.to_json().expect("json")).expect("write");

    let mut variant = project.clone();
    variant.tracks.retain(|track| track.name == "bass");
    let index = variant
        .presets
        .iter()
        .position(|preset| preset.name == "demo_bass")
        .expect("preset");
    let gain = variant.presets[index]
        .params
        .iter()
        .find(|(name, _)| name == "output.gain")
        .and_then(|(_, value)| value.as_f64())
        .expect("gain");
    variant.presets[index].params = vec![
        (
            "open_synth_state_file".into(),
            ParamValue::String(path.to_string_lossy().into_owned()),
        ),
        ("output.gain".into(), ParamValue::Number(gain)),
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

struct Level {
    peak: f64,
    rms: f64,
}

fn level(samples: &[f32], frames: usize) -> Level {
    let mut peak = 0.0_f64;
    let mut power = 0.0_f64;
    for frame in samples.chunks_exact(2).take(frames) {
        for sample in frame {
            let value = f64::from(*sample);
            peak = peak.max(value.abs());
            power += value * value;
        }
    }
    Level {
        peak,
        rms: (power / (frames * 2) as f64).sqrt(),
    }
}

fn synth_patch() -> SynthPatch {
    let project = project();
    let mut patch = bass_patch(&project);
    patch.fx.clear();
    patch.fx_mod_routes.clear();
    patch
}

#[test]
#[ignore = "measurement harness for docs/fx-laws.md"]
fn delay_and_distortion_wet_law() {
    let project = project();
    let base_patch = synth_patch();

    let delay = SynthFx {
        fx_type: SynthFxType::Delay,
        enabled: true,
        params: vec![
            ("time".into(), 0.0408),
            ("feedback".into(), 0.40),
            ("tone".into(), 0.4941),
            ("mix".into(), 0.0),
            ("level_out".into(), 1.0),
        ],
    };
    let distortion = SynthFx {
        fx_type: SynthFxType::Distortion,
        enabled: true,
        params: vec![
            ("drive".into(), 0.4868),
            ("tone".into(), 1.0),
            ("level".into(), 1.0),
            ("mix".into(), 0.0),
            ("level_out".into(), 1.0),
            ("bw".into(), 0.0),
            ("mode".into(), 0.0),
            ("stages".into(), 2.0),
        ],
    };

    for (name, module) in [("delay", delay), ("distortion", distortion)] {
        let mut rows = Vec::new();
        for wet in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let mut patch = base_patch.clone();
            let mut module = module.clone();
            module.set_param("mix", wet);
            patch.fx = vec![module];
            let samples = render(&project, &patch, &format!("{name}-{wet:.2}"));
            rows.push((wet, samples));
        }
        let frames = rows.iter().map(|(_, s)| s.len() / 2).min().unwrap();
        let dry = level(&rows[0].1, frames);
        println!("\n=== {name} ===");
        println!(
            "{:>5} {:>9} {:>9} {:>8} {:>8} {:>8}",
            "wet", "peak", "rms", "crest", "dPeak", "dRMS"
        );
        for (wet, samples) in &rows {
            let value = level(samples, frames);
            println!(
                "{wet:>5.2} {:>9.5} {:>9.6} {:>8.2} {:>+8.2} {:>+8.2}",
                value.peak,
                value.rms,
                20.0 * (value.peak / value.rms).log10(),
                20.0 * (value.peak / dry.peak).log10(),
                20.0 * (value.rms / dry.rms).log10()
            );
        }
    }
}

#[test]
#[ignore = "measurement harness for the voice laws"]
fn saw_edge_probe() {
    let project = project();
    let base = bass_patch(&project);
    let mut raw = base.clone();
    raw.fx.clear();
    raw.fx_mod_routes.clear();
    raw.filter_wet = 0.0;
    raw.filter2_wet = 0.0;
    raw.oscillator_b_level = 0.0;
    raw.oscillator_c_level = 0.0;
    raw.sub_level = 0.0;
    raw.noise_level = 0.0;
    raw.ring_mod_level = 0.0;
    raw.unison_voices = 1;
    raw.oscillator_b_unison = 1;
    let mut a = raw.clone();
    a.oscillator_a_warp_amount = 0.0;
    a.oscillator_a_warp2_amount = 0.0;
    let mut b = raw.clone();
    b.oscillator_a_level = 0.0;
    b.oscillator_b_level = base.oscillator_b_level;
    b.oscillator_b_warp_amount = 0.0;
    b.oscillator_b_warp2_amount = 0.0;
    let mut full_raw = raw.clone();
    full_raw.oscillator_a_level = base.oscillator_a_level;
    full_raw.oscillator_b_level = base.oscillator_b_level;
    full_raw.sub_level = base.sub_level;
    let mut filtered = base.clone();
    filtered.fx.clear();
    filtered.fx_mod_routes.clear();
    filtered.unison_voices = 1;
    filtered.oscillator_b_unison = 1;
    let mut filtered_random = filtered.clone();
    filtered_random.oscillator_random_phase = 1.0;

    for (name, patch) in [
        ("a_raw", a),
        ("b_raw", b),
        ("full_raw", full_raw),
        ("full_filter", filtered),
        ("full_filter_random", filtered_random),
    ] {
        let samples = render(&project, &patch, name);
        let frames = samples.len() / 2;
        let value = level(&samples, frames);
        println!(
            "\n{name}: peak {:.5} rms {:.5} crest {:.2} dB",
            value.peak,
            value.rms,
            20.0 * (value.peak / value.rms).log10()
        );
        let mono: Vec<f64> = samples
            .chunks_exact(2)
            .map(|frame| (f64::from(frame[0]) + f64::from(frame[1])) * 0.5)
            .collect();
        let start = 14_400;
        let len = 16_384;
        if mono.len() < start + len {
            continue;
        }
        let f0 = 65.406_391;
        let mut harmonics = Vec::new();
        for harmonic in 1..=24 {
            let omega = std::f64::consts::TAU * f0 * harmonic as f64 / 48_000.0;
            let (mut re, mut im) = (0.0, 0.0);
            for (index, sample) in mono[start..start + len].iter().enumerate() {
                let window =
                    0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (len - 1) as f64).cos();
                re += sample * window * (omega * index as f64).cos();
                im -= sample * window * (omega * index as f64).sin();
            }
            harmonics.push((re * re + im * im).sqrt());
        }
        let h1 = harmonics[0].max(1e-12);
        let table: Vec<String> = harmonics
            .iter()
            .enumerate()
            .map(|(index, value)| format!("{}:{:.1}", index + 1, 20.0 * (value / h1).log10()))
            .collect();
        println!("  harmonics dB re H1: {}", table.join(" "));
    }
}
