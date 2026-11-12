//! Preview audio correctness against references computed outside the preview path.
//!
//! Every assertion here compares a preview against either a full `ocp render`
//! (the export path in `oc_engine::render`) or a known signal, so a preview that
//! renders something plausible but wrong (wrong rate, wrong window, wrong track
//! subset, stale cache, a block short) cannot pass.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use oc_dsl::ast::{BarBeat, ParamValue, Project};

use crate::commands::{RangeRenderResult, RangeRequest, render_range, render_range_for_agent};
use crate::state::AppState;

use super::support::TempDir;

/// Two tracks, a stateful master reverb and notes that started before the
/// window. Export format is f32 so the WAV is the internal mix, not quantised.
const MIXED: &str = r#"@ocp "0.2.0"

@project "preview exactness"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track low | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @4 ch0 note_off 72 0; }

@master
  effects:
    - reverb: decay=0.8s, predelay=10ms, damping=0.5, mix=50%
"#;

/// One oscillator at a known frequency and level, gated by one whole note.
const TONE: &str = r#"@ocp "0.2.0"

@project "known signal"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: { frequency: 440, amplitude: 0.2, waveform: 0 }

@track tone | preset: tone, instrument: synth/lead_square
  @phrase hold [1:1:0 - 2:1:0]
    C4 w mf
"#;

fn parsed(source: &str) -> Project {
    oc_dsl::parse(source).expect("fixture parses")
}

fn app_state(source: &str, rate: u32) -> Arc<AppState> {
    let state = AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = parsed(source);
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.audio.sample_rate = rate;
    }
    Arc::new(state)
}

/// The `ocp render` path: a one-shot whole-project render, not the range cache.
fn export_reference(project: &Project, rate: u32, minimum_duration: f64) -> Vec<f32> {
    oc_engine::render::render_full_for_export(
        project,
        Path::new(""),
        rate,
        0.0,
        &mut HashMap::new(),
        None,
        minimum_duration,
        0.0,
    )
    .expect("reference export")
    .0
}

/// Read a written preview as interleaved f32 plus its header rate.
fn read_preview(path: &str) -> (u32, Vec<f32>) {
    let buffer = oc_engine::analysis::read_wav(path).expect("preview WAV reads");
    assert_eq!(buffer.channels.len(), 2, "a preview is stereo");
    let frames = buffer.frames();
    let mut interleaved = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        interleaved.push(buffer.channels[0][frame] as f32);
        interleaved.push(buffer.channels[1][frame] as f32);
    }
    (buffer.sample_rate, interleaved)
}

/// Timeline reference: the request's own timeline reversed into sample bounds.
fn slice_padded(buffer: &[f32], start: usize, frames: usize) -> Vec<f32> {
    let mut output = vec![0.0; frames * 2];
    let available = buffer.len() / 2;
    if start < available {
        let take = frames.min(available - start);
        output[..take * 2].copy_from_slice(&buffer[start * 2..(start + take) * 2]);
    }
    output
}

fn window_out(
    state: &AppState,
    dir: &TempDir,
    name: &str,
    start: f64,
    end: f64,
) -> RangeRenderResult {
    render_range(
        dir.path().join(name).display().to_string(),
        RangeRequest {
            start_s: Some(start),
            end_s: Some(end),
            ..Default::default()
        },
        state,
    )
    .expect("window renders")
}

fn max_abs_difference(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

fn positive_crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count()
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

// ---------------------------------------------------------------------------
// 1. Exactness against the full render
// ---------------------------------------------------------------------------

#[test]
fn a_full_mix_window_is_an_exact_slice_of_the_exported_render() {
    let rate = 48_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-full-window");
    std::fs::create_dir_all(dir.path()).unwrap();
    let project = parsed(MIXED);
    let reference = export_reference(&project, rate, 0.0);

    // Both notes start before 1.0 s; the high note ends at 1.5 s inside the
    // window, and the reverb tail continues past it.
    let rendered = window_out(&state, &dir, "full.wav", 1.0, 2.0);
    let (header_rate, audio) = read_preview(&rendered.path);
    let expected = slice_padded(&reference, rate as usize, rate as usize);
    let difference = max_abs_difference(&audio, &expected);

    assert_eq!(
        difference, 0.0,
        "a full-mix window must be the exported render's own samples (max abs {difference})"
    );
    assert_eq!(header_rate, rate);
    assert_eq!(rendered.sample_rate, rate);
    assert_eq!(rendered.frames, rate as usize);
    assert_eq!(rendered.duration_s, 1.0);
    assert_eq!(rendered.tracks, vec![0, 1]);
    assert_eq!(
        rendered.window, None,
        "a full-mix window has no subset path"
    );
    assert!(!rendered.cache_hit, "the first window has no warm cache");

    // The same window again is served from the cached full render, bit for bit.
    let repeat = window_out(&state, &dir, "repeat.wav", 1.0, 2.0);
    let (_, again) = read_preview(&repeat.path);
    assert!(repeat.cache_hit, "a warm repeat must report the cache hit");
    assert_eq!(again, audio, "the repeat window differs from the first");
}

#[test]
fn a_subset_window_equals_the_full_mix_with_the_other_tracks_muted() {
    let rate = 48_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-subset");
    std::fs::create_dir_all(dir.path()).unwrap();

    // Reference: the same project with the other track muted, rendered whole.
    let mut muted = parsed(MIXED);
    muted.tracks[0].mix.mute = true;
    let reference = export_reference(&muted, rate, 0.0);
    let expected = slice_padded(&reference, rate as usize, rate as usize);

    // Warm the cache with a full-mix window first, so this is the snapshot path.
    window_out(&state, &dir, "warm.wav", 1.0, 2.0);
    let subset = render_range(
        dir.path().join("subset.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            tracks: Some(vec![1]),
            ..Default::default()
        },
        &state,
    )
    .expect("subset window renders");
    let (_, audio) = read_preview(&subset.path);
    let difference = max_abs_difference(&audio, &expected);
    assert_eq!(
        difference, 0.0,
        "a subset must be the full mix with the other tracks muted (max abs {difference})"
    );
    assert_eq!(subset.tracks, vec![1], "the effective selection is track 1");
    assert!(
        matches!(subset.window, Some("snapshot" | "recomputed")),
        "a warm subset window must come from the cached timeline, got {:?}",
        subset.window
    );
    assert!(
        subset.cache_hit,
        "a window cut from the clean cached timeline must report the cache hit, \
         got window={:?}",
        subset.window
    );

    // It is not the full mix: the muted track contributes real samples.
    let (_, full) = read_preview(&dir.path().join("warm.wav").display().to_string());
    assert!(
        max_abs_difference(&full, &audio) > 1e-3,
        "the subset mix is indistinguishable from the full mix"
    );

    // A cold subset (no cache at all) must land on the same audio.
    let cold_state = app_state(MIXED, rate);
    let cold = render_range(
        dir.path().join("cold.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            tracks: Some(vec![1]),
            ..Default::default()
        },
        &cold_state,
    )
    .expect("cold subset renders");
    let (_, cold_audio) = read_preview(&cold.path);
    assert_eq!(cold.window, Some("cold"));
    assert_eq!(
        max_abs_difference(&cold_audio, &expected),
        0.0,
        "a cold subset must match the muted full render too"
    );
}

#[test]
fn a_subset_of_every_track_is_the_full_mix_again() {
    let rate = 8_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-subset-all");
    std::fs::create_dir_all(dir.path()).unwrap();
    let project = parsed(MIXED);
    let reference = export_reference(&project, rate, 0.0);

    let rendered = render_range(
        dir.path().join("all.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            tracks: Some(vec![0, 1, 1]),
            ..Default::default()
        },
        &state,
    )
    .expect("all-track subset renders");
    let (_, audio) = read_preview(&rendered.path);
    let expected = slice_padded(&reference, rate as usize, rate as usize);
    assert_eq!(
        max_abs_difference(&audio, &expected),
        0.0,
        "selecting every track must reproduce the full mix"
    );
    assert_eq!(rendered.tracks, vec![0, 1]);
    assert_eq!(rendered.window, None, "every track is the full-mix path");
}

// ---------------------------------------------------------------------------
// 2. Known signal
// ---------------------------------------------------------------------------

#[test]
fn a_known_sine_keeps_its_frequency_level_and_length() {
    // 440 Hz, amplitude 0.2, centred mono track: per-channel level is
    // 0.2 * cos(pi/4) / sqrt(2) = 0.1 RMS. The window is half a second of a
    // note that is active for two seconds from frame 0.
    for rate in [44_100, 48_000] {
        let state = app_state(TONE, rate);
        let dir = TempDir::new("preview-sine");
        std::fs::create_dir_all(dir.path()).unwrap();
        let rendered = window_out(&state, &dir, "sine.wav", 0.5, 1.0);
        let (header_rate, audio) = read_preview(&rendered.path);
        assert_eq!(header_rate, rate);
        assert_eq!(rendered.sample_rate, rate);
        assert_eq!(
            rendered.frames,
            rate as usize / 2,
            "the window is half a second"
        );

        let left: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[0]).collect();
        let right: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[1]).collect();
        assert_eq!(
            left, right,
            "a centred mono source must be channel-identical"
        );

        // 440 Hz over 0.5 s is 220 cycles; count the rising zero crossings.
        let crossings = positive_crossings(&left);
        assert!(
            crossings.abs_diff(220) <= 1,
            "expected 220 cycles at 440 Hz in 0.5 s at {rate} Hz, counted {crossings}"
        );
        let level = rms(&left);
        assert!(
            (level - 0.1).abs() < 0.002,
            "expected 0.1 RMS per channel at {rate} Hz, measured {level:.6}"
        );
        let peak = left
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            (peak - 0.141_421).abs() < 0.003,
            "expected a 0.1414 peak per channel at {rate} Hz, measured {peak:.6}"
        );

        // The window is still the exported timeline, not a separate mix.
        let reference = export_reference(&parsed(TONE), rate, 0.0);
        let start = (0.5 * f64::from(rate)).round() as usize;
        let expected = slice_padded(&reference, start, rendered.frames);
        assert_eq!(
            max_abs_difference(&audio, &expected),
            0.0,
            "the sine window differs from the exported render at {rate} Hz"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Rate honesty
// ---------------------------------------------------------------------------

#[test]
fn the_reported_rate_the_wav_header_and_the_frame_count_agree() {
    let dir = TempDir::new("preview-rates");
    std::fs::create_dir_all(dir.path()).unwrap();
    let mut seen = Vec::new();
    for rate in [44_100, 48_000] {
        let state = app_state(TONE, rate);
        let rendered = window_out(&state, &dir, &format!("rate-{rate}.wav"), 0.5, 1.5);
        let (header_rate, audio) = read_preview(&rendered.path);
        assert_eq!(
            rendered.sample_rate, rate,
            "the result must report the render rate"
        );
        assert_eq!(
            header_rate, rate,
            "the WAV header disagrees with the result"
        );
        assert_eq!(audio.len(), rendered.frames * 2);
        assert_eq!(rendered.frames, rate as usize, "1.0 s of audio");
        assert!(
            (rendered.duration_s - 1.0).abs() < 1e-12,
            "duration must be frames / rate, got {}",
            rendered.duration_s
        );
        assert!((rendered.end_s - rendered.start_s - 1.0).abs() < 1e-9);
        assert_eq!(
            rendered.start_s, 0.5,
            "the window must start where it was asked to"
        );
        // The same musical window covers the same duration at either rate.
        seen.push((rate, rendered.frames as f64 / f64::from(rate)));
    }
    assert_eq!(
        seen[0].1, seen[1].1,
        "the window duration moved with the rate"
    );
}

// ---------------------------------------------------------------------------
// 4. Cache correctness
// ---------------------------------------------------------------------------

#[test]
fn an_edit_between_windows_is_reflected_and_cache_hit_is_honest() {
    let rate = 8_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-cache-edit");
    std::fs::create_dir_all(dir.path()).unwrap();

    let before = window_out(&state, &dir, "before.wav", 1.0, 2.0);
    let (_, before_audio) = read_preview(&before.path);
    assert!(!before.cache_hit);
    assert_eq!(before.window, None);

    // An identical request is served from the cache and says so.
    let cached = window_out(&state, &dir, "cached.wav", 1.0, 2.0);
    let (_, cached_audio) = read_preview(&cached.path);
    assert!(cached.cache_hit);
    assert_eq!(cached_audio, before_audio);

    // A committed mixer edit bumps the revision, exactly like `update_track_mix`.
    {
        let mut inner = state.lock_inner();
        inner.project.tracks[0].mix.volume_db = -12.0;
        inner.bump_revision();
    }
    let edited = window_out(&state, &dir, "edited.wav", 1.0, 2.0);
    let (_, edited_audio) = read_preview(&edited.path);
    assert!(
        !edited.cache_hit,
        "a window after an edit must not report the previous render's cache"
    );

    // The reference is the edited project's own export, not the old window.
    let reference = export_reference(&state.lock_inner().project.clone(), rate, 0.0);
    let expected = slice_padded(&reference, rate as usize, rate as usize);
    assert_eq!(
        max_abs_difference(&edited_audio, &expected),
        0.0,
        "the post-edit window does not reflect the edit"
    );
    assert!(
        max_abs_difference(&edited_audio, &before_audio) > 1e-3,
        "the volume edit never reached the second window"
    );

    // The replaced cache is warm for the new revision.
    let after = window_out(&state, &dir, "after.wav", 1.0, 2.0);
    assert!(after.cache_hit);
}

// ---------------------------------------------------------------------------
// 5. Edges and refusals
// ---------------------------------------------------------------------------

#[test]
fn windows_at_the_edges_are_exact_and_bad_windows_are_refused() {
    let rate = 8_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-edges");
    std::fs::create_dir_all(dir.path()).unwrap();
    let project = parsed(MIXED);
    let reference = export_reference(&project, rate, 0.0);
    let horizon = reference.len() / 2;

    // A window that starts inside the reverb tail after every note and runs
    // past the rendered length: exact head samples, zero padding, exact length.
    let start = horizon + rate as usize / 2;
    let rendered = window_out(&state, &dir, "past.wav", start as f64 / f64::from(rate), {
        (start + rate as usize) as f64 / f64::from(rate)
    });
    let (_, audio) = read_preview(&rendered.path);
    assert_eq!(
        audio.len(),
        rate as usize * 2,
        "the length is exactly end - start"
    );
    assert!(
        audio.iter().all(|sample| *sample == 0.0),
        "a window past the media must be silence"
    );
    assert_eq!(rendered.window, None);

    // A window that straddles the end of the media: the head is the timeline,
    // the tail is silence, never a re-render.
    let half = horizon / 2;
    let end = horizon + rate as usize;
    let straddle = window_out(
        &state,
        &dir,
        "straddle.wav",
        half as f64 / f64::from(rate),
        end as f64 / f64::from(rate),
    );
    let (_, straddle_audio) = read_preview(&straddle.path);
    let expected = slice_padded(&reference, half, end - half);
    assert_eq!(max_abs_difference(&straddle_audio, &expected), 0.0);

    // Zero-length, reversed, negative and over-long windows are refused, not
    // clamped into something plausible.
    for request in [
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        RangeRequest {
            start_s: Some(2.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        RangeRequest {
            start_s: Some(-1.0),
            end_s: Some(1.0),
            ..Default::default()
        },
        RangeRequest {
            start_s: Some(0.0),
            end_s: Some(601.0),
            ..Default::default()
        },
        RangeRequest {
            start_s: Some(0.0),
            end_s: None,
            start_bar: Some(1),
            ..Default::default()
        },
        RangeRequest {
            start_bar: Some(0),
            end_bar: Some(2),
            ..Default::default()
        },
        RangeRequest {
            start_bar: Some(2),
            end_bar: Some(2),
            ..Default::default()
        },
        RangeRequest {
            start_bar: Some(3),
            end_bar: Some(2),
            ..Default::default()
        },
    ] {
        let result = render_range(
            dir.path().join("refused.wav").display().to_string(),
            request,
            &state,
        );
        assert!(result.is_err(), "a bad window was accepted: {result:?}");
    }
    assert!(
        !dir.path().join("refused.wav").exists(),
        "a refused window wrote a file"
    );

    // A zero-length bar window and a reversed one are refused before writing.
    assert!(
        render_range(
            dir.path().join("bars.wav").display().to_string(),
            RangeRequest {
                start_bar: Some(1),
                end_bar: Some(2),
                ..Default::default()
            },
            &state,
        )
        .is_ok()
    );
    let bar = render_range(
        dir.path().join("bars2.wav").display().to_string(),
        RangeRequest {
            start_bar: Some(1),
            end_bar: Some(2),
            ..Default::default()
        },
        &state,
    )
    .unwrap();
    // 120 BPM, 4/4: one bar is 2 s, resolved through the project clock.
    assert_eq!(bar.start_s, 0.0);
    assert_eq!(bar.end_s, 2.0);
    assert_eq!(bar.frames, 2 * rate as usize);
    assert_eq!(bar.start_bar, Some(1));
    assert_eq!(bar.end_bar, Some(2));
}

#[test]
fn the_tracks_field_reports_the_effective_selection_in_every_branch() {
    let rate = 8_000;
    let dir = TempDir::new("preview-tracks-field");
    std::fs::create_dir_all(dir.path()).unwrap();

    // The session's report: a one-track project with no subset asked for. The
    // docs define `tracks` as the effective selection, so it must list the one
    // track even though the full mix was rendered.
    let one = app_state(
        r#"@ocp "0.2.0"
@project "one track"
  tempo: 120
@preset lead
  source: builtin
  plugin: "builtin.open_synth"
@track only | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 60 90; @3 ch0 note_off 60 0; }
"#,
        rate,
    );
    let full = window_out(&one, &dir, "one.wav", 0.0, 1.0);
    assert_eq!(full.tracks, vec![0]);
    assert_eq!(full.window, None);

    // A two-track project: full mix lists both, a subset lists only its own.
    let two = app_state(MIXED, rate);
    let full = window_out(&two, &dir, "two.wav", 0.0, 1.0);
    assert_eq!(full.tracks, vec![0, 1]);
    let subset = render_range(
        dir.path().join("subset-tracks.wav").display().to_string(),
        RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            tracks: Some(vec![1, 1, 1]),
            ..Default::default()
        },
        &two,
    )
    .unwrap();
    assert_eq!(subset.tracks, vec![1], "duplicates must collapse");
    assert_eq!(
        subset.window,
        Some("recomputed"),
        "the first subset has no snapshot"
    );

    // A subset that names every track is the full-mix path and lists them all.
    let all = render_range(
        dir.path().join("subset-all.wav").display().to_string(),
        RangeRequest {
            start_s: Some(0.0),
            end_s: Some(1.0),
            tracks: Some(vec![1, 0]),
            ..Default::default()
        },
        &two,
    )
    .unwrap();
    assert_eq!(all.tracks, vec![0, 1]);
    assert_eq!(all.window, None);
}

// ---------------------------------------------------------------------------
// 6. The clip preview path (`preview_block` -> `render_block_window`)
// ---------------------------------------------------------------------------

#[test]
fn a_clip_preview_matches_the_soloed_timeline_from_the_block_start() {
    let rate = 8_000;
    let project = parsed(MIXED);
    // Block [1:1, 2:1) starts at frame 0, so "from reset at the block start" and
    // "the timeline slice" are the same request.
    let buffer = crate::audio::AudioState::render_block_window(
        &project,
        1,
        BarBeat::new(1, 1, 0)..BarBeat::new(2, 1, 0),
        rate,
        Path::new(""),
        &mut HashMap::new(),
        None,
    )
    .expect("clip preview renders");
    let frames = buffer.len() / 2;
    // One bar at 120 BPM is 2 s; the preview adds its 4 s ring-out. The length
    // is exactly that sum, with no padding past it and no truncation short of it.
    assert_eq!(
        frames,
        rate as usize * 6,
        "the clip preview length is the block plus the ring-out"
    );

    // Reference: the whole project with the other track muted, over at least the
    // preview length, sliced from frame 0.
    let mut muted = project.clone();
    muted.tracks[0].mix.mute = true;
    let reference = export_reference(&muted, rate, frames as f64 / f64::from(rate));
    let expected = slice_padded(&reference, 0, frames);
    assert_eq!(
        max_abs_difference(&buffer, &expected),
        0.0,
        "a clip preview from the block start must be the soloed timeline"
    );
}

#[test]
fn an_audio_clip_preview_keeps_processing_and_silences_overlapping_siblings() {
    let rate = 8_000;
    let directory = TempDir::new("isolated-audio-preview");
    std::fs::create_dir_all(directory.path()).unwrap();
    for (file, frequency, amplitude) in [
        ("chosen.wav", 173.0_f32, 0.2_f32),
        ("other.wav", 379.0_f32, 0.6_f32),
    ] {
        let samples: Vec<_> = (0..rate * 2)
            .flat_map(|frame| {
                let phase = std::f32::consts::TAU * frequency * frame as f32 / rate as f32;
                [amplitude * phase.sin(); 2]
            })
            .collect();
        oc_engine::wav::write_stereo_f32_wav(directory.path().join(file), &samples, rate).unwrap();
    }
    let target_only = r#"@project "Audio preview"
  tempo: 120
  time_signature: 4/4
@audio_clip chosen [2:1 - 3:1] | track: tape
  file: "chosen.wav"
  gain: -6.0206dB
@track tape | instrument: audio/clip
"#;
    let overlapped = format!(
        "{target_only}\n@audio_clip other [2:1 - 3:1] | track: tape\n  file: \"other.wav\"\n"
    );
    let project = parsed(&overlapped);
    let start = BarBeat::new(2, 1, 0);
    let end = BarBeat::new(3, 1, 0);
    let preview = crate::commands::transport::isolated_audio_clip_preview_project(
        &project, 0, start, end, "chosen",
    )
    .unwrap();
    assert!(!project.audio_clip(1).unwrap().muted);
    assert!(preview.audio_clip(1).unwrap().muted);
    assert!(
        crate::commands::transport::isolated_audio_clip_preview_project(
            &project,
            0,
            BarBeat::new(1, 1, 0),
            end,
            "chosen",
        )
        .is_err(),
        "a stale range must not preview whichever clip is now there"
    );

    let render = |project: &Project| {
        use crate::render_thread::{RenderCommand, RenderThreadHandle};
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let (start_frame, end_frame) =
            crate::audio::block_preview_sample_range(project, 0, start, end, rate).unwrap();
        let project =
            oc_engine::block_source::prepare_project(project, Some(directory.path())).unwrap();
        let worker = RenderThreadHandle::spawn(rate);
        worker.send(RenderCommand::ProjectChanged {
            root: None,
            project: Box::new(project),
            soundfont_path: Default::default(),
            warm: false,
        });
        worker.send(RenderCommand::Play {
            start_frame,
            resume: false,
            end_frame: Some(end_frame),
        });
        worker.playback.active.store(true, Ordering::Release);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let playback = &worker.playback;
        while output.len() < rate as usize {
            let mut block = [0.0; 128];
            let seek_pending = playback.seek_generation.load(Ordering::Acquire)
                != playback.callback_generation.load(Ordering::Acquire);
            // Wait for a complete block so scheduler speed cannot insert
            // underrun fades into the PCM oracle.
            // SAFETY: this test thread is the sole consumer; the render worker
            // only writes through the independent producer endpoint.
            let queued = unsafe { (&*playback.consumer_ptr()).slots() };
            if seek_pending || queued >= block.len() {
                let before = playback.timeline_frames();
                crate::audio::streaming_audio_callback(&mut block, playback);
                let after = playback.timeline_frames();
                if before >= start_frame && after == before + (block.len() / 2) as u64 {
                    output.extend_from_slice(&block);
                    continue;
                }
            }
            assert!(Instant::now() < deadline, "{:?}", worker.load_error());
            std::thread::sleep(Duration::from_millis(1));
        }
        output
    };
    let expected = render(&parsed(target_only));
    let actual = render(&preview);
    assert!(rms(&actual[..rate as usize]) > 0.01);
    assert_eq!(max_abs_difference(&actual, &expected), 0.0);
    assert!(max_abs_difference(&render(&project), &expected) > 0.25);
}

#[test]
fn a_clip_preview_that_starts_inside_a_note_starts_from_silence() {
    // The documented difference between a clip preview and a range window: a
    // block preview resets at the block, so a note that began in an earlier
    // block is not resumed. Pin it so a later change is a visible decision.
    let rate = 8_000;
    let project = parsed(MIXED);
    let buffer = crate::audio::AudioState::render_block_window(
        &project,
        0,
        // 0.5 s, inside the low note (0 s .. 4 s).
        BarBeat::new(2, 1, 0)..BarBeat::new(3, 1, 0),
        rate,
        Path::new(""),
        &mut HashMap::new(),
        None,
    )
    .unwrap();
    let head = &buffer[..rate as usize / 4 * 2];
    let level = rms(head);
    assert!(
        level < 1e-6,
        "a block preview resumed a note from before the block (RMS {level:.6})"
    );
}

// ---------------------------------------------------------------------------
// 7. The A/B audition path
// ---------------------------------------------------------------------------

#[test]
fn the_ab_audition_take_is_the_projects_own_render() {
    let rate = 8_000;
    let project = parsed(TONE);
    let state = app_state(TONE, rate);
    {
        let mut inner = state.lock_inner();
        inner.audio.sample_rate = rate;
    }
    let context = crate::audition::candidate_render_context(&state);
    let name = "A candidate";
    let params = vec![
        (
            "output.gain".to_owned(),
            oc_dsl::ast::ParamValue::Number(0.5),
        ),
        (
            "osc.a.waveform".to_owned(),
            oc_dsl::ast::ParamValue::Number(1.0),
        ),
    ];
    let rendered =
        crate::audition::render_candidate(&context, name, &params, &[]).expect("candidate renders");

    // Reference: the same prepared audition project through the export path.
    let mut audition = project.clone();
    audition.presets.push(oc_dsl::ast::PresetDef {
        name: name.to_owned(),
        source: Some("builtin".into()),
        plugin: Some(oc_synth::synth::PLUGIN_ID.into()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params,
        effects: Vec::new(),
        extends: None,
    });
    let prepared =
        crate::audio::prepare_preset_audition(&audition, name, None).expect("audition project");
    let reference = export_reference(&prepared, rate, 0.0);

    assert!(!rendered.is_empty(), "the A/B render is empty");
    assert_eq!(
        max_abs_difference(&rendered, &reference),
        0.0,
        "the A/B audition take is not the project's own render"
    );
}

// ---------------------------------------------------------------------------
// 8. What a preview is not: the export's loudness normalisation
// ---------------------------------------------------------------------------

#[test]
fn a_loudness_target_changes_the_export_but_not_the_preview() {
    // `ocp render` normalises to the declared target; `render_range` returns the
    // pre-normalisation mix. Pin the divergence so nobody claims they are equal.
    let rate = 8_000;
    let source = r#"@ocp "0.2.0"

@project "normalised"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track low | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @4 ch0 note_off 72 0; }

@master
  loudness: -23dB
  effects:
    - reverb: decay=0.8s, predelay=10ms, damping=0.5, mix=50%
"#;
    let project = oc_dsl::parse(source).expect("loudness fixture parses");
    assert_eq!(
        project
            .master
            .as_ref()
            .and_then(|master| master.loudness_target),
        Some(-23.0)
    );
    let state = {
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = project.clone();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.audio.sample_rate = rate;
        }
        Arc::new(state)
    };
    let dir = TempDir::new("preview-loudness");
    std::fs::create_dir_all(dir.path()).unwrap();
    let rendered = window_out(&state, &dir, "loud.wav", 1.0, 2.0);
    let (_, audio) = read_preview(&rendered.path);

    let exported = export_reference(&project, rate, 0.0);
    let expected = slice_padded(&exported, rate as usize, rate as usize);
    let difference = max_abs_difference(&audio, &expected);
    assert!(
        difference > 1e-3,
        "the preview unexpectedly matches the normalised export"
    );
    // The preview is the raw mix: it equals a render whose master has no target.
    let mut raw = project.clone();
    raw.master.as_mut().unwrap().loudness_target = None;
    let raw_reference = export_reference(&raw, rate, 0.0);
    let raw_expected = slice_padded(&raw_reference, rate as usize, rate as usize);
    assert_eq!(
        max_abs_difference(&audio, &raw_expected),
        0.0,
        "the preview is not the pre-normalisation mix"
    );
}

// ---------------------------------------------------------------------------
// 9. The pattern (clip) audition path
// ---------------------------------------------------------------------------

#[test]
fn a_pattern_preview_starts_at_the_pattern_not_at_the_project_start() {
    // A pattern first placed at bar 5 must audition from its own start; the
    // audition otherwise opens with eight seconds of silence.
    let rate = 8_000;
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "late pattern"
  tempo: 120
  time_signature: 4/4
@preset native
  source: builtin
  plugin: "builtin.open_synth"
@pattern hook | preset: native, instrument: synth/lead_square
  events:
    C4 q mf
    E4 q mf
@track synth | preset: native, instrument: synth/lead_square
  clip: hook [5:1:0 - 5:3:0]
"#,
    )
    .unwrap();
    let audition =
        crate::audio::pattern_audition_project(&project, "hook").expect("pattern audition");
    let buffer = crate::audio::AudioState::render_prepared_audition(&audition, rate, None, None)
        .expect("pattern audition renders");
    let first_sounding = buffer
        .chunks_exact(2)
        .position(|frame| frame[0].abs() > 1e-3 || frame[1].abs() > 1e-3)
        .expect("the audition sounds");
    assert!(
        first_sounding < rate as usize / 10,
        "the pattern audition has a silent lead-in of {first_sounding} frames"
    );

    // It is the pattern's own audio: the same events rendered at the pattern's
    // placement inside the full project, from the pattern's first note.
    let full = export_reference(&project, rate, 0.0);
    let start = 8 * rate as usize; // bar 5 = 8 s at 120 BPM
    let head = rate as usize / 2; // 0.5 s
    let expected = &full[start * 2..(start + head) * 2];
    let actual = &buffer[..head * 2];
    assert_eq!(
        max_abs_difference(actual, expected),
        0.0,
        "the pattern audition is not the pattern's timeline audio"
    );

    // It ends with the pattern, not with the whole arranged project.
    assert!(
        buffer.len() < full.len(),
        "the pattern audition rendered the whole project"
    );
}

// ---------------------------------------------------------------------------
// 10. Stems: the written files are the engine's own track taps
// ---------------------------------------------------------------------------

#[test]
fn range_and_audition_report_peaks_before_integer_clipping() {
    let rate = 8_000;
    let dir = TempDir::new("range-pre-encode");
    std::fs::create_dir_all(dir.path()).unwrap();
    oc_engine::wav::write_stereo_f32_wav(dir.path().join("loud.wav"), &vec![0.9_f32; 8_000], rate)
        .unwrap();
    let source = r#"@project "Range headroom"
  tempo: 120
  export: { format: i16 }
@audio_clip loud [1:1 - 2:1] | track: tape
  file: "loud.wav"
  gain: 6.0206dB
@track tape | instrument: audio/clip
"#;
    let state = app_state(source, rate);
    state.lock_inner().file_path = Some(dir.path().join("headroom.ocp"));
    let request = RangeRequest {
        start_s: Some(0.0),
        end_s: Some(0.5),
        stems: true,
        ..Default::default()
    };
    let integer = render_range(
        dir.path().join("integer.wav").display().to_string(),
        request.clone(),
        &state,
    )
    .unwrap();
    for report in [&integer.pre_encode, &integer.stems[0].pre_encode] {
        let peak = report.peak_dbfs.unwrap();
        assert!(peak > 0.5, "{report:?}");
        assert!(report.true_peak_dbfs.unwrap() >= peak - 0.01);
        assert!(report.above_full_scale && report.clipped);
    }
    assert!(
        read_preview(&integer.path)
            .1
            .iter()
            .all(|sample| sample.abs() <= 1.0)
    );

    let wrong_extension = render_range(
        dir.path().join("not-wav.flac").display().to_string(),
        RangeRequest {
            float_wav: true,
            ..request.clone()
        },
        &state,
    )
    .unwrap_err();
    assert!(wrong_extension.contains(".wav"));
    assert!(!dir.path().join("not-wav.flac").exists());

    let float = crate::commands::audition_range(
        Some(dir.path().join("float.wav").display().to_string()),
        RangeRequest {
            float_wav: true,
            ..request
        },
        crate::commands::AuditionChannel::Mono,
        rate,
        &state,
    )
    .unwrap();
    assert_eq!(float.pre_encode.peak_dbfs, integer.pre_encode.peak_dbfs);
    assert_eq!(
        float.pre_encode.true_peak_dbfs,
        integer.pre_encode.true_peak_dbfs
    );
    assert!(float.pre_encode.above_full_scale && !float.pre_encode.clipped);
    assert_eq!(
        hound::WavReader::open(&float.path)
            .unwrap()
            .spec()
            .sample_format,
        hound::SampleFormat::Float
    );
    let peak = read_preview(&float.path)
        .1
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 1.0);
    assert!((20.0 * f64::from(peak).log10() - integer.pre_encode.peak_dbfs.unwrap()).abs() < 0.001);

    let silence = window_out(&state, &dir, "silent.wav", 10.0, 10.1);
    assert!(silence.pre_encode.peak_dbfs.is_none());
    assert!(silence.pre_encode.true_peak_dbfs.is_none());
    assert!(!silence.pre_encode.above_full_scale && !silence.pre_encode.clipped);
}

#[test]
fn written_stems_are_the_engine_track_taps_for_the_window() {
    let rate = 8_000;
    let state = app_state(MIXED, rate);
    let dir = TempDir::new("preview-stems");
    std::fs::create_dir_all(dir.path()).unwrap();
    let project = parsed(MIXED);

    // Reference: the export path's own stem capture, at the normalisation point.
    let mut cache = oc_engine::render_cache::RenderCache::new();
    cache.capture_stems(true);
    cache
        .render_full(&project, Path::new(""), rate, &mut HashMap::new())
        .expect("reference render");
    let taps = cache.stems().expect("stems captured").tracks.clone();
    assert_eq!(taps.len(), 2);

    let rendered = render_range(
        dir.path().join("stems.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            tracks: Some(vec![0, 1]),
            stems: true,
            ..Default::default()
        },
        &state,
    )
    .expect("stem window renders");
    assert_eq!(rendered.stems.len(), 2, "one stem per selected track");
    for stem in &rendered.stems {
        let (name, samples) = &taps[stem.track_index];
        assert_eq!(&stem.name, name, "the stem file is named for its track");
        let (_, audio) = read_preview(&stem.path);
        let expected = &samples[rate as usize * 2..rate as usize * 4];
        assert_eq!(
            max_abs_difference(&audio, expected),
            0.0,
            "stem '{}' is not the engine's track tap for the window",
            stem.name
        );
        assert_eq!(audio.len(), rate as usize * 2, "a stem is end - start long");
    }

    // A subset writes only the selected tracks' stems.
    let subset = render_range(
        dir.path().join("subset-stems.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            tracks: Some(vec![1]),
            stems: true,
            ..Default::default()
        },
        &state,
    )
    .expect("subset stem window renders");
    assert_eq!(
        subset.stems.len(),
        1,
        "unselected tracks must not get a file"
    );
    assert_eq!(subset.stems[0].track_index, 1);
    assert!(Path::new(&subset.stems[0].path).is_file());
}

#[test]
fn the_sum_of_the_track_stems_is_the_master_mix() {
    // With no bus or master processing, the post-fader track taps must add up
    // to the master: this catches a stem written from the wrong buffer.
    let rate = 8_000;
    let source = r#"@ocp "0.2.0"

@project "stem sum"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track low | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @4 ch0 note_off 72 0; }
"#;
    let state = app_state(source, rate);
    let dir = TempDir::new("preview-stem-sum");
    std::fs::create_dir_all(dir.path()).unwrap();
    let rendered = render_range(
        dir.path().join("sum.wav").display().to_string(),
        RangeRequest {
            start_s: Some(1.0),
            end_s: Some(2.0),
            stems: true,
            ..Default::default()
        },
        &state,
    )
    .expect("stem window renders");
    let (_, master) = read_preview(&rendered.path);
    let mut sum = vec![0.0f32; master.len()];
    for stem in &rendered.stems {
        let (_, audio) = read_preview(&stem.path);
        for (total, sample) in sum.iter_mut().zip(&audio) {
            *total += sample;
        }
    }
    let difference = max_abs_difference(&sum, &master);
    assert!(
        difference < 1e-6,
        "the written stems do not add up to the master (max abs {difference})"
    );
}

// ---------------------------------------------------------------------------
// 7. A transaction's working state is what renders
// ---------------------------------------------------------------------------

/// Energy at `frequency` Hz over the window, from a single-bin DFT.
fn band_energy(samples: &[f32], sample_rate: u32, frequency: f64) -> f64 {
    let omega = std::f64::consts::TAU * frequency / f64::from(sample_rate);
    let frames = samples.len() / 2;
    let (mut real, mut imaginary) = (0.0, 0.0);
    for frame in 0..frames {
        let mono = 0.5 * (f64::from(samples[frame * 2]) + f64::from(samples[frame * 2 + 1]));
        let phase = omega * frame as f64;
        real += mono * phase.cos();
        imaginary -= mono * phase.sin();
    }
    (real * real + imaginary * imaginary).sqrt() / frames.max(1) as f64
}

/// Edit the fixture's tone the way a device-parameter mutation would.
fn retune_tone(state: &AppState, frequency: f64) {
    let mut inner = state.lock_inner();
    let preset = inner
        .project
        .presets
        .first_mut()
        .expect("the fixture has a preset");
    let slot = preset
        .params
        .iter_mut()
        .find(|(name, _)| name == "frequency")
        .expect("the tone preset carries a frequency");
    *slot = ("frequency".to_string(), ParamValue::Number(frequency));
    inner.dsl_source = oc_dsl::serialize(&inner.project);
    inner.bump_revision();
}

#[test]
fn a_window_renders_the_working_state_of_an_open_transaction() {
    let rate = 48_000;
    let state = app_state(TONE, rate);
    let dir = TempDir::new("preview-transaction-edit");
    std::fs::create_dir_all(dir.path()).unwrap();

    let before = window_out(&state, &dir, "before.wav", 0.0, 1.0);
    let (_, before_audio) = read_preview(&before.path);
    assert!(!before.cache_hit, "the first window has no warm cache");
    let low_before = band_energy(&before_audio, rate, 440.0);
    let high_before = band_energy(&before_audio, rate, 1760.0);
    assert!(
        low_before > 10.0 * high_before,
        "the fixture is not a 440 Hz tone: 440 {low_before}, 1760 {high_before}"
    );

    crate::agent_api::transaction_begin(&state, Some("probe".into()), "test").unwrap();
    retune_tone(&state, 1760.0);

    let during = window_out(&state, &dir, "during.wav", 0.0, 1.0);
    let (_, during_audio) = read_preview(&during.path);
    assert!(
        !during.cache_hit,
        "a window rendered after an edit inside the transaction answered from the pre-edit cache"
    );
    let low_during = band_energy(&during_audio, rate, 440.0);
    let high_during = band_energy(&during_audio, rate, 1760.0);
    assert!(
        high_during > 10.0 * low_during,
        "the window did not follow the edit inside the transaction: \
         440 {low_during}, 1760 {high_during}"
    );
    assert!(
        max_abs_difference(&before_audio, &during_audio) > 1e-3,
        "the in-transaction window is the pre-edit audio"
    );

    crate::agent_api::transaction_commit(&state, "test").unwrap();
    let committed = window_out(&state, &dir, "committed.wav", 0.0, 1.0);
    let (_, committed_audio) = read_preview(&committed.path);
    assert!(
        !committed.cache_hit,
        "the post-commit state must not reuse the in-transaction render"
    );
    assert_eq!(
        max_abs_difference(&during_audio, &committed_audio),
        0.0,
        "the committed window differs from the window rendered inside the batch"
    );
}

#[test]
fn a_rollback_serves_the_pre_transaction_window_again() {
    let rate = 48_000;
    let state = app_state(TONE, rate);
    let dir = TempDir::new("preview-transaction-rollback");
    std::fs::create_dir_all(dir.path()).unwrap();

    let before = window_out(&state, &dir, "before.wav", 0.0, 1.0);
    let (_, before_audio) = read_preview(&before.path);
    assert!(!before.cache_hit);

    crate::agent_api::transaction_begin(&state, Some("probe".into()), "test").unwrap();
    retune_tone(&state, 1760.0);
    let during = window_out(&state, &dir, "during.wav", 0.0, 1.0);
    let (_, during_audio) = read_preview(&during.path);
    assert!(!during.cache_hit, "the edit's render is real work");
    assert!(
        max_abs_difference(&before_audio, &during_audio) > 1e-3,
        "the in-transaction render is the pre-edit audio"
    );

    crate::agent_api::transaction_rollback(&state, "test").unwrap();
    let restored = window_out(&state, &dir, "restored.wav", 0.0, 1.0);
    let (_, restored_audio) = read_preview(&restored.path);
    assert!(
        restored.cache_hit,
        "a rollback threw away the render of the state it restored"
    );
    assert_eq!(
        restored_audio, before_audio,
        "the window after a rollback is not the pre-transaction window"
    );
}

/// A bus tap renders that return, not the master mix, and keeps the graph.
#[test]
fn a_bus_tap_renders_the_return_and_refuses_an_unknown_bus() {
    let source = r#"@ocp "0.4.0"

@project "bus tap"
  tempo: 120
  time_signature: 4/4
  export: { format: f32 }

@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 0, osc.a.level: 0.6, output.gain: 0.5}

@bus space
  routes: [master]
  effects:
    - eq: high_gain=-6dB

@track tone | preset: tone, instrument: synth/lead_square
  mix: vol=-6dB, send(space=0dB)
  @phrase hold [1:1:0 - 3:1:0]
    C4 w mf
"#;
    let state = app_state(source, 48_000);
    let dir = TempDir::new("bus-tap");
    std::fs::create_dir_all(dir.path()).expect("scratch directory");
    let path = dir.path().join("space.wav").display().to_string();
    let rendered = render_range_for_agent(
        path.clone(),
        RangeRequest {
            start_bar: Some(1),
            end_bar: Some(3),
            ..Default::default()
        },
        Some("space".to_string()),
        &state,
    )
    .expect("the bus return renders");
    assert_eq!(rendered.bus.as_deref(), Some("space"));
    let (rate, samples) = read_preview(&rendered.path);
    assert_eq!(rate, 48_000);
    assert!(
        samples.iter().any(|sample| sample.abs() > 1e-4),
        "the return carries the send"
    );

    let refused = render_range_for_agent(
        path,
        RangeRequest {
            start_bar: Some(1),
            end_bar: Some(3),
            ..Default::default()
        },
        Some("missing".to_string()),
        &state,
    )
    .expect_err("an unknown bus is refused");
    assert!(refused.contains("no bus named 'missing'"), "{refused}");
}
