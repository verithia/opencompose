//! Derived vocal-analysis files (`.ocn`) and the clip entry point.
//!
//! The DSP lives in `oc_dsp::vocal_analysis`; this module keeps the analysis
//! keyed on the same media identity the renderer uses (decoded file version,
//! project rate, baked stretch plan) and owns the on-disk format. Offline or
//! worker path only; the audio callback never calls this.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use oc_dsl::ast::{AudioClip, Project};
use oc_dsp::vocal_analysis::{self as dsp, DetectorParams, VocalAnalysis};

use crate::audio_clip;
use crate::derived_cache::StretchKey;

/// Bump when a change upstream can move any analysis bit.
pub const ANALYZER_VERSION: u32 = dsp::ANALYZER_VERSION;
/// Header version of the `.ocn` container itself.
pub const CACHE_VERSION: u8 = 2;

const CACHE_MAGIC: &[u8; 4] = b"OCNV";
const HEADER_BYTES: usize = 78;
const FRAME_BYTES: usize = 25;
const NOTE_BYTES: usize = 56;
const CHECKSUM_BYTES: usize = 8;

/// Identity of one analysis result.
///
/// `stretch` carries the source `MediaKey`, the target rate, the ratio bits,
/// the stretch mode and the stretch algorithm version, so a re-render that
/// changes the playback buffer cannot hit an old analysis.
#[derive(Debug, Clone)]
pub struct VocalAnalysisKey {
    pub stretch: StretchKey,
    /// First frame of the analysed playback window inside the stretched media.
    pub offset_frames: u64,
    /// Length of the analysed window in mono frames.
    pub length_frames: u64,
    pub detector: DetectorParams,
}

impl VocalAnalysisKey {
    /// Whole-media analysis at offset zero.
    pub fn new(stretch: StretchKey, detector: DetectorParams) -> Self {
        Self {
            stretch,
            offset_frames: 0,
            length_frames: 0,
            detector,
        }
    }

    /// Analysis of one clip's audible window inside the stretched media.
    pub fn for_window(
        stretch: StretchKey,
        offset_frames: u64,
        length_frames: u64,
        detector: DetectorParams,
    ) -> Self {
        Self {
            stretch,
            offset_frames,
            length_frames,
            detector,
        }
    }

    /// Stable string used for the file name and the embedded key hash.
    pub fn stamp(&self) -> String {
        format!(
            "{}|{}|{}|{}|{:016x}",
            self.stretch.stamp(),
            self.offset_frames,
            self.length_frames,
            ANALYZER_VERSION,
            self.detector.hash()
        )
    }

    /// Cache file name.
    pub fn file_name(&self) -> String {
        format!("{:032x}.ocn", Md5::digest(self.stamp().as_bytes()))
    }

    /// First 64 bits of the key, embedded so an explicit `--output` file can
    /// be validated even when its name is user-chosen.
    pub fn hash(&self) -> u64 {
        dsp::fnv1a(self.stamp().as_bytes())
    }
}

/// One analysis fetched from the cache or computed now.
#[derive(Debug, Clone)]
pub struct CachedVocalAnalysis {
    pub analysis: VocalAnalysis,
    pub from_cache: bool,
    pub path: Option<PathBuf>,
}

/// Analysis plus the clip surface it came from.
#[derive(Debug, Clone)]
pub struct ClipAnalysis {
    pub clip_name: String,
    pub track: String,
    pub file: String,
    pub target_rate: u32,
    pub clip_start_s: f64,
    pub duration_s: f64,
    pub analysis: VocalAnalysis,
    pub from_cache: bool,
    /// Where the `.ocn` lives; `None` when no cache directory is available.
    pub output: Option<PathBuf>,
    pub key: VocalAnalysisKey,
}

/// Default directory for generated analysis files, shared with the media cache.
pub fn default_cache_dir() -> Option<PathBuf> {
    std::env::var_os("OCP_DERIVED_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("OCP_MEDIA_CACHE_DIR").map(PathBuf::from))
        .or_else(crate::media_cache::default_cache_dir)
}

/// Cache file name for one key inside `dir`.
pub fn cache_path(dir: &Path, key: &VocalAnalysisKey) -> PathBuf {
    dir.join(key.file_name())
}

/// Analysed mono playback buffer for one clip, plus the key it maps to.
///
/// `samples` is the clip's playback buffer after stretch and offset, at
/// `target_rate`; the returned mono buffer is what the analyzer consumes.
pub fn clip_playback(
    project: &Project,
    clip: &AudioClip,
    target_rate: u32,
    root: Option<&Path>,
) -> Result<(VocalAnalysisKey, Vec<f32>), String> {
    let (key, playback) = clip_stereo_playback(project, clip, target_rate, root)?;
    let mut mono = Vec::with_capacity(playback.len() / 2);
    for frame in playback.chunks_exact(2) {
        mono.push(0.5 * (frame[0] + frame[1]));
    }
    Ok((key, mono))
}

/// Audible playback window for one clip as interleaved stereo, plus its key.
///
/// The window starts at the clip's offset and is what the analyzer consumes
/// (as a mono sum) and what a bake writes; both see identical samples.
pub fn clip_stereo_playback(
    project: &Project,
    clip: &AudioClip,
    target_rate: u32,
    root: Option<&Path>,
) -> Result<(VocalAnalysisKey, Vec<f32>), String> {
    let (stretch, playback) = audio_clip::clip_playback_pcm(
        crate::media_cache::shared(),
        crate::derived_cache::shared(),
        project,
        clip,
        target_rate,
        root,
    )?;
    let media_frames = playback.frames();
    let offset = (clip.offset_seconds * f64::from(target_rate)).round() as usize;
    if offset >= media_frames {
        return Err(format!(
            "audio clip '{}': offset is past the media",
            clip.name
        ));
    }
    let (_, length) = audio_clip::range_frames(project, clip, target_rate)
        .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
    let frames = (length as usize).min(media_frames - offset);
    if frames == 0 {
        return Err(format!("audio clip '{}': range is empty", clip.name));
    }
    Ok((
        VocalAnalysisKey::for_window(
            stretch,
            offset as u64,
            frames as u64,
            DetectorParams::default(),
        ),
        playback.window(offset..offset + frames)?,
    ))
}

/// Analyse one clip's playback buffer, reading and writing its `.ocn`.
///
/// `output` names an explicit cache file; without it the shared media cache
/// directory is used. A read failure is a miss, never an error.
pub fn analyze_clip(
    project: &Project,
    clip: &AudioClip,
    target_rate: u32,
    root: Option<&Path>,
    params: &DetectorParams,
    output: Option<&Path>,
) -> Result<ClipAnalysis, String> {
    let (key, mono) = clip_playback(project, clip, target_rate, root)?;
    let key = VocalAnalysisKey {
        detector: params.clone(),
        ..key
    };
    let cached = match output {
        // An explicit --output is a request to write that file: a failure is
        // reported rather than hidden. The shared cache is best-effort.
        Some(path) => get_or_analyze_file(path, &key, &mono, target_rate)?,
        None => get_or_analyze_dir(default_cache_dir().as_deref(), &key, &mono, target_rate)?,
    };
    let (start, _) = audio_clip::range_frames(project, clip, target_rate)
        .map_err(|error| format!("audio clip '{}': {error}", clip.name))?;
    Ok(ClipAnalysis {
        clip_name: clip.name.clone(),
        track: clip.track.clone(),
        file: clip.file.clone(),
        target_rate,
        clip_start_s: start as f64 / f64::from(target_rate),
        duration_s: cached.analysis.duration_s,
        analysis: cached.analysis,
        from_cache: cached.from_cache,
        output: cached.path,
        key,
    })
}

/// Resolve a selector to a clip: exact clip name first, then a track that owns
/// exactly one clip. Shared by `vocal analyze` and `vocal apply`.
pub fn resolve_clip<'a>(project: &'a Project, selector: &str) -> Result<&'a AudioClip, String> {
    let clips = project.audio_clips();
    if let Some(clip) = clips.iter().find(|clip| clip.name == selector) {
        return Ok(clip);
    }
    let on_track: Vec<&AudioClip> = clips
        .iter()
        .copied()
        .filter(|clip| clip.track == selector)
        .collect();
    match on_track.len() {
        0 => Err(format!("audio clip or track '{selector}' not found")),
        1 => Ok(on_track[0]),
        count => {
            let names: Vec<&str> = on_track.iter().map(|clip| clip.name.as_str()).collect();
            Err(format!(
                "track '{selector}' has {count} audio clips; name one of: {}",
                names.join(", ")
            ))
        }
    }
}

/// Read a valid file or analyse `samples` and persist the result.
pub fn get_or_analyze_file(
    path: &Path,
    key: &VocalAnalysisKey,
    samples: &[f32],
    sample_rate: u32,
) -> Result<CachedVocalAnalysis, String> {
    if let Some(analysis) = read_file(path, key) {
        return Ok(CachedVocalAnalysis {
            analysis,
            from_cache: true,
            path: Some(path.to_path_buf()),
        });
    }
    let analysis = dsp::analyze(samples, sample_rate, &key.detector)?;
    write_file(path, key, samples.len(), &analysis)?;
    Ok(CachedVocalAnalysis {
        analysis,
        from_cache: false,
        path: Some(path.to_path_buf()),
    })
}

/// Cache-directory variant of `get_or_analyze_file`; `None` means memory only.
pub fn get_or_analyze_dir(
    dir: Option<&Path>,
    key: &VocalAnalysisKey,
    samples: &[f32],
    sample_rate: u32,
) -> Result<CachedVocalAnalysis, String> {
    let Some(dir) = dir else {
        return Ok(CachedVocalAnalysis {
            analysis: dsp::analyze(samples, sample_rate, &key.detector)?,
            from_cache: false,
            path: None,
        });
    };
    let path = cache_path(dir, key);
    if let Some(analysis) = read_file(&path, key) {
        return Ok(CachedVocalAnalysis {
            analysis,
            from_cache: true,
            path: Some(path),
        });
    }
    let analysis = dsp::analyze(samples, sample_rate, &key.detector)?;
    let path = match write_file(&path, key, samples.len(), &analysis) {
        Ok(()) => Some(path),
        Err(_) => None,
    };
    Ok(CachedVocalAnalysis {
        analysis,
        from_cache: false,
        path,
    })
}

/// Write one analysis atomically. Never used by the audio callback.
pub fn write_file(
    path: &Path,
    key: &VocalAnalysisKey,
    source_frames: usize,
    analysis: &VocalAnalysis,
) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let frames = u32::try_from(analysis.frames.len())
        .map_err(|_| "analysis has too many frames".to_string())?;
    let notes = u32::try_from(analysis.notes.len())
        .map_err(|_| "analysis has too many notes".to_string())?;
    let mut bytes = Vec::with_capacity(
        HEADER_BYTES
            + analysis.frames.len() * FRAME_BYTES
            + analysis.notes.len() * NOTE_BYTES
            + CHECKSUM_BYTES,
    );
    bytes.extend_from_slice(CACHE_MAGIC);
    bytes.push(CACHE_VERSION);
    bytes.push(0);
    bytes.extend_from_slice(&now_nanos().to_le_bytes());
    bytes.extend_from_slice(&target_rate(key).to_le_bytes());
    bytes.extend_from_slice(&(source_frames as u64).to_le_bytes());
    bytes.extend_from_slice(&ANALYZER_VERSION.to_le_bytes());
    bytes.extend_from_slice(&key.detector.hash().to_le_bytes());
    bytes.extend_from_slice(&key.hash().to_le_bytes());
    bytes.extend_from_slice(&analysis.analysis_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&analysis.hop_seconds.to_le_bytes());
    bytes.extend_from_slice(&analysis.duration_s.to_le_bytes());
    bytes.extend_from_slice(&frames.to_le_bytes());
    bytes.extend_from_slice(&notes.to_le_bytes());
    debug_assert_eq!(bytes.len(), HEADER_BYTES);
    for (time_s, f0_hz, confidence, voiced) in analysis.frames.iter().map(|frame| {
        (
            frame.time_s,
            frame.f0_hz,
            frame.confidence,
            u8::from(frame.voiced),
        )
    }) {
        bytes.extend_from_slice(&time_s.to_le_bytes());
        bytes.extend_from_slice(&f0_hz.to_le_bytes());
        bytes.extend_from_slice(&confidence.to_le_bytes());
        bytes.push(voiced);
    }
    for note in &analysis.notes {
        bytes.extend_from_slice(&note.start_s.to_le_bytes());
        bytes.extend_from_slice(&note.end_s.to_le_bytes());
        bytes.extend_from_slice(&note.onset_s.to_le_bytes());
        bytes.extend_from_slice(&note.offset_s.to_le_bytes());
        bytes.extend_from_slice(&note.f0_median_hz.to_le_bytes());
        bytes.extend_from_slice(&note.confidence.to_le_bytes());
        bytes.extend_from_slice(&note.voiced_ratio.to_le_bytes());
    }
    let checksum = dsp::fnv1a(&bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());

    let temp = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    };
    match write() {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(format!("cannot write {}: {error}", path.display()))
        }
    }
}

/// Read one analysis, validating every header field and the checksum.
///
/// Returns `None` for a missing, stale, truncated or corrupt file; callers
/// treat that as a cache miss and re-analyse.
pub fn read_file(path: &Path, key: &VocalAnalysisKey) -> Option<VocalAnalysis> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < HEADER_BYTES + CHECKSUM_BYTES {
        return None;
    }
    if &bytes[0..4] != CACHE_MAGIC || bytes[4] != CACHE_VERSION {
        return None;
    }
    let stored_rate = u32::from_le_bytes(bytes[14..18].try_into().ok()?);
    let source_frames = u64::from_le_bytes(bytes[18..26].try_into().ok()?);
    let analyzer_version = u32::from_le_bytes(bytes[26..30].try_into().ok()?);
    let params_hash = u64::from_le_bytes(bytes[30..38].try_into().ok()?);
    let key_hash = u64::from_le_bytes(bytes[38..46].try_into().ok()?);
    if stored_rate != target_rate(key)
        || (key.length_frames > 0 && source_frames != key.length_frames)
        || analyzer_version != ANALYZER_VERSION
        || params_hash != key.detector.hash()
        || key_hash != key.hash()
    {
        return None;
    }
    let analysis_rate_hz = f64::from_le_bytes(bytes[46..54].try_into().ok()?);
    let hop_seconds = f64::from_le_bytes(bytes[54..62].try_into().ok()?);
    let duration_s = f64::from_le_bytes(bytes[62..70].try_into().ok()?);
    if !analysis_rate_hz.is_finite()
        || analysis_rate_hz <= 0.0
        || !hop_seconds.is_finite()
        || hop_seconds <= 0.0
        || !duration_s.is_finite()
        || duration_s <= 0.0
    {
        return None;
    }
    let frame_count = u32::from_le_bytes(bytes[70..74].try_into().ok()?) as usize;
    let note_count = u32::from_le_bytes(bytes[74..78].try_into().ok()?) as usize;
    let expected = HEADER_BYTES
        .checked_add(frame_count.checked_mul(FRAME_BYTES)?)?
        .checked_add(note_count.checked_mul(NOTE_BYTES)?)?
        .checked_add(CHECKSUM_BYTES)?;
    if bytes.len() != expected {
        return None;
    }
    let body = &bytes[..bytes.len() - CHECKSUM_BYTES];
    let checksum = u64::from_le_bytes(bytes[bytes.len() - CHECKSUM_BYTES..].try_into().ok()?);
    if dsp::fnv1a(body) != checksum {
        return None;
    }
    let mut frames = Vec::with_capacity(frame_count);
    let mut cursor = HEADER_BYTES;
    for _ in 0..frame_count {
        let time_s = read_f64(&bytes, &mut cursor)?;
        let f0_hz = read_f64(&bytes, &mut cursor)?;
        let confidence = read_f64(&bytes, &mut cursor)?;
        let voiced = *bytes.get(cursor)? != 0;
        cursor += 1;
        if !time_s.is_finite() || !f0_hz.is_finite() || !confidence.is_finite() {
            return None;
        }
        if voiced {
            if f0_hz <= 0.0 || !(0.0..=1.0).contains(&confidence) {
                return None;
            }
        } else if f0_hz != 0.0 || confidence != 0.0 {
            return None;
        }
        frames.push(dsp::VocalFrame {
            time_s,
            f0_hz,
            confidence,
            voiced,
        });
    }
    let mut notes = Vec::with_capacity(note_count);
    for _ in 0..note_count {
        let start_s = read_f64(&bytes, &mut cursor)?;
        let end_s = read_f64(&bytes, &mut cursor)?;
        let onset_s = read_f64(&bytes, &mut cursor)?;
        let offset_s = read_f64(&bytes, &mut cursor)?;
        let f0_median_hz = read_f64(&bytes, &mut cursor)?;
        let confidence = read_f64(&bytes, &mut cursor)?;
        let voiced_ratio = read_f64(&bytes, &mut cursor)?;
        if !start_s.is_finite()
            || !end_s.is_finite()
            || !onset_s.is_finite()
            || !offset_s.is_finite()
            || !f0_median_hz.is_finite()
            || !confidence.is_finite()
            || !voiced_ratio.is_finite()
        {
            return None;
        }
        if end_s <= start_s
            || offset_s <= onset_s
            || onset_s < start_s
            || offset_s > end_s
            || f0_median_hz <= 0.0
            || !(0.0..=1.0).contains(&confidence)
            || !(0.0..=1.0).contains(&voiced_ratio)
        {
            return None;
        }
        notes.push(dsp::VocalNote {
            start_s,
            end_s,
            onset_s,
            offset_s,
            f0_median_hz,
            confidence,
            voiced_ratio,
        });
    }
    Some(VocalAnalysis {
        analysis_rate_hz,
        hop_seconds,
        duration_s,
        frames,
        notes,
    })
}

fn target_rate(key: &VocalAnalysisKey) -> u32 {
    key.stretch.target_rate
}

fn read_f64(bytes: &[u8], cursor: &mut usize) -> Option<f64> {
    let value = f64::from_le_bytes(bytes.get(*cursor..*cursor + 8)?.try_into().ok()?);
    *cursor += 8;
    Some(value)
}

fn now_nanos() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_cache::MediaKey;
    use oc_dsl::ast::StretchMode;

    fn media(name: &str) -> MediaKey {
        MediaKey {
            path: PathBuf::from(format!("/tmp/{name}.wav")),
            mtime: (1_700_000_000, 7),
            size: 4096,
        }
    }

    fn key(rate: u32) -> VocalAnalysisKey {
        VocalAnalysisKey::new(
            StretchKey::new(
                media("take"),
                rate,
                1.0,
                StretchMode::Off,
                crate::stretch::ALGO_VERSION,
            ),
            DetectorParams::default(),
        )
    }

    fn temp_dir(name: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-vocal", name)
    }

    fn tone(seconds: f64, rate: u32) -> Vec<f32> {
        let frames = (rate as f64 * seconds) as usize;
        (0..frames)
            .map(|index| {
                let phase = std::f64::consts::TAU * 220.0 * index as f64 / rate as f64;
                (0.3 * phase.sin()) as f32
            })
            .collect()
    }

    fn same_analysis(left: &VocalAnalysis, right: &VocalAnalysis) -> bool {
        left.analysis_rate_hz.to_bits() == right.analysis_rate_hz.to_bits()
            && left.hop_seconds.to_bits() == right.hop_seconds.to_bits()
            && left.duration_s.to_bits() == right.duration_s.to_bits()
            && left.frames.len() == right.frames.len()
            && left.notes.len() == right.notes.len()
            && left.frames.iter().zip(&right.frames).all(|(a, b)| {
                a.time_s.to_bits() == b.time_s.to_bits()
                    && a.f0_hz.to_bits() == b.f0_hz.to_bits()
                    && a.confidence.to_bits() == b.confidence.to_bits()
                    && a.voiced == b.voiced
            })
            && left.notes.iter().zip(&right.notes).all(|(a, b)| {
                a.start_s.to_bits() == b.start_s.to_bits()
                    && a.end_s.to_bits() == b.end_s.to_bits()
                    && a.f0_median_hz.to_bits() == b.f0_median_hz.to_bits()
                    && a.confidence.to_bits() == b.confidence.to_bits()
                    && a.voiced_ratio.to_bits() == b.voiced_ratio.to_bits()
            })
    }

    #[test]
    fn cache_round_trip_is_bit_identical() {
        let dir = temp_dir("round-trip");
        let key = key(48_000);
        let samples = tone(0.6, 48_000);
        let first = get_or_analyze_dir(Some(&dir), &key, &samples, 48_000).unwrap();
        assert!(!first.from_cache);
        let path = first.path.clone().unwrap();
        assert!(path.exists());
        let second = get_or_analyze_dir(Some(&dir), &key, &samples, 48_000).unwrap();
        assert!(second.from_cache);
        assert!(same_analysis(&first.analysis, &second.analysis));
        assert_eq!(first.analysis.notes.len(), 1);
        assert!(
            (first.analysis.notes[0].f0_median_hz - 220.0).abs() < 0.5,
            "cached note {}",
            first.analysis.notes[0].f0_median_hz
        );
    }

    #[test]
    fn stale_analyzer_version_is_a_miss() {
        let dir = temp_dir("stale-version");
        let key = key(48_000);
        let samples = tone(0.4, 48_000);
        let path = cache_path(&dir, &key);
        write_file(
            &path,
            &key,
            samples.len(),
            &dsp::analyze(&samples, 48_000, &key.detector).unwrap(),
        )
        .unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[26..30].copy_from_slice(&(ANALYZER_VERSION + 1).to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(read_file(&path, &key).is_none());
        let refreshed = get_or_analyze_file(&path, &key, &samples, 48_000).unwrap();
        assert!(!refreshed.from_cache);
        assert!(read_file(&path, &key).is_some());
        assert!(
            get_or_analyze_file(&path, &key, &samples, 48_000)
                .unwrap()
                .from_cache
        );
    }

    #[test]
    fn corrupt_files_are_ignored_not_fatal() {
        let dir = temp_dir("corrupt");
        let key = key(48_000);
        let samples = tone(0.4, 48_000);
        let path = cache_path(&dir, &key);
        write_file(
            &path,
            &key,
            samples.len(),
            &dsp::analyze(&samples, 48_000, &key.detector).unwrap(),
        )
        .unwrap();
        let original = fs::read(&path).unwrap();

        for (name, bytes) in [
            ("truncated", original[..original.len() - 20].to_vec()),
            ("magic", {
                let mut bytes = original.clone();
                bytes[0] = b'X';
                bytes
            }),
            ("payload", {
                let mut bytes = original.clone();
                let index = HEADER_BYTES + FRAME_BYTES + 3;
                bytes[index] ^= 0x40;
                bytes
            }),
            ("checksum", {
                let mut bytes = original.clone();
                let last = bytes.len() - 1;
                bytes[last] ^= 0x01;
                bytes
            }),
        ] {
            fs::write(&path, &bytes).unwrap();
            assert!(read_file(&path, &key).is_none(), "{name} was accepted");
            let refreshed = get_or_analyze_file(&path, &key, &samples, 48_000).unwrap();
            assert!(!refreshed.from_cache, "{name} reported a cache hit");
            assert!(
                get_or_analyze_file(&path, &key, &samples, 48_000)
                    .unwrap()
                    .from_cache
            );
        }
    }

    #[test]
    fn detector_params_and_media_change_the_key() {
        let base = key(48_000);
        let params = DetectorParams {
            split_cents: 55.0,
            ..Default::default()
        };
        let changed = VocalAnalysisKey::new(base.stretch.clone(), params);
        assert_ne!(base.file_name(), changed.file_name());
        assert_ne!(base.hash(), changed.hash());

        let mut other_media = base.clone();
        other_media.stretch.media = media("other");
        assert_ne!(base.file_name(), other_media.file_name());

        let mut stretched = base.clone();
        stretched.stretch.ratio_bits = 1.5_f64.to_bits();
        assert_ne!(base.file_name(), stretched.file_name());

        let windowed = VocalAnalysisKey::for_window(
            base.stretch.clone(),
            48_000,
            96_000,
            DetectorParams::default(),
        );
        assert_ne!(base.file_name(), windowed.file_name());
    }

    #[test]
    fn no_cache_directory_still_analyses() {
        let key = key(44_100);
        let samples = tone(0.3, 44_100);
        let result = get_or_analyze_dir(None, &key, &samples, 44_100).unwrap();
        assert!(!result.from_cache);
        assert!(result.path.is_none());
        assert!(!result.analysis.frames.is_empty());
    }
}
