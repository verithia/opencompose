//! Per-patch loudness normalisation for OpenSynth.
//!
//! A patch is measured once, through its resolved preset and its effect chain,
//! on a fixed figure at 48 kHz. The measurement becomes a trim that every
//! render path applies to the instrument's master gain, so the same preset
//! plays at the same level in an offline render, in streaming playback and in
//! the preset browser. The document keeps the level the operator wrote;
//! `output.gain` is never rewritten.
//!
//! The trim is a property of the preset's content: its source, plugin,
//! program, file, full parameter list and effect chain. Two patches that share
//! parameters but differ by an effect are measured separately.

use oc_dsl::ast::{BaseDuration, Duration, NoteEvent, ParamValue, Pitch, PresetDef, Project};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use md5::{Digest, Md5};

/// Loudness the law holds a patch near, in dBFS RMS.
///
/// The factory bank's 62 patches measure -23.82 dBFS RMS at the median on this
/// probe, so most of them keep the level they already have and only the
/// outliers move.
pub const REFERENCE_RMS_DB: f64 = -23.8;
/// Largest correction in either direction. A near-silent patch is reported,
/// not amplified into hiss.
pub const MAX_TRIM_DB: f64 = 12.0;
/// Rate every probe renders at, independent of the transport's output rate.
pub const PROBE_RATE: u32 = 48_000;
/// Engine-owned parameter that carries the trim into `load_project_patch`.
///
/// A preset that names this parameter pins its own trim and is not measured.
pub const TRIM_PARAM: &str = oc_synth::synth::LOUDNESS_TRIM_PARAM;
/// The probe figure: the audition's shape, six ascending eighths.
const FIGURE: [(u8, u8); 6] = [
    (60, 100),
    (62, 92),
    (64, 96),
    (67, 100),
    (69, 92),
    (72, 104),
];
/// One step is an eighth note; the figure spans three quarters.
const STEP_QUARTERS: f64 = 0.5;
/// Below this cutoff the figure drops two octaves, between the two constants one.
const LOW_CUTOFF_HZ: f64 = 500.0;
/// At or above this cutoff the figure stays where it is.
const MID_CUTOFF_HZ: f64 = 1_200.0;

/// Trims already decided, keyed by the whole preset's content.
type TrimKey = [u8; 16];
type TrimCache = Mutex<HashMap<TrimKey, Arc<OnceLock<Option<f64>>>>>;
static TRIMS: LazyLock<TrimCache> = LazyLock::new(TrimCache::default);

// Bump for changes to the on-disk record format.
const TRIM_CACHE_VERSION: &str = "1";
// One entry per slot. A collision replaces the slot and never reuses its value.
const TRIM_CACHE_SLOTS: u16 = 512;

/// Trim in dB that the preset's instrument applies to its master gain.
///
/// The first call for a preset content renders the probe and caches the
/// result for the process; later calls are a lookup. A preset that is not
/// OpenSynth is left alone.
pub fn trim_db(preset: &PresetDef) -> f64 {
    trim_db_at(preset, None)
}

/// Read a completed in-process calibration without waiting for a probe or
/// reading the disk cache. Live keys use this on the latency-sensitive path.
pub fn ready_native_trim(preset: &PresetDef) -> Option<f64> {
    if !can_prepare_in_parallel(preset) {
        return None;
    }
    let key = preset_key(preset);
    TRIMS.try_lock().ok()?.get(&key)?.get().copied().flatten()
}

fn trim_db_at(preset: &PresetDef, resource_root: Option<&Path>) -> f64 {
    if preset.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
        return 0.0;
    }
    // A patch document can reference mutable files or a library directory.
    // Its authored fields alone do not identify the measured audio.
    if uses_external_assets(preset) {
        return resolve_trim(preset, resource_root, None).unwrap_or(0.0);
    }
    let key = preset_key(preset);
    memoized_trim(&TRIMS, key, || {
        let disk = can_prepare_in_parallel(preset)
            .then(trim_cache_dir)
            .flatten()
            .map(|dir| (dir, key));
        resolve_trim(preset, resource_root, disk)
    })
    .unwrap_or(0.0)
}

fn memoized_trim(
    cache: &TrimCache,
    key: TrimKey,
    calculate: impl FnOnce() -> Option<f64>,
) -> Option<f64> {
    let entry = cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .entry(key)
        .or_default()
        .clone();
    // Independent presets can render in parallel. A second request for this
    // preset waits on its own result without holding the map lock.
    let result = *entry.get_or_init(calculate);
    if result.is_none() {
        let mut entries = cache.lock().unwrap_or_else(|error| error.into_inner());
        if entries
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &entry))
        {
            entries.remove(&key);
        }
    }
    result
}

fn resolve_trim(
    preset: &PresetDef,
    resource_root: Option<&Path>,
    disk: Option<(PathBuf, TrimKey)>,
) -> Option<f64> {
    let trim = if let Some(trim) = disk.as_ref().and_then(|(dir, key)| read_trim(dir, *key)) {
        trim
    } else {
        let rms_db = measure_preset(preset, resource_root)?;
        let trim = trim_from_rms(rms_db);
        if let Some((dir, key)) = disk.as_ref() {
            write_trim(dir, *key, trim);
        }
        log::debug!(
            "[loudness] preset '{}' measured {rms_db:.2} dBFS RMS, trim {trim:+.2} dB",
            preset.name
        );
        trim
    };
    Some(trim)
}

fn trim_cache_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OCP_PRESET_TRIM_CACHE_DIR") {
        return Some(PathBuf::from(path));
    }
    // Development builds can change DSP without changing the app version.
    if cfg!(debug_assertions) || oc_audio::private_fs::running_under_test() {
        return None;
    }
    let media_dir = crate::media_cache::default_cache_dir()?;
    Some(media_dir.parent()?.join("preset-trims"))
}

fn trim_slot(dir: &Path, key: TrimKey) -> PathBuf {
    let slot = u16::from_le_bytes([key[0], key[1]]) % TRIM_CACHE_SLOTS;
    dir.join(format!("{slot:03x}.trim"))
}

fn read_trim(dir: &Path, key: TrimKey) -> Option<f64> {
    let mut entry = [0; 24];
    let mut file = std::fs::File::open(trim_slot(dir, key)).ok()?;
    file.read_exact(&mut entry).ok()?;
    if entry[..16] != key || file.read(&mut [0]).ok()? != 0 {
        return None;
    }
    let trim = f64::from_le_bytes(entry[16..].try_into().ok()?);
    (trim.is_finite() && trim.abs() <= MAX_TRIM_DB).then_some(trim)
}

fn write_trim(dir: &Path, key: TrimKey, trim: f64) {
    let mut entry = [0; 24];
    entry[..16].copy_from_slice(&key);
    entry[16..].copy_from_slice(&trim.to_le_bytes());
    // The cache is optional. A read-only or full disk cannot stop a render.
    let _ = oc_audio::private_fs::write_atomic(&trim_slot(dir, key), &entry);
}

fn uses_external_assets(preset: &PresetDef) -> bool {
    preset.file.is_some()
        || preset.params.iter().any(|(key, _)| {
            matches!(
                key.as_str(),
                "open_synth_state_file" | "wavetable.file" | "serum_file" | "serum_root"
            )
        })
}

/// Warm independent native patches before the streaming graph needs them.
pub fn prepare_parallel<'a>(presets: impl IntoIterator<Item = &'a PresetDef>) {
    prepare_parallel_with_options(
        presets,
        crate::render_parallel::RenderWorkerOptions::default(),
    );
}

/// Warm patches using the same worker snapshot as the owning render request.
pub fn prepare_parallel_with_options<'a>(
    presets: impl IntoIterator<Item = &'a PresetDef>,
    worker_options: crate::render_parallel::RenderWorkerOptions,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cached = TRIMS.lock().unwrap_or_else(|error| error.into_inner());
    let mut keys = HashSet::new();
    let pending: Vec<_> = presets
        .into_iter()
        .filter(|preset| can_prepare_in_parallel(preset))
        .filter(|preset| {
            let key = preset_key(preset);
            !cached.contains_key(&key) && keys.insert(key)
        })
        .collect();
    drop(cached);

    let workers = worker_options.workers().min(4).min(pending.len());
    if workers < 2 {
        return;
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(preset) = pending.get(next.fetch_add(1, Ordering::Relaxed)) {
                    trim_db(preset);
                }
            });
        }
    });
}

/// Whether the native probe can run without a project directory or hosted code.
pub fn can_prepare_in_parallel(preset: &PresetDef) -> bool {
    let has_text_parameter =
        |(_, value): &(String, ParamValue)| matches!(value, ParamValue::String(_));
    preset.source.as_deref() == Some("builtin")
        && preset.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)
        && !uses_external_assets(preset)
        && preset.patch.is_none()
        && !preset.params.iter().any(|(key, _)| key == TRIM_PARAM)
        && !preset.params.iter().any(has_text_parameter)
        && preset.effects.iter().all(|effect| {
            effect.effect_type.device_id().is_some()
                && !effect.params.iter().any(has_text_parameter)
        })
}

/// The law itself: hold `rms_db` at [`REFERENCE_RMS_DB`], clamped.
pub fn trim_from_rms(rms_db: f64) -> f64 {
    if !rms_db.is_finite() {
        return 0.0;
    }
    (REFERENCE_RMS_DB - rms_db).clamp(-MAX_TRIM_DB, MAX_TRIM_DB)
}

/// Append the trim to a built-in track's parameters, where every instrument
/// path reads it. A preset that names the trim itself keeps the level it pins,
/// which is how a fixture can compare patches at their authored level.
pub(crate) fn append_trim(
    params: &mut Vec<(String, ParamValue)>,
    preset: &PresetDef,
    resource_root: Option<&Path>,
) {
    if params.iter().any(|(key, _)| key == TRIM_PARAM) {
        return;
    }
    let trim = trim_db_at(preset, resource_root);
    if trim != 0.0 {
        params.push((TRIM_PARAM.to_owned(), ParamValue::Number(trim)));
    }
}

/// Integrated RMS of the preset on the fixed figure, in dBFS.
///
/// The preset is rendered through a one-track project of its own, so its
/// effect chain takes part in the measurement exactly as it takes part in a
/// real render. The probe pins the trim at zero on its own copy, so this
/// measurement is always the patch's raw level.
fn measure_preset(preset: &PresetDef, resource_root: Option<&Path>) -> Option<f64> {
    rms_dbfs(&probe_buffer_at(preset, true, resource_root)?)
}

#[cfg(test)]
fn probe_buffer(preset: &PresetDef, untrimmed: bool) -> Option<Vec<f32>> {
    probe_buffer_at(preset, untrimmed, None)
}

/// Render the probe figure with or without the law applied.
fn probe_buffer_at(
    preset: &PresetDef,
    untrimmed: bool,
    resource_root: Option<&Path>,
) -> Option<Vec<f32>> {
    let probe = probe_project(preset, untrimmed).ok()?;
    crate::render::render_to_buffer_per_track_at(&probe, Path::new(""), PROBE_RATE, resource_root)
        .ok()
}

/// One-track project that plays the fixed figure on a copy of `preset`.
///
/// The figure moves down for a low-cutoff patch: a sub-bass preset that is
/// probed two octaves above its own cutoff reports the filter, not the
/// patch's level.
fn probe_project(preset: &PresetDef, untrimmed: bool) -> Result<Project, String> {
    let mut probe = oc_dsl::parse(
        "@project\n@track probe | instrument: synth/lead_square\n  @phrase figure [1:1:0 - 2:1:0]\n",
    )
    .map_err(|error| error.to_string())?;
    probe.properties.tempo = 140.0;
    let mut probe_preset = preset.clone();
    if untrimmed {
        probe_preset
            .params
            .push((TRIM_PARAM.to_owned(), ParamValue::Number(0.0)));
    }
    probe.presets = vec![probe_preset];
    probe.tracks[0].preset = Some(preset.name.clone());
    let register = register_offset(preset);
    probe.tracks[0].phrases[0].events = figure_events(register);
    Ok(probe)
}

/// Octaves the figure moves down for a dark patch, following its own filter.
fn register_offset(preset: &PresetDef) -> i16 {
    let cutoff = preset
        .params
        .iter()
        .find_map(|(key, value)| match (key.as_str(), value) {
            ("filter.cutoff", ParamValue::Number(hz)) => Some(*hz),
            _ => None,
        });
    match cutoff {
        Some(hz) if hz < LOW_CUTOFF_HZ => -24,
        Some(hz) if hz < MID_CUTOFF_HZ => -12,
        _ => 0,
    }
}

/// The six figure notes as phrase events, transposed by `semitones`.
fn figure_events(semitones: i16) -> Vec<NoteEvent> {
    FIGURE
        .iter()
        .enumerate()
        .map(|(index, &(note, velocity))| NoteEvent {
            start_beat: index as f64 * STEP_QUARTERS,
            explicit_position: false,
            pitch: Pitch::from_midi((i32::from(note) + i32::from(semitones)).clamp(0, 127) as u8)
                .expect("audition transposition is clamped to seven-bit MIDI"),
            duration: Duration {
                base: BaseDuration::Eighth,
                dots: 0,
                triplet: false,
                tuplet: None,
                ties: Vec::new(),
            },
            dynamics: None,
            velocity: Some(velocity),
            articulation: None,
            vibrato: None,
            expression: None,
            pitch_bend: None,
            muted: false,
            grace: false,
            text: None,
            modifiers: Vec::new(),
        })
        .collect()
}

fn rms_dbfs(buffer: &[f32]) -> Option<f64> {
    if buffer.is_empty() {
        return None;
    }
    let power = buffer.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / buffer.len() as f64;
    (power > 0.0).then(|| 10.0 * power.log10())
}

/// Content digest of a preset: every field that can move its level, and
/// nothing else. The name and `extends` are browsing or resolution identity,
/// so two projects that carry the same patch share one measurement.
fn preset_key(preset: &PresetDef) -> TrimKey {
    fn text(hash: &mut Md5, value: &str) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut hash = Md5::new();
    for version in [
        TRIM_CACHE_VERSION,
        env!("CARGO_PKG_VERSION"),
        env!("OC_PRESET_TRIM_BUILD"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "dev"
        } else {
            "release"
        },
    ] {
        text(&mut hash, version);
    }
    for field in [
        preset.source.as_deref(),
        preset.plugin.as_deref(),
        preset.patch.as_deref(),
        preset.file.as_deref(),
    ] {
        text(&mut hash, field.unwrap_or(""));
    }
    text(&mut hash, &format!("{:?}", preset.program));
    text(&mut hash, &format!("{:?}", preset.root_note));
    hash.update((preset.params.len() as u64).to_le_bytes());
    for (key, value) in &preset.params {
        text(&mut hash, key);
        text(&mut hash, &format!("{value:?}"));
    }
    for effect in &preset.effects {
        text(&mut hash, &format!("{effect:?}"));
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_dsl::ast::PresetDef;

    fn preset(params: &str) -> PresetDef {
        let project = oc_dsl::parse(&format!(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {{{params}}}\n"
        ))
        .expect("the fixture parses");
        project.presets[0].clone()
    }

    #[test]
    fn concurrent_calibration_reuses_the_same_probe_and_allows_independent_work() {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let cache = TrimCache::default();
        let cache = &cache;
        let key = [1; 16];
        let (started, start) = channel();
        let (release, wait) = channel();
        let (independent, completed) = channel();
        std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                memoized_trim(cache, key, || {
                    started.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    Some(-3.25)
                })
            });
            start.recv_timeout(Duration::from_secs(5)).unwrap();
            let repeated = scope
                .spawn(move || memoized_trim(cache, key, || panic!("the same probe ran twice")));
            scope.spawn(move || {
                independent
                    .send(memoized_trim(cache, [2; 16], || Some(2.5)))
                    .unwrap();
            });
            let other = completed.recv_timeout(Duration::from_secs(5));
            release.send(()).unwrap();
            assert_eq!(other.unwrap(), Some(2.5));
            assert_eq!(first.join().unwrap(), Some(-3.25));
            assert_eq!(repeated.join().unwrap(), Some(-3.25));
        });
    }

    #[test]
    fn failed_calibration_can_be_retried() {
        let cache = TrimCache::default();
        let key = [1; 16];
        assert_eq!(memoized_trim(&cache, key, || None), None);
        assert_eq!(memoized_trim(&cache, key, || Some(0.0)), Some(0.0));
        assert_eq!(
            memoized_trim(&cache, key, || panic!("trim was not cached")),
            Some(0.0)
        );
    }

    #[test]
    fn parallel_preparation_preserves_the_original_measurement() {
        let first = preset("osc.a.level: 0.113, output.gain: 0.47");
        let second = preset("osc.a.level: 0.321, output.gain: 0.37");
        let asset = preset(r#"wavetable.file: "./user-table.wav""#);
        let pinned = preset("output.loudness_trim_db: 1");
        let mut hosted = first.clone();
        hosted.effects.push(oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Plugin("external.effect".into()),
            params: Vec::new(),
            bypass: false,
            sidechain_source: None,
        });
        assert!(can_prepare_in_parallel(&first));
        assert!(!can_prepare_in_parallel(&asset));
        assert!(!can_prepare_in_parallel(&pinned));
        assert!(!can_prepare_in_parallel(&hosted));
        let expected =
            [&first, &second].map(|preset| trim_from_rms(measure_preset(preset, None).unwrap()));
        prepare_parallel([&first, &second, &first, &asset, &pinned, &hosted]);
        assert_eq!([trim_db(&first), trim_db(&second)], expected);
    }

    #[test]
    fn disk_trim_reuses_exact_bits_and_refuses_invalid_records() {
        let dir = tempfile::tempdir().unwrap();
        let original = preset("osc.a.level: 0.423, output.gain: 0.17");
        let key = preset_key(&original);
        let measured = trim_from_rms(measure_preset(&original, None).unwrap());
        assert!(read_trim(dir.path(), key).is_none());
        write_trim(dir.path(), key, measured);
        let mut renamed = original.clone();
        renamed.name = "same sound".into();
        assert_eq!(
            read_trim(dir.path(), preset_key(&renamed))
                .unwrap()
                .to_bits(),
            measured.to_bits()
        );
        assert!(read_trim(dir.path(), preset_key(&preset("output.gain: 0.8"))).is_none());

        let mut collision = key;
        collision[2] ^= 1;
        assert_eq!(trim_slot(dir.path(), collision), trim_slot(dir.path(), key));
        write_trim(dir.path(), collision, -3.25);
        assert!(read_trim(dir.path(), key).is_none());
        assert_eq!(read_trim(dir.path(), collision), Some(-3.25));

        write_trim(dir.path(), key, f64::NAN);
        assert!(read_trim(dir.path(), key).is_none());
        std::fs::write(trim_slot(dir.path(), key), b"partial record").unwrap();
        assert!(read_trim(dir.path(), key).is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_relative_patch_is_measured_again_after_its_file_changes() {
        use oc_synth::synth::{SynthPatch, document::PatchDocument};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("patch.json");
        let mut document = PatchDocument {
            version: 1,
            name: "Probe fixture".into(),
            author: "OpenCompose".into(),
            category: "Test".into(),
            use_policy: "Fixture".into(),
            patch: SynthPatch {
                gain: 0.21,
                ..Default::default()
            },
        };
        let relative = preset(r#"open_synth_state_file: "patch.json""#);
        let mut absolute = relative.clone();
        absolute.params[0].1 = ParamValue::String(file.to_string_lossy().into_owned());
        let expected = |preset: &PresetDef| trim_from_rms(measure_preset(preset, None).unwrap());
        std::fs::write(&file, document.to_json().unwrap()).unwrap();
        let first = trim_db_at(&relative, Some(dir.path()));
        assert_eq!(first, expected(&absolute));
        assert_ne!(first, 0.0);
        assert_eq!(trim_db(&absolute), first);

        document.patch.gain = 0.8;
        std::fs::write(&file, document.to_json().unwrap()).unwrap();
        let replaced = trim_db_at(&relative, Some(dir.path()));
        assert_ne!(replaced, first);
        assert_eq!(replaced, expected(&absolute));
        assert_eq!(trim_db(&absolute), replaced);
    }

    #[test]
    fn a_patch_at_the_reference_is_left_at_unity() {
        assert_eq!(trim_from_rms(REFERENCE_RMS_DB), 0.0);
        assert_eq!(trim_from_rms(REFERENCE_RMS_DB + 4.0), -4.0);
        assert_eq!(trim_from_rms(REFERENCE_RMS_DB - 4.0), 4.0);
        assert_eq!(trim_from_rms(f64::NEG_INFINITY), 0.0);
        assert_eq!(trim_from_rms(REFERENCE_RMS_DB - 40.0), MAX_TRIM_DB);
        assert_eq!(trim_from_rms(REFERENCE_RMS_DB + 40.0), -MAX_TRIM_DB);
    }

    /// Two patches that differ only by their master gain are twelve decibels
    /// apart before the law and close after it. The gains stay below the
    /// master ceiling's knee, so the reading is the gain ratio and not the
    /// soft clip.
    #[test]
    fn two_patches_twelve_db_apart_land_within_three_db() {
        let loud = preset("osc.a.waveform: 1, osc.a.level: 0.9, output.gain: 0.5");
        let quiet = preset("osc.a.waveform: 1, osc.a.level: 0.9, output.gain: 0.125");
        let loud_rms = measure_preset(&loud, None).expect("the loud patch renders");
        let quiet_rms = measure_preset(&quiet, None).expect("the quiet patch renders");
        let raw_gap = loud_rms - quiet_rms;
        assert!(
            (raw_gap - 12.0).abs() < 1.0,
            "the fixtures are {raw_gap:.2} dB apart, not 12"
        );
        let loud_after = loud_rms + trim_db(&loud);
        let quiet_after = quiet_rms + trim_db(&quiet);
        assert!(
            (loud_after - quiet_after).abs() <= 3.0,
            "after normalisation the patches are {:.2} dB apart",
            loud_after - quiet_after
        );
    }

    /// The cache is keyed on the whole preset, effect chain included. Both
    /// copies carry the same name, so only the effect chain separates them.
    #[test]
    fn a_duplicate_preset_with_another_effect_chain_gets_its_own_trim() {
        let plain = preset("osc.a.waveform: 1, osc.a.level: 0.9, output.gain: 1.0");
        let softened = {
            let project = oc_dsl::parse(
                "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.waveform: 1, osc.a.level: 0.9, output.gain: 1.0}\n  effects:\n    - gain: factor=0.5\n",
            )
            .expect("the fixture parses");
            project.presets[0].clone()
        };
        assert_eq!(plain.name, softened.name);
        assert_eq!(plain.params, softened.params);
        assert!(!softened.effects.is_empty());
        let plain_trim = trim_db(&plain);
        let softened_trim = trim_db(&softened);
        assert_ne!(
            plain_trim, softened_trim,
            "the effect chain was left out of the cache key"
        );
        assert!(
            (softened_trim - plain_trim - 6.0).abs() < 1.5,
            "a -6 dB effect moved the trim by {:.2} dB",
            softened_trim - plain_trim
        );
    }

    /// Only OpenSynth is normalised; the measurement never rewrites the patch.
    #[test]
    fn the_law_leaves_other_instruments_and_the_patch_alone() {
        let mut soundfont = preset("output.gain: 0.5");
        soundfont.plugin = None;
        soundfont.source = Some("soundfont".into());
        assert_eq!(trim_db(&soundfont), 0.0);

        let patch = preset("osc.a.level: 0.9, output.gain: 0.5");
        let before = patch.clone();
        let _ = trim_db(&patch);
        assert_eq!(patch, before, "measuring must not rewrite the preset");
    }

    /// The trim reaches the instrument through the shared parameter path.
    #[test]
    fn the_shared_parameter_path_carries_the_trim() {
        let project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.9, output.gain: 1.0}\n@track t | preset: p, instrument: synth/lead_saw\n  @phrase n [1:1:0 - 2:1:0]\n    C4 q\n",
        )
        .expect("the fixture parses");
        let params = crate::preset::builtin_track_params(
            &project,
            project.presets.first(),
            &project.tracks[0],
        );
        let trim = params
            .iter()
            .find(|(key, _)| key == TRIM_PARAM)
            .map(|(_, value)| value);
        assert_eq!(
            trim,
            Some(&ParamValue::Number(trim_db(&project.presets[0])))
        );
        assert!(
            project.presets[0]
                .params
                .iter()
                .all(|(key, _)| key != TRIM_PARAM),
            "the documented preset gained a runtime parameter"
        );
    }

    /// A preset that names the trim keeps it; the engine does not measure it.
    #[test]
    fn a_pinned_trim_is_left_alone() {
        let project = oc_dsl::parse(
            "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.9, output.loudness_trim_db: 0}\n@track t | preset: p, instrument: synth/lead_saw\n  @phrase n [1:1:0 - 2:1:0]\n    C4 q\n",
        )
        .expect("the fixture parses");
        let params = crate::preset::builtin_track_params(
            &project,
            project.presets.first(),
            &project.tracks[0],
        );
        let pins: Vec<_> = params
            .iter()
            .filter(|(key, _)| key == TRIM_PARAM)
            .map(|(_, value)| value)
            .collect();
        assert_eq!(pins, vec![&ParamValue::Number(0.0)]);
    }

    fn peak_dbfs(buffer: &[f32]) -> f64 {
        let peak = buffer
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())));
        if peak <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * peak.log10()
        }
    }

    /// Report every OpenSynth preset's own level, the trim the law gives it
    /// and the level it lands on, so a before/after table can be read off one
    /// binary.
    #[test]
    #[ignore = "reads OCP_PRESET_PROBE_PROJECT and reports each preset's level"]
    fn review_preset_levels() {
        let Some(path) = std::env::var_os("OCP_PRESET_PROBE_PROJECT") else {
            eprintln!("PRESET_LEVEL set OCP_PRESET_PROBE_PROJECT to an .ocp path");
            return;
        };
        let text = std::fs::read_to_string(&path).expect("the probe project");
        let project = oc_dsl::preset::resolve(&oc_dsl::parse(&text).expect("the probe parses"))
            .expect("the probe resolves");
        let mut raw_levels = Vec::new();
        for preset in &project.presets {
            if preset.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
                continue;
            }
            let Some(raw) = measure_preset(preset, None) else {
                eprintln!("PRESET_LEVEL name={:?} probe render failed", preset.name);
                continue;
            };
            let trim = trim_db(preset);
            let after = probe_buffer(preset, false);
            let after_rms = after.as_deref().and_then(rms_dbfs);
            let after_peak = after.as_deref().map(peak_dbfs);
            raw_levels.push(raw);
            eprintln!(
                "PRESET_LEVEL name={:?} raw_rms_db={raw:.2} trim_db={trim:+.2} after_rms_db={:.2} after_peak_db={:.2}",
                preset.name,
                after_rms.unwrap_or(f64::NAN),
                after_peak.unwrap_or(f64::NAN)
            );
        }
        raw_levels.sort_by(|left, right| left.total_cmp(right));
        if let Some(median) = raw_levels.get(raw_levels.len() / 2) {
            eprintln!(
                "PRESET_LEVEL presets={} median_raw_rms_db={median:.2}",
                raw_levels.len()
            );
        }
    }
}
