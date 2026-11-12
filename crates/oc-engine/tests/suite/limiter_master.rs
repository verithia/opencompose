//! Master-limiter measurement harness.
//!
//! Set `OCP_DEMO_PROJECT` to a project with a limiter on the master, then run:
//! `cargo test -p oc-engine --test all limiter_master:: -- --ignored --nocapture`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_dsp::limiter::{GainReductionSummary, Limiter};
use oc_engine::analysis::{self, LevelMetrics};
use oc_engine::dsp::AudioEffect;
use oc_engine::render::normalise_loudness;
use oc_engine::render_cache::RenderCache;

const RATE: u32 = 48_000;
fn demo_path() -> PathBuf {
    std::env::var_os("OCP_DEMO_PROJECT")
        .map(PathBuf::from)
        .expect("set OCP_DEMO_PROJECT to a demo .ocp")
}

fn demo_project() -> Project {
    let path = demo_path();
    let text = std::fs::read_to_string(&path).expect("demo project");
    oc_dsl::parse(&text).expect("demo parses")
}

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(demo_path().parent().unwrap().to_path_buf()));
    let mut plugins = HashMap::new();
    cache
        .render_full(
            project,
            Path::new("/nonexistent/soundfont.sf2"),
            RATE,
            &mut plugins,
        )
        .expect("render")
}

fn level(samples: &[f32]) -> LevelMetrics {
    analysis::measure_stereo_interleaved(samples, RATE)
}

fn true_peak(samples: &[f32]) -> f64 {
    analysis::true_peak_dbfs(samples)
}

fn sample_peak(samples: &[f32]) -> f64 {
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    20.0 * f64::from(peak.max(1e-12)).log10()
}

fn gr_line(name: &str, summary: &GainReductionSummary) {
    println!(
        "{name} GR: max {:.2} dB, mean {:.3} dB, p50 {:.1}, p95 {:.1}, p99 {:.1}, limited {:.1} %",
        summary.max_db,
        summary.mean_db,
        summary.p50_db,
        summary.p95_db,
        summary.p99_db,
        summary.limited_fraction * 100.0
    );
}

fn level_line(name: &str, samples: &[f32]) {
    let level = level(samples);
    println!(
        "{name}: integrated {} LUFS, short-term max {} LUFS, true peak {:.2} dBTP, sample peak {:.2} dBFS",
        level
            .integrated_lufs
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "n/a".to_string()),
        level
            .short_term_max_lufs
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "n/a".to_string()),
        true_peak(samples),
        sample_peak(samples)
    );
}

fn demo_limiter() -> Limiter {
    let mut limiter = Limiter::new(RATE);
    limiter.set_param("ceiling", -0.8);
    limiter
}

fn sine(frequency: f64, amplitude: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|index| {
            let value = (amplitude
                * (2.0 * std::f64::consts::PI * frequency * index as f64 / f64::from(RATE)).sin())
                as f32;
            [value, value]
        })
        .collect()
}

/// THD estimate: project onto the fundamental and report the residual.
fn thd_db(samples: &[f32], frequency: f64) -> f64 {
    let frames = samples.len() / 2;
    let omega = 2.0 * std::f64::consts::PI * frequency / f64::from(RATE);
    let (mut cos_sum, mut sin_sum, mut power) = (0.0_f64, 0.0_f64, 0.0_f64);
    for frame in 0..frames {
        let value = f64::from(samples[frame * 2]);
        cos_sum += value * (omega * frame as f64).cos();
        sin_sum += value * (omega * frame as f64).sin();
        power += value * value;
    }
    let n = frames as f64;
    let a = 2.0 * cos_sum / n;
    let b = 2.0 * sin_sum / n;
    let fundamental = (a * a + b * b) / 2.0;
    let total = power / n;
    let residual = (total - fundamental).max(0.0);
    10.0 * (residual.max(1e-24) / fundamental.max(1e-24)).log10()
}

fn pumping_ripple_db(samples: &[f32]) -> f64 {
    let frames = samples.len() / 2;
    let block = 480;
    let mut levels = Vec::new();
    let mut start = frames / 2;
    while start + block <= frames {
        let mut power = 0.0_f64;
        for frame in start..start + block {
            let value = f64::from(samples[frame * 2]);
            power += value * value;
        }
        levels.push(10.0 * (power / block as f64).max(1e-24).log10());
        start += block;
    }
    let low = levels.iter().copied().fold(f64::INFINITY, f64::min);
    let high = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    high - low
}

#[test]
#[ignore = "measurement harness for docs/limiter.md"]
fn limiter_master_measurement() {
    let demo = demo_project();
    let mut dry_project = demo.clone();
    dry_project.master.as_mut().expect("master").effects.clear();
    let dry = render(&dry_project);
    let raw = render(&demo);

    println!("=== demo master ===");
    level_line("dry", &dry);
    level_line("raw", &raw);
    println!(
        "raw frames {} ({:.3} s), dry frames {} ({:.3} s)",
        raw.len() / 2,
        raw.len() as f64 / 2.0 / f64::from(RATE),
        dry.len() / 2,
        dry.len() as f64 / 2.0 / f64::from(RATE)
    );

    // Reconstruct the master limiter from the pre-master mix: the demo's
    // master chain contains exactly one limiter. Offline render compensates
    // the lookahead, so compare against the delayed reconstruction.
    let mut limiter = demo_limiter();
    let latency_samples = limiter.latency_samples() as usize * 2;
    let mut reconstructed = dry.clone();
    reconstructed.resize(dry.len() + latency_samples, 0.0);
    limiter.process(&mut reconstructed, RATE);
    let mut max_diff = 0.0_f32;
    for (index, sample) in raw.iter().enumerate() {
        max_diff = max_diff.max((sample - reconstructed[index + latency_samples]).abs());
    }
    println!(
        "reconstruction vs render: max |diff| {:.3e} after {} frames of PDC, raw {} frames",
        max_diff,
        limiter.latency_samples(),
        raw.len() / 2
    );
    gr_line("demo limiter", &limiter.gain_reduction_summary());

    // The demo sits 7.4 dB under the ceiling, so drive the same program into
    // the limiter to measure a real gain-reduction distribution.
    let mut hot_program = dry.clone();
    let hot_gain = 10.0_f64.powf(12.0 / 20.0) as f32;
    for sample in &mut hot_program {
        *sample *= hot_gain;
    }
    let mut hot_limiter = demo_limiter();
    hot_limiter.process(&mut hot_program, RATE);
    gr_line(
        "demo +12 dB into limiter",
        &hot_limiter.gain_reduction_summary(),
    );
    level_line("hot programme output", &hot_program);

    let mut normalised = raw.clone();
    let mut norm_project = demo.clone();
    norm_project.master.as_mut().unwrap().loudness_target = Some(-14.0);
    let report = normalise_loudness(&norm_project, &mut normalised, RATE)
        .expect("normalise")
        .expect("loudness target");
    println!("=== -14 LUFS normalisation ===");
    level_line("normalised", &normalised);
    println!(
        "target {:.2} LUFS, achieved {:.2} LUFS, applied gain {:+.2} dB, passes {}, shortfall {:+.2} LU",
        report.target_lufs,
        report.achieved_lufs,
        report.applied_gain_db,
        report.passes,
        report.shortfall_lu
    );
    println!(
        "normalisation limiter: max {:.2} dB, mean {:.3} dB, p50 {:.1}, p95 {:.1}, p99 {:.1}, limited {:.1} %",
        report.limiter_gain_reduction_max_db,
        report.limiter_gain_reduction_mean_db,
        report.limiter_gain_reduction_p50_db,
        report.limiter_gain_reduction_p95_db,
        report.limiter_gain_reduction_p99_db,
        report.limiter_limited_fraction * 100.0
    );
    println!(
        "normalisation input true peak {:.2} dBTP, output {:.2} dBTP",
        report.input_true_peak_dbfs, report.true_peak_dbfs
    );

    println!("=== artifacts ===");
    let quiet = sine(1_000.0, 0.4, 48_000);
    let quiet_input = quiet.clone();
    let mut quiet_limiter = demo_limiter();
    let mut quiet_shifted = quiet.clone();
    quiet_limiter.process(&mut quiet_shifted, RATE);
    let latency = quiet_limiter.latency_samples() as usize * 2;
    let mut quiet_diff = 0.0_f32;
    for index in 0..quiet.len() - latency {
        quiet_diff = quiet_diff.max((quiet_shifted[index + latency] - quiet_input[index]).abs());
    }
    println!(
        "below-ceiling 1 kHz sine: max |diff| {:.3e} (bit-transparent)",
        quiet_diff
    );

    let mut hot = sine(1_000.0, 4.0, 48_000);
    let mut hot_limiter = demo_limiter();
    hot_limiter.process(&mut hot, RATE);
    println!(
        "limited 1 kHz sine (+13 dB over): THD {:.1} dB, pumping ripple {:.4} dB, output true peak {:.2} dBTP",
        thd_db(&hot[hot_limiter.latency_samples() as usize * 2..], 1_000.0),
        pumping_ripple_db(&hot),
        true_peak(&hot)
    );

    let modest_amplitude = 0.912 * 10.0_f64.powf(3.0 / 20.0);
    let mut modest = sine(1_000.0, modest_amplitude, 48_000);
    let mut modest_limiter = demo_limiter();
    modest_limiter.process(&mut modest, RATE);
    println!(
        "limited 1 kHz sine (+3 dB over): THD {:.1} dB, pumping ripple {:.4} dB, output true peak {:.2} dBTP",
        thd_db(
            &modest[modest_limiter.latency_samples() as usize * 2..],
            1_000.0
        ),
        pumping_ripple_db(&modest),
        true_peak(&modest)
    );

    let mut tone = sine(1_000.0, 2.0, RATE as usize * 2);
    let mut tone_limiter = demo_limiter();
    tone_limiter.process(&mut tone, RATE);
    println!(
        "steady 1 kHz tone: pumping ripple {:.4} dB",
        pumping_ripple_db(&tone)
    );
    println!(
        "demo frames without gain reduction: {:.1} %",
        (1.0 - limiter.gain_reduction_summary().limited_fraction) * 100.0
    );
}
