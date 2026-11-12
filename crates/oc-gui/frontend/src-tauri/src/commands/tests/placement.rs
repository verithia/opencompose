//! Fine placement preserves media attacks across editing and history.
use super::support::{TempDir, state_with_project};
use crate::commands::*;
use oc_dsl::{ast::BarBeat, timing::ProjectClock};

fn fixture() -> (TempDir, crate::state::AppState) {
    let dir = TempDir::new("clip-placement");
    std::fs::create_dir_all(dir.path()).unwrap();
    let state = state_with_project(
        r#"@project "Fine placement"
  tempo: 137

@track drum | instrument: audio/clip

@audio_clip hit [1:2 - 2:2] | track: drum
  file: "source.wav"
  placement_start: 1ms
  placement_end: 3ms
"#,
        dir.path(),
    );
    let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
    let samples: Vec<f32> = (0..rate * 3)
        .flat_map(|i| {
            let value = ((i * 17 % 997) as f32 / 997.0 - 0.5) * 0.25;
            [value, -value]
        })
        .collect();
    oc_engine::wav::write_stereo_f32_wav(dir.path().join("source.wav"), &samples, rate).unwrap();
    (dir, state)
}

fn render(inner: &crate::state::AppStateInner) -> Vec<f32> {
    oc_engine::render::render_to_buffer_with_plugins_at(
        &inner.project,
        std::path::Path::new(""),
        inner.audio.audio_io.spec().sample_rate,
        &mut std::collections::HashMap::new(),
        inner.file_path.as_deref().and_then(std::path::Path::parent),
    )
    .unwrap()
}

#[test]
fn audio_clip_placement_nudges_and_absolute_start_preserve_duration_source_and_undo() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    inner.project.properties.tempo_map = vec![oc_dsl::ast::TempoPoint {
        position: BarBeat::new(2, 1, 0),
        offset_quarters: 0.0,
        bpm: 80.0,
    }];
    let original = inner.project.clone();
    let clock = ProjectClock::new(&inner.project.properties).unwrap();
    let old = clock
        .audio_clip_range_seconds(inner.project.audio_clip(0).unwrap())
        .unwrap();
    set_audio_clip_placement_inner(&mut inner, 0, None, Some(-1.0)).unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.offset_seconds, 0.0);
    let moved = clock.audio_clip_range_seconds(clip).unwrap();
    assert!((moved.0 - old.0 + 0.001).abs() < 1e-12);
    assert!(((moved.1 - moved.0) - (old.1 - old.0)).abs() < 1e-12);
    set_audio_clip_placement_inner(&mut inner, 0, Some(8.0), None).unwrap();
    let moved = clock
        .audio_clip_range_seconds(inner.project.audio_clip(0).unwrap())
        .unwrap();
    assert!((moved.0 - 8.0).abs() < 1e-12);
    assert!(((moved.1 - moved.0) - (old.1 - old.0)).abs() < 1e-12);
    let inner = &mut *inner;
    inner.undo.undo(&mut inner.project).unwrap();
    inner.undo.undo(&mut inner.project).unwrap();
    assert_eq!(inner.project, original);
}

#[test]
fn audio_clip_placement_refuses_invalid_edits_without_history() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    let original = inner.project.clone();
    for (start, nudge) in [
        (None, None),
        (Some(1.0), Some(1.0)),
        (Some(-1.0), None),
        (Some(f64::NAN), None),
        (None, Some(f64::INFINITY)),
        (None, Some(-5000.0)),
    ] {
        assert!(set_audio_clip_placement_inner(&mut inner, 0, start, nudge).is_err());
        assert_eq!(inner.project, original);
        assert!(inner.undo.undo.is_empty());
    }
}

#[test]
fn audio_clip_placement_split_preserves_pcm_at_the_live_render_rate() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    let before = render(&inner);
    split_audio_clip_inner(&mut inner, 0, BarBeat::new(1, 4, 0)).unwrap();
    assert_eq!(
        inner
            .project
            .audio_clip(0)
            .unwrap()
            .placement
            .start_offset_seconds,
        0.001
    );
    assert_eq!(
        inner
            .project
            .audio_clip(0)
            .unwrap()
            .placement
            .end_offset_seconds,
        0.0
    );
    assert_eq!(
        inner
            .project
            .audio_clip(1)
            .unwrap()
            .placement
            .start_offset_seconds,
        0.0
    );
    assert_eq!(
        inner
            .project
            .audio_clip(1)
            .unwrap()
            .placement
            .end_offset_seconds,
        0.003
    );
    assert_eq!(
        render(&inner),
        before,
        "split must not skip or repeat a source sample"
    );
}

#[test]
fn audio_clip_placement_trim_same_anchor_clears_only_the_explicit_edge() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    let range = inner.project.audio_clip(0).unwrap().time_range;
    trim_audio_clip_edge_inner(&mut inner, 0, range.start, range.end, Some("end")).unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.placement.start_offset_seconds, 0.001);
    assert_eq!(clip.placement.end_offset_seconds, 0.0);
    assert_eq!(clip.offset_seconds, 0.0);
}

#[test]
fn audio_clip_placement_stretch_preserves_head_and_clears_tail_residual() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    set_audio_clip_playback_inner(
        &mut inner,
        0,
        AudioClipPlayback {
            stretch_to: Some(BarBeat::new(3, 1, 0)),
            ..Default::default()
        },
    )
    .unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.placement.start_offset_seconds, 0.001);
    assert_eq!(clip.placement.end_offset_seconds, 0.0);
    assert_eq!(clip.time_range.end, BarBeat::new(3, 1, 0));
}

#[test]
fn audio_clip_placement_short_clip_can_move_with_shared_musical_anchors() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.time_range.end = clip.time_range.start;
    clip.placement.end_offset_seconds = 0.003;
    move_audio_clip_inner(&mut inner, 0, BarBeat::new(2, 1, 0), None).unwrap();
    let clip = inner.project.audio_clip(0).unwrap();
    assert_eq!(clip.time_range.start, clip.time_range.end);
    let clock = ProjectClock::new(&inner.project.properties).unwrap();
    let (start, end) = clock.audio_clip_range_seconds(clip).unwrap();
    assert!((end - start - 0.002).abs() < 1e-12);
}

#[test]
fn audio_clip_placement_absolute_zero_does_not_fail_from_cancellation() {
    for (tempo, beat, residual) in [(123.0, 3, 0.1), (137.0, 2, 1.2345)] {
        let (_dir, state) = fixture();
        let mut inner = state.lock_inner();
        inner.project.properties.tempo = tempo;
        let clip = inner.project.audio_clip_mut(0).unwrap();
        clip.time_range.start = BarBeat::new(1, beat, 0);
        clip.time_range.end = BarBeat::new(4, 1, 0);
        clip.placement.start_offset_seconds = residual;
        clip.placement.end_offset_seconds = residual;
        let clock = ProjectClock::new(&inner.project.properties).unwrap();
        let (start, end) = clock
            .audio_clip_range_seconds(inner.project.audio_clip(0).unwrap())
            .unwrap();
        set_audio_clip_placement_inner(&mut inner, 0, Some(0.0), None).unwrap();
        let clip = inner.project.audio_clip(0).unwrap();
        let moved = clock.audio_clip_range_seconds(clip).unwrap();
        assert_eq!(moved.0, 0.0);
        assert!((moved.1 - (end - start)).abs() < 1e-12);
        assert_eq!(clip.offset_seconds, 0.0);
        let inner = &mut *inner;
        inner.undo.undo(&mut inner.project).unwrap();
        set_audio_clip_placement_inner(inner, 0, None, Some(-start * 1000.0)).unwrap();
        assert_eq!(
            clock
                .audio_clip_range_seconds(inner.project.audio_clip(0).unwrap())
                .unwrap()
                .0,
            0.0
        );
    }
}

#[test]
fn audio_clip_placement_split_refuses_a_half_that_quantizes_to_zero_frames() {
    let (_dir, state) = fixture();
    let mut inner = state.lock_inner();
    inner.project.properties.tempo = 120.0;
    let rate = inner.audio.audio_io.spec().sample_rate;
    let clip = inner.project.audio_clip_mut(0).unwrap();
    clip.time_range.start = BarBeat::new(1, 1, 0);
    clip.placement.start_offset_seconds = 0.5 - 0.1 / f64::from(rate);
    let original = inner.project.clone();
    let error = split_audio_clip_inner(&mut inner, 0, BarBeat::new(1, 2, 0)).unwrap_err();
    assert!(error.contains("one rendered sample"), "{error}");
    assert_eq!(inner.project, original);
    assert!(inner.undo.undo.is_empty());
}
