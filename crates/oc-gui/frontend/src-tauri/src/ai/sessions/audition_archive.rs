//! The content-addressed audition archive beside the session store. A request
//! replays a bounded delivery clip; the operator keeps the full-rate render.
//! Files outlive individual sessions so a fork retains valid references, and a
//! full archive evicts its oldest unreferenced file first.

use super::{CONTEXT_AUDITION, MESSAGES_FILE, clip, message_protocol};
use crate::private_fs;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The stage file an MP3 encode writes before it is published.
pub(super) const AUDITION_RENDER_STAGE: &str = ".audition-render.part";

/// Content-addressed assets outlive individual sessions so forks retain valid
/// references. A full archive makes room by evicting its oldest files; a file a
/// live conversation names is evicted only after every unreferenced one.
pub(super) fn persist_auditions(
    root: &Path,
    messages: &mut [Value],
    settings: &crate::gui_settings::SessionAudioSettings,
) -> Result<Vec<EvictedAudition>, String> {
    use base64::Engine as _;
    let mut archive = ArchiveSweep::new(root);
    for message in messages {
        let Some(audio) = message.get("input_audio") else {
            continue;
        };
        super::super::media::inline_audio(audio).map_err(|error| error.message)?;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(audio["data"].as_str().unwrap_or_default())
            .map_err(|_| "Invalid audition audio")?;
        // The clip is the audio the model already listened to. Archive those
        // bytes and point the wire at them, so a later request replays the same
        // duration, channel count and rate. The render the clip was cut from can
        // be longer, wider and larger than one message may carry, so it stays
        // out of the delivery.
        let clip_path = archive.clip(&raw)?;
        let clip = super::super::media::attach(&clip_path.to_string_lossy())?;
        let descriptor = super::super::media::to_wire(clip.kind, &clip.media);
        // Keep the render by path. A later audition may request another
        // passage, but normal replay sends only the bounded delivery.
        let mut render_unavailable = audio["source_render_unavailable"]
            .as_bool()
            .unwrap_or(false);
        let render = audio
            .get("source_path")
            .filter(|_| !render_unavailable)
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .and_then(|source| {
                let result = archive.render(&source, settings);
                crate::commands::audition_scratch::finish_handoff(&source);
                match result {
                    Ok(path) if path != clip_path => Some(path),
                    Ok(_) => None,
                    Err(error) => {
                        log::warn!("audition render not archived: {error}");
                        render_unavailable = true;
                        None
                    }
                }
            });
        message_protocol::tag_new(message);
        let host_audition =
            message_protocol::kind(message).is_some_and(message_protocol::is_audition);
        let object = message.as_object_mut().ok_or("Invalid audition message")?;
        object.remove("input_audio");
        // The delivery is host context, not something the user typed: the
        // transcript and later compactions read this marker, not the label.
        if host_audition {
            object.insert("context".into(), json!(CONTEXT_AUDITION));
        }
        if let Some(render) = render {
            object.insert("source_render".into(), json!(render.to_string_lossy()));
        }
        if render_unavailable {
            object.insert("source_render_unavailable".into(), json!(true));
        }
        let media = object
            .entry("media")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Invalid audition attachments")?;
        media.push(descriptor);
    }
    Ok(archive.evicted)
}

/// The audition archive's cap, shared by its two directories.
pub const AUDITION_ARCHIVE_BUDGET: u64 = 256 * 1024 * 1024;

/// One directory of the audition archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditionDir {
    pub dir: String,
    pub files: usize,
    pub bytes: u64,
}

/// What the audition archive holds. The budget covers both directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditionArchive {
    pub media: AuditionDir,
    pub renders: AuditionDir,
    pub budget_bytes: u64,
}

/// A file the archive removed to stay inside its budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvictedAudition {
    /// The directory's name: `audition-media` or `audition-renders`.
    pub dir: String,
    pub file: String,
    pub bytes: u64,
}

/// Delivery clips: the bounded audio a request inlines, keyed by content.
pub(super) fn clip_dir(root: &Path) -> PathBuf {
    root.join("audition-media")
}

/// Full-rate renders kept for the operator. No request reads this directory.
pub(super) fn render_dir(root: &Path) -> PathBuf {
    root.join("audition-renders")
}

/// The archive as the Storage page reports it.
pub fn audition_archive(root: &Path) -> AuditionArchive {
    AuditionArchive {
        media: audition_dir(&clip_dir(root)),
        renders: audition_dir(&render_dir(root)),
        budget_bytes: AUDITION_ARCHIVE_BUDGET,
    }
}

fn audition_dir(dir: &Path) -> AuditionDir {
    let (files, bytes) = dir_usage(dir);
    AuditionDir {
        dir: dir.display().to_string(),
        files,
        bytes,
    }
}

fn dir_usage(dir: &Path) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        files += 1;
        bytes += meta.len();
    }
    (files, bytes)
}

/// Every hash a saved conversation names, and each session's version folder.
pub(super) fn session_references(root: &Path) -> (std::collections::HashSet<String>, Vec<PathBuf>) {
    let mut referenced = std::collections::HashSet::new();
    let mut version_dirs = Vec::new();
    for workspace in read_dirs(root) {
        for session in read_dirs(&workspace) {
            version_dirs.push(session.join("versions"));
            let Ok(text) = std::fs::read_to_string(session.join(MESSAGES_FILE)) else {
                continue;
            };
            for token in text.split(|c: char| !c.is_ascii_hexdigit()) {
                if token.len() == 64 {
                    referenced.insert(token.to_ascii_lowercase());
                }
            }
        }
    }
    (referenced, version_dirs)
}

fn referenced_tokens(root: &Path) -> std::collections::HashSet<String> {
    session_references(root).0
}

/// The directories under `root`, so the archive beside the sessions is never
/// read as a workspace.
fn read_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "audition-media" || name == "audition-renders")
        })
        .collect()
}

/// Delete every file in `dir` whose content hash no conversation names.
pub(super) fn sweep(dir: &Path, referenced: &std::collections::HashSet<String>, removed: &mut u64) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if referenced.contains(&stem) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            *removed += 1;
        }
    }
}

/// One write into the shared archive: the hashes conversations name, the files
/// this write already produced, and what it had to remove for room.
struct ArchiveSweep<'a> {
    root: &'a Path,
    referenced: Option<std::collections::HashSet<String>>,
    protected: std::collections::HashSet<String>,
    evicted: Vec<EvictedAudition>,
}

impl<'a> ArchiveSweep<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            referenced: None,
            protected: std::collections::HashSet::new(),
            evicted: Vec::new(),
        }
    }

    /// Reads the conversations once, and only when the archive is actually over
    /// its budget.
    fn referenced(&mut self) -> std::collections::HashSet<String> {
        self.referenced
            .get_or_insert_with(|| referenced_tokens(self.root))
            .clone()
    }

    /// Makes room for `add` bytes, evicting the archive's oldest files.
    fn room(&mut self, add: u64) -> Result<(), String> {
        let used = archived_bytes(self.root);
        if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
            return Ok(());
        }
        if add > AUDITION_ARCHIVE_BUDGET {
            return Err(format!(
                "one audition file of {add} bytes is larger than the {} MiB archive",
                AUDITION_ARCHIVE_BUDGET / (1024 * 1024)
            ));
        }
        let referenced = self.referenced();
        let evicted = make_room(self.root, add, &referenced, &self.protected);
        let freed: u64 = evicted.iter().map(|entry| entry.bytes).sum();
        self.evicted.extend(evicted);
        if used.saturating_add(add).saturating_sub(freed) > AUDITION_ARCHIVE_BUDGET {
            return Err("The audition archive is full of files this turn is still writing".into());
        }
        Ok(())
    }

    /// Keep the bounded model delivery byte-for-byte so later turns hear
    /// the same sample rate, channels and duration.
    fn clip(&mut self, raw: &[u8]) -> Result<PathBuf, String> {
        let hash = format!("{:x}", Sha256::digest(raw));
        let path = clip_dir(self.root).join(format!("{hash}.wav"));
        self.protected.insert(hash);
        if !path.is_file() {
            self.room(raw.len() as u64)?;
            private_fs::write_atomic(&path, raw)
                .map_err(|error| format!("Could not save audition audio: {error}"))?;
        }
        Ok(path)
    }

    /// Archive a rendered file at its own rate and channel count, so a stereo
    /// render stays stereo. Falls back to the clip's bytes when the container
    /// cannot hold the file's rate.
    fn render(
        &mut self,
        source: &Path,
        settings: &crate::gui_settings::SessionAudioSettings,
    ) -> Result<PathBuf, String> {
        let _scratch = crate::commands::audition_scratch::pin_reader(source)?;
        let media_dir = render_dir(self.root);
        let mut input = std::fs::File::open(source)
            .map_err(|error| format!("Could not open the render: {error}"))?;
        let length = input
            .metadata()
            .map_err(|error| format!("Could not measure the render: {error}"))?
            .len();
        if length > AUDITION_ARCHIVE_BUDGET {
            return Err("The full render exceeds the audition archive budget".into());
        }
        let hash = read_render_bytes(&mut input, &mut std::io::sink(), length)
            .map_err(|error| format!("Could not read the render: {error}"))?;
        let write_original = |input: &mut std::fs::File, path: &Path| {
            private_fs::write_atomic_with(path, |file| {
                if read_render_bytes(input, file, length)? != hash {
                    return Err(std::io::Error::other("The render changed while saving"));
                }
                Ok(())
            })
            .map_err(|error| format!("Could not save audition render: {error}"))
        };
        // Use another handle to the pinned file, including when the path is
        // replaced while the archive is being written.
        let open_wav = |input: &std::fs::File| oc_engine::wav::open_file_reader(input.try_clone()?);
        let archive_key =
            |profile: &str| format!("{:x}", Sha256::digest(format!("{hash}:{profile}")));
        let verify_source = || {
            let source_hash = std::fs::File::open(source)
                .and_then(|mut file| read_render_bytes(&mut file, &mut std::io::sink(), length))
                .map_err(|error| format!("Could not verify the render: {error}"))?;
            if source_hash != hash {
                return Err("The render changed while saving".into());
            }
            Ok::<(), String>(())
        };
        match settings.format {
            crate::gui_settings::SessionAudioFormat::Wav => {
                let key = archive_key("wav-44100");
                let path = media_dir.join(format!("{key}.wav"));
                self.protected.insert(key);
                if !path.is_file() {
                    // The archive is a standard 44.1 kHz stereo file a DAW can
                    // open, independent of the rate the model's clip is shaped to.
                    if let Ok(wav) = open_wav(&input) {
                        let spec = wav.spec();
                        if spec.sample_rate == 44_100 {
                            drop(wav);
                            self.room(length)?;
                            write_original(&mut input, &path)?;
                            return Ok(path);
                        }
                        if let Some(mut conversion) =
                            oc_engine::wav::PcmWavConversion::new(wav, 44_100)
                        {
                            self.room(conversion.encoded_len())?;
                            let saved = private_fs::write_atomic_with(&path, |file| {
                                conversion.write(file)?;
                                drop(conversion);
                                if read_render_bytes(&mut input, &mut std::io::sink(), length)?
                                    != hash
                                {
                                    return Err(std::io::Error::other(
                                        "The render changed while saving",
                                    ));
                                }
                                Ok(())
                            });
                            match saved {
                                Ok(()) => return Ok(path),
                                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {}
                                Err(error) => {
                                    return Err(format!("Could not save audition audio: {error}"));
                                }
                            }
                        }
                    }
                    let decoded = oc_engine::audio_clip::decode(source)
                        .ok()
                        .filter(|audio| audio.sample_rate != 44_100);
                    if let Some(decoded) = decoded {
                        let resampled = oc_engine::resample::resample(
                            &decoded.samples,
                            decoded.sample_rate,
                            44_100,
                            oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
                        )?;
                        let wav = oc_engine::wav::pcm_wav_bytes(&resampled, 44_100, 2)?;
                        verify_source()?;
                        self.room(wav.len() as u64)?;
                        private_fs::write_atomic(&path, &wav)
                            .map_err(|error| format!("Could not save audition audio: {error}"))?;
                    } else {
                        self.room(length)?;
                        write_original(&mut input, &path)?;
                    }
                }
                Ok(path)
            }
            crate::gui_settings::SessionAudioFormat::Mp3 => {
                let wav = open_wav(&input).ok();
                let profile = match &wav {
                    Some(wav) => {
                        let rate = archive_mp3_rate(wav.spec().sample_rate);
                        let bitrate = mp3_bitrate(rate, settings.mp3_kbps);
                        format!("mp3-cbr-{rate}-{bitrate}")
                    }
                    None => format!("mp3-cbr-requested-{}", settings.mp3_kbps),
                };
                let key = archive_key(&profile);
                let path = media_dir.join(format!("{key}.mp3"));
                self.protected.insert(key);
                if path.is_file() {
                    return Ok(path);
                }
                let mp3_spec = |rate| oc_engine::encode::EncodeSpec {
                    container: oc_dsl::ast::ExportContainer::Mp3,
                    wav: oc_engine::encode::WavOptions::default(),
                    flac: oc_engine::encode::FlacOptions::default(),
                    mp3: oc_engine::encode::Mp3Options {
                        bitrate: mp3_bitrate(rate, settings.mp3_kbps),
                        vbr_quality: None,
                    },
                    sample_rate: rate,
                    channels: 2,
                };
                private_fs::ensure_dir(&media_dir).map_err(|error| {
                    format!("Could not create the audition render archive: {error}")
                })?;
                // Keep incomplete encodes out of the archive and account for
                // the encoded size before making the final name visible.
                let stage = self.root.join(AUDITION_RENDER_STAGE);
                private_fs::write_atomic(&stage, &[])
                    .map_err(|error| format!("Could not stage the audition render: {error}"))?;
                let encoded = (|| {
                    if let Some(wav) = wav {
                        let rate = wav.spec().sample_rate;
                        if archive_mp3_rate(rate) == rate {
                            return oc_engine::encode::mp3::transcode_at_source_rate(
                                source,
                                &stage,
                                &mp3_spec(rate),
                            );
                        }
                        if let Some(mut conversion) =
                            oc_engine::wav::PcmWavConversion::new(wav, 48_000)
                        {
                            return oc_engine::encode::mp3::transcode_resampled_wav(
                                source,
                                &mut conversion,
                                &stage,
                                &mp3_spec(48_000),
                            );
                        }
                    }
                    let audio = oc_engine::audio_clip::decode(source)?;
                    if archive_mp3_rate(audio.sample_rate) == audio.sample_rate {
                        return oc_engine::encode::encode_to_file(
                            &stage,
                            &mp3_spec(audio.sample_rate),
                            &audio.samples,
                        )
                        .map(|_| ());
                    }
                    let resampled = oc_engine::resample::resample(
                        &audio.samples,
                        audio.sample_rate,
                        48_000,
                        oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
                    )?;
                    oc_engine::encode::encode_to_file(&stage, &mp3_spec(48_000), &resampled)
                        .map(|_| ())
                })();
                if let Err(error) = encoded {
                    log::warn!("Could not encode the audition render as MP3: {error}");
                    let _ = std::fs::remove_file(&stage);
                    let key = archive_key("original-fallback");
                    let wav = media_dir.join(format!("{key}.wav"));
                    self.protected.insert(key);
                    if !wav.is_file() {
                        self.room(length)?;
                        write_original(&mut input, &wav)?;
                    }
                    return Ok(wav);
                }
                let published = (|| {
                    verify_source()?;
                    let length = std::fs::metadata(&stage)
                        .map_err(|error| format!("Could not measure the archived MP3: {error}"))?
                        .len();
                    self.room(length)?;
                    std::fs::rename(&stage, &path)
                        .map_err(|error| format!("Could not publish the archived MP3: {error}"))
                })();
                if published.is_err() {
                    let _ = std::fs::remove_file(&stage);
                }
                published?;
                Ok(path)
            }
        }
    }
}

/// Hash or copy the same bounded source without materializing the full render.
fn read_render_bytes(
    input: &mut std::fs::File,
    output: &mut impl std::io::Write,
    length: u64,
) -> std::io::Result<String> {
    use std::io::{Read, Seek};
    input.rewind()?;
    let mut hash = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..count])?;
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(std::io::Error::other("The render grew while saving"));
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn archived_bytes(root: &Path) -> u64 {
    dir_usage(&clip_dir(root)).1 + dir_usage(&render_dir(root)).1
}

/// Removes the oldest files the archive may give up until `add` more bytes fit.
///
/// Unreferenced files go first, oldest first. A referenced render goes before a
/// referenced clip, because a clip is what a later request replays; a render is
/// the operator's copy, and a message that names one keeps the path after the
/// file is gone. Never removes a file this write produced.
fn make_room(
    root: &Path,
    add: u64,
    referenced: &std::collections::HashSet<String>,
    protected: &std::collections::HashSet<String>,
) -> Vec<EvictedAudition> {
    let mut used = archived_bytes(root);
    if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
        return Vec::new();
    }
    let mut candidates: Vec<(bool, bool, SystemTime, String, String, u64)> = Vec::new();
    for (dir, is_clip) in [(clip_dir(root), true), (render_dir(root), false)] {
        let label = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if protected.contains(&stem) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let referenced = referenced.contains(&stem);
            candidates.push((
                referenced,
                is_clip && referenced,
                meta.modified().unwrap_or(UNIX_EPOCH),
                label.clone(),
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string(),
                meta.len(),
            ));
        }
    }
    candidates.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.cmp(&right.3))
            .then(left.4.cmp(&right.4))
    });
    let mut evicted = Vec::new();
    for (_, _, _, dir, file, bytes) in candidates {
        if used.saturating_add(add) <= AUDITION_ARCHIVE_BUDGET {
            break;
        }
        if std::fs::remove_file(root.join(&dir).join(&file)).is_ok() {
            used = used.saturating_sub(bytes);
            evicted.push(EvictedAudition { dir, file, bytes });
        }
    }
    evicted
}

/// Keep a supported source rate, or use the archive's resampling rate.
pub(super) fn archive_mp3_rate(source_rate: u32) -> u32 {
    if oc_engine::encode::mp3::Mp3Version::for_sample_rate(source_rate).is_some() {
        source_rate
    } else {
        48_000
    }
}

/// Round a stored preference down to a CBR value the encoder accepts.
pub(super) fn mp3_bitrate(rate: u32, wanted: u32) -> u32 {
    let table = oc_engine::encode::mp3::Mp3Version::for_sample_rate(rate)
        .expect("archive MP3 rate is supported")
        .bitrates();
    table
        .iter()
        .rev()
        .copied()
        .find(|&bitrate| bitrate <= wanted)
        .unwrap_or(table[0])
}

/// Bound replay only, never the stored conversation. The newest explicit user
/// attachments win; older saved clips stay on disk and are named as omitted.
pub fn bound_media_replay(outbound: &mut [Value], incoming: &[Value]) {
    let newest = incoming
        .iter()
        .rev()
        .find(|message| message["role"] == "user");
    let mut required = std::collections::HashSet::new();
    if let Some(message) = incoming.iter().rev().find(|message| {
        message["role"] == "user"
            && message["media"]
                .as_array()
                .is_some_and(|media| !media.is_empty())
    }) && let Some(media) = message["media"].as_array()
    {
        for item in media {
            required.insert(item["sha256"].as_str().unwrap_or_default().to_owned());
        }
    }
    if let Some(message) = newest
        && let Some(data) = message["input_audio"]["data"].as_str()
    {
        use base64::Engine as _;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            required.insert(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    let cost = |entry: &Value| {
        if entry["local_reference"].as_bool() == Some(true) {
            return 0;
        }
        entry["bytes"].as_u64().unwrap_or(0).saturating_add(2) / 3 * 4
    };
    let mut used = outbound
        .iter()
        .filter_map(|m| m["media"].as_array())
        .flatten()
        .filter(|entry| required.contains(entry["sha256"].as_str().unwrap_or_default()))
        .map(cost)
        .sum::<u64>();
    let mut omitted = Vec::new();
    let mut omitted_count = 0;
    for message in outbound.iter_mut().rev() {
        let Some(media) = message.get_mut("media").and_then(Value::as_array_mut) else {
            continue;
        };
        media.retain(|entry| {
            if entry["local_reference"].as_bool() == Some(true)
                || required.contains(entry["sha256"].as_str().unwrap_or_default())
            {
                return true;
            }
            let size = cost(entry);
            if used.saturating_add(size) <= super::super::media::MAX_REQUEST_INLINE_BYTES as u64 {
                used += size;
                true
            } else {
                omitted_count += 1;
                if omitted.len() < 8 {
                    omitted.push(format!(
                        "{} at {}",
                        clip(entry["name"].as_str().unwrap_or("audio"), 100),
                        clip(entry["path"].as_str().unwrap_or(""), 300)
                    ));
                }
                false
            }
        });
    }
    if omitted_count > 0
        && let Some(message) = outbound
            .iter_mut()
            .rev()
            .find(|message| message["role"] == "user")
    {
        let content = message["content"].as_str().unwrap_or_default();
        message["content"] = json!(format!(
            "{content}\n[Audio replay budget: {omitted_count} older attachment(s) are saved on disk but NOT included in this request. Do not claim to have heard them. Request reattachment of a listed file when needed. Up to 8 omitted files: {}]",
            omitted.join("; ")
        ));
    }
}

