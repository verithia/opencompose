//! Range/track-subset rendering tests.

use super::super::*;

fn range_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "range"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track low | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 48 90; @3 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @3 ch0 note_off 72 0; }
"#,
    )
    .expect("range project parses")
}

fn sustained_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "sustained"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track low | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @3 ch0 note_off 72 0; }
"#,
    )
    .expect("sustained project parses")
}

fn render_full(project: &Project, rate: u32) -> Vec<f32> {
    RenderCache::new()
        .render_full(project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .expect("full render")
}

#[test]
fn mixing_progress_completes_only_after_a_successful_render() {
    use crate::render_cancel::RenderStage;
    use std::sync::{Arc, Mutex};
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache.set_progress(Box::new(move |progress| {
        captured.lock().unwrap().push(progress);
    }));
    let audio = cache
        .render_full(
            &range_project(),
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(audio.iter().any(|sample| sample.abs() > 0.001));
    let records = records.lock().unwrap();
    let mixing: Vec<_> = records
        .iter()
        .filter(|p| p.stage == RenderStage::Mixing)
        .collect();
    assert!(
        mixing.len() >= 2,
        "mixing must report both in-progress and completed states"
    );
    assert_eq!(mixing.first().unwrap().fraction, 0.0);
    assert_eq!(mixing.last().unwrap().fraction, 1.0);
    assert!(mixing[..mixing.len() - 1].iter().all(|p| p.fraction < 1.0));
}

#[test]
fn cancelling_at_mix_start_never_reports_completion_or_publishes_pcm() {
    use crate::render_cancel::{CancelToken, RenderStage};
    use std::sync::{Arc, Mutex};
    let cancel = CancelToken::new();
    let request_cancel = cancel.clone();
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache.set_cancel_token(cancel);
    cache.set_progress(Box::new(move |progress| {
        captured.lock().unwrap().push(progress);
        if progress.stage == RenderStage::Mixing {
            request_cancel.cancel();
        }
    }));
    let error = cache
        .render_full(
            &range_project(),
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
        )
        .unwrap_err();
    assert_eq!(error, crate::render_cancel::CANCELLED_MESSAGE);
    assert!(cache.master_buffer().is_none());
    assert!(cache.is_dirty());
    let records = records.lock().unwrap();
    assert!(records.iter().any(|p| p.stage == RenderStage::Mixing));
    assert!(
        records
            .iter()
            .filter(|p| p.stage == RenderStage::Mixing)
            .all(|p| p.fraction < 1.0)
    );
}

fn render_range(
    project: &Project,
    rate: u32,
    start_sample: u64,
    end_sample: u64,
    tracks: Option<Vec<usize>>,
    stems: bool,
) -> RangeRenderOutput {
    RenderCache::new()
        .render_range(
            project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample,
                end_sample,
                tracks,
                stems,
            },
        )
        .expect("range render")
}

#[test]
fn range_render_matches_the_full_render_slice() {
    let rate = 8_000;
    let project = range_project();
    let full = render_full(&project, rate);
    let output = render_range(&project, rate, 8_000, 16_000, None, false);
    assert_eq!(output.frames, 8_000);
    assert_eq!(output.audio.len(), 16_000);
    assert_eq!(output.audio, full[16_000..32_000]);
}

#[test]
fn range_render_reuses_a_clean_cache_without_rerendering() {
    let rate = 8_000;
    let project = range_project();
    let mut cache = RenderCache::new();
    let full = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let cached_ptr = cache.master_buffer().unwrap().as_ptr();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 16_000,
                tracks: None,
                stems: false,
            },
        )
        .unwrap();
    assert!(output.cache_hit, "the warm cache must serve the window");
    assert_eq!(output.audio, full[16_000..32_000]);
    assert!(!cache.is_dirty());
    assert_eq!(
        cache.master_buffer().unwrap().as_ptr(),
        cached_ptr,
        "a range request must not replace the cached mix"
    );
}

#[test]
fn range_render_is_exact_with_stateful_master_effects() {
    let rate = 44_100;
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "fx range"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track lead | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 60 90; @3 ch0 note_off 60 0; }

@master
  effects:
    - reverb: decay=0.8s, predelay=10ms, damping=0.5, mix=50%
"#,
    )
    .expect("effect project parses");
    let full = render_full(&project, rate);
    let frames = rate as usize;
    let output = render_range(
        &project,
        rate,
        frames as u64,
        2 * frames as u64,
        None,
        false,
    );
    assert_eq!(output.audio, full[frames * 2..4 * frames]);
}

#[test]
fn range_render_is_deterministic() {
    let rate = 8_000;
    let project = range_project();
    let first = render_range(&project, rate, 8_000, 16_000, None, false);
    let second = render_range(&project, rate, 8_000, 16_000, None, false);
    assert_eq!(first.audio, second.audio);
}

#[test]
fn range_render_pads_beyond_the_rendered_length() {
    let rate = 8_000;
    let project = range_project();
    let full = render_full(&project, rate);
    let start = full.len() as u64 / 2 + 1_000;
    let output = render_range(&project, rate, start, start + 1_000, None, false);
    assert_eq!(output.audio.len(), 2_000);
    assert!(output.audio.iter().all(|sample| *sample == 0.0));
}

#[test]
fn range_render_can_select_a_track_subset_with_stems() {
    let rate = 8_000;
    let project = range_project();
    let output = render_range(&project, rate, 8_000, 16_000, Some(vec![0]), true);
    assert_eq!(output.stems.len(), 2);
    let (index, name, stem) = &output.stems[0];
    assert_eq!(*index, 0);
    assert_eq!(name, "low");
    assert_eq!(stem.len(), 16_000);
    assert_eq!(output.tracks, vec![0]);
    let (_, _, unselected) = &output.stems[1];
    assert!(unselected.iter().all(|sample| *sample == 0.0));
    let difference = output
        .audio
        .iter()
        .zip(stem)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(difference < 1e-6, "subset/stem difference {difference}");
    assert!(stem.iter().any(|sample| sample.abs() > 0.001));
}

#[test]
fn subset_range_is_a_slice_of_the_selected_tracks_timeline() {
    let rate = 8_000;
    let project = sustained_project();
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 12_000,
                tracks: Some(vec![0]),
                stems: false,
            },
        )
        .unwrap();
    assert_eq!(output.tracks, vec![0]);
    assert!(
        output.audio.iter().any(|sample| sample.abs() > 1e-4),
        "a note that started before the window must ring through the slice"
    );
    // The same selection as a project of its own is the reference timeline.
    let mut solo = project.clone();
    solo.tracks.truncate(1);
    let solo_full = render_full(&solo, rate);
    assert_eq!(output.audio, solo_full[16_000..24_000]);
}

#[test]
fn subset_range_request_keeps_the_cached_full_mix_valid() {
    let rate = 8_000;
    let project = range_project();
    let mut cache = RenderCache::new();
    let full = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let pointer = cache.master_buffer().unwrap().as_ptr();
    let subset = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 16_000,
                tracks: Some(vec![0]),
                stems: false,
            },
        )
        .unwrap();
    assert!(subset.audio.iter().any(|sample| sample.abs() > 1e-4));
    assert!(
        !cache.is_dirty(),
        "a subset request must leave the cached full mix valid"
    );
    assert_eq!(
        cache.master_buffer().unwrap().as_ptr(),
        pointer,
        "a subset request must not replace the cached mix"
    );
    assert_eq!(cache.master_buffer().unwrap(), &full[..]);
    let after = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 16_000,
                tracks: None,
                stems: false,
            },
        )
        .unwrap();
    assert!(
        after.cache_hit,
        "the full mix must not be re-rendered after a subset request"
    );
    assert_eq!(after.audio, full[16_000..32_000]);
}

#[test]
fn solo_window_starts_from_silence_at_the_window_start() {
    let rate = 8_000;
    let project = sustained_project();
    let mut cache = RenderCache::new();
    let output = cache
        .render_solo_window(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 12_000,
                tracks: Some(vec![0]),
                stems: false,
            },
        )
        .unwrap();
    assert!(
        output.audio.iter().all(|sample| *sample == 0.0),
        "a solo window renders from reset, so the earlier note is not resumed"
    );
    assert!(cache.is_dirty(), "a solo window invalidates the cache");
}

#[test]
fn range_stems_are_slices_of_the_stem_capture() {
    let rate = 8_000;
    let project = range_project();
    let mut cache = RenderCache::new();
    cache.capture_stems(true);
    let full = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let stems: Vec<Vec<f32>> = cache
        .stems()
        .unwrap()
        .tracks
        .iter()
        .map(|(_, buffer)| buffer.clone())
        .collect();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 8_000,
                end_sample: 16_000,
                tracks: None,
                stems: true,
            },
        )
        .unwrap();
    assert!(output.cache_hit);
    assert_eq!(output.audio, full[16_000..32_000]);
    assert_eq!(output.stems.len(), stems.len());
    for (index, _, buffer) in &output.stems {
        assert_eq!(buffer, &stems[*index][16_000..32_000]);
    }
}

#[test]
fn range_render_outside_every_event_is_silent() {
    let rate = 8_000;
    let project = range_project();
    let output = render_range(&project, rate, 20_000, 24_000, None, false);
    assert_eq!(output.frames, 4_000);
    assert!(output.audio.iter().all(|sample| sample.abs() < 1e-6));
}

#[test]
fn range_render_rejects_invalid_bounds_and_tracks() {
    let project = range_project();
    let mut cache = RenderCache::new();
    let error = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 100,
                end_sample: 100,
                tracks: None,
                stems: false,
            },
        )
        .unwrap_err();
    assert!(error.contains("range end"), "{error}");
    let error = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 0,
                end_sample: 100,
                tracks: Some(vec![9]),
                stems: false,
            },
        )
        .unwrap_err();
    assert!(error.contains("out of range"), "{error}");
    let error = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 0,
                end_sample: 100,
                tracks: Some(Vec::new()),
                stems: false,
            },
        )
        .unwrap_err();
    assert!(error.contains("must not be empty"), "{error}");
}

#[test]
fn range_render_windows_audio_region_tracks() {
    let rate = 8_000;
    let root = std::env::temp_dir().join(format!(
        "oc-range-region-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let mut wav = hound::WavWriter::create(
        root.join("source.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for index in 0..rate {
        let value = 0.1 + (index as f32 / rate as f32) * 0.2;
        wav.write_sample(value).unwrap();
    }
    wav.finalize().unwrap();
    std::fs::write(
        root.join("regions.json"),
        serde_json::json!({"sample_rate":rate,"regions":[{"file":"source.wav",
            "region":{"timeline_start":0,"source_start":0,"length":rate,"gain":1.0,"envelope":[]}}]})
        .to_string(),
    )
    .unwrap();
    let project = oc_dsl::parse(
        r#"@project "Regions"
  tempo: 120
@preset audio
  source: builtin
  plugin: "builtin.audio_regions"
  params: {audio_region_state_file: "regions.json"}
@track audio | preset: audio
"#,
    )
    .unwrap();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    let full = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let mut range_cache = RenderCache::new();
    range_cache.set_resource_root(Some(root.clone()));
    let output = range_cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 4_000,
                end_sample: 8_000,
                tracks: None,
                stems: false,
            },
        )
        .unwrap();
    assert_eq!(output.audio, full[8_000..16_000]);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Track-subset windows: snapshots, bounded passes, fallbacks
// ---------------------------------------------------------------------------

/// Two stereo tracks with a reverb send and a lookahead master limiter, so the
/// pan-law class, the send return and the master latency all matter.
fn window_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "window"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@bus space
  mix: vol=-3dB
  routes: [master]
  effects:
    - reverb: decay=0.6s, predelay=5ms, damping=0.5, mix=40%

@master
  effects:
    - limiter: ceiling=-1dB, release=80ms, lookahead=6ms, true_peak=true

@track low | preset: lead, instrument: synth/lead_square
  effects:
    - chorus: rate=0.3, depth=3, mix=50%
  mix: send(space=-6dB)
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track high | preset: lead, instrument: synth/lead_square
  effects:
    - chorus: rate=0.4, depth=2, mix=40%
  performance: { @2 ch0 note_on 72 90; @3 ch0 note_off 72 0; }
"#,
    )
    .expect("window project parses")
}

/// The same selection as a project of its own, rendered in full and sliced:
/// the reference every subset window has to reproduce.
fn selected_timeline(
    project: &Project,
    rate: u32,
    tracks: &[usize],
    start: u64,
    frames: usize,
) -> Vec<f32> {
    let mut solo = project.clone();
    solo.tracks = project
        .tracks
        .iter()
        .enumerate()
        .filter(|(index, _)| tracks.contains(index))
        .map(|(_, track)| track.clone())
        .collect();
    let full = render_full(&solo, rate);
    let start = start as usize;
    full[start * 2..(start + frames) * 2].to_vec()
}

fn window_request(start: u64, end: u64, tracks: Option<Vec<usize>>) -> RangeRenderRequest {
    RangeRenderRequest {
        start_sample: start,
        end_sample: end,
        tracks,
        stems: false,
    }
}

#[test]
fn subset_window_does_not_re_mix_the_full_timeline() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();
    let full = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let reference = selected_timeline(&project, rate, &[0], 16_000, 8_000);
    let before = cache.render_counters();

    // The first window has no snapshot yet: it runs one bounded chain pass.
    let first = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(first.audio, reference);
    assert_eq!(first.window, Some(RangeWindowPath::Recomputed));
    let after_first = cache.render_counters();
    assert_eq!(after_first.full_render_calls, before.full_render_calls);
    assert_eq!(after_first.source_renders, before.source_renders);
    assert_eq!(
        after_first.full_timeline_mix_passes, before.full_timeline_mix_passes,
        "a window inside the timeline must not re-mix all of it"
    );
    assert!(cache.post_fx_snapshot_bytes() > 0);

    // The repeat window is served from the snapshot: no chain pass at all.
    let second = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(second.audio, reference);
    assert_eq!(second.window, Some(RangeWindowPath::Snapshot));
    let after_second = cache.render_counters();
    assert_eq!(after_second.full_render_calls, before.full_render_calls);
    assert_eq!(after_second.source_renders, before.source_renders);
    assert_eq!(
        after_second.full_timeline_mix_passes,
        before.full_timeline_mix_passes
    );

    // The cached full mix and a full window are untouched by all of this.
    assert_eq!(cache.master_buffer().unwrap(), &full[..]);
    assert!(!cache.is_dirty());
}

#[test]
fn cold_and_warm_subset_windows_match_the_selected_timeline() {
    let rate = 8_000;
    let project = window_project();
    let reference = selected_timeline(&project, rate, &[0], 16_000, 8_000);

    let cold = RenderCache::new()
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(cold.window, Some(RangeWindowPath::Cold));
    assert_eq!(cold.audio, reference);

    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let recomputed = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(recomputed.window, Some(RangeWindowPath::Recomputed));
    assert_eq!(recomputed.audio, reference);
    let snapshot = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(snapshot.window, Some(RangeWindowPath::Snapshot));
    assert_eq!(snapshot.audio, reference);
}

#[test]
fn cold_subset_window_sources_only_the_selected_tracks() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(output.window, Some(RangeWindowPath::Cold));
    assert_eq!(
        output.audio,
        selected_timeline(&project, rate, &[0], 16_000, 8_000)
    );
    let counters = cache.render_counters();
    assert_eq!(counters.full_render_calls, 0, "{counters:?}");
    assert!(
        counters.source_renders <= 1,
        "a cold subset must source only the selection: {counters:?}"
    );
    assert!(
        cache.is_dirty(),
        "a cold subset must not claim a clean cache"
    );
    assert!(cache.master_buffer().is_none());
}

#[test]
fn post_fx_snapshot_memory_is_accounted_and_evicted() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    let frames = cache.post_fx_snapshot_frames(0).expect("snapshot");
    assert!(frames >= 24_000, "the snapshot must cover the window end");
    // One stereo f32 frame is 8 bytes; nothing else is stored.
    assert_eq!(cache.post_fx_snapshot_bytes(), frames * 8);
    assert_eq!(cache.post_fx_snapshot_frames(1), None);

    // A second track adds exactly its own buffer.
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![1])),
        )
        .unwrap();
    let both = cache.post_fx_snapshot_bytes();
    assert_eq!(both, frames * 8 * 2);

    // A budget that cannot hold one buffer stores nothing.
    cache.set_post_fx_snapshot_budget(frames * 8 - 1);
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(output.window, Some(RangeWindowPath::Recomputed));
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);

    // A budget that holds one keeps one and evicts the other.
    cache.set_post_fx_snapshot_budget(frames * 8);
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![1])),
        )
        .unwrap();
    assert_eq!(cache.post_fx_snapshot_bytes(), frames * 8);

    // Disabling drops everything and stays empty.
    cache.set_post_fx_snapshot_budget(0);
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);
}

#[test]
fn subset_window_recomputes_when_no_snapshot_covers_it() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let early = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(early.window, Some(RangeWindowPath::Recomputed));

    // Past the captured horizon the window has to recompute, and it captures
    // at its own horizon.
    let late = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(40_000, 48_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(late.window, Some(RangeWindowPath::Recomputed));
    assert_eq!(
        late.audio,
        selected_timeline(&project, rate, &[0], 40_000, 8_000)
    );
    assert!(cache.post_fx_snapshot_frames(0).unwrap() >= 48_000);

    // The earlier window now lies inside the snapshot again.
    let again = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(again.window, Some(RangeWindowPath::Snapshot));
    assert_eq!(
        again.audio,
        selected_timeline(&project, rate, &[0], 16_000, 8_000)
    );
}

#[test]
fn a_sidechain_source_outside_the_selection_blocks_the_snapshot() {
    let rate = 8_000;
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "ducked"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track high | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 72 90; @2 ch0 note_off 72 0; }

@track low | preset: lead, instrument: synth/lead_square
  effects:
    - compression: threshold=-24dB, ratio=4:1, attack=2ms, release=80ms, sidechain=high
  performance: { @1 ch0 note_on 48 90; @9 ch0 note_off 48 0; }

@track pad | preset: lead, instrument: synth/lead_square
  performance: { @1 ch0 note_on 60 90; @2 ch0 note_off 60 0; }
"#,
    )
    .expect("sidechain project parses");
    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    // `low` reads `high` as its key, so its chain cannot be replayed from a
    // snapshot taken while `high` was audible unless `high` is selected too.
    let first = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![1])),
        )
        .unwrap();
    assert_eq!(first.window, Some(RangeWindowPath::Recomputed));
    let second = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![1])),
        )
        .unwrap();
    assert_eq!(
        second.window,
        Some(RangeWindowPath::Recomputed),
        "an unsafe snapshot must not be reused"
    );
    assert_eq!(second.audio, first.audio);

    // Selecting both tracks makes the snapshot safe again.
    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0, 1])),
        )
        .unwrap();
    let paired = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0, 1])),
        )
        .unwrap();
    assert_eq!(paired.window, Some(RangeWindowPath::Snapshot));
}

#[test]
fn dirty_tracks_drop_their_post_fx_snapshots() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert!(cache.post_fx_snapshot_bytes() > 0);
    cache.mark_track_dirty(0);
    assert_eq!(cache.post_fx_snapshot_frames(0), None);
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);

    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![1])),
        )
        .unwrap();
    assert!(cache.post_fx_snapshot_bytes() > 0);
    cache.mark_all_dirty();
    assert_eq!(cache.post_fx_snapshot_bytes(), 0);
}

#[test]
fn a_selection_covering_every_track_is_a_full_window() {
    let rate = 8_000;
    let project = range_project();
    let full = render_full(&project, rate);
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(8_000, 16_000, Some(vec![1, 0])),
        )
        .unwrap();
    assert_eq!(output.window, None, "every track is the full-mix path");
    assert_eq!(output.tracks, vec![0, 1]);
    assert_eq!(
        output.audio,
        full[16_000..32_000],
        "selecting every track is the full render's slice"
    );
    assert!(!cache.is_dirty(), "a full selection may fill the cache");
    let again = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(8_000, 16_000, Some(vec![0, 1])),
        )
        .unwrap();
    assert!(again.cache_hit);
    assert_eq!(again.audio, output.audio);
}

#[test]
fn subset_window_stems_are_slices_of_the_selected_timeline() {
    let rate = 8_000;
    let project = window_project();
    // The reference stems come from the same selection rendered as its own
    // project, captured after its track chains.
    let mut solo = project.clone();
    solo.tracks.truncate(1);
    let mut reference_cache = RenderCache::new();
    reference_cache.capture_stems(true);
    reference_cache
        .render_full(&solo, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let reference = reference_cache.stems().unwrap().tracks[0].1[16_000 * 2..24_000 * 2].to_vec();

    let mut cache = RenderCache::new();
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    for attempt in 0..2 {
        let mut request = window_request(16_000, 24_000, Some(vec![0]));
        request.stems = true;
        let output = cache
            .render_range(
                &project,
                Path::new("unused.sf2"),
                rate,
                &mut HashMap::new(),
                request,
            )
            .unwrap();
        let expected = if attempt == 0 {
            RangeWindowPath::Recomputed
        } else {
            RangeWindowPath::Snapshot
        };
        assert_eq!(output.window, Some(expected));
        assert_eq!(output.stems.len(), 2);
        assert_eq!(output.stems[0].1, "low");
        assert_eq!(output.stems[0].2, reference);
        assert!(output.stems[1].2.iter().all(|sample| *sample == 0.0));
    }
}

#[test]
fn a_window_served_from_the_cached_timeline_reports_a_cache_hit() {
    let rate = 8_000;
    let project = window_project();
    let mut cache = RenderCache::new();

    // Cold: no clean full render exists, so the sources were synthesised.
    let cold = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(cold.window, Some(RangeWindowPath::Cold));
    assert!(
        !cold.cache_hit,
        "a cold subset did re-synthesise its sources"
    );

    // Warm: the cached full render answers, so every window is a hit.
    cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    for attempt in 0..2 {
        let output = cache
            .render_range(
                &project,
                Path::new("unused.sf2"),
                rate,
                &mut HashMap::new(),
                window_request(16_000, 24_000, Some(vec![0])),
            )
            .unwrap();
        let expected = if attempt == 0 {
            RangeWindowPath::Recomputed
        } else {
            RangeWindowPath::Snapshot
        };
        assert_eq!(output.window, Some(expected));
        assert!(
            output.cache_hit,
            "a {:?} window was cut from the clean cached timeline",
            output.window
        );
    }

    // A window past the rendered timeline is answered by the cached horizon.
    let horizon = cache.render_horizon_samples as u64;
    let silent = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(horizon + 1_000, horizon + 2_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(silent.window, Some(RangeWindowPath::Silent));
    assert!(silent.cache_hit, "silence from the cached horizon is a hit");
    assert!(silent.audio.iter().all(|sample| *sample == 0.0));

    // The full-mix slice keeps reporting its own hit.
    let full = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, None),
        )
        .unwrap();
    assert_eq!(full.window, None);
    assert!(full.cache_hit);
}

#[test]
fn a_solo_window_reads_automation_at_the_window_position() {
    let rate = 8_000;
    // The gain lane steps to 1 at bar 5 (8 s). A block preview of [8.5 s, 9 s)
    // must hear the lane at 8.5 s, not at the window-relative 0 s.
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "solo automation"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@track lead | preset: lead, instrument: synth/lead_square
  effects:
    - gain: factor=1
  automation: gain.factor {1:1:0=0 curve=stairs, 5:1:0=1}
  @phrase later [5:2:0 - 6:1:0]
    C4 q mf
"#,
    )
    .expect("automation project parses");
    let full = render_full(&project, rate);
    let start = 68_000u64; // 8.5 s
    let frames = 4_000usize; // 0.5 s
    let expected = full[start as usize * 2..(start as usize + frames) * 2].to_vec();
    assert!(
        expected.iter().any(|sample| sample.abs() > 1e-3),
        "the full render must hear the stepped gain in this window"
    );

    let mut cache = RenderCache::new();
    let output = cache
        .render_solo_window(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(start, start + frames as u64, Some(vec![0])),
        )
        .unwrap();
    let level = output
        .audio
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f32, f32::max);
    assert!(
        level > 1e-3,
        "the solo window applied the automation of the project start (peak {level})"
    );
    assert_eq!(
        output.audio, expected,
        "the solo window does not match the timeline at its own position"
    );
}

/// A builtin-source track whose insert chain holds the 0-in/2-out fake.
fn instrument_insert_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "instrument insert"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@track Lead | preset: tone, instrument: synth/lead_square
  effects:
    - plugin:"test.fake.two_bus"
  @phrase held [1:1 - 2:1]
    C4 w mf
"#,
    )
    .expect("instrument insert project parses")
}

const INSTRUMENT_INSERT_REFUSAL: &str = "VST3 Plugin: an instrument plugin (0 audio inputs, 2 stereo outputs) cannot be used as \
     an effect on track 'Lead'; put it on an instrument track, or wrap it in a bus";

#[test]
fn a_range_render_refuses_an_instrument_insert_before_rendering() {
    let rate = 8_000;
    let project = instrument_insert_project();
    let mut cache = RenderCache::new();
    let error = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(0, 8_000, None),
        )
        .expect_err("an instrument insert must not render");
    assert_eq!(error, INSTRUMENT_INSERT_REFUSAL);
    assert!(
        cache.master_buffer().is_none(),
        "the refused range render left a mix behind"
    );
}

#[test]
fn a_solo_window_refuses_an_instrument_insert_before_rendering() {
    let rate = 8_000;
    let project = instrument_insert_project();
    let mut cache = RenderCache::new();
    let error = cache
        .render_solo_window(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(0, 8_000, Some(vec![0])),
        )
        .expect_err("a clip preview must refuse an instrument insert");
    assert_eq!(error, INSTRUMENT_INSERT_REFUSAL);
}

// ---------------------------------------------------------------------------
// Media identity: a file replaced in place must invalidate the cached audio
// ---------------------------------------------------------------------------

fn media_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("oc-range-media-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_tone(path: &Path, rate: u32, seconds: f32, frequency: f32) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..(rate as f32 * seconds) as usize {
        let phase = 2.0 * std::f32::consts::PI * frequency * frame as f32 / rate as f32;
        writer.write_sample(phase.sin() * 0.5).unwrap();
    }
    writer.finalize().unwrap();
}

/// A 4 s media clip on a track of its own, so a replaced file is the only
/// difference between two renders.
fn media_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "media identity"
  tempo: 120

@audio_clip hit [1:1 - 3:1] | track: tape
  file: "hit.wav"

@track tape | instrument: audio/clip
"#,
    )
    .expect("media project parses")
}

/// The same clip beside a synth track, so a track-subset window has a source
/// that is not the replaced one.
fn media_window_project() -> Project {
    oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "media window identity"
  tempo: 120

@preset lead
  source: builtin
  plugin: "builtin.open_synth"

@audio_clip hit [1:1 - 3:1] | track: tape
  file: "hit.wav"

@track tape | instrument: audio/clip

@track lead | preset: lead, instrument: synth/lead_square
  performance: { @2 ch0 note_on 72 90; @3 ch0 note_off 72 0; }
"#,
    )
    .expect("media window project parses")
}

fn render_media_range(
    cache: &mut RenderCache,
    project: &Project,
    rate: u32,
    tracks: Option<Vec<usize>>,
) -> Result<RangeRenderOutput, String> {
    cache.render_range(
        project,
        Path::new("unused.sf2"),
        rate,
        &mut HashMap::new(),
        RangeRenderRequest {
            start_sample: 0,
            end_sample: u64::from(rate) * 4,
            tracks,
            stems: false,
        },
    )
}

fn cold_media_render(
    project: &Project,
    root: &Path,
    rate: u32,
    tracks: Option<Vec<usize>>,
) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.to_path_buf()));
    render_media_range(&mut cache, project, rate, tracks)
        .expect("cold render")
        .audio
}

#[test]
fn a_solo_window_preserves_the_late_audio_clips_loop_and_fade_phase() {
    let rate = 8_000;
    let root = tempfile::tempdir().unwrap();
    write_tone(&root.path().join("hit.wav"), rate, 1.0, 173.0);
    let project = oc_dsl::parse(
        r#"@project "Late media"
  tempo: 120
@audio_clip take [2:1 - 3:1] | track: tape
  file: "hit.wav"
  offset: 35ms
  loop: 90ms
  fade_in: 50ms
  fade_out: 70ms
  gain: -6dB
@track tape | instrument: audio/clip
"#,
    )
    .unwrap();
    let soundfont = Path::new("unused.sf2");
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.path().to_path_buf()));
    let full = cache
        .render_full(&project, soundfont, rate, &mut HashMap::new())
        .unwrap();

    // Cross the clip start, skip into a loop iteration, then cross the clip end.
    for (start, end) in [(15_980, 16_444), (16_871, 18_001), (31_880, 32_200)] {
        let expected = &full[start * 2..end * 2];
        assert!(expected.iter().any(|sample| sample.abs() > 0.01));
        let window = cache
            .render_solo_window(
                &project,
                soundfont,
                rate,
                &mut HashMap::new(),
                window_request(start as u64, end as u64, Some(vec![0])),
            )
            .unwrap();
        assert_eq!(window.audio, expected, "window {start}..{end}");
    }

    let mut one_shot = RenderCache::new();
    one_shot.set_resource_root(Some(root.path().to_path_buf()));
    one_shot.consume_source_buffers = true;
    let window = one_shot
        .render_solo_window(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            window_request(16_871, 18_001, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(window.audio, full[16_871 * 2..18_001 * 2]);

    // A window strictly before the clip does not even need its media.
    std::fs::remove_file(root.path().join("hit.wav")).unwrap();
    let window = cache
        .render_solo_window(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            window_request(15_900, 16_000, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(window.audio, vec![0.0; 200]);
}

#[test]
fn a_failed_solo_window_does_not_poison_the_next_incremental_render() {
    let rate = 8_000;
    let root = tempfile::tempdir().unwrap();
    write_tone(&root.path().join("first.wav"), rate, 1.0, 173.0);
    let project = oc_dsl::parse(
        r#"@project "Window failure"
  tempo: 120
@audio_clip first [2:1 - 3:1] | track: first
  file: "first.wav"
@audio_clip second [2:1 - 3:1] | track: second
  file: "second.wav"
@track first | instrument: audio/clip
@track second | instrument: audio/clip
"#,
    )
    .unwrap();
    let soundfont = Path::new("unused.sf2");
    let mut warm = RenderCache::new();
    warm.set_resource_root(Some(root.path().to_path_buf()));
    let error = warm
        .render_solo_window(
            &project,
            soundfont,
            rate,
            &mut HashMap::new(),
            window_request(16_000, 20_000, None),
        )
        .unwrap_err();
    assert!(error.contains("second"), "{error}");
    write_tone(&root.path().join("second.wav"), rate, 1.0, 379.0);

    let recovered = warm
        .render_incremental(&project, soundfont, rate, &mut HashMap::new())
        .unwrap();
    let mut cold = RenderCache::new();
    cold.set_resource_root(Some(root.path().to_path_buf()));
    let expected = cold
        .render_full(&project, soundfont, rate, &mut HashMap::new())
        .unwrap();
    assert_eq!(recovered, expected);
}

#[test]
fn replacing_a_media_file_invalidates_the_cached_range() {
    let rate = 8_000;
    let root = media_root("replace");
    let media = root.join("hit.wav");
    write_tone(&media, rate, 4.0, 220.0);
    let project = media_project();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));

    let first = render_media_range(&mut cache, &project, rate, None).unwrap();
    assert!(
        first.audio.iter().any(|sample| *sample != 0.0),
        "the clip must be audible"
    );

    // An unchanged file reuses the rendered source instead of decoding it again.
    let before = cache.render_counters();
    let unchanged = render_media_range(&mut cache, &project, rate, None).unwrap();
    let hit = cache.render_counters();
    assert_eq!(
        hit.source_renders, before.source_renders,
        "an unchanged file must reuse the rendered source"
    );
    assert!(
        unchanged.audio == first.audio,
        "an unchanged file must reuse the rendered audio"
    );

    // Same path and length, new content: the modification time is what moves.
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    write_tone(&media, rate, 4.0, 440.0);
    let before = cache.render_counters();
    let replaced = render_media_range(&mut cache, &project, rate, None).unwrap();
    let after = cache.render_counters();
    assert_eq!(
        after.source_renders - before.source_renders,
        1,
        "a replaced media file must re-render its track"
    );
    assert!(
        replaced.audio != unchanged.audio,
        "the range render served the audio the file used to hold"
    );
    assert!(
        replaced.audio == cold_media_render(&project, &root, rate, None),
        "a warm re-render must match a cold render"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_a_media_file_invalidates_and_reports_the_missing_media() {
    let rate = 8_000;
    let root = media_root("delete");
    let media = root.join("hit.wav");
    write_tone(&media, rate, 4.0, 220.0);
    let project = media_project();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    render_media_range(&mut cache, &project, rate, None).unwrap();
    let before = cache.render_counters();
    render_media_range(&mut cache, &project, rate, None).unwrap();
    assert_eq!(
        cache.render_counters().source_renders,
        before.source_renders,
        "the entry must be a hit while the file is there"
    );

    std::fs::remove_file(&media).unwrap();
    let before = cache.render_counters();
    let error = render_media_range(&mut cache, &project, rate, None).unwrap_err();
    assert!(
        cache.render_counters().source_renders > before.source_renders,
        "a missing file must invalidate the entry instead of serving stale audio"
    );
    assert!(error.contains("audio clip 'hit'"), "{error}");
    assert!(error.contains("file not found"), "{error}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn replacing_a_media_file_invalidates_a_subset_window() {
    let rate = 8_000;
    let root = media_root("subset");
    let media = root.join("hit.wav");
    write_tone(&media, rate, 4.0, 220.0);
    let project = media_window_project();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.clone()));
    render_media_range(&mut cache, &project, rate, None).unwrap();

    // The first window processes the selected chain and keeps its snapshot;
    // the second one answers from that snapshot.
    let processed = render_media_range(&mut cache, &project, rate, Some(vec![0])).unwrap();
    assert_eq!(processed.window, Some(RangeWindowPath::Recomputed));
    let snapshot = render_media_range(&mut cache, &project, rate, Some(vec![0])).unwrap();
    assert!(snapshot.cache_hit);
    assert_eq!(
        snapshot.window,
        Some(RangeWindowPath::Snapshot),
        "the window must reuse its post-FX snapshot"
    );

    std::thread::sleep(std::time::Duration::from_millis(1_100));
    write_tone(&media, rate, 4.0, 440.0);
    let replaced = render_media_range(&mut cache, &project, rate, Some(vec![0])).unwrap();
    assert!(
        replaced.audio != snapshot.audio,
        "the subset window served the audio the file used to hold"
    );
    assert_eq!(
        replaced.window,
        Some(RangeWindowPath::Cold),
        "a replaced media file must drop the snapshot"
    );
    assert!(
        replaced.audio == cold_media_render(&project, &root, rate, Some(vec![0])),
        "the re-render must match a cold subset window"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cold_full_window_bounds_the_prefix_without_publishing_a_full_cache() {
    use std::sync::{Arc, Mutex};
    let rate = 8_000;
    let project = window_project();
    let full = render_full(&project, rate);
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = records.clone();
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache.set_progress(Box::new(move |progress| {
        sink.lock().unwrap().push(progress)
    }));
    let request = window_request(16_000, 24_000, None);
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            request.clone(),
        )
        .unwrap();
    assert_eq!(
        output.audio,
        full[32_000..48_000],
        "stateful bus/master history must match from frame0"
    );
    assert!(!output.cache_hit);
    assert_eq!(output.window, None);
    assert!(
        cache.master_buffer().is_none(),
        "a prefix cannot become a complete cached mix"
    );
    assert!(cache.is_dirty());
    assert!(
        cache.progress.is_some(),
        "scratch returns the owned callback"
    );
    assert!(
        records
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.stage == crate::render_cancel::RenderStage::Mixing)
    );
    let next = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(24_000, 32_000, None),
        )
        .unwrap();
    assert_eq!(next.audio, full[48_000..64_000]);
    assert!(
        !next.cache_hit,
        "a different range cannot hit a partial full cache"
    );
    let complete = cache
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    assert_eq!(complete, full);
    assert!(!cache.is_dirty());
}

#[test]
fn cold_full_window_keeps_export_bus_captures_on_the_complete_path() {
    let project = window_project();
    let rate = 8_000;
    let mut cache = RenderCache::new();
    cache.capture_export_stems();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(16_000, 24_000, None),
        )
        .unwrap();
    assert!(!output.cache_hit);
    let master = cache
        .master_buffer()
        .expect("capture requests retain the full cache");
    assert!(master.len() > 48_000);
    assert_eq!(output.audio, master[32_000..48_000]);
    let capture = cache.stem_capture.as_ref().unwrap();
    assert!(!capture.buses.is_empty());
    assert!(
        capture
            .buses
            .iter()
            .all(|(_, buffer)| buffer.len() >= master.len())
    );
}

#[test]
fn cold_full_window_cancellation_restores_progress_and_publishes_nothing() {
    let project = window_project();
    let token = crate::render_cancel::CancelToken::new();
    let signal = token.clone();
    let mut cache = RenderCache::new();
    cache
        .render_full(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
        )
        .unwrap();
    cache.mark_all_dirty();
    cache.set_cancel_token(token);
    cache.set_progress(Box::new(move |_| signal.cancel()));
    let error = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            window_request(16_000, 24_000, None),
        )
        .unwrap_err();
    assert_eq!(error, crate::render_cancel::CANCELLED_MESSAGE);
    assert!(cache.master_buffer().is_none());
    assert!(cache.is_dirty());
    assert!(
        cache.progress.is_some(),
        "cancelled scratch must return its callback"
    );
}

#[test]
fn cold_full_window_falls_back_when_future_stereo_changes_earlier_pan_gains() {
    let rate = 8_000;
    let root = oc_test_support::temp_dir("oc-range", "future-width");
    let mut writer = hound::WavWriter::create(
        root.join("hit.wav"),
        hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..rate * 4 {
        let value = (frame as f32 * 0.1).sin() * 0.25;
        writer.write_sample(value).unwrap();
        writer
            .write_sample(if frame < rate * 3 { value } else { -value })
            .unwrap();
    }
    writer.finalize().unwrap();
    let project = media_project();
    let mut reference = RenderCache::new();
    reference.set_resource_root(Some(root.to_path_buf()));
    let full = reference
        .render_full(&project, Path::new("unused.sf2"), rate, &mut HashMap::new())
        .unwrap();
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.to_path_buf()));
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(0, 1_000, None),
        )
        .unwrap();
    assert_eq!(
        output.audio,
        full[..2_000],
        "late stereo must use balance gain even in the mono prefix"
    );
    assert!(
        cache.master_buffer().is_some(),
        "ambiguous mono prefix requires the complete fallback"
    );
    assert!(!cache.is_dirty());
}

#[test]
fn a_cold_far_past_end_window_does_not_allocate_its_offset_as_a_prefix() {
    let project = window_project();
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            window_request(1_000_000_000_000, 1_000_000_000_001, None),
        )
        .unwrap();
    assert_eq!(output.audio, [0.0, 0.0]);
    assert!(cache.master_buffer().unwrap().len() < 1_000_000);
}

#[test]
fn a_cold_far_past_end_subset_renders_real_tails_without_allocating_its_offset() {
    let project = window_project();
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            8_000,
            &mut HashMap::new(),
            window_request(1_000_000_000_000, 1_000_000_000_001, Some(vec![0])),
        )
        .unwrap();
    assert_eq!(output.audio, [0.0, 0.0]);
    assert_eq!(output.tracks, [0]);
    assert_eq!(output.window, Some(RangeWindowPath::Cold));
    assert!(!output.cache_hit);
    assert!(cache.master_buffer().is_none());
    assert!(cache.is_dirty());
}

#[test]
fn cold_full_window_resolves_only_mono_widths_and_their_sidechain_feeds() {
    let project = oc_dsl::parse(
        r#"@project "Mono width witness"
  tempo: 120
@preset mono
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 0, osc.b.level: 0, sub.level: 0, noise.level: 0, unison.voices: 1}
@track feed | preset: mono, instrument: synth/lead_square
  performance: { @0 ch0 note_on 60 100; @12 ch0 note_off 60 0; }
@track target | preset: mono, instrument: synth/lead_square
  effects:
    - compression: threshold=-24dB, ratio=4:1, attack=2ms, release=80ms, sidechain=feed
  performance: { @0 ch0 note_on 48 100; @12 ch0 note_off 48 0; }
@track stable | preset: mono, instrument: synth/lead_square
  mix: pan_law=balance
  performance: { @0 ch0 note_on 72 100; @12 ch0 note_off 72 0; }
"#,
    )
    .unwrap();
    let rate = 8_000;
    let full = render_full(&project, rate);
    let mut cache = RenderCache::new();
    let output = cache
        .render_range(
            &project,
            Path::new("unused.sf2"),
            rate,
            &mut HashMap::new(),
            window_request(1_000, 8_000, None),
        )
        .unwrap();
    assert_eq!(output.audio, full[2_000..16_000]);
    assert!(
        cache.master_buffer().is_none(),
        "resolved mono must keep the prefix path instead of rerendering the complete mix"
    );
    assert!(cache.is_dirty());
    assert!(!output.cache_hit);
    let counters = cache.render_counters();
    assert_eq!(
        counters.source_renders, 5,
        "3prefix sources plus2mono/dependency witnesses, no second stable source: {counters:?}"
    );
    assert_eq!(counters.full_render_calls, 0);
}

#[test]
fn captured_bus_range_has_exact_length_padding_and_atomic_refusal() {
    let mut cache = RenderCache::new();
    assert!(cache.captured_bus_range("room", 0, 1).is_err());
    cache.capture_export_stems();
    cache
        .stem_capture
        .as_mut()
        .unwrap()
        .buses
        .push(("room".into(), vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0]));
    assert!(
        cache.captured_bus_range("room", 0, 1).is_err(),
        "a dirty capture cannot be read"
    );
    cache.mix_dirty = false;
    let before = cache.stems().unwrap().buses.clone();
    assert_eq!(
        cache.captured_bus_range("room", 1, 4).unwrap(),
        [2.0, -2.0, 3.0, -3.0, 0.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        cache.captured_bus_range("room", u64::MAX, 2).unwrap(),
        [0.0; 4]
    );
    assert!(cache.captured_bus_range("missing", 0, 1).is_err());
    assert!(cache.captured_bus_range("room", 0, usize::MAX).is_err());
    assert!(cache.captured_bus_range("room", 0, usize::MAX / 2).is_err());
    assert_eq!(cache.stems().unwrap().buses, before);
}
