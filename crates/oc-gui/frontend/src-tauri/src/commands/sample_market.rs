//! Sample marketplace commands for the bottom panel.
//!
//! Search and pull reuse the agent tools in [`crate::samples`], so the panel
//! and the agent answer with one vocabulary. This module adds what only the
//! panel needs: the provider chip list without a search, a preview that plays
//! through the audition stream, and an add that places the file at the
//! playhead as an `@audio_clip`.
//!
//! Every network call runs on the blocking pool; the webview thread only
//! parses the request and the reply.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use tauri::Manager;

use oc_dsl::ast::{AudioClip, BarBeat, InstrumentPath, MixState, Project, TimeRange, Track};
use oc_sample::model::{SampleError, SampleItem};
use oc_sample::provider::SampleFetch;
use oc_sample::store::SampleReceipt;

use crate::samples::preview_cache::{
    CacheLimits, CachedFetch, PREVIEW_CACHE_DIR, PreviewCache, matches_declared, url_extension,
};
use crate::samples::{self, Keys};
use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

/// A preview plays at most this long. The audition stream is a preview, not a
/// transport, and a long file must not pin memory.
const PREVIEW_SECONDS: f64 = 30.0;
/// Preview downloads are bounded far below the store's sample limit.
const PREVIEW_MAX_BYTES: usize = 48 * 1024 * 1024;
/// Shortest clip a placement may create, in quarter notes.
const MIN_CLIP_QUARTERS: f64 = 0.25;

/// The error a sample-market command rejects with. `code` is the stable
/// refusal code (`licence_notice`, `key_required`, `invalid_cursor`, ...) or
/// `null` for a failure that is not a refusal; the panel branches on it and
/// shows `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SampleCommandError {
    pub code: Option<&'static str>,
    pub message: String,
}

impl From<String> for SampleCommandError {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}

impl From<SampleError> for SampleCommandError {
    fn from(error: SampleError) -> Self {
        Self {
            code: error.code(),
            message: error.message().to_string(),
        }
    }
}

/// Run sample-market work on the blocking pool and keep its typed error.
async fn run_sample<F>(work: F) -> Result<Value, SampleCommandError>
where
    F: FnOnce() -> Result<Value, SampleCommandError> + Send + 'static,
{
    super::shared::run_blocking(move || Ok(work()))
        .await
        .map_err(SampleCommandError::from)?
}

/// The session's preview cache in the app cache directory, or `None` when
/// that directory is unavailable, in which case every preview downloads.
fn preview_cache(app: &tauri::AppHandle) -> Option<&'static PreviewCache> {
    static CACHE: OnceLock<Option<PreviewCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let dir = app.path().app_cache_dir().ok()?.join(PREVIEW_CACHE_DIR);
            PreviewCache::open(&dir, CacheLimits::default())
                .map_err(|error| log::warn!("[samples] no preview cache: {error}"))
                .ok()
        })
        .as_ref()
}

/// The markets and their state, for the panel's provider chips.
#[tauri::command]
pub async fn sample_market_providers() -> Result<Vec<Value>, String> {
    super::shared::run_blocking(|| {
        let keys = samples::load_keys();
        Ok(oc_sample::provider_list(keys.view()))
    })
    .await
}

/// `sample_market_search`: one page across the markets the panel names.
#[tauri::command]
pub async fn sample_market_search(args: Value) -> Result<Value, SampleCommandError> {
    run_sample(move || {
        let request =
            <samples::SearchArgs as crate::tool_registry::arguments::ToolArguments>::decode(&args)
                .map_err(|error| SampleCommandError::from(error.to_string()))?;
        Ok(samples::search_request_typed(&request)?)
    })
    .await
}

/// Download and play one result through the audition stream.
#[tauri::command]
pub async fn sample_preview(
    app: tauri::AppHandle,
    provider: String,
    id: String,
    label: Option<String>,
    start_paused: Option<bool>,
) -> Result<Value, SampleCommandError> {
    let started = Instant::now();
    let generation = {
        let state = app.state::<AppState>();
        let mut inner = state.lock_inner();
        inner
            .audio
            .replace_private_audition(start_paused.unwrap_or(false))?;
        state.audition.begin(label.as_deref().unwrap_or(&id))
    };
    run_sample(move || {
        let state = app.state::<AppState>();
        let cache = preview_cache(&app);
        let request = PreviewRequest {
            provider: &provider,
            id: &id,
            label: label.as_deref().unwrap_or_default(),
            generation,
            started,
        };
        let result = preview_blocking(&state, cache, &request);
        if let Err(error) = &result {
            state.audition.fail(generation, error.message.clone());
        }
        result
    })
    .await
}

/// Download one result, pull it into the project and place an `@audio_clip` at
/// the playhead on the selected track, or on a new audio track when none is
/// named.
#[tauri::command]
pub async fn sample_market_add(
    app: tauri::AppHandle,
    provider: String,
    id: String,
    track: Option<String>,
    accept_licence: Option<bool>,
) -> Result<Value, SampleCommandError> {
    run_sample(move || add_blocking(&app, &provider, &id, track, accept_licence.unwrap_or(false)))
        .await
}

/// Save a market's API key in the OS keyring, from the panel's inline prompt.
#[tauri::command]
pub async fn sample_market_save_key(provider: String, key: String) -> Result<Value, String> {
    super::shared::run_blocking(move || {
        samples::save_key(&provider, &key)?;
        Ok(json!({ "status": "ok", "provider": provider }))
    })
    .await
}

/// Return whether the credential is saved or provided by the environment.
#[tauri::command]
pub async fn sample_market_key_status(provider: String) -> Result<&'static str, String> {
    super::shared::run_blocking(move || samples::key_status(&provider)).await
}

/// Remove the saved credential for a documented sample provider.
#[tauri::command]
pub async fn sample_market_remove_key(provider: String) -> Result<(), String> {
    super::shared::run_blocking(move || samples::remove_key(&provider)).await
}

/// One preview click: the result it names, the label the panel shows, and
/// the audition generation that must still be current when it plays.
#[derive(Clone, Copy)]
struct PreviewRequest<'a> {
    provider: &'a str,
    id: &'a str,
    label: &'a str,
    generation: u64,
    started: Instant,
}

fn preview_blocking(
    state: &AppState,
    cache: Option<&PreviewCache>,
    request: &PreviewRequest<'_>,
) -> Result<Value, SampleCommandError> {
    let PreviewRequest {
        provider,
        id,
        label,
        generation,
        started,
    } = *request;
    if !state.audition.is_current(generation) {
        return Ok(json!({ "status": "cancelled" }));
    }
    let rate = state.lock_inner().audio.audio_io.spec().sample_rate;
    // A sample heard earlier in the session plays from its cached file
    // without a resolve or a download.
    let cached = cache.and_then(|cache| {
        let path = cache.file_for(provider, id)?;
        match decode_preview(&path, rate) {
            Ok(samples) => Some(samples),
            Err(error) => {
                log::warn!("[samples] a cached preview is unreadable: {error}");
                cache.forget(&path);
                None
            }
        }
    });
    let (provider_id, name, samples) = match cached {
        Some(samples) => (provider.to_string(), None, samples),
        None => {
            let (provider_id, item, samples) =
                fetch_preview(state, cache, provider, id, generation, rate)?;
            let Some(samples) = samples else {
                return Ok(preview_reply(
                    "cancelled",
                    &provider_id,
                    id,
                    &item.name,
                    0.0,
                ));
            };
            (provider_id, Some(item.name), samples)
        }
    };
    let name = name.unwrap_or_else(|| label.to_string());
    let seconds = (samples.len() / 2) as f64 / f64::from(rate);
    let mut inner = state.lock_inner();
    if !play_preview(state, &mut inner, generation, rate, samples)? {
        return Ok(preview_reply("cancelled", &provider_id, id, &name, 0.0));
    }
    state.audition.complete(generation, started);
    Ok(preview_reply("ok", &provider_id, id, &name, seconds))
}

/// Resolve and download one preview, keep it in the cache, and decode it.
/// The samples are `None` when a newer preview or a Stop replaced this one.
fn fetch_preview(
    state: &AppState,
    cache: Option<&PreviewCache>,
    provider: &str,
    id: &str,
    generation: u64,
    rate: u32,
) -> Result<(String, SampleItem, Option<Vec<f32>>), SampleCommandError> {
    let keys: Keys = samples::load_keys();
    let fetch = samples::http_fetch()?;
    let (provider_id, item) = samples::resolve_item(Some(provider), id, &keys, &fetch)?;
    let url = item
        .preview_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .or_else(|| (!item.download_url.trim().is_empty()).then_some(item.download_url.as_str()))
        .ok_or_else(|| format!("'{}' has no preview or download URL.", item.name))?
        .to_string();
    if !state.audition.is_current(generation) {
        return Ok((provider_id, item, None));
    }
    let body = fetch
        .get(&url, PREVIEW_MAX_BYTES)
        .map_err(|error| SampleError::from_fetch(&item.provider, error))?;
    if !state.audition.is_current(generation) {
        return Ok((provider_id, item, None));
    }
    // The download URL's bytes are only cached when they match what the item
    // declares, so a later add never starts from a file the store refuses.
    let cacheable = url != item.download_url || matches_declared(&item, &body.body);
    let stored = cache
        .filter(|_| cacheable)
        .and_then(|cache| cache.store(&provider_id, id, &url, url_extension(&url), &body.body));
    let samples = match stored {
        Some(path) => decode_preview(&path, rate)?,
        None => {
            let mut file = tempfile::Builder::new()
                .prefix("opencompose-sample-preview-")
                .suffix(&format!(".{}", url_extension(&url)))
                .tempfile()
                .map_err(|error| format!("Could not stage the preview: {error}"))?;
            file.write_all(&body.body)
                .map_err(|error| format!("Could not stage the preview: {error}"))?;
            decode_preview(file.path(), rate)?
        }
    };
    Ok((provider_id, item, Some(samples)))
}

fn decode_preview(path: &Path, rate: u32) -> Result<Vec<f32>, String> {
    let audio = oc_engine::audio_clip::decode_passage(path, 0.0, PREVIEW_SECONDS)?.audio;
    if audio.sample_rate == rate {
        return Ok(Arc::unwrap_or_clone(audio.samples));
    }
    oc_engine::resample::resample(
        &audio.samples,
        audio.sample_rate,
        rate,
        oc_engine::resample::ResampleQuality::DEFAULT_LIVE,
    )
}

fn play_preview(
    state: &AppState,
    inner: &mut AppStateInner,
    generation: u64,
    rate: u32,
    samples: Vec<f32>,
) -> Result<bool, String> {
    if !state.audition.is_current(generation) {
        return Ok(false);
    }
    if inner.audio.audio_io.spec().sample_rate != rate {
        return Err("The output sample rate changed while loading the preview".into());
    }
    inner
        .audio
        .play_audition_buffer(samples)
        .map_err(|error| format!("the preview could not play: {error}"))?;
    Ok(true)
}

fn preview_reply(status: &str, provider: &str, id: &str, name: &str, seconds: f64) -> Value {
    json!({
        "status": status,
        "provider": provider,
        "id": id,
        "name": name,
        "seconds": seconds,
    })
}

fn add_blocking(
    app: &tauri::AppHandle,
    provider: &str,
    id: &str,
    track: Option<String>,
    accept_licence: bool,
) -> Result<Value, SampleCommandError> {
    let keys: Keys = samples::load_keys();
    let network = samples::http_fetch()?;
    // The resolve always reaches the market: the licence and URL a pull uses
    // are never taken from a cache or from the caller.
    let (provider_id, item) = samples::resolve_item(Some(provider), id, &keys, &network)?;
    let state = app.state::<AppState>();
    // The pull takes seconds; the clip may only land in the document it started
    // in, because the media folder can be the same for two projects.
    let identity = DocumentIdentity::capture(&state);
    let (store, root) = samples::store_for(&state)?;
    let root =
        root.ok_or_else(|| "Save the project first: a sample needs a media folder.".to_string())?;
    // A file previewed from the same URL is read back from the cache; the
    // store verifies its size and checksum exactly as for a download.
    let cached;
    let fetch: &dyn SampleFetch = match preview_cache(app) {
        Some(cache) => {
            cached = CachedFetch {
                inner: &network,
                cache,
                provider: &provider_id,
                id,
            };
            &cached
        }
        None => &network,
    };
    // A Freesound token makes the pull try the original; a refused original
    // falls back to the preview with the same licence and credit.
    let receipt = samples::pull_item(&store, &item, fetch, accept_licence, keys.freesound_token())?;
    let seconds = clip_seconds(&receipt)?;
    let mut inner = state.lock_inner();
    let (snapshot, track_name) =
        place_pulled(&mut inner, &identity, &receipt, seconds, track.as_deref())?;
    drop(inner);
    super::arrangement::warm_audio_clip_peaks(&snapshot.project, Some(&root));
    Ok(json!({
        "status": "ok",
        "provider": provider_id,
        "provider_name": oc_sample::registry::spec(&item.provider).map_or("", |spec| spec.label),
        "receipt": serde_json::to_value(&receipt).unwrap_or(Value::Null),
        "track": track_name,
        "snapshot": serde_json::to_value(&snapshot).unwrap_or(Value::Null),
    }))
}

/// The document a pending download belongs to: the file it is saved as and the
/// undo revision it started from. Either moving means another document is open.
#[derive(Clone, PartialEq, Eq)]
struct DocumentIdentity {
    file_path: Option<PathBuf>,
    revision: u64,
}

impl DocumentIdentity {
    fn capture(state: &AppState) -> Self {
        Self::of(&state.lock_inner())
    }

    fn of(inner: &AppStateInner) -> Self {
        Self {
            file_path: inner.file_path.clone(),
            revision: inner.revision,
        }
    }
}

/// Place a downloaded sample in the document the pull started in, and refuse
/// every other one. The pulled file stays in the media folder either way.
fn place_pulled(
    inner: &mut AppStateInner,
    identity: &DocumentIdentity,
    receipt: &SampleReceipt,
    seconds: f64,
    requested_track: Option<&str>,
) -> Result<(super::shared::ProjectSnapshot, String), String> {
    if DocumentIdentity::of(inner) != *identity {
        return Err(format!(
            "the project changed while the sample downloaded; '{}' stays in the media folder",
            receipt.path
        ));
    }
    place_clip(inner, receipt, seconds, requested_track)
}

/// A receipt's declared duration, or the length of the file it wrote.
fn clip_seconds(receipt: &SampleReceipt) -> Result<f64, String> {
    if let Some(seconds) = receipt.duration_seconds
        && seconds > 0.0
    {
        return Ok(seconds);
    }
    let decoded = oc_engine::audio_clip::decode(Path::new(&receipt.path))?;
    Ok(decoded.frames() as f64 / f64::from(decoded.sample_rate))
}

/// Place the pulled file at the playhead and commit one undo entry.
fn place_clip(
    inner: &mut AppStateInner,
    receipt: &SampleReceipt,
    seconds: f64,
    requested_track: Option<&str>,
) -> Result<(super::shared::ProjectSnapshot, String), String> {
    let root = inner
        .file_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "Save the project first: a sample needs a media folder.".to_string())?;
    let file = Path::new(&receipt.path)
        .strip_prefix(&root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            format!(
                "'{}' is outside the project folder; a clip path must be relative to the project.",
                receipt.path
            )
        })?;
    let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties)?;
    let rate = inner.audio.sample_rate;
    let start_frame = inner.audio.timeline_cursor_frame();
    let position = clock.position_at_sample(start_frame, rate)?;
    let start = clock.bar_beat_floor_at_quarter(position.quarter)?;
    // The sample keeps its real span: walk the tempo map to the destination
    // frame instead of scaling by the tempo at the playhead alone.
    let end_frame = start_frame.saturating_add((seconds * f64::from(rate)) as u64);
    let destination = clock.position_at_sample(end_frame, rate)?;
    let end = grid_end(
        &clock,
        destination
            .quarter
            .max(position.quarter + MIN_CLIP_QUARTERS),
        position.signature.denominator.into(),
    )?;
    let mut next = inner.project.clone();
    let track =
        match requested_track.filter(|name| next.tracks.iter().any(|track| track.name == *name)) {
            Some(name) => name.to_string(),
            None => {
                let name = unique_track_name(&next);
                next.tracks.push(sample_track(&name));
                name
            }
        };
    let name = unique_clip_name(&next, &receipt.name);
    next.add_audio_clip(plain_audio_clip(
        name,
        track.clone(),
        file,
        TimeRange { start, end },
    ))?;
    oc_dsl::validate(&next).map_err(|errors| format!("Invalid sample clip: {errors:?}"))?;
    let old = std::mem::replace(&mut inner.project, next);
    super::shared::resync_with_undo(inner, old, UndoEdit::new("add sample", UndoScope::Project))?;
    Ok((super::shared::snapshot(inner), track))
}

/// The first notated position at or after `quarter`, so a length that does not
/// land on the grid still covers the whole sample.
fn grid_end(
    clock: &oc_dsl::timing::ProjectClock,
    quarter: f64,
    denominator: u32,
) -> Result<BarBeat, String> {
    let step = 1.0 / f64::from(denominator.max(1));
    // The candidate starts on the first notated position at or after the
    // requested length; adding whole steps keeps it on that grid.
    let mut candidate = (quarter / step).ceil() * step;
    for _ in 0..256 {
        if let Ok(position) = clock.bar_beat_at_quarter(candidate) {
            return Ok(position);
        }
        candidate += step;
    }
    Err("the sample length does not land on the notated grid".into())
}

pub(super) fn plain_audio_clip(
    name: String,
    track: String,
    file: String,
    time_range: TimeRange,
) -> AudioClip {
    AudioClip {
        name,
        track,
        file,
        time_range,
        placement: Default::default(),
        offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
        invert: false,
        crossfade_seconds: 0.0,
        stretch: 1.0,
        stretch_mode: oc_dsl::ast::StretchMode::default(),
        tempo_follow: false,
        pitch: 0.0,
        formant: 0.0,
        bypass_processing: false,
        preserve_formants: false,
        warp_markers: Vec::new(),
        loop_seconds: 0.0,
        take_group: None,
        take: None,
        muted: false,
        gain_envelope: Vec::new(),
    }
}

pub(super) fn sample_track(name: &str) -> Track {
    Track {
        name: name.to_string(),
        instrument: InstrumentPath::new("audio", "clip"),
        preset: None,
        engine: None,
        mix: MixState::default(),
        effects: Vec::new(),
        phrases: Vec::new(),
        patterns: Vec::new(),
        inline_patterns: Vec::new(),
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: Vec::new(),
    }
}

fn unique_track_name(project: &Project) -> String {
    for index in 1.. {
        let candidate = if index == 1 {
            "Samples".to_string()
        } else {
            format!("Samples {index}")
        };
        if !project.tracks.iter().any(|track| track.name == candidate) {
            return candidate;
        }
    }
    unreachable!("the track counter always finds a free name")
}

pub(super) fn unique_clip_name(project: &Project, sample_name: &str) -> String {
    let base = if sample_name.trim().is_empty() {
        "sample".to_string()
    } else {
        sample_name.trim().to_string()
    };
    for index in 1.. {
        let candidate = if index == 1 {
            base.clone()
        } else {
            format!("{base} {index}")
        };
        if !project
            .audio_clips()
            .iter()
            .any(|clip| clip.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("the clip counter always finds a free name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_sample::license::License;
    use oc_sample::model::{Quality, SampleKind};

    #[test]
    fn a_refusal_reaches_the_panel_as_its_code_and_message() {
        let error = SampleCommandError::from(SampleError::refused(
            "licence_notice",
            "Licence: CC BY-NC 4.0.",
        ));
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "code": "licence_notice", "message": "Licence: CC BY-NC 4.0." })
        );
        let failure = SampleCommandError::from(SampleError::failed("offline"));
        assert_eq!(
            serde_json::to_value(&failure).unwrap(),
            json!({ "code": null, "message": "offline" })
        );
        let text = SampleCommandError::from("no media folder".to_string());
        assert_eq!(text.code, None);
    }

    /// A cached preview plays without a resolve or a download: the test has no
    /// network fixture, so reaching the market would fail the call.
    #[test]
    fn a_cached_preview_plays_without_reaching_the_market() {
        let root = tempfile::tempdir().unwrap();
        let cache = PreviewCache::open(root.path().join(PREVIEW_CACHE_DIR), CacheLimits::default())
            .unwrap();
        let state = AppState::new();
        let rate = {
            let mut inner = state.lock_inner();
            inner.audio.enable_simulated_output();
            inner.audio.replace_private_audition(true).unwrap();
            inner.audio.audio_io.spec().sample_rate
        };
        let wav = oc_engine::wav::pcm_wav_bytes(&vec![0.1; rate as usize], rate, 2).unwrap();
        cache
            .store(
                "openverse",
                "openverse:cached",
                "https://example.org/cached.wav",
                "wav",
                &wav,
            )
            .unwrap();
        let generation = state.audition.begin("Cached");
        let reply = preview_blocking(
            &state,
            Some(&cache),
            &PreviewRequest {
                provider: "openverse",
                id: "openverse:cached",
                label: "Cached",
                generation,
                started: Instant::now(),
            },
        )
        .unwrap();
        assert_eq!(reply["status"], "ok");
        assert_eq!(reply["name"], "Cached");
        assert!((reply["seconds"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_preview_file_keeps_the_audio_extension() {
        assert_eq!(url_extension("https://example.org/a/b.flac?x=1"), "flac");
        assert_eq!(url_extension("https://example.org/stream"), "bin");
    }

    #[test]
    fn a_grid_end_rounds_up_to_the_next_notated_position() {
        let project =
            oc_dsl::parse("@project\n  tempo: 120\n@track tape | instrument: audio/clip\n")
                .unwrap();
        let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
        // 0.84 quarter past the start is not on the 16th grid; the range covers
        // the sample at the next 16th.
        let end = grid_end(&clock, 0.84, 4).unwrap();
        assert_eq!(end, BarBeat::new(1, 2, 0));
        let on_grid = grid_end(&clock, 1.0, 4).unwrap();
        assert_eq!(on_grid, BarBeat::new(1, 2, 0));
    }

    #[test]
    fn a_stopped_or_replaced_preview_never_starts_the_stream() {
        let (state, dir) = saved_state("stale-preview");
        let mut inner = state.lock_inner();
        let rate = inner.audio.audio_io.spec().sample_rate;

        // A preview that began before a Stop is stale and drops its buffer.
        let stopped = state.audition.begin("Stopped");
        state.audition.cancel_all();
        assert!(!play_preview(&state, &mut inner, stopped, rate, silent(rate)).unwrap());
        assert!(!inner.audio.audition_active());

        // A newer preview wins while the older one is still downloading.
        let older = state.audition.begin("Older");
        let newer = state.audition.begin("Newer");
        assert!(!play_preview(&state, &mut inner, older, rate, silent(rate)).unwrap());
        assert!(play_preview(&state, &mut inner, newer, rate, silent(rate)).unwrap());
        assert!(inner.audio.audition_active());

        drop(inner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_market_preview_stays_at_zero_until_its_buffer_is_resumed() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        inner.audio.enable_simulated_output();
        let rate = inner.audio.audio_io.spec().sample_rate;
        let generation = state.audition.begin("Paused sample");
        inner.audio.replace_private_audition(true).unwrap();
        let samples = vec![0.1; rate as usize * 2];
        assert!(play_preview(&state, &mut inner, generation, rate, samples).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(40));
        let mut snapshot = state.audition.snapshot();
        inner.audio.fill_audition_snapshot(&mut snapshot);
        assert!(snapshot.paused);
        assert_eq!(snapshot.position_seconds, 0.0);
        assert_eq!(inner.audio.heard_audition_blocks(), 0);
        inner.audio.resume_private_audition().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while inner.audio.heard_audition_blocks() == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn market_preview_decodes_only_the_audible_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        let rate = 8_000;
        let mut samples = vec![0.1; rate as usize * 32 * 2];
        samples[rate as usize * 30 * 2..].fill(0.9);
        std::fs::write(
            &path,
            oc_engine::wav::pcm_wav_bytes(&samples, rate, 2).unwrap(),
        )
        .unwrap();
        let preview = decode_preview(&path, 16_000).unwrap();
        assert_eq!(preview.len(), 16_000 * 30 * 2);
        assert!(preview.iter().all(|sample| sample.abs() < 0.2));
    }

    /// One second of stereo silence, long enough to read as playing.
    fn silent(rate: u32) -> Vec<f32> {
        vec![0.0; rate as usize * 2]
    }

    #[test]
    fn a_sample_keeps_its_real_span_across_a_tempo_change() {
        let (state, dir) = saved_state("tempo-change");
        {
            let mut inner = state.lock_inner();
            inner.project =
                oc_dsl::parse("@project\n  tempo: 120\n  tempo_map:\n    2:1 = 60 bpm\n").unwrap();
            inner.dsl_source = oc_dsl::serialize(&inner.project);
        }
        let receipt = receipt(&dir);
        let mut inner = state.lock_inner();
        // Four seconds at 120 BPM alone would reach bar 3. The 60 BPM from bar 2
        // makes the sample cover only two more quarters, ending at bar 2 beat 3.
        let (snapshot, _) = place_clip(&mut inner, &receipt, 4.0, None).unwrap();
        let clip = snapshot.project.audio_clip(0).unwrap();
        assert_eq!(clip.time_range.start, BarBeat::new(1, 1, 0));
        assert_eq!(clip.time_range.end, BarBeat::new(2, 3, 0));
        drop(inner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_into_another_document_is_refused() {
        let (state, dir) = saved_state("changed-document");
        let receipt = receipt(&dir);
        let media = PathBuf::from(&receipt.path);
        std::fs::create_dir_all(media.parent().unwrap()).unwrap();
        std::fs::write(&media, b"pulled").unwrap();

        // Another project opens while the pull runs: the clip must not land in
        // it, and the pulled file stays where the store wrote it.
        let mut inner = state.lock_inner();
        let identity = DocumentIdentity::of(&inner);
        inner.file_path = Some(dir.join("other.ocp"));
        let error = place_pulled(&mut inner, &identity, &receipt, 0.42, None).unwrap_err();
        assert!(
            error.contains("the project changed while the sample downloaded"),
            "{error}"
        );
        assert!(inner.project.audio_clips().is_empty());
        assert!(inner.undo.undo_entries().is_empty());

        // The same project with a newer revision refuses as well.
        inner.file_path = Some(dir.join("song.ocp"));
        let identity = DocumentIdentity::of(&inner);
        inner.revision += 1;
        let error = place_pulled(&mut inner, &identity, &receipt, 0.42, None).unwrap_err();
        assert!(
            error.contains("the project changed while the sample downloaded"),
            "{error}"
        );
        assert!(inner.project.audio_clips().is_empty());
        assert!(media.exists(), "the pulled file stays in the media folder");

        // An untouched document places the clip.
        let identity = DocumentIdentity::of(&inner);
        let (snapshot, _) = place_pulled(&mut inner, &identity, &receipt, 0.42, None).unwrap();
        assert_eq!(snapshot.project.audio_clips().len(), 1);
        drop(inner);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_track_and_clip_name_never_collide() {
        let mut project = oc_dsl::parse(
            "@project\n  tempo: 120\n@track Samples | instrument: audio/clip\n@track \"Samples 2\" | instrument: audio/clip\n",
        )
        .unwrap();
        assert_eq!(unique_track_name(&project), "Samples 3");
        project
            .add_audio_clip(AudioClip {
                name: "Snare".into(),
                track: "Samples".into(),
                file: "media/samples/snare.wav".into(),
                time_range: TimeRange {
                    start: BarBeat::new(1, 1, 0),
                    end: BarBeat::new(1, 2, 0),
                },
                placement: Default::default(),
                offset_seconds: 0.0,
                gain_db: 0.0,
                fade_in_seconds: 0.0,
                fade_out_seconds: 0.0,
                invert: false,
                crossfade_seconds: 0.0,
                stretch: 1.0,
                stretch_mode: oc_dsl::ast::StretchMode::default(),
                tempo_follow: false,
                pitch: 0.0,
                formant: 0.0,
                bypass_processing: false,
                preserve_formants: false,
                warp_markers: Vec::new(),
                loop_seconds: 0.0,
                take_group: None,
                take: None,
                muted: false,
                gain_envelope: Vec::new(),
            })
            .unwrap();
        assert_eq!(unique_clip_name(&project, "Snare"), "Snare 2");
    }

    fn receipt(dir: &Path) -> SampleReceipt {
        SampleReceipt {
            id: "openverse:snare".into(),
            provider: "openverse".into(),
            name: "Snare hit 01".into(),
            kind: SampleKind::DrumOneShot,
            duration_seconds: Some(0.42),
            channels: Some(1),
            sample_rate: Some(44_100),
            license: License::cc0(),
            attribution: "bassmosphere — CC0 1.0 — https://freesound.org/s/2/".into(),
            source_url: "https://freesound.org/s/2/".into(),
            quality: Quality::PreviewMp3,
            file: "openverse-snare-hit-01-72a91f6b.mp3".into(),
            path: dir
                .join("media/samples/openverse-snare-hit-01-72a91f6b.mp3")
                .display()
                .to_string(),
            bytes: 16_384,
            sha256: "00".repeat(32),
            fetched_at: 0,
            licence_accepted: false,
        }
    }

    fn saved_state(label: &str) -> (crate::state::AppState, PathBuf) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "oc-sample-add-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = crate::state::AppState::new();
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.file_path = Some(dir.join("song.ocp"));
        drop(inner);
        (state, dir)
    }

    #[test]
    fn adding_a_sample_places_an_audio_clip_on_a_new_track() {
        let (state, dir) = saved_state("new-track");
        let receipt = receipt(&dir);
        let mut inner = state.lock_inner();
        let (snapshot, track) = place_clip(&mut inner, &receipt, 0.42, None).unwrap();
        assert_eq!(track, "Samples");
        assert_eq!(snapshot.project.tracks.len(), 1);
        assert_eq!(snapshot.project.tracks[0].instrument.category, "audio");
        let clip = snapshot.project.audio_clip(0).unwrap();
        assert_eq!(clip.name, "Snare hit 01");
        assert_eq!(clip.track, "Samples");
        assert_eq!(
            clip.file,
            "media/samples/openverse-snare-hit-01-72a91f6b.mp3"
        );
        // At 120 BPM and the stop position, the clip starts at bar 1 beat 1 and
        // covers the 0.42 s sample on the 16th grid.
        assert_eq!(clip.time_range.start, BarBeat::new(1, 1, 0));
        assert_eq!(clip.time_range.end, BarBeat::new(1, 2, 0));
        assert!(clip.time_range.end != clip.time_range.start);
        assert_eq!(inner.undo.undo_entries().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_a_sample_uses_a_named_track_when_it_exists() {
        let (state, dir) = saved_state("named-track");
        let receipt = receipt(&dir);
        let mut inner = state.lock_inner();
        inner.project.tracks.push(sample_track("Tape"));
        let clock = oc_dsl::timing::ProjectClock::new(&inner.project.properties).unwrap();
        inner.audio.seek(5.0, &clock).unwrap();
        let (snapshot, track) = place_clip(&mut inner, &receipt, 1.0, Some("Tape")).unwrap();
        assert_eq!(track, "Tape");
        assert_eq!(snapshot.project.tracks.len(), 1);
        let clip = snapshot.project.audio_clip(0).unwrap();
        assert_eq!(clip.track, "Tape");
        assert_eq!(clip.time_range.start, BarBeat::new(2, 2, 0));
        assert_eq!(clip.time_range.end, BarBeat::new(2, 4, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_a_sample_refuses_an_unsaved_project() {
        let state = crate::state::AppState::new();
        let mut inner = state.lock_inner();
        let receipt = receipt(Path::new("/tmp"));
        let error = place_clip(&mut inner, &receipt, 1.0, None).unwrap_err();
        assert!(error.contains("Save the project first"), "{error}");
    }

    #[test]
    fn a_created_track_is_an_audio_clip_track() {
        let track = sample_track("Samples");
        assert_eq!(track.instrument.category, "audio");
        assert_eq!(track.instrument.name, "clip");
        assert!(track.clips.audio_clips().next().is_none());
    }
}
