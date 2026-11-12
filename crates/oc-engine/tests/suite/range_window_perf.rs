//! Timing harness for the track-subset range window.
//!
//! Part of the `perf-tests` set: a plain `cargo test` neither compiles nor runs
//! it. Run it with the optimised `perf` profile:
//!
//! ```sh
//! OCP_RENDER_THREADS=4 cargo test -p oc-engine --features perf-tests --profile perf \
//!     --test all range_window_perf:: -- --nocapture
//! ```
//!
//! `OCP_RANGE_PROJECT` overrides the project, `OCP_SF2` the SoundFont and
//! `OCP_RANGE_REPEAT` the number of timed runs (default 3). Every scenario uses
//! a fresh cache, and the load average is printed beside each run.
#![cfg(feature = "perf-tests")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oc_engine::render_cache::{RangeRenderOutput, RangeRenderRequest, RenderCache};

fn project_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OCP_RANGE_PROJECT") {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/session-20tracks.ocp")
        .canonicalize()
        .expect("benchmark project")
}

fn soundfont_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OCP_SF2") {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

fn repeat() -> usize {
    std::env::var("OCP_RANGE_REPEAT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(3)
}

fn load_average() -> String {
    let output = std::process::Command::new("uptime").output().ok();
    let text = output
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    text.split("load averages:")
        .nth(1)
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn gui_cache() -> RenderCache {
    // Match the GUI range cache: relative drum-kit samples resolve from the
    // project directory, and pattern previews stay enabled.
    let mut cache = RenderCache::new();
    cache.set_resource_root(project_path().parent().map(Path::to_path_buf));
    cache
}

fn render_full(cache: &mut RenderCache, project: &oc_dsl::ast::Project, ratio: u32) -> Vec<f32> {
    cache
        .render_full(project, &soundfont_path(), ratio, &mut HashMap::new())
        .expect("full render")
}

fn range(
    cache: &mut RenderCache,
    project: &oc_dsl::ast::Project,
    rate: u32,
    start: f64,
    end: f64,
    tracks: Option<Vec<usize>>,
    stems: bool,
) -> RangeRenderOutput {
    cache
        .render_range(
            project,
            &soundfont_path(),
            rate,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: (start * f64::from(rate)).round() as u64,
                end_sample: (end * f64::from(rate)).round() as u64,
                tracks,
                stems,
            },
        )
        .expect("range render")
}

fn timed<T>(label: &str, run: impl FnOnce() -> T) -> (T, Duration) {
    oc_engine::stage_log::begin();
    let started = Instant::now();
    let value = run();
    let elapsed = started.elapsed();
    let stages = oc_engine::stage_log::take();
    println!(
        "  {label:<28} {:>8.1} ms   load {}",
        elapsed.as_secs_f64() * 1000.0,
        load_average()
    );
    for stage in &stages {
        let track = stage
            .track
            .map(|track| format!(" #{track}"))
            .unwrap_or_default();
        println!(
            "      stage {:<22} {:>8.1} ms",
            format!("{}{track}", stage.label),
            stage.elapsed.as_secs_f64() * 1000.0
        );
    }
    (value, elapsed)
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

#[test]
fn range_window_cost() {
    let project_text = std::fs::read_to_string(project_path()).expect("project file");
    let project = oc_dsl::parse(&project_text).expect("benchmark project parses");
    let rate = 48_000;
    let runs = repeat();
    println!(
        "project {} ({} tracks), soundfont {}, threads {}, {} runs",
        project_path().display(),
        project.tracks.len(),
        soundfont_path().display(),
        oc_engine::render_parallel::render_workers(),
        runs,
    );

    let mut cold = Vec::new();
    let mut full = Vec::new();
    let mut warm_first = Vec::new();
    let mut warm_second = Vec::new();
    let mut warm_late = Vec::new();
    let mut four_tracks = Vec::new();
    let mut full_after = Vec::new();

    for run in 0..runs {
        println!("run {} / {runs}", run + 1);

        // A cold subset request: no cached mix at all.
        let mut cache = gui_cache();
        let (output, elapsed) = timed("cold subset [0,1] 10-11s", || {
            range(
                &mut cache,
                &project,
                rate,
                10.0,
                11.0,
                Some(vec![0, 1]),
                false,
            )
        });
        cold.push(elapsed);
        println!("      window path {:?}", output.window);
        assert_eq!(output.frames, rate as usize);

        // The reference: one full render of the whole project.
        let mut cache = gui_cache();
        let (_, elapsed) = timed("full render", || render_full(&mut cache, &project, rate));
        full.push(elapsed);

        // Warm subset windows against the cached full mix.
        let mut cache = gui_cache();
        render_full(&mut cache, &project, rate);
        let (first_window, elapsed) = timed("warm subset [0,1] 10-11s", || {
            range(
                &mut cache,
                &project,
                rate,
                10.0,
                11.0,
                Some(vec![0, 1]),
                false,
            )
        });
        warm_first.push(elapsed);
        println!(
            "      window path {:?}, snapshot {} bytes",
            first_window.window,
            cache.post_fx_snapshot_bytes()
        );
        let (again, elapsed) = timed("warm subset [0,1] 10-11s again", || {
            range(
                &mut cache,
                &project,
                rate,
                10.0,
                11.0,
                Some(vec![0, 1]),
                false,
            )
        });
        warm_second.push(elapsed);
        println!(
            "      window path {:?}, snapshot {} bytes",
            again.window,
            cache.post_fx_snapshot_bytes()
        );
        let (late, elapsed) = timed("warm subset [0,1] 60-61s", || {
            range(
                &mut cache,
                &project,
                rate,
                60.0,
                61.0,
                Some(vec![0, 1]),
                false,
            )
        });
        warm_late.push(elapsed);
        println!(
            "      window path {:?}, snapshot {} bytes",
            late.window,
            cache.post_fx_snapshot_bytes()
        );
        let (four, elapsed) = timed("warm subset [0..3] 20-22s", || {
            range(
                &mut cache,
                &project,
                rate,
                20.0,
                22.0,
                Some(vec![0, 1, 2, 3]),
                false,
            )
        });
        four_tracks.push(elapsed);
        println!(
            "      window path {:?}, snapshot {} bytes",
            four.window,
            cache.post_fx_snapshot_bytes()
        );

        // A full window after the subset requests must still be a cache hit.
        let (output, elapsed) = timed("full window 10-11s after", || {
            range(&mut cache, &project, rate, 10.0, 11.0, None, false)
        });
        full_after.push(elapsed);
        assert!(output.cache_hit, "the full mix must still serve the window");
    }

    println!("medians:");
    for (label, values) in [
        ("cold subset [0,1] 10-11s", cold),
        ("full render", full),
        ("warm subset [0,1] 10-11s", warm_first),
        ("warm subset [0,1] 10-11s again", warm_second),
        ("warm subset [0,1] 60-61s", warm_late),
        ("warm subset [0..3] 20-22s", four_tracks),
        ("full window 10-11s after", full_after),
    ] {
        println!(
            "  {label:<28} {:>8.1} ms",
            median(values).as_secs_f64() * 1000.0
        );
    }
}
