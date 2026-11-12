#![cfg(feature = "plugin-host")]
//! `@vocal_tune` resolution, the amount=0 pass-through and the render gate.
//!
//! Every signal is generated here; the only inputs are the project text and a
//! synthetic two-note take.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::vocal_analysis::{self, ClipAnalysis};
use oc_engine::vocal_tune::{self, NoteOutcome};
use oc_engine::{audio_clip, render, wav};

const RATE: u32 = 48_000;

use crate::cache_support;

fn init_cache() {
    cache_support::init();
}

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("opencompose-vocal-tune", name)
}

/// 2 s stereo take: 220 Hz from 0.2 s, 330 Hz from 1.2 s, silence between.
fn write_take(dir: &Path) {
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
    wav::write_stereo_wav(dir.join("take.wav"), &samples, RATE).unwrap();
}

fn project_text(block: &str) -> String {
    format!(
        "@project \"Vocal tune\"\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"take.wav\"\n{block}@track vocal | instrument: audio/clip\n  mix: vol=0dB\n"
    )
}

fn parse(text: &str) -> Project {
    let project = oc_dsl::parse(text).unwrap();
    oc_dsl::validate(&project).unwrap();
    project
}

fn analysis_of(project: &Project, root: &Path, output: &Path) -> ClipAnalysis {
    let clip = vocal_analysis::resolve_clip(project, "lead").unwrap();
    vocal_analysis::analyze_clip(
        project,
        clip,
        RATE,
        Some(root),
        &oc_dsp::vocal_analysis::DetectorParams::default(),
        Some(output),
    )
    .unwrap()
}

fn block(amount: &str, notes: &str) -> String {
    format!("@vocal_tune lead_tune | clip: lead\n  amount: {amount}\n  notes:\n{notes}")
}

fn render(project: &Project, root: &Path) -> Result<Vec<f32>, String> {
    render::render_to_buffer_with_plugins_at(
        project,
        Path::new(""),
        RATE,
        &mut HashMap::new(),
        Some(root),
    )
}

fn assert_same_samples(left: &[f32], right: &[f32], what: &str) {
    assert_eq!(left.len(), right.len(), "{what}: length");
    for (index, (a, b)) in left.iter().zip(right).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "{what}: frame {index}");
    }
}

#[test]
fn zero_amount_block_renders_byte_identically() {
    init_cache();
    let dir = temp_dir("render-identity");
    write_take(&dir);
    let without = parse(&project_text(""));
    let analysis = analysis_of(&without, &dir, &dir.join("probe.ocn"));
    assert!(
        analysis.analysis.notes.len() >= 2,
        "probe analysis found {} notes",
        analysis.analysis.notes.len()
    );
    let first = analysis.analysis.notes[0].start_s;
    let second = analysis.analysis.notes[1].start_s;
    let with_block = parse(&project_text(&block(
        "0%",
        &format!("    - at={first:.3}s tgt=A4\n    - at={second:.3}s tgt=+2st\n"),
    )));
    let left = render(&with_block, &dir).unwrap();
    let right = render(&without, &dir).unwrap();
    assert_same_samples(&left, &right, "amount=0 render");

    wav::write_stereo_wav(dir.join("with.wav"), &left, RATE).unwrap();
    wav::write_stereo_wav(dir.join("without.wav"), &right, RATE).unwrap();
    let with_bytes = std::fs::read(dir.join("with.wav")).unwrap();
    let without_bytes = std::fs::read(dir.join("without.wav")).unwrap();
    assert_eq!(with_bytes, without_bytes, "rendered WAV bytes moved");
    assert!(!with_bytes.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_non_zero_amount_is_baked_by_the_render_path() {
    init_cache();
    let dir = temp_dir("render-bake");
    write_take(&dir);
    let without = parse(&project_text(""));
    let analysis = analysis_of(&without, &dir, &dir.join("probe.ocn"));
    let first = analysis.analysis.notes[0].start_s;
    let project = parse(&project_text(&block(
        "100%",
        &format!("    - at={first:.3}s tgt=+100ct\n"),
    )));
    let tuned = render(&project, &dir).unwrap();
    assert!(tuned.iter().all(|sample| sample.is_finite()));
    let plain = render(&without, &dir).unwrap();
    assert!(
        tuned
            .iter()
            .zip(&plain)
            .any(|(left, right)| left.to_bits() != right.to_bits()),
        "a non-zero amount did not move the render"
    );

    // The same block with no shift keeps the render bit-identical.
    let zero = parse(&project_text(&block(
        "0%",
        &format!("    - at={first:.3}s tgt=+100ct\n"),
    )));
    assert_same_samples(&render(&zero, &dir).unwrap(), &plain, "amount=0 render");

    // The render-time bake is cached under the bake key, so a second render
    // reads the baked window instead of shifting again.
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let (key, _) = vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(&dir)).unwrap();
    let bake_key = vocal_tune::BakeKey::new(
        key,
        vocal_tune::tune_for_clip(&project, clip).unwrap(),
        None,
    );
    let cache = vocal_analysis::default_cache_dir()
        .unwrap()
        .join("bake")
        .join(bake_key.dir_name())
        .join(bake_key.file_name("lead"));
    assert!(cache.is_file(), "{}", cache.display());
    let written = std::fs::metadata(&cache).unwrap().modified().unwrap();
    let again = render(&project, &dir).unwrap();
    assert_same_samples(&tuned, &again, "second render");
    assert_eq!(
        std::fs::metadata(&cache).unwrap().modified().unwrap(),
        written,
        "the second render rewrote the bake"
    );

    // Vibrato editing is consumed by the same bake instead of being refused.
    let vibrato = parse(&project_text(
        "@vocal_tune lead_tune | clip: lead\n  vibrato: flatten\n  snap: off\n",
    ));
    let flattened = render(&vibrato, &dir).unwrap();
    assert!(flattened.iter().all(|sample| sample.is_finite()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_anchor_renders_dry_and_warns() {
    let _warnings = crate::RENDER_WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    init_cache();
    let dir = temp_dir("render-stale");
    write_take(&dir);
    let without = parse(&project_text(""));
    let project = parse(&project_text(&block("100%", "    - at=1.500s tgt=A4\n")));
    oc_engine::render_warnings::clear();
    let tuned = render(&project, &dir).unwrap();
    let warnings = oc_engine::render_warnings::take();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("no longer matches a detected onset")),
        "{warnings:?}"
    );
    assert_same_samples(
        &tuned,
        &render(&without, &dir).unwrap(),
        "stale anchor render",
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn zero_amount_bake_is_sample_exact_and_reuses_its_cache() {
    init_cache();
    let dir = temp_dir("bake");
    write_take(&dir);
    let source = dir.join("song.ocp");
    let without = parse(&project_text(""));
    let analysis = analysis_of(&without, &dir, &dir.join("probe.ocn"));
    let anchors: Vec<f64> = analysis
        .analysis
        .notes
        .iter()
        .take(2)
        .map(|note| note.start_s)
        .collect();
    let text = project_text(&block(
        "0%",
        &format!(
            "    - at={:.3}s tgt=A4\n    - at={:.3}s tgt=+2st\n",
            anchors[0], anchors[1]
        ),
    ));
    std::fs::write(&source, &text).unwrap();
    let project = parse(&text);

    let out = dir.join("out.ocp");
    let applied = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    assert!(applied.resolved.pass_through());
    assert_eq!(applied.resolved.shifted_notes(), 0);
    let baked = applied.baked.clone().expect("baked media");
    assert!(baked.path.exists(), "{}", baked.path.display());
    assert_eq!(baked.sample_rate, RATE);
    assert!(!baked.from_cache);

    // The baked WAV carries exactly the analysed playback window.
    let decoded = audio_clip::decode(&baked.path).unwrap();
    assert_eq!(decoded.sample_rate, RATE);
    let clip = vocal_analysis::resolve_clip(&project, "lead").unwrap();
    let (_, window) =
        vocal_analysis::clip_stereo_playback(&project, clip, RATE, Some(&dir)).unwrap();
    assert_eq!(decoded.samples.len(), window.len(), "baked frame count");
    assert_same_samples(&decoded.samples, &window, "baked window");

    // The written project drops the block and renders identically.
    let reopened = parse(&std::fs::read_to_string(&out).unwrap());
    assert!(reopened.properties.vocal_tunes.is_empty());
    let clip = reopened.audio_clip(0).unwrap();
    assert_eq!(clip.file, baked.file);
    assert_eq!(clip.offset_seconds, 0.0);
    assert_eq!(clip.stretch, 1.0);
    let original_render = render(&project, &dir).unwrap();
    let baked_render = render(&reopened, &dir).unwrap();
    assert_same_samples(&original_render, &baked_render, "baked project render");

    // Same decisions hit the cache; a changed decision renames the key.
    let again = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    assert!(again.baked.unwrap().from_cache);

    let edited = text.replace("tgt=A4", "tgt=A#4");
    let edited_path = dir.join("edited.ocp");
    std::fs::write(&edited_path, &edited).unwrap();
    let edited_project = parse(&edited);
    let edited_out = dir.join("edited-out.ocp");
    let shifted = vocal_tune::apply(
        &edited_project,
        &edited_path,
        "lead",
        RATE,
        Some(&edited_out),
    )
    .unwrap();
    let edited_baked = shifted.baked.unwrap();
    assert_ne!(edited_baked.path, baked.path, "decision edit kept the key");
    assert!(!edited_baked.from_cache);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_anchor_warns_and_does_not_retarget() {
    init_cache();
    let dir = temp_dir("stale");
    write_take(&dir);
    let source = dir.join("song.ocp");
    let text = project_text(&block("100%", "    - at=1.500s tgt=A4\n"));
    std::fs::write(&source, &text).unwrap();
    let project = parse(&text);
    let out = dir.join("out.ocp");
    let applied = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    assert!(applied.resolved.pass_through());
    let note = &applied.resolved.notes[0];
    assert_eq!(note.outcome, NoteOutcome::StaleAnchor);
    assert_eq!(note.shift_cents, None);
    assert!(note.warnings[0].contains("no longer matches a detected onset"));
    assert_eq!(note.detected_index, None);
    assert!(applied.baked.is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_reuses_the_analysis_cache() {
    init_cache();
    let dir = temp_dir("stale-ocn");
    write_take(&dir);
    let source = dir.join("song.ocp");
    let text = project_text(&block("0%", ""));
    std::fs::write(&source, &text).unwrap();
    let project = parse(&text);
    let out = dir.join("out.ocp");
    let first = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    assert!(!first.resolved.analysis_from_cache);
    let second = vocal_tune::apply(&project, &source, "lead", RATE, Some(&out)).unwrap();
    assert!(second.resolved.analysis_from_cache);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn target_helpers_hold_their_octave_convention() {
    // A4 = MIDI 69 = 6900 cents; C-1 = 0 cents.
    assert!((vocal_tune::hz_to_midi_cents(440.0) - 6_900.0).abs() < 1e-9);
    let a_major = oc_dsl::ast::KeySignature {
        root: oc_dsl::ast::NoteName::A,
        accidental: oc_dsl::ast::Accidental::Natural,
        mode: oc_dsl::ast::KeyMode::Major,
    };
    assert_eq!(vocal_tune::scale_degree_midi(&a_major, 1, 4).unwrap(), 69);
    assert_eq!(vocal_tune::scale_degree_midi(&a_major, 3, 4).unwrap(), 73);
    // 220 Hz is A3, so an A4 target is one octave up.
    let detected = oc_dsp::vocal_analysis::VocalNote {
        start_s: 0.0,
        end_s: 1.0,
        onset_s: 0.0,
        offset_s: 1.0,
        f0_median_hz: 220.0,
        confidence: 0.9,
        voiced_ratio: 1.0,
    };
    let note = oc_dsl::ast::VocalTuneNote {
        at: 0.0,
        target: Some(oc_dsl::ast::VocalTarget::Pitch(69)),
        amount: None,
        retune_seconds: None,
        formant_cents: None,
        start_seconds: None,
        length: None,
        vibrato: None,
        bypass: false,
    };
    let resolved = vocal_tune::resolve_override(&note, &detected, None, 100.0, 0.5).unwrap();
    assert!((resolved.shift_cents.unwrap() - 1_200.0).abs() < 1e-9);
    assert!((resolved.resulting_hz.unwrap() - 440.0).abs() < 1e-6);
    assert!(resolved.warnings.is_empty());
}
