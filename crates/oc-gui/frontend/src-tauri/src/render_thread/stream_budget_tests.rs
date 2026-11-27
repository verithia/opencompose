//! Streaming render budget on a real project.
//!
//! Playback must stay ahead of the device with headroom for the UI, other
//! applications and plugin spikes: every chunk of duration `T` renders in at
//! most `0.1 T` at p99, and the first chunk after Play is ready within 100 ms.
//! The harness drives the render worker's own state machine, so it measures
//! the production lane loop, inserts, mixer, meters and click.

use super::*;
use std::time::{Duration, Instant};

/// Budget of one chunk as a fraction of the audio it produces.
const CHUNK_BUDGET_RATIO: f64 = 0.1;
/// Budget of the first chunk after Play on a loaded project.
const FIRST_CHUNK_BUDGET: Duration = Duration::from_millis(100);

struct ChunkTimes {
    ratios: Vec<f64>,
    first: Duration,
    rendered_frames: u64,
    wall: Duration,
    waits: usize,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

/// Render one chunk, waiting out a clip lane that is still decoding. The wait
/// counts against the chunk: on a device it is a starved callback.
fn timed_chunk(
    state: &mut RenderState,
    frames: usize,
    out: &mut Vec<f32>,
    waits: &mut usize,
) -> Duration {
    let started = Instant::now();
    loop {
        state.render_chunk_into(frames, out);
        assert!(state.render_error.is_none(), "{:?}", state.render_error);
        if !state.waiting_for_clip || !out.is_empty() {
            return started.elapsed();
        }
        *waits += 1;
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a clip lane never became ready"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn stream_from(state: &mut RenderState, start: u64, seconds: f64, rate: u32) -> ChunkTimes {
    state.seek_to(start).unwrap();
    state.start_playing();
    let mut out = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut waits = 0;
    let wall = Instant::now();
    let first = timed_chunk(state, FIRST_CHUNK_FRAMES, &mut out, &mut waits);
    let mut rendered_frames = (out.len() / 2) as u64;
    let target = (seconds * f64::from(rate)) as u64;
    let mut ratios = Vec::new();
    while rendered_frames < target && state.render_cursor < state.playback_end() {
        let elapsed = timed_chunk(state, CHUNK_FRAMES, &mut out, &mut waits);
        let frames = out.len() / 2;
        if frames == 0 {
            break;
        }
        rendered_frames += frames as u64;
        ratios.push(elapsed.as_secs_f64() / (frames as f64 / f64::from(rate)));
    }
    ChunkTimes {
        ratios,
        first,
        rendered_frames,
        wall: wall.elapsed(),
        waits,
    }
}

fn report(label: &str, times: &ChunkTimes, rate: u32) -> (f64, Duration) {
    let mut sorted = times.ratios.clone();
    sorted.sort_by(f64::total_cmp);
    let p99 = percentile(&sorted, 0.99);
    let audio = times.rendered_frames as f64 / f64::from(rate);
    eprintln!(
        "[stream-budget] {label} first_chunk_ms={:.2} chunks={} p50={:.4} p90={:.4} p99={:.4} \
         max={:.4} audio_s={audio:.1} wall_s={:.2} realtime_x={:.2} clip_waits={}",
        times.first.as_secs_f64() * 1_000.0,
        sorted.len(),
        percentile(&sorted, 0.5),
        percentile(&sorted, 0.9),
        p99,
        sorted.last().copied().unwrap_or(0.0),
        times.wall.as_secs_f64(),
        audio / times.wall.as_secs_f64().max(f64::EPSILON),
        times.waits,
    );
    (p99, times.first)
}

/// `OCP_STREAM_PROJECT` names the project. `OCP_STREAM_SECONDS` bounds each
/// pass (default: the whole song), `OCP_STREAM_SEEK_SECONDS` adds a pass that
/// starts mid-song, and `OCP_SOUNDFONT` supplies a bank for General MIDI lanes.
#[test]
#[ignore = "requires OCP_STREAM_PROJECT; run with --profile perf --nocapture"]
fn review_stream_chunk_budget() {
    let path = std::path::PathBuf::from(
        std::env::var_os("OCP_STREAM_PROJECT").expect("set OCP_STREAM_PROJECT to the project path"),
    );
    let seconds = std::env::var("OCP_STREAM_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::INFINITY);
    let soundfont = std::env::var_os("OCP_SOUNDFONT")
        .map(PathBuf::from)
        .unwrap_or_default();
    let rate = 48_000;
    // Hosted instruments are created and retired through the application main
    // executor; the test process provides one on a pumped thread.
    let _main = oc_host::application_main::ApplicationMainExecutor::ensure_test_executor()
        .expect("test application main executor");
    #[cfg(target_os = "macos")]
    let _bridge_scope =
        oc_host::vst2_bridge::BridgeScope::enter(oc_host::vst2_bridge::BridgeMode::Streaming);

    let parse_started = Instant::now();
    let project = oc_dsl::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let parse = parse_started.elapsed();
    let mut state = RenderState::new(rate);
    state.meter_hub = Some(Arc::new(MeterHub::new()));
    state.block_workers = spawn_block_workers();
    let prepare_started = Instant::now();
    state
        .load_project_from(&project, soundfont, path.parent().map(Path::to_path_buf), 0)
        .unwrap();
    state.prepare_streaming().unwrap();
    let prepare = prepare_started.elapsed();
    eprintln!(
        "[stream-budget] project={:?} tracks={} lanes={} parse_ms={:.1} prepare_ms={:.1} \
         workers={}",
        path.file_name().unwrap_or_default(),
        project.tracks.len(),
        state.renderers.len(),
        parse.as_secs_f64() * 1_000.0,
        prepare.as_secs_f64() * 1_000.0,
        state.lane_workers(),
    );

    if let Some(graph) = state.graph.as_ref() {
        eprintln!("[stream-budget] shape {:?}", graph.parallel_shape());
    }
    let mut misses = Vec::new();
    let mut check = |label: &str, times: ChunkTimes| {
        let (p99, first) = report(label, &times, rate);
        if p99 > CHUNK_BUDGET_RATIO {
            misses.push(format!(
                "{label}: p99 chunk ratio {p99:.4} > {CHUNK_BUDGET_RATIO}"
            ));
        }
        if first > FIRST_CHUNK_BUDGET {
            misses.push(format!(
                "{label}: first chunk {first:?} > {FIRST_CHUNK_BUDGET:?}"
            ));
        }
    };
    check("from_start", stream_from(&mut state, 0, seconds, rate));
    if let Some(seek) = std::env::var("OCP_STREAM_SEEK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
    {
        state.playing = false;
        let frame = (seek * f64::from(rate)) as u64;
        check("after_seek", stream_from(&mut state, frame, seconds, rate));
    }
    assert!(misses.is_empty(), "streaming budget missed: {misses:#?}");
}
