//! Note-onset probe for `docs/fx-laws.md` and
//! `docs/synth-character.md`.
//!
//! Renders one held note through the imported bass rack, one rack prefix at a
//! time, and prints the first-20 ms onset peak plus the whole-note level. A
//! second harness renders the lead's first four notes and prints the per-note
//! peak so the first-note gain can be compared with the settled notes.
//!
//! Run with:
//! `cargo test -p oc-engine --test all fx_onset_probe:: -- --ignored --nocapture`

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
const TEMP: &str = "/tmp/fx-onset/patches";
const ONSET_FRAMES: usize = RATE as usize / 50;

fn demo_project() -> Project {
    let demo = demo_path();
    let text = std::fs::read_to_string(&demo).expect("demo project");
    oc_dsl::parse(&text).expect("demo parses")
}

fn demo_patch(project: &Project, preset: &str) -> SynthPatch {
    let preset = project
        .presets
        .iter()
        .find(|candidate| candidate.name == preset)
        .expect("preset");
    let demo = demo_path();
    load_project_patch(&preset.params, Some(demo.parent().unwrap())).expect("patch")
}

/// One track of the fixture with its phrase replaced by `phrase`, keeping the
/// fixture's preset and mix state.
fn probe_project(track: &str, phrase: &str) -> Project {
    let mut project = demo_project();
    project.tracks.retain(|candidate| candidate.name == track);
    let text = format!(
        "@ocp \"0.2.0\"\n\n@project \"probe\"\n  tempo: 96\n  time_signature: 4/4\n  key: C minor\n\n@track {track} | instrument: synth/virtual_analog, preset: demo_{track}\n  mix: {mix}\n  @phrase probe [1:1 - 9:1]\n{phrase}\n\n@master\n",
        mix = if track == "bass" {
            "vol=-6dB, pan=0"
        } else {
            "vol=-10dB, pan=-0.1"
        }
    );
    let parsed = oc_dsl::parse(&text).expect("probe parses");
    let mut probe = parsed.tracks.into_iter().next().expect("probe track");
    probe.preset = project.tracks[0].preset.clone();
    project.tracks[0].phrases = probe.phrases;
    project.tracks[0].inline_patterns = probe.inline_patterns;
    project.tracks[0].mix = probe.mix;
    project
}

fn patch_file(patch: &SynthPatch, tag: &str) -> PathBuf {
    std::fs::create_dir_all(TEMP).expect("temp dir");
    let path = Path::new(TEMP).join(format!("{tag}.json"));
    let document = PatchDocument {
        version: 1,
        name: format!("fx-onset-{tag}"),
        author: "OpenCompose".into(),
        category: "probe".into(),
        use_policy: "measurement".into(),
        patch: patch.clone(),
    };
    std::fs::write(&path, document.to_json().expect("patch json")).expect("write patch");
    path
}

/// Render one track with `patch` substituted and return its stem from the first
/// non-zero frame.
fn render_track(project: &Project, patch: &SynthPatch, tag: &str) -> Vec<f32> {
    let mut variant = project.clone();
    let track_name = variant.tracks[0].name.clone();
    let preset_name = variant.tracks[0]
        .preset
        .clone()
        .expect("probe track preset");
    let index = variant
        .presets
        .iter()
        .position(|preset| preset.name == preset_name)
        .expect("preset");
    let output_gain = variant.presets[index]
        .params
        .iter()
        .find(|(name, _)| name == "output.gain")
        .and_then(|(_, value)| value.as_f64())
        .expect("output.gain");
    let path = patch_file(patch, tag);
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
        .find(|stem| stem.name == track_name)
        .expect("probe stem");
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
        let count = (frames * 2).max(1) as f64;
        Self {
            peak,
            rms: (power / count).sqrt(),
        }
    }

    fn db(self) -> (f64, f64) {
        (
            20.0 * self.peak.max(1e-12).log10(),
            20.0 * self.rms.max(1e-12).log10(),
        )
    }
}

fn enabled_set(patch: &SynthPatch, keep: &[usize]) -> SynthPatch {
    let mut variant = patch.clone();
    for (index, module) in variant.fx.iter_mut().enumerate() {
        module.enabled = keep.contains(&index);
    }
    variant
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

fn describe_rack(tag: &str, patch: &SynthPatch) {
    println!("=== imported {tag} rack ===");
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
}

#[test]
#[ignore = "measurement harness for docs/fx-laws.md"]
fn bass_held_note_onset_by_stage() {
    let project = probe_project("bass", "    G2 w f");
    let patch = demo_patch(&project, "demo_bass");
    describe_rack("bass", &patch);
    let variants: Vec<(String, Vec<usize>)> = vec![
        ("voice".into(), vec![]),
        ("+distortion".into(), vec![0]),
        ("+eq1".into(), vec![0, 1]),
        ("+delay1".into(), vec![0, 1, 2]),
        ("+reverb".into(), vec![0, 1, 2, 3]),
        ("+eq2".into(), vec![0, 1, 2, 3, 4]),
        ("+flanger".into(), vec![0, 1, 2, 3, 4, 5]),
        ("+delay2".into(), vec![0, 1, 2, 3, 4, 5, 6]),
        ("+compressor".into(), (0..patch.fx.len()).collect()),
    ];
    let mut rows = Vec::new();
    for (tag, keep) in variants {
        let variant = enabled_set(&patch, &keep);
        let samples = render_track(&project, &variant, &tag);
        rows.push((tag, samples));
    }
    let frames = rows.iter().map(|(_, s)| s.len() / 2).min().unwrap();
    let whole = (frames).min(RATE as usize * 2);
    let onset = ONSET_FRAMES.min(whole);
    let voice = Level::measure(&rows[0].1, onset);
    let voice_db = voice.db();
    println!(
        "\n=== held G2, first {} ms, {:.2} s note ===",
        onset as f64 * 1000.0 / RATE as f64,
        whole as f64 / RATE as f64
    );
    println!(
        "{:<12} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8}",
        "stage", "onsetPk", "dPk", "onsetRMS", "dRMS", "notePk", "noteRMS"
    );
    for (tag, samples) in &rows {
        let value = Level::measure(samples, onset);
        let note = Level::measure(samples, whole);
        let (peak_db, rms_db) = value.db();
        println!(
            "{:<12} {:>9.5} {:>+8.2} {:>9.6} {:>+8.2} {:>9.5} {:>9.6}",
            tag,
            value.peak,
            peak_db - voice_db.0,
            value.rms,
            rms_db - voice_db.1,
            note.peak,
            note.rms
        );
    }
}

fn note_peak_gain(voice: &[f32], full: &[f32], start: usize, length: usize) -> (f64, f64, f64) {
    let voice_peak = Level::measure(&voice[start * 2..], length).peak;
    let full_peak = Level::measure(&full[start * 2..], length).peak;
    (
        voice_peak,
        full_peak,
        20.0 * (full_peak / voice_peak.max(1e-12)).log10(),
    )
}

#[test]
#[ignore = "measurement harness for the voice laws"]
fn lead_four_note_stage_gain() {
    let phrase = "    C5 q mf\n    Eb5 q mf\n    G5 h f\n    |\n    Ab5 q f";
    let project = probe_project("lead", phrase);
    let patch = demo_patch(&project, "demo_lead");
    let voice = render_track(&project, &enabled_set(&patch, &[]), "lead-stage-voice");
    let beat = (60.0 / 96.0 * RATE as f64) as usize;
    let starts = [0, beat, 2 * beat, 4 * beat];
    let lengths = [beat, beat, 2 * beat, beat];
    let prefixes: [(&str, usize); 7] = [
        ("voice", 0),
        ("+utils", 1),
        ("+phaser", 2),
        ("+reverb", 3),
        ("+eq", 4),
        ("+multiband", 5),
        ("+chorus", 6),
    ];
    println!("\n=== lead four notes, chain gain over the note window (full/voice) ===");
    println!(
        "{:<12} {:>9} {:>9} {:>9} {:>9} {:>10}",
        "stage", "note1", "note2", "note3", "note4", "n2-4 mean"
    );
    for (tag, count) in prefixes {
        let keep: Vec<usize> = (0..count).collect();
        if count == 0 {
            println!(
                "{:<12} {:>9} {:>9} {:>9} {:>9} {:>10}",
                tag, "-", "-", "-", "-", "-"
            );
            continue;
        }
        let samples = render_track(
            &project,
            &enabled_set(&patch, &keep),
            &format!("lead-stage-{count}"),
        );
        let gains: Vec<f64> = starts
            .iter()
            .zip(lengths.iter())
            .map(|(&start, &length)| note_peak_gain(&voice, &samples, start, length).2)
            .collect();
        let mean = (gains[1] + gains[2] + gains[3]) / 3.0;
        println!(
            "{:<12} {:>+9.2} {:>+9.2} {:>+9.2} {:>+9.2} {:>+10.2}",
            tag, gains[0], gains[1], gains[2], gains[3], mean
        );
    }
}

#[test]
#[ignore = "measurement harness for the voice laws"]
fn song_lead_first_note() {
    let project = demo_project();
    let mut variant = project.clone();
    variant.tracks.retain(|track| track.name == "lead");
    let patch = demo_patch(&variant, "demo_lead");
    let voice = render_track(&variant, &enabled_set(&patch, &[]), "song-lead-voice");
    let full = render_track(&variant, &patch, "song-lead-full");
    let beat = (60.0 / 96.0 * RATE as f64) as usize;
    let bar = beat * 4;
    let starts: Vec<usize> = (0..8).map(|index| index * bar).collect();
    println!("\n=== song lead, bar peaks (first non-silent sample is bar 5) ===");
    println!(
        "{:<5} {:>10} {:>10} {:>8} {:>10} {:>10} {:>8}",
        "bar", "voicePk", "fullPk", "gain", "voiceRMS", "fullRMS", "gainRMS"
    );
    for (index, &start) in starts.iter().enumerate() {
        if start * 2 >= voice.len() {
            continue;
        }
        let length = bar.min((voice.len() / 2).saturating_sub(start));
        let voice_level = Level::measure(&voice[start * 2..], length);
        let full_level = Level::measure(&full[start * 2..], length);
        println!(
            "{:<5} {:>10.5} {:>10.5} {:>+8.2} {:>10.6} {:>10.6} {:>+8.2}",
            index + 5,
            voice_level.peak,
            full_level.peak,
            20.0 * (full_level.peak / voice_level.peak.max(1e-12)).log10(),
            voice_level.rms,
            full_level.rms,
            20.0 * (full_level.rms / voice_level.rms.max(1e-12)).log10()
        );
    }
}

#[test]
#[ignore = "measurement harness for the voice laws"]
fn lead_compander_state_candidates() {
    let phrase = "    C5 q mf\n    Eb5 q mf\n    G5 h f\n    |\n    Ab5 q f";
    let project = probe_project("lead", phrase);
    let patch = demo_patch(&project, "demo_lead");
    let voice = render_track(&project, &enabled_set(&patch, &[]), "lead-cand-voice");
    let beat = (60.0 / 96.0 * RATE as f64) as usize;
    let starts = [0, beat, 2 * beat, 4 * beat];
    let lengths = [beat, beat, 2 * beat, beat];
    let comp_index = patch
        .fx
        .iter()
        .position(|module| module.fx_type == SynthFxType::Multiband)
        .expect("multiband");
    let mut variants: Vec<(String, SynthPatch)> = Vec::new();
    variants.push(("imported".into(), patch.clone()));
    let mut no_up = patch.clone();
    for band in 1..=3 {
        no_up.fx[comp_index].set_param(&format!("band{band}_ratio_below"), 1.0);
    }
    variants.push(("no up path".into(), no_up));
    let mut fast = patch.clone();
    for band in 1..=3 {
        fast.fx[comp_index].set_param(&format!("band{band}_attack"), 0.1);
    }
    variants.push(("attack 0.1 ms".into(), fast));
    let mut fast_heavy = patch.clone();
    for band in 1..=3 {
        fast_heavy.fx[comp_index].set_param(&format!("band{band}_attack"), 0.1);
        fast_heavy.fx[comp_index].set_param(&format!("band{band}_ratio_below"), 1.0);
    }
    variants.push(("attack .1 no up".into(), fast_heavy));
    println!("\n=== lead compander start-state candidates (gain over voice) ===");
    println!(
        "{:<16} {:>8} {:>8} {:>8} {:>8} {:>10}",
        "variant", "note1", "note2", "note3", "note4", "excess"
    );
    for (tag, variant) in variants {
        let samples = render_track(&project, &variant, &format!("lead-cand-{tag}"));
        let gains: Vec<f64> = starts
            .iter()
            .zip(lengths.iter())
            .map(|(&start, &length)| note_peak_gain(&voice, &samples, start, length).2)
            .collect();
        let later = (gains[1] + gains[2] + gains[3]) / 3.0;
        println!(
            "{:<16} {:>+8.2} {:>+8.2} {:>+8.2} {:>+8.2} {:>+10.2}",
            tag,
            gains[0],
            gains[1],
            gains[2],
            gains[3],
            gains[0] - later
        );
    }
}

#[test]
#[ignore = "measurement harness for docs/fx-laws.md"]
fn all_imported_racks() {
    let project = demo_project();
    for preset in ["demo_bass", "demo_pad", "demo_pluck", "demo_lead"] {
        describe_rack(preset, &demo_patch(&project, preset));
    }
}

#[test]
#[ignore = "measurement harness for the voice laws"]
fn lead_multiband_gain_envelope() {
    let phrase = "    C5 q mf\n    Eb5 q mf\n    G5 h f\n    |\n    Ab5 q f";
    let project = probe_project("lead", phrase);
    let patch = demo_patch(&project, "demo_lead");
    let before = render_track(
        &project,
        &enabled_set(&patch, &[0, 1, 2, 3]),
        "lead-env-before",
    );
    let after = render_track(
        &project,
        &enabled_set(&patch, &[0, 1, 2, 3, 4]),
        "lead-env-after",
    );
    let window = RATE as usize / 100;
    let mut line = String::new();
    for index in 0..120 {
        let start = index * window;
        let a = Level::measure(&before[start * 2..], window).rms;
        let b = Level::measure(&after[start * 2..], window).rms;
        line.push_str(&format!("{:+.1} ", 20.0 * (b / a.max(1e-12)).log10()));
        if index % 20 == 19 {
            line.push('\n');
        }
    }
    println!("\n=== multiband gain over time (dB), 10 ms windows, first 1.2 s ===\n{line}");
}

#[test]
#[ignore = "measurement harness for docs/fx-laws.md"]
fn bass_held_note_onset_envelope() {
    let project = probe_project("bass", "    G2 w f");
    let patch = demo_patch(&project, "demo_bass");
    let stages: Vec<(String, Vec<usize>)> = vec![
        ("voice".into(), vec![]),
        ("+eq2".into(), vec![0, 1, 2, 3, 4]),
        ("+flanger".into(), vec![0, 1, 2, 3, 4, 5]),
        ("full".into(), (0..patch.fx.len()).collect()),
    ];
    let window = RATE as usize / 1000;
    println!("\n=== held G2, 1 ms peaks, first 30 ms ===");
    for (tag, keep) in stages {
        let samples = render_track(&project, &enabled_set(&patch, &keep), &format!("env-{tag}"));
        let peaks: Vec<String> = (0..30)
            .map(|index| {
                format!(
                    "{:.4}",
                    Level::measure(&samples[index * window * 2..], window).peak
                )
            })
            .collect();
        println!("{tag:>8}: {}", peaks.join(" "));
    }
}
