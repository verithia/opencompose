//! The agent narrow-window guard: a model that keeps measuring ever-smaller
//! slices of one revision is refused with the remedy, a listening clip or a
//! wide window is not, and no human render path is counted.
//!
//! One bar of `SONG` is 2 s at 120 bpm, so a 0.5 s window is narrow by both
//! readings (under four seconds and under a bar) and a whole bar is not.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use oc_dsl::ast::BarBeat;

use crate::commands::{RangeRequest, audition_range_for_agent, render_range_for_agent};
use crate::state::AppState;

use super::support::TempDir;

const SONG: &str = r#"@ocp "0.2.0"

@project "window guard"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: { frequency: 440, amplitude: 0.2, waveform: 0 }

@track tone | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 2:1]
    C4 q
"#;

fn guard_state() -> Arc<AppState> {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(SONG).expect("fixture parses");
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.audio.sample_rate = 48_000;
    }
    Arc::new(state)
}

fn revision(state: &AppState) -> u64 {
    state.lock_inner().revision
}

fn narrow_count(state: &AppState) -> u32 {
    let revision = revision(state);
    state.window_guard().narrow_count(revision)
}

/// A 0.5 s agent window: the shape of a per-beat measurement pass.
fn narrow(state: &AppState, dir: &TempDir, name: &str) -> Result<(), String> {
    render_range_for_agent(
        dir.path().join(name).display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(1.5),
            ..Default::default()
        },
        None,
        state,
    )
    .map(|_| ())
}

fn render_until_refused(state: &AppState, dir: &TempDir) {
    for index in 0..8 {
        narrow(state, dir, &format!("narrow-{index}.wav"))
            .unwrap_or_else(|error| panic!("window {index} was refused: {error}"));
    }
}

const REFUSAL: &str = "narrow-window loop: 8 windows under 4 s of the same revision. Measuring \
     more windows will not tell you how it sounds; call audition_range on the section you care \
     about, or analyze_notes to get the notes (or render a section, not a beat), or read the \
     audition-and-listen skill.";

#[test]
fn eight_narrow_agent_windows_render_and_the_ninth_refuses_with_the_remedy() {
    let state = guard_state();
    let dir = TempDir::new("window-guard-limit");
    std::fs::create_dir_all(dir.path()).unwrap();

    render_until_refused(&state, &dir);
    assert_eq!(narrow_count(&state), 8, "eight windows are admitted");

    let error = narrow(&state, &dir, "ninth.wav").unwrap_err();
    assert_eq!(error, REFUSAL);
    assert_eq!(narrow_count(&state), 8, "a refusal does not count itself");

    // The message reaches the model with the skill id the hints table maps it to.
    let payload = crate::ai::skill_hints::refusal(&error);
    assert_eq!(payload["error"], REFUSAL);
    assert_eq!(payload["skill"], "audition-and-listen");
    assert_eq!(
        crate::ai::skill_hints::for_message(&error),
        Some("audition-and-listen")
    );
    assert!(
        error.contains("analyze_notes to get the notes"),
        "the refusal names the note-level alternative: {error}"
    );
}

#[test]
fn a_wide_window_after_the_refusal_renders_and_is_not_counted() {
    let state = guard_state();
    let dir = TempDir::new("window-guard-wide");
    std::fs::create_dir_all(dir.path()).unwrap();
    render_until_refused(&state, &dir);
    assert!(narrow(&state, &dir, "refused.wav").is_err());

    let wide = render_range_for_agent(
        dir.path().join("wide.wav").display().to_string(),
        RangeRequest {
            start_s: Some(0.0),
            end_s: Some(10.0),
            ..Default::default()
        },
        None,
        &state,
    )
    .expect("a wide window is not the loop");
    assert_eq!(wide.duration_s, 10.0);
    assert_eq!(narrow_count(&state), 8, "a wide window is not counted");

    // The refusal is about narrow slices only: the same wide window twice more.
    for index in 0..2 {
        render_range_for_agent(
            dir.path()
                .join(format!("wide-{index}.wav"))
                .display()
                .to_string(),
            RangeRequest {
                start_s: Some(0.0),
                end_s: Some(10.0),
                ..Default::default()
            },
            None,
            &state,
        )
        .expect("wide windows keep rendering");
    }
    assert_eq!(narrow_count(&state), 8);
}

#[test]
fn an_audition_always_renders_and_clears_the_counter() {
    let state = guard_state();
    let dir = TempDir::new("window-guard-audition");
    std::fs::create_dir_all(dir.path()).unwrap();
    render_until_refused(&state, &dir);
    assert!(narrow(&state, &dir, "refused.wav").is_err());

    let audition = audition_range_for_agent(
        Some(dir.path().join("audition.wav").display().to_string()),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(1.5),
            ..Default::default()
        },
        crate::commands::AuditionChannel::Mono,
        crate::commands::AUDITION_SAMPLE_RATE,
        &state,
        false,
    )
    .expect("listening is never refused");
    assert!(!audition.audio.base64.is_empty(), "the clip carries audio");
    assert_eq!(narrow_count(&state), 0, "listening clears the loop counter");

    narrow(&state, &dir, "after-audition.wav").expect("the counter is clear");
    assert_eq!(narrow_count(&state), 1);
}

#[test]
fn the_counter_is_per_revision_so_an_edit_allows_narrow_windows_again() {
    let state = guard_state();
    let dir = TempDir::new("window-guard-revision");
    std::fs::create_dir_all(dir.path()).unwrap();
    render_until_refused(&state, &dir);
    assert!(narrow(&state, &dir, "refused.wav").is_err());

    let before = revision(&state);
    state.lock_inner().bump_revision();
    assert_ne!(revision(&state), before);

    narrow(&state, &dir, "new-revision.wav").expect("a new revision is a new budget");
    assert_eq!(narrow_count(&state), 1);
    assert_eq!(state.window_guard().narrow_count(before), 0);
}

#[test]
fn opening_a_project_clears_the_counter() {
    let state = guard_state();
    let dir = TempDir::new("window-guard-open");
    std::fs::create_dir_all(dir.path()).unwrap();
    render_until_refused(&state, &dir);
    assert!(narrow(&state, &dir, "refused.wav").is_err());

    {
        let mut inner = state.lock_inner();
        let opened = oc_dsl::parse(SONG).expect("fixture parses");
        crate::commands::replace_project(&mut inner, opened, None).expect("project opens");
    }

    narrow(&state, &dir, "after-open.wav").expect("opening a project clears the loop counter");
    assert_eq!(narrow_count(&state), 1);
}

#[test]
fn a_human_clip_preview_does_not_count_narrow_windows() {
    let state = guard_state();
    let project = state.lock_inner().project.clone();
    let soundfont = state
        .lock_inner()
        .audio
        .soundfont_path
        .clone()
        .unwrap_or_default();
    let mut instances = HashMap::new();

    for _ in 0..9 {
        let audio = crate::audio::AudioState::render_block_window(
            &project,
            0,
            BarBeat::new(1, 1, 0)..BarBeat::new(1, 2, 0),
            48_000,
            Path::new(&soundfont),
            &mut instances,
            None,
        )
        .expect("the panel's clip preview renders");
        assert!(!audio.is_empty(), "the preview carries audio");
    }
    assert_eq!(
        narrow_count(&state),
        0,
        "a human preview is not an agent window"
    );
}
