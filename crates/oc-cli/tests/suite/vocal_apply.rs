//! `ocp vocal apply` end to end: resolve, bake the amount=0 pass-through,
//! rewrite the project and refuse a non-zero amount instead of ignoring it.

use std::path::{Path, PathBuf};
use std::process::Output;

use sha2::{Digest, Sha256};

const RATE: u32 = 48_000;

/// The bake directory version, from the engine's own constants so a bump does not
/// have to be chased through this file.
fn bake_version() -> String {
    format!(
        "{}.{}",
        oc_engine::vocal_tune::BAKE_VERSION,
        oc_dsp::vocal_shift::ALGO_VERSION
    )
}

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-apply", name)
}

fn write_take(path: &Path) {
    let frames = RATE as usize * 2;
    let mut samples = Vec::with_capacity(frames * 2);
    let mut phase: f64 = 0.0;
    for index in 0..frames {
        let time = index as f64 / f64::from(RATE);
        let (frequency, amplitude) = if (0.2..1.0).contains(&time) {
            (220.0, 0.3)
        } else if (1.2..2.0).contains(&time) {
            (330.0, 0.3)
        } else {
            (0.0, 0.0)
        };
        phase += std::f64::consts::TAU * frequency / f64::from(RATE);
        let value = (amplitude * phase.sin()) as f32;
        samples.push(value);
        samples.push(value);
    }
    oc_engine::wav::write_stereo_wav(path, &samples, RATE).unwrap();
}

fn run(dir: &Path, args: &[&str]) -> Output {
    crate::ocp()
        .args(args)
        .current_dir(dir)
        .env("OCP_MEDIA_CACHE_DIR", dir.join("media"))
        .env("OCP_DERIVED_CACHE_DIR", dir.join("derived"))
        .output()
        .unwrap()
}

fn run_ok(dir: &Path, args: &[&str]) -> Output {
    let result = run(dir, args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

fn project_text(block: &str, clip_file: &str) -> String {
    format!(
        "@project \"Vocal apply\"\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"{clip_file}\"\n{block}@track vocal | instrument: audio/clip\n  mix: vol=0dB\n"
    )
}

fn tune_block(amount: &str, notes: &str) -> String {
    format!("@vocal_tune lead_tune | clip: lead\n  amount: {amount}\n  notes:\n{notes}")
}

/// Run `vocal analyze --json` and return the detected note onsets.
fn detected_onsets(dir: &Path, project: &Path) -> Vec<f64> {
    let output = run_ok(
        dir,
        &[
            "vocal",
            "analyze",
            project.to_str().unwrap(),
            "lead",
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    report["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["start_seconds"].as_f64().unwrap())
        .collect()
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    format!("{:x}", Sha256::digest(&bytes))
}

#[test]
fn apply_bakes_a_sample_exact_pass_through_and_rewrites_the_project() {
    let dir = temp_dir("pass-through");
    write_take(&dir.join("take.wav"));
    let base = project_text("", "take.wav");
    let base_path = dir.join("base.ocp");
    std::fs::write(&base_path, &base).unwrap();
    let onsets = detected_onsets(&dir, &base_path);
    assert!(onsets.len() >= 2, "detected {onsets:?}");

    let notes = format!(
        "    - at={:.3}s tgt=A4\n    - at={:.3}s tgt=+2st\n",
        onsets[0], onsets[1]
    );
    let source = dir.join("song.ocp");
    std::fs::write(&source, project_text(&tune_block("0%", &notes), "take.wav")).unwrap();

    let out = dir.join("out.ocp");
    let applied = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["clip"], "lead");
    assert_eq!(report["tune"], "lead_tune");
    assert_eq!(report["pass_through"], true);
    assert_eq!(report["shifted_notes"], 0);
    assert_eq!(
        report["analyzer_version"],
        oc_dsp::vocal_analysis::ANALYZER_VERSION
    );
    assert_eq!(report["bake_version"], bake_version());
    assert_eq!(report["notes"].as_array().unwrap().len(), 2);
    let baked = &report["baked"];
    let baked_path = PathBuf::from(baked["path"].as_str().unwrap());
    assert!(baked_path.exists());
    assert_eq!(baked["cache"], "miss");
    let baked_file = baked["file"].as_str().unwrap();
    assert!(
        baked_file.starts_with(&format!("out.vocal/v{}/lead-", bake_version())),
        "{baked_file}"
    );

    // The rewritten project drops the decisions and reads the bake.
    let rewritten = std::fs::read_to_string(&out).unwrap();
    assert!(!rewritten.contains("@vocal_tune"), "{rewritten}");
    let project = oc_dsl::parse(&rewritten).unwrap();
    oc_dsl::validate(&project).unwrap();
    assert_eq!(project.audio_clip(0).unwrap().file, baked_file);
    assert_eq!(project.audio_clip(0).unwrap().offset_seconds, 0.0);
    assert_eq!(project.audio_clip(0).unwrap().stretch, 1.0);

    // Every baked sample equals the analysed playback window, bit for bit.
    let decoded = oc_engine::audio_clip::decode(&baked_path).unwrap();
    let source_project = oc_dsl::parse(&std::fs::read_to_string(&source).unwrap()).unwrap();
    let clip = oc_engine::vocal_analysis::resolve_clip(&source_project, "lead").unwrap();
    let (_, window) =
        oc_engine::vocal_analysis::clip_stereo_playback(&source_project, clip, RATE, Some(&dir))
            .unwrap();
    assert_eq!(decoded.samples.len(), window.len());
    for (index, (left, right)) in decoded.samples.iter().zip(&window).enumerate() {
        assert_eq!(left.to_bits(), right.to_bits(), "sample {index}");
    }

    // Rendering both projects is byte-identical end to end.
    let source_wav = dir.join("source.wav");
    let baked_wav = dir.join("baked.wav");
    for (project, wav) in [(&source, &source_wav), (&out, &baked_wav)] {
        run_ok(
            &dir,
            &[
                "render",
                project.to_str().unwrap(),
                "--output",
                wav.to_str().unwrap(),
                "--sample-rate",
                "48000",
            ],
        );
    }
    let source_bytes = std::fs::read(&source_wav).unwrap();
    let baked_bytes = std::fs::read(&baked_wav).unwrap();
    assert_eq!(source_bytes, baked_bytes, "amount=0 render moved");
    assert_eq!(sha256(&source_wav), sha256(&baked_wav));

    // Re-running the same decisions hits the bake cache.
    let again = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let again: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["baked"]["cache"], "hit");
    assert_eq!(again["baked"]["path"], baked["path"]);

    std::fs::remove_dir_all(dir).unwrap();
}

/// 32-bit float stereo WAV, so the reference keeps the shifter's exact samples.
fn write_float_wav(path: &Path, samples: &[f32], rate: u32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

#[test]
fn apply_then_render_matches_a_manually_shifted_reference() {
    let dir = temp_dir("shift");
    write_take(&dir.join("take.wav"));
    let base_path = dir.join("base.ocp");
    std::fs::write(&base_path, project_text("", "take.wav")).unwrap();
    let onsets = detected_onsets(&dir, &base_path);
    let source = dir.join("song.ocp");
    std::fs::write(
        &source,
        project_text(
            &tune_block("100%", &format!("    - at={:.3}s tgt=+100ct\n", onsets[0])),
            "take.wav",
        ),
    )
    .unwrap();
    let out = dir.join("out.ocp");
    let applied = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["pass_through"], false);
    assert_eq!(report["shifted_notes"], 1);
    assert_eq!(report["bake_version"], bake_version());
    let shift = &report["shift"];
    assert!(shift["wet_frames"].as_u64().unwrap() > 0);
    assert!(
        shift["boundary_step_ratio"].as_f64().unwrap() <= 1.5,
        "{}",
        shift["boundary_step_ratio"]
    );
    let bake_path = PathBuf::from(report["baked"]["path"].as_str().unwrap());

    // Manual reference, following the documented recipe: the analysed window,
    // the detected span of the matched note, the resolved cents and the block
    // retune as the glide.
    let source_project = oc_dsl::parse(&std::fs::read_to_string(&source).unwrap()).unwrap();
    let clip = oc_engine::vocal_analysis::resolve_clip(&source_project, "lead").unwrap();
    let (_, window) =
        oc_engine::vocal_analysis::clip_stereo_playback(&source_project, clip, RATE, Some(&dir))
            .unwrap();
    let analysis = oc_engine::vocal_analysis::analyze_clip(
        &source_project,
        clip,
        RATE,
        Some(&dir),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        Some(&dir.join("reference.ocn")),
    )
    .unwrap();
    let note = &analysis.analysis.notes[0];
    let frames = window.len() / 2;
    let cents = report["notes"][0]["shift_cents"].as_f64().unwrap() as f32;
    assert!((cents - 100.0).abs() < 1e-3, "{cents}");
    let segments = [oc_dsp::vocal_shift::ShiftSegment::new(
        (note.start_s * f64::from(RATE)).round() as usize,
        ((note.end_s * f64::from(RATE)).round() as usize).min(frames),
        cents,
    )];
    let mut config = oc_dsp::vocal_shift::ShiftConfig::new(RATE);
    config.ramp_frames = (0.025 * f64::from(RATE)).round() as usize;
    let reference = oc_dsp::vocal_shift::shift(
        &window,
        2,
        RATE,
        &oc_dsp::vocal_shift::ShiftCurve::from_segments(frames, &segments).unwrap(),
        &config,
    )
    .unwrap();

    // The bake is that reference, sample for sample.
    let decoded = oc_engine::audio_clip::decode(&bake_path).unwrap();
    assert_eq!(decoded.samples.len(), reference.samples.len());
    for (index, (left, right)) in decoded.samples.iter().zip(&reference.samples).enumerate() {
        assert_eq!(left.to_bits(), right.to_bits(), "baked sample {index}");
    }

    // Rendering the applied project equals rendering a project that reads the
    // reference WAV, byte for byte.
    let reference_wav = dir.join("reference.wav");
    write_float_wav(&reference_wav, &reference.samples, RATE);
    let reference_project = dir.join("reference.ocp");
    std::fs::write(&reference_project, project_text("", "reference.wav")).unwrap();
    let applied_wav = dir.join("applied.wav");
    let reference_render = dir.join("reference-render.wav");
    for (project, wav) in [
        (&out, &applied_wav),
        (&reference_project, &reference_render),
    ] {
        run_ok(
            &dir,
            &[
                "render",
                project.to_str().unwrap(),
                "--output",
                wav.to_str().unwrap(),
                "--sample-rate",
                "48000",
            ],
        );
    }
    assert_eq!(
        std::fs::read(&applied_wav).unwrap(),
        std::fs::read(&reference_render).unwrap(),
        "rendered tuning moved away from the reference"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Run `ocp vocal analyze --json` and return the detected note onsets.
#[test]
fn apply_reports_a_timing_edit_and_renders_it() {
    let dir = temp_dir("timing");
    write_take(&dir.join("take.wav"));
    let base_path = dir.join("base.ocp");
    std::fs::write(&base_path, project_text("", "take.wav")).unwrap();
    let onsets = detected_onsets(&dir, &base_path);
    assert!(onsets.len() >= 2, "detected {onsets:?}");
    let source = dir.join("song.ocp");
    std::fs::write(
        &source,
        project_text(
            &tune_block(
                "100%",
                &format!("    - at={:.3}s start=30ms length=1.05\n", onsets[0]),
            ),
            "take.wav",
        ),
    )
    .unwrap();
    let out = dir.join("out.ocp");
    let applied = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["bake_version"], bake_version());
    assert_eq!(report["pass_through"], false);
    assert_eq!(report["time"]["notes"], 1);
    assert!(report["time"]["boundary_step_ratio"].as_f64().unwrap() <= 1.5);
    let timing = &report["notes"][0]["timing"];
    assert!(timing.is_object(), "{timing}");
    let requested_move = timing["requested_move_ms"].as_f64().unwrap();
    let applied_move = timing["applied_move_ms"].as_f64().unwrap();
    let residual = timing["onset_residual_ms"].as_f64().unwrap();
    let length_error = timing["length_error_ms"].as_f64().unwrap();
    let audible_error = timing["audible_length_error_ms"].as_f64().unwrap();
    println!(
        "requested {requested_move:+.2} ms, applied {applied_move:+.2} ms, residual {residual:+.2} ms, length error {length_error:+.2} ms, audible {audible_error:+.2} ms"
    );
    assert!((requested_move - 30.0).abs() < 0.01, "{requested_move}");
    assert!((applied_move - 30.0).abs() < 0.01, "{applied_move}");
    assert!(residual.abs() <= 5.0, "{residual}");
    assert!(length_error.abs() <= 3.0, "{length_error}");
    assert!(audible_error.abs() <= 3.0, "{audible_error}");

    // Table mode prints the same numbers.
    let table = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--dry-run",
        ],
    );
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.contains("timing edits:"), "{text}");
    assert!(text.contains("+30.0ms"), "{text}");

    // Rendering the tuned project equals rendering the applied one.
    let source_wav = dir.join("timing-source.wav");
    let baked_wav = dir.join("timing-baked.wav");
    for (project, wav) in [(&source, &source_wav), (&out, &baked_wav)] {
        run_ok(
            &dir,
            &[
                "render",
                project.to_str().unwrap(),
                "--output",
                wav.to_str().unwrap(),
                "--sample-rate",
                "48000",
            ],
        );
    }
    assert_eq!(
        std::fs::read(&source_wav).unwrap(),
        std::fs::read(&baked_wav).unwrap(),
        "the render path ignored the timing edit"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_anchor_is_reported_and_still_bakes_the_pass_through() {
    let dir = temp_dir("stale");
    write_take(&dir.join("take.wav"));
    let source = dir.join("song.ocp");
    std::fs::write(
        &source,
        project_text(&tune_block("100%", "    - at=1.500s tgt=A4\n"), "take.wav"),
    )
    .unwrap();
    let out = dir.join("out.ocp");
    let result = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["pass_through"], true);
    assert_eq!(report["notes"][0]["outcome"], "stale-anchor");
    assert!(
        report["notes"][0]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("no longer matches a detected onset")
    );
    assert!(out.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dry_run_resolves_without_writing_and_in_place_replaces_the_project() {
    let dir = temp_dir("modes");
    write_take(&dir.join("take.wav"));
    let base_path = dir.join("base.ocp");
    std::fs::write(&base_path, project_text("", "take.wav")).unwrap();
    let onsets = detected_onsets(&dir, &base_path);
    let source = dir.join("song.ocp");
    let text = project_text(
        &tune_block("0%", &format!("    - at={:.3}s tgt=A4\n", onsets[0])),
        "take.wav",
    );
    std::fs::write(&source, &text).unwrap();
    let before: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let dry = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--dry-run",
            "--json",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(report["pass_through"], true);
    assert!(report["baked"].is_null());
    assert!(report["project_out"].is_null());
    let after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before.len(), after.len(), "dry run wrote files");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), text);

    let result = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--in-place",
            "--json",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        report["project_out"].as_str().unwrap(),
        source.display().to_string()
    );
    let rewritten = std::fs::read_to_string(&source).unwrap();
    assert!(!rewritten.contains("@vocal_tune"));
    assert!(source.with_file_name("song.vocal").is_dir());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_reports_missing_blocks_and_a_foreign_output_directory() {
    let dir = temp_dir("errors");
    write_take(&dir.join("take.wav"));
    let source = dir.join("song.ocp");
    std::fs::write(&source, project_text("", "take.wav")).unwrap();
    let result = run(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            dir.join("out.ocp").to_str().unwrap(),
        ],
    );
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("has no @vocal_tune block"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let elsewhere = temp_dir("errors-elsewhere");
    std::fs::write(&source, project_text(&tune_block("0%", ""), "take.wav")).unwrap();
    let result = run(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            elsewhere.join("out.ocp").to_str().unwrap(),
        ],
    );
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("must live next to the source project"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let neither = run(&dir, &["vocal", "apply", source.to_str().unwrap(), "lead"]);
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr).contains("choose --output <FILE>"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );

    // Relative paths inside the project directory are the normal spelling.
    let relative = run_ok(
        &dir,
        &[
            "vocal", "apply", "song.ocp", "lead", "--output", "rel.ocp", "--json",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&relative.stdout).unwrap();
    assert!(dir.join("rel.ocp").is_file());
    let baked = report["baked"]["path"].as_str().unwrap();
    assert!(
        baked.contains(&format!("/rel.vocal/v{}/lead-", bake_version())),
        "{baked}"
    );
    assert!(Path::new(baked).is_file());

    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(elsewhere).unwrap();
}

/// A 2 s take with one 1.4 s note 40 cents above A3 (220 Hz).
fn write_sharp_take(path: &Path) {
    let frames = RATE as usize * 2;
    let frequency = 220.0 * 2f64.powf(40.0 / 1_200.0);
    let mut samples = Vec::with_capacity(frames * 2);
    let mut phase = 0.0_f64;
    for index in 0..frames {
        let time = index as f64 / f64::from(RATE);
        let amplitude = if (0.2..1.6).contains(&time) { 0.3 } else { 0.0 };
        phase += std::f64::consts::TAU * frequency / f64::from(RATE);
        let value = (amplitude * phase.sin()) as f32;
        samples.push(value);
        samples.push(value);
    }
    oc_engine::wav::write_stereo_wav(path, &samples, RATE).unwrap();
}

/// Autocorrelation f0 around `expected`, independent of the engine detector.
fn measured_f0(samples: &[f32], start_s: f64, expected: f64) -> f64 {
    let rate = f64::from(RATE);
    let start = (start_s * rate) as usize * 2;
    let width = (rate * 2.0 / expected) as usize * 2;
    let nominal = rate / expected;
    let low = (nominal * 0.95).floor().max(2.0) as usize;
    let high = (nominal * 1.05).ceil() as usize;
    let mut best = (0.0_f64, 0.0_f64);
    for lag in low..=high {
        let mut ab = 0.0;
        let mut aa = 0.0;
        let mut bb = 0.0;
        for offset in 0..width {
            let a = f64::from(samples[start + offset]);
            let b = f64::from(samples[start + offset + lag * 2]);
            ab += a * b;
            aa += a * a;
            bb += b * b;
        }
        let score = ab / (aa * bb).sqrt().max(1e-18);
        if score > best.0 {
            best = (score, lag as f64);
        }
    }
    rate / best.1
}

#[test]
fn apply_snaps_a_scale_only_block_and_measures_within_five_cents() {
    let dir = temp_dir("scale-snap");
    write_sharp_take(&dir.join("take.wav"));
    let base_path = dir.join("base.ocp");
    std::fs::write(&base_path, project_text("", "take.wav")).unwrap();
    let base_wav = dir.join("base.wav");
    run_ok(
        &dir,
        &[
            "render",
            base_path.to_str().unwrap(),
            "--output",
            base_wav.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    let base_bytes = std::fs::read(&base_wav).unwrap();

    // `snap: off` with a scale and no stored targets is the old behaviour: the
    // render is byte-identical to the project without a block.
    let off_path = dir.join("off.ocp");
    std::fs::write(
        &off_path,
        project_text(
            "@vocal_tune lead_tune | clip: lead\n  snap: off\n  scale: C major\n",
            "take.wav",
        ),
    )
    .unwrap();
    let off_wav = dir.join("off.wav");
    run_ok(
        &dir,
        &[
            "render",
            off_path.to_str().unwrap(),
            "--output",
            off_wav.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(
        std::fs::read(&off_wav).unwrap(),
        base_bytes,
        "snap: off moved a render"
    );

    // The automatic block needs no notes: the scale alone is the proposal.
    let source = dir.join("song.ocp");
    std::fs::write(
        &source,
        project_text(
            "@vocal_tune lead_tune | clip: lead\n  scale: C major\n",
            "take.wav",
        ),
    )
    .unwrap();
    let out = dir.join("out.ocp");
    let applied = run_ok(
        &dir,
        &[
            "vocal",
            "apply",
            source.to_str().unwrap(),
            "lead",
            "--output",
            out.to_str().unwrap(),
            "--json",
            "--sample-rate",
            "48000",
        ],
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["snap_policy"], "scale");
    assert_eq!(report["notes"].as_array().unwrap().len(), 0);
    assert_eq!(report["shifted_notes"], 1);
    let proposals = report["proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0]["outcome"], "snapped");
    let shift = proposals[0]["shift_cents"].as_f64().unwrap();
    assert!(shift < -30.0 && shift > -50.0, "shift {shift}");

    // The baked take measures within 5 cents of A3 (220 Hz).
    let baked = PathBuf::from(report["baked"]["path"].as_str().unwrap());
    let decoded = oc_engine::audio_clip::decode(&baked).unwrap();
    let measured = measured_f0(&decoded.samples, 0.5, 220.0);
    let cents = 1_200.0 * (measured / 220.0).log2();
    println!("scale-only bake: {cents:+.3} cents from A3");
    assert!(cents.abs() <= 5.0, "{cents:+.3} cents");

    std::fs::remove_dir_all(dir).unwrap();
}
