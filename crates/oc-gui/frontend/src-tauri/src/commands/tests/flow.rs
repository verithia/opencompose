//! The agent flow end to end, scripted: a model reads audio, understands the
//! project, writes a part, listens to it, iterates, saves the result, and
//! recovers from the two refusals it is most likely to hit.
//!
//! No model and no network: every step drives the same command-layer function
//! the agent HTTP/MCP surface dispatches to, in the order a model would call
//! them. The fixture's audio is a real WAV on disk, so the listening clip comes
//! from the real decode/resample/render path rather than from memory.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::commands::*;
use crate::state::AppState;

use super::support::TempDir;

const REFERENCE_OCP: &str = r#"@ocp "0.2.0"

@project "agent flow fixture"
  tempo: 120
  time_signature: 4/4

@audio_clip reference [1:1 - 4:1] | track: reference
  file: "reference.wav"

@track reference | instrument: audio/clip
"#;

const MELODY_TRACK: &str = r#"@track melody | preset: melody_synth, instrument: synth/lead_square
  @phrase line [1:1 - 3:1]
    C4 h mf
    E4 h mf
    G4 h mf
    C5 h mf
"#;

/// Six seconds of a 440 Hz tone, 16-bit mono. The flow reads it back through
/// the audio-clip decoder, so silence here is a fixture bug, not a render bug.
fn write_reference_wav(path: &Path) {
    const SAMPLE_RATE: u32 = 48_000;
    let frames = 6 * SAMPLE_RATE as usize;
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..frames {
        let phase = 2.0 * PI * 440.0 * frame as f64 / f64::from(SAMPLE_RATE);
        let sample = (phase.sin() * 0.5 * f64::from(i16::MAX)) as i16;
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}

/// Open the fixture the way `project_open` does after reading the file.
fn open(state: &AppState, path: &Path) -> oc_dsl::ast::Project {
    let source = std::fs::read_to_string(path).expect("the fixture reads");
    let project = oc_dsl::parse(&source).expect("the fixture parses");
    let mut inner = state.lock_inner();
    inner.audio.soundfont_path = None;
    replace_project(&mut inner, project.clone(), Some(path.to_path_buf()))
        .expect("the fixture opens");
    project
}

fn range(start: f64, end: f64, tracks: Option<Vec<usize>>) -> RangeRequest {
    RangeRequest {
        start_s: Some(start),
        end_s: Some(end),
        tracks,
        ..Default::default()
    }
}

fn decode_clip(result: &AuditionRangeResult) -> (hound::WavSpec, Vec<i16>, Vec<u8>) {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.audio.base64)
        .expect("the clip is base64");
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes.clone())).unwrap();
    let spec = reader.spec();
    let samples = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    (spec, samples, bytes)
}

fn peak_dbfs(samples: &[i16]) -> f64 {
    let peak = samples
        .iter()
        .map(|sample| f64::from(sample.unsigned_abs()))
        .fold(0.0_f64, f64::max);
    if peak == 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * (peak / 32768.0).log10()
    }
}

fn track_index(project: &oc_dsl::ast::Project, name: &str) -> usize {
    project
        .tracks
        .iter()
        .position(|track| track.name == name)
        .unwrap_or_else(|| panic!("track '{name}' is missing"))
}

#[test]
fn the_agent_flow_takes_a_reference_to_a_finished_part() {
    let dir = TempDir::new("agent-flow");
    std::fs::create_dir_all(dir.path()).unwrap();
    let reference_wav = dir.path().join("reference.wav");
    let reference_ocp = dir.path().join("reference.ocp");
    write_reference_wav(&reference_wav);
    std::fs::write(&reference_ocp, REFERENCE_OCP).unwrap();

    let state = Arc::new(AppState::new());

    // 1. Open the project the way the agent does. The clip names a real file and
    //    its destination track exists, so opening decodes nothing yet.
    let opened = open(&state, &reference_ocp);
    assert_eq!(opened.tracks.len(), 1, "the fixture opens with its track");
    assert_eq!(opened.tracks[0].name, "reference");
    assert_eq!(opened.tracks[0].instrument.as_path(), "audio/clip");
    assert_eq!(opened.audio_clip_count(), 1);
    assert_eq!(opened.audio_clip(0).unwrap().file, "reference.wav");

    // 2. Hear it: audition_range 0–6 s returns an inline 16 kHz mono clip built
    //    from the real clip decode and mix, and the tone is audibly there.
    let heard = audition_range_for_agent(
        Some(
            dir.path()
                .join("reference-audition.wav")
                .display()
                .to_string(),
        ),
        range(0.0, 6.0, Some(vec![0])),
        AuditionChannel::Mono,
        AUDITION_SAMPLE_RATE,
        &state,
        false,
    )
    .expect("the reference auditions");
    assert_eq!(heard.audio.mime, "audio/wav");
    assert_eq!(heard.audio.sample_rate, 16_000);
    assert_eq!(heard.audio.channels, 1);
    assert!(!heard.truncated, "a 6 s window fits the inline budget");
    let (spec, samples, bytes) = decode_clip(&heard);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);
    let duration = samples.len() as f64 / 16_000.0;
    assert!(
        (duration - 6.0).abs() <= 1.0 / 16_000.0,
        "the listening clip is {duration} s, not 6 s"
    );
    let reference_peak = peak_dbfs(&samples);
    assert!(
        reference_peak > -40.0,
        "the reference clip is silent: {reference_peak} dBFS"
    );

    // 3. Read the project: the DSL names the clip, the track list reports it,
    //    and the subtree extract shows the clip host instrument.
    let dsl = state.lock_inner().dsl_source.clone();
    assert!(dsl.contains("@audio_clip reference"), "{dsl}");
    assert!(dsl.contains("file: \"reference.wav\""), "{dsl}");
    let tracks = crate::agent_api::tracks_list(&state).expect("tracks_list answers");
    assert_eq!(tracks["count"], 1);
    assert_eq!(tracks["tracks"][0]["name"], "reference");
    assert_eq!(tracks["tracks"][0]["instrument"], "audio/clip");
    let extracted = {
        let inner = state.lock_inner();
        oc_dsl::ops::extract(&inner.project, "track:reference").expect("the track extracts")
    };
    assert_eq!(extracted.affected_tracks, vec![0]);
    assert!(
        extracted.dsl_text.contains("@track reference"),
        "{extracted:?}"
    );
    assert!(extracted.dsl_text.contains("audio/clip"), "{extracted:?}");

    // 4. Write a part: a builtin OpenSynth preset, then a track that uses it.
    let preset = {
        let mut inner = state.lock_inner();
        add_preset_inner(
            &mut inner,
            "melody_synth".into(),
            "builtin".into(),
            None,
            Some("builtin.open_synth".into()),
            None,
            None,
        )
    }
    .expect("presets_add adds the builtin preset");
    assert_eq!(preset.project.presets.len(), 1);
    assert_eq!(preset.project.presets[0].name, "melody_synth");

    let created = {
        let mut inner = state.lock_inner();
        create_track_checked(
            &mut inner,
            "melody",
            "synth",
            "lead_square",
            Some("melody_synth"),
            MELODY_TRACK,
        )
    }
    .expect("tracks_create accepts a playable builtin part");
    assert_eq!(created.track_count, 2, "the project now has two tracks");
    let melody = track_index(&created.project, "melody");
    assert_eq!(melody, 1);
    assert_eq!(
        created.project.tracks[melody].preset.as_deref(),
        Some("melody_synth")
    );
    assert_eq!(created.project.tracks[melody].phrases.len(), 1);
    assert_eq!(created.project.tracks[melody].phrases[0].events.len(), 4);

    // The new track's own DSL round-trips through project_validate.
    let melody_dsl = {
        let inner = state.lock_inner();
        oc_dsl::ops::extract(&inner.project, "track:melody")
            .expect("the new track extracts")
            .dsl_text
    };
    let check = {
        let inner = state.lock_inner();
        validate_edit_in_project(&inner, melody_dsl.clone(), "track:melody".into())
    };
    assert!(check.valid, "{:?}", check.errors);

    // 5. Iterate: change one note on a track a tool just created, and the diff
    //    is exactly that one line replacement.
    let before = state.lock_inner().dsl_source.clone();
    assert!(melody_dsl.contains("G4 h mf"), "{melody_dsl}");
    let edited_dsl = melody_dsl.replace("G4 h mf", "A4 h mf");
    assert_ne!(edited_dsl, melody_dsl);
    let applied = {
        let mut inner = state.lock_inner();
        apply_edit_to_project(
            &mut inner,
            edited_dsl,
            "track:melody".into(),
            before.clone(),
        )
    }
    .expect("the second-round edit applies");
    let diff = diff_dsl(before, applied.dsl_source.clone());
    let mut changed: Vec<(&str, &str)> = Vec::new();
    for hunk in &diff.hunks {
        for line in &hunk.lines {
            match line.kind {
                oc_dsl::ai::DiffKind::Removed => changed.push(("removed", line.text.as_str())),
                oc_dsl::ai::DiffKind::Added => changed.push(("added", line.text.as_str())),
                oc_dsl::ai::DiffKind::Same => {}
            }
        }
    }
    assert_eq!(
        changed.len(),
        2,
        "the diff is not one line pair: {changed:?}"
    );
    assert_eq!(changed[0].0, "removed");
    assert!(changed[0].1.contains("G4 h mf"), "{changed:?}");
    assert_eq!(changed[1].0, "added");
    assert!(changed[1].1.contains("A4 h mf"), "{changed:?}");
    let edited = state.lock_inner().project.clone();
    let melody = track_index(&edited, "melody");
    let midis: Vec<u8> = edited.tracks[melody].phrases[0]
        .events
        .iter()
        .map(|event| event.pitch.to_midi())
        .collect();
    assert_eq!(midis, vec![60, 64, 69, 72]);

    // 6. Listen to what was written: the melody track alone, in its own window.
    let melody_only = Some(vec![melody]);
    let listened = audition_range_for_agent(
        Some(dir.path().join("melody-audition.wav").display().to_string()),
        range(0.0, 4.0, melody_only.clone()),
        AuditionChannel::Mono,
        AUDITION_SAMPLE_RATE,
        &state,
        false,
    )
    .expect("the melody auditions");
    let (spec, samples, _) = decode_clip(&listened);
    assert_eq!(spec.sample_rate, 16_000);
    assert!(!samples.is_empty(), "the melody listening clip is empty");
    let melody_peak = peak_dbfs(&samples);
    assert!(
        melody_peak > -40.0,
        "the melody listening clip is silent: {melody_peak} dBFS"
    );

    let rendered = render_range_for_agent(
        dir.path().join("melody.wav").display().to_string(),
        range(0.0, 4.0, melody_only),
        None,
        &state,
    )
    .expect("the melody renders");
    assert!((rendered.duration_s - 4.0).abs() < 1e-9);
    let analysis = oc_engine::analysis::analyze_wav_with_peaks(&rendered.path, 8)
        .expect("the render analyses");
    assert!(
        analysis.level.peak_dbfs > -40.0,
        "the rendered melody is below the noise floor: {} dBFS",
        analysis.level.peak_dbfs
    );

    // 7. Finish: save the part, then re-open the artefact and validate it.
    let saved_path = dir.path().join("finished.ocp");
    let saved = save_file_inner(Some(saved_path.display().to_string()), &state)
        .expect("project_save writes the part");
    assert_eq!(PathBuf::from(&saved), saved_path);
    let saved_text = std::fs::read_to_string(&saved_path).expect("the saved part reads");
    assert_eq!(
        saved_text,
        state.lock_inner().dsl_source,
        "the file is the document the session edited"
    );
    let saved_project = oc_dsl::parse(&saved_text).expect("the saved part parses");
    oc_dsl::validate(&saved_project).expect("the saved document validates");
    assert_eq!(saved_project.tracks.len(), 2);
    assert!(saved_text.contains("A4 h mf"), "{saved_text}");
    assert!(
        saved_text.contains("file: \"reference.wav\""),
        "{saved_text}"
    );
    {
        let mut inner = state.lock_inner();
        replace_project(&mut inner, saved_project, Some(saved_path))
            .expect("the saved part re-opens");
    }
    let reopened = {
        let inner = state.lock_inner();
        oc_dsl::ops::extract(&inner.project, "track:melody")
            .expect("the re-opened melody extracts")
            .dsl_text
    };
    let check = {
        let inner = state.lock_inner();
        validate_edit_in_project(&inner, reopened, "track:melody".into())
    };
    assert!(
        check.valid,
        "the re-opened document does not validate: {:?}",
        check.errors
    );

    // 8a. Refusal the model must recover from: a family with no built-in
    // default still needs a SoundFont, and the refusal names the skill.
    let refusal = {
        let mut inner = state.lock_inner();
        create_track_checked(
            &mut inner,
            "take",
            "sample",
            "take",
            None,
            "@track take | instrument: sample/take\n  @phrase chord [1:1 - 2:1]\n    C4 h mf\n",
        )
    }
    .expect_err("a sample track without a file is refused");
    assert!(refusal.contains("Track 'take'"), "{refusal}");
    assert!(refusal.contains("needs a SoundFont"), "{refusal}");
    let payload = crate::ai::skill_hints::refusal(&refusal);
    assert_eq!(payload["skill"], "playable-instruments");
    assert_eq!(
        state.lock_inner().project.tracks.len(),
        2,
        "the refusal changed nothing"
    );

    // 8b. Refusal the model must recover from: nine narrow windows in a row.
    let narrow_dir = dir.path().join("narrow");
    std::fs::create_dir_all(&narrow_dir).unwrap();
    for index in 0..8 {
        render_range_for_agent(
            narrow_dir
                .join(format!("narrow-{index}.wav"))
                .display()
                .to_string(),
            range(1.0, 1.5, None),
            None,
            &state,
        )
        .unwrap_or_else(|error| panic!("narrow window {index} was refused: {error}"));
    }
    let refusal = render_range_for_agent(
        narrow_dir.join("narrow-9.wav").display().to_string(),
        range(1.0, 1.5, None),
        None,
        &state,
    )
    .expect_err("the ninth narrow window is refused");
    assert!(refusal.contains("narrow-window loop"), "{refusal}");
    assert_eq!(
        crate::ai::skill_hints::for_message(&refusal),
        Some("audition-and-listen")
    );
    // The recovery: a wide window still renders.
    let wide = render_range_for_agent(
        dir.path().join("wide.wav").display().to_string(),
        range(0.0, 6.0, None),
        None,
        &state,
    )
    .expect("a wide window is not the loop");
    assert!((wide.duration_s - 6.0).abs() < 1e-9);

    // 9. Clean up the temp dir.
    std::fs::remove_dir_all(dir.path()).expect("the temp dir is removed");
    assert!(!dir.path().exists());
}
