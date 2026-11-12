//! Attached media: validated file references that become inline base64 only
//! while a request body is built. The saved conversation keeps a path and a
//! content hash, so a session or `.ocp` never carries audio or image bytes.
use super::{Block, ProviderError, ProviderRequest, Turn, catalog, limits};
use crate::commands::{AiProviderEntry, Wire};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Inline media is base64, which inflates by 4/3. Gemini documents 20 MB as the
/// total inline request budget, so a 12 MiB file leaves ~4 MB for the prompt.
pub const MAX_FILE_BYTES: u64 = 12 * 1024 * 1024;
/// Disk-backed references do not contribute to the provider request.
pub const MAX_LOCAL_AUDIO_BYTES: u64 = 512 * 1024 * 1024;
/// Documented Gemini inline request limit, applied to the base64 payload.
pub const MAX_REQUEST_INLINE_BYTES: usize = 20_000_000;
/// A single message never needs a pile of clips or screenshots; this bounds the
/// request.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 4;
const MAX_PATH_CHARS: usize = 4096;
const MAX_NAME_CHARS: usize = 255;

/// Accepted audio extensions and the MIME type sent on the wire.
pub const ACCEPTED_AUDIO: &[(&str, &str)] = &[
    ("wav", "audio/wav"),
    ("mp3", "audio/mpeg"),
    ("aif", "audio/aiff"),
    ("aiff", "audio/aiff"),
    ("aac", "audio/aac"),
    ("ogg", "audio/ogg"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("opus", "audio/opus"),
];

/// Accepted image extensions and the MIME type sent on the wire.
pub const ACCEPTED_IMAGE: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Kind of media attached to a request.
pub enum MediaKind {
    Image,
    Audio,
}

/// Canonical, persistable media descriptor: a path plus a content hash. The
/// picker writes an absolute path; a client may supply one relative to its
/// working directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub name: String,
    pub path: String,
    pub mime: String,
    pub bytes: u64,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub local_reference: bool,
}

/// A descriptor the picker returns: its kind plus the canonical fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedMedia {
    pub kind: MediaKind,
    #[serde(flatten)]
    pub media: MediaRef,
}

#[derive(Debug, Clone, PartialEq)]
/// Saved or inline media payload.
pub enum MediaData {
    /// Saved form: resolved from disk when the request is built.
    File(MediaRef),
    /// Send-only form: base64 lives in memory for one request, never on disk.
    Inline { source: MediaRef, base64: String },
}

impl MediaData {
    /// Canonical media reference.
    pub fn source(&self) -> &MediaRef {
        match self {
            Self::File(source) => source,
            Self::Inline { source, .. } => source,
        }
    }
}

/// One attachment whose bytes changed since it was attached, or whose file is
/// gone. A missing attachment is skipped and the rest of the message still
/// sends, so the user sees a state instead of a failed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaNotice {
    pub name: String,
    pub path: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Reads, validates and hashes a file the user picked. Audio and the three
/// image formats every provider accepts are allowed; the model gate runs when
/// the request is built, not here.
pub fn attach(path: &str) -> Result<AttachedMedia, String> {
    if path.is_empty() || path.chars().count() > MAX_PATH_CHARS {
        return Err("Invalid attachment path".into());
    }
    let path = PathBuf::from(path);
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.chars().count() <= MAX_NAME_CHARS)
        .ok_or_else(|| format!("{} has no usable file name", path.display()))?
        .to_owned();
    let extension = extension(&path);
    let Some((kind, mime)) = media_mime(&extension) else {
        return Err(format!(
            "{name} is not an accepted audio or image format. Use {}.",
            accepted_extensions()
        ));
    };
    let limit = if kind == MediaKind::Audio {
        MAX_LOCAL_AUDIO_BYTES
    } else {
        MAX_FILE_BYTES
    };
    if metadata.len() > limit {
        return Err(attachment_size_error(&name, metadata.len(), limit));
    }
    let file = std::fs::File::open(&path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let bytes = std::io::copy(&mut file.take(limit + 1), &mut hash)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    if bytes > limit {
        return Err(attachment_size_error(&name, bytes, limit));
    }
    Ok(AttachedMedia {
        kind,
        media: MediaRef {
            name,
            path: path.to_string_lossy().into_owned(),
            mime: mime.to_owned(),
            bytes,
            duration_secs: (kind == MediaKind::Audio)
                .then(|| probe_duration(&path))
                .flatten(),
            sha256: format!("{:x}", hash.finalize()),
            local_reference: bytes > MAX_FILE_BYTES,
        },
    })
}

fn attachment_size_error(name: &str, bytes: u64, limit: u64) -> String {
    let label = if limit == MAX_FILE_BYTES {
        "inline media"
    } else {
        "local audio reference"
    };
    format!(
        "{name} is {}; the {label} limit is {} per file.",
        megabytes(bytes),
        megabytes(limit)
    )
}

/// Refuses before any network call: catalog capability, wire support, format,
/// size and the per-request inline budget. A file that is already gone is not
/// refused here: `resolve` skips it so the rest of the message still sends.
pub fn validate(entry: &AiProviderEntry, turns: &[Turn]) -> Result<(), ProviderError> {
    let wire = entry.wire();
    let mut inline_total = 0usize;
    for turn in turns {
        for block in &turn.blocks {
            let Block::Media { kind, data, .. } = block else {
                continue;
            };
            if gone(data) {
                continue;
            }
            let media = data.source();
            if media.local_reference {
                continue;
            }
            let audio = *kind == MediaKind::Audio;
            if audio {
                if !audio_supported(entry) {
                    return Err(ProviderError::config(format!(
                        "{} cannot accept audio input: its catalog entry has audio_in = false. \
                         Remove the attachment or choose a model that accepts audio.",
                        model_label(entry)
                    )));
                }
                if wire == Wire::AnthropicMessages {
                    return Err(ProviderError::config(format!(
                        "{} cannot accept audio input: the Anthropic Messages wire has no audio \
                         content block. Remove the attachment or choose a Gemini or OpenAI model.",
                        model_label(entry)
                    )));
                }
                if !wire_accepts(wire, &media.name) {
                    return Err(ProviderError::config(format!(
                        "{} cannot be sent over this wire: {} accepts {}. Convert the file or \
                         choose another model.",
                        media.name,
                        wire_label(wire),
                        wire_extensions(wire)
                    )));
                }
            }
            if *kind == MediaKind::Image
                && !limits::modality_allowed(entry, limits::InputModality::Image)
            {
                return Err(ProviderError::config(format!(
                    "{} cannot accept image input: its model limits declare {}. Remove the image \
                     or choose a model that accepts images.",
                    model_label(entry),
                    limits::input_modalities(entry).label()
                )));
            }
            if media.bytes > MAX_FILE_BYTES {
                return Err(ProviderError::config(format!(
                    "{} is {}; the inline limit is {} per file.",
                    media.name,
                    megabytes(media.bytes),
                    megabytes(MAX_FILE_BYTES)
                )));
            }
            inline_total += base64_len(media.bytes);
            if inline_total > MAX_REQUEST_INLINE_BYTES {
                return Err(ProviderError::config(format!(
                    "Attachments exceed the {} inline request limit (base64). Remove one or send a \
                     shorter clip.",
                    megabytes(MAX_REQUEST_INLINE_BYTES as u64)
                )));
            }
        }
    }
    Ok(())
}

/// True when the selected model declares audio input, through the same merged
/// view the send gate uses: a user override for a provider whose saved id
/// differs from the catalog id is still honoured.
pub fn audio_supported(entry: &AiProviderEntry) -> bool {
    limits::modality_allowed(entry, limits::InputModality::Audio)
}

/// Re-reads every file attachment, re-hashes it and replaces it with inline
/// base64 for the request body. A changed hash is returned as a notice; the new
/// bytes are what gets sent. A text block naming the path is inserted before
/// each media block, so every wire that lowers blocks carries it.
pub fn resolve(request: &mut ProviderRequest) -> Result<Vec<MediaNotice>, ProviderError> {
    let mut notices = Vec::new();
    let mut inline_total = 0usize;
    for turn in &mut request.turns {
        let mut index = 0;
        while index < turn.blocks.len() {
            let found = match &turn.blocks[index] {
                Block::Media {
                    kind,
                    data: MediaData::File(reference),
                    ..
                } => Some((*kind, reference.clone())),
                _ => None,
            };
            let Some((kind, reference)) = found else {
                index += 1;
                continue;
            };
            if reference.local_reference {
                turn.blocks[index] = Block::Text(if Path::new(&reference.path).is_file() {
                    describe_local_reference(&reference)
                } else {
                    notices.push(MediaNotice {
                        name: reference.name.clone(),
                        path: reference.path.clone(),
                        sha256: reference.sha256.clone(),
                        missing: true,
                    });
                    format!(
                        "Local audio reference {} is missing at {}.",
                        serde_json::json!(reference.name),
                        serde_json::json!(reference.path)
                    )
                });
                index += 1;
                continue;
            }
            let mut bytes = Vec::new();
            let read = std::fs::File::open(&reference.path)
                .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
            match read {
                Ok(_) => {}
                // A reference to a deleted file is not a failed turn: drop the
                // block, say so in the text and let the rest of the message go.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    turn.blocks[index] = Block::Text(format!(
                        "Attachment \"{}\" is missing at {}; it was not sent.",
                        reference.name, reference.path
                    ));
                    notices.push(MediaNotice {
                        name: reference.name.clone(),
                        path: reference.path.clone(),
                        sha256: reference.sha256.clone(),
                        missing: true,
                    });
                    index += 1;
                    continue;
                }
                Err(error) => {
                    return Err(ProviderError::config(format!(
                        "Attachment {} cannot be read at {}: {error}. Remove it or re-attach.",
                        reference.name, reference.path
                    )));
                }
            }
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err(ProviderError::config(format!(
                    "{} is {}; the inline limit is {} per file.",
                    reference.name,
                    megabytes(bytes.len() as u64),
                    megabytes(MAX_FILE_BYTES)
                )));
            }
            let sha256 = digest(&bytes);
            let changed = !reference.sha256.eq_ignore_ascii_case(&sha256);
            let base64 = STANDARD.encode(&bytes);
            inline_total += base64.len();
            if inline_total > MAX_REQUEST_INLINE_BYTES {
                return Err(ProviderError::config(format!(
                    "Attachments exceed the {} inline request limit (base64). Remove one or send a \
                     shorter clip.",
                    megabytes(MAX_REQUEST_INLINE_BYTES as u64)
                )));
            }
            let source = MediaRef {
                sha256: sha256.clone(),
                bytes: bytes.len() as u64,
                duration_secs: if changed && kind == MediaKind::Audio {
                    probe_duration(Path::new(&reference.path)).or(reference.duration_secs)
                } else {
                    reference.duration_secs
                },
                ..reference
            };
            if changed {
                notices.push(MediaNotice {
                    name: source.name.clone(),
                    path: source.path.clone(),
                    sha256,
                    missing: false,
                });
            }
            let described = describe(kind, &source, changed);
            if let Block::Media { data, .. } = &mut turn.blocks[index] {
                *data = MediaData::Inline { source, base64 };
            }
            if let Some(text) = described {
                turn.blocks.insert(index, Block::Text(text));
                index += 2;
            } else {
                index += 1;
            }
        }
    }
    Ok(notices)
}

fn describe_local_reference(media: &MediaRef) -> String {
    let duration = media
        .duration_secs
        .map(|seconds| format!(", {seconds:.2} s at attachment time"))
        .unwrap_or_default();
    format!(
        "Local audio reference {} at {} ({size}{duration}). No audio was sent with this reference.          Use audio_analyze, analyze_notes, separate_stems or audio_compare with this path.          To listen to a bounded source excerpt, use audition_clip_source with this path if your model accepts audio.",
        serde_json::json!(media.name),
        serde_json::json!(media.path),
        size = human_size(media.bytes),
    )
}

/// The sentence a model needs to point a measurement tool at an attachment.
/// No path, no sentence: the inline audition clip has none and is sent-only.
fn describe(kind: MediaKind, media: &MediaRef, changed: bool) -> Option<String> {
    let path = media.path.trim();
    if path.is_empty() {
        return None;
    }
    let size = human_size(media.bytes);
    let mut text = match kind {
        MediaKind::Audio => {
            let duration = media
                .duration_secs
                .map(|seconds| format!("{seconds:.2} s, "))
                .unwrap_or_default();
            format!(
                "Attached audio \"{}\" is at {path} ({duration}{size}); the analysis tools \
                 (audio_analyze, analyze_notes, separate_stems, audio_compare) accept this path \
                 directly.",
                media.name
            )
        }
        MediaKind::Image => format!("Attached image \"{}\" is at {path} ({size}).", media.name),
    };
    if changed {
        text.push_str(
            " It changed on disk since it was attached, so earlier measurements may be stale.",
        );
    }
    Some(text)
}

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    if bytes as f64 >= 1024.0 * KIB {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * KIB))
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// The shared lowering primitive: kind, MIME and base64 for a resolved block.
pub fn inline_media(block: &Block) -> Result<(MediaKind, &str, &str), ProviderError> {
    let Block::Media { kind, mime, data } = block else {
        return Err(ProviderError::config("Expected a media block"));
    };
    match data {
        MediaData::Inline { base64, .. } => Ok((*kind, mime.as_str(), base64.as_str())),
        MediaData::File(_) => Err(ProviderError::config(
            "Attachment was not resolved before the request body was built.",
        )),
    }
}

/// One inline `input_audio` part the panel delivers for a listening tool call.
/// The bytes stay in memory for one request and never become a file.
pub fn inline_audio(value: &serde_json::Value) -> Result<Block, ProviderError> {
    let invalid = || ProviderError::config("Invalid inline audio part");
    let data = value["data"]
        .as_str()
        .filter(|data| !data.is_empty())
        .ok_or_else(invalid)?;
    if value["format"].as_str() != Some("wav") {
        return Err(ProviderError::config("Inline audio must be wav"));
    }
    let bytes = STANDARD.decode(data).map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ProviderError::config(format!(
            "Inline audio is {}; the inline limit is {} per file.",
            megabytes(bytes.len() as u64),
            megabytes(MAX_FILE_BYTES)
        )));
    }
    Ok(Block::Media {
        kind: MediaKind::Audio,
        mime: "audio/wav".into(),
        data: MediaData::Inline {
            source: MediaRef {
                name: "audition.wav".into(),
                path: String::new(),
                mime: "audio/wav".into(),
                bytes: bytes.len() as u64,
                duration_secs: None,
                sha256: String::new(),
                local_reference: false,
            },
            base64: data.to_owned(),
        },
    })
}

/// Parses one `media` entry from the panel's wire messages.
pub fn from_wire(value: &serde_json::Value) -> Result<(MediaKind, MediaRef), ProviderError> {
    let kind = value["kind"]
        .as_str()
        .ok_or_else(|| ProviderError::config("Invalid attachment: missing kind"))?;
    let kind = match kind {
        "audio" => MediaKind::Audio,
        "image" => MediaKind::Image,
        _ => return Err(ProviderError::config("Invalid attachment kind")),
    };
    let text = |field: &str, max: usize| -> Result<String, ProviderError> {
        value[field]
            .as_str()
            .filter(|text| !text.is_empty() && text.chars().count() <= max && !text.contains('\0'))
            .map(str::to_owned)
            .ok_or_else(|| ProviderError::config(format!("Invalid attachment {field}")))
    };
    let name = text("name", MAX_NAME_CHARS)?;
    let path = text("path", MAX_PATH_CHARS)?;
    let mime = text("mime", MAX_NAME_CHARS)?;
    let sha256 = text("sha256", 64)?;
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProviderError::config("Invalid attachment hash"));
    }
    let local_reference = match &value["local_reference"] {
        serde_json::Value::Null | serde_json::Value::Bool(false) => false,
        serde_json::Value::Bool(true) if kind == MediaKind::Audio => true,
        _ => return Err(ProviderError::config("Only audio may be a local reference")),
    };
    let limit = if local_reference {
        MAX_LOCAL_AUDIO_BYTES
    } else {
        MAX_FILE_BYTES
    };
    let bytes = value["bytes"]
        .as_u64()
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(|| ProviderError::config("Invalid attachment size"))?;
    let duration_secs = match &value["duration_secs"] {
        serde_json::Value::Null => None,
        other => Some(
            other
                .as_f64()
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .ok_or_else(|| ProviderError::config("Invalid attachment duration"))?,
        ),
    };
    let reference = MediaRef {
        name,
        path,
        mime,
        bytes,
        duration_secs,
        sha256: sha256.to_ascii_lowercase(),
        local_reference,
    };
    Ok((kind, reference))
}

/// A saved descriptor as the wire reads it. An entry written before `kind` was
/// recorded gets it back from the MIME type, so an old session still replays its
/// attachment instead of failing the turn.
pub(crate) fn from_saved(
    value: &serde_json::Value,
) -> Result<(MediaKind, MediaRef), ProviderError> {
    if let Ok(parsed) = from_wire(value) {
        return Ok(parsed);
    }
    let kind = match value["mime"].as_str() {
        Some(mime) if mime.starts_with("audio/") => "audio",
        Some(mime) if mime.starts_with("image/") => "image",
        _ => return from_wire(value),
    };
    let mut repaired = value.clone();
    repaired["kind"] = serde_json::json!(kind);
    from_wire(&repaired)
}

/// The persistable shape of one attachment (path + hash, never bytes).
pub(crate) fn to_wire(kind: MediaKind, media: &MediaRef) -> serde_json::Value {
    let mut wire = serde_json::json!({
        "kind": kind,
        "name": media.name,
        "path": media.path,
        "mime": media.mime,
        "bytes": media.bytes,
        "duration_secs": media.duration_secs,
        "sha256": media.sha256,
    });
    if media.local_reference {
        wire["local_reference"] = serde_json::json!(true);
    }
    wire
}

/// True when a saved file reference no longer points at a file.
pub(crate) fn gone(data: &MediaData) -> bool {
    match data {
        MediaData::File(reference) => !Path::new(&reference.path).is_file(),
        MediaData::Inline { .. } => false,
    }
}

/// The blocks of a saved conversation, as the panel holds them. Media entries
/// carry a path and a hash; base64 from a resolved request never appears here.
#[cfg(test)]
pub(crate) fn conversation_wire(turns: &[Turn]) -> serde_json::Value {
    use super::Role;
    let messages = turns
        .iter()
        .map(|turn| {
            let text = super::block_text(&turn.blocks);
            let media = turn
                .blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Media { kind, data, .. } => Some(to_wire(*kind, data.source())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let role = match turn.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            serde_json::json!({"role": role, "content": text, "media": media})
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(messages)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn audio_mime(extension: &str) -> Option<&'static str> {
    ACCEPTED_AUDIO
        .iter()
        .find(|(accepted, _)| *accepted == extension)
        .map(|(_, mime)| *mime)
}

/// The kind and MIME type of an accepted picker file.
fn media_mime(extension: &str) -> Option<(MediaKind, &'static str)> {
    if let Some(mime) = audio_mime(extension) {
        return Some((MediaKind::Audio, mime));
    }
    ACCEPTED_IMAGE
        .iter()
        .find(|(accepted, _)| *accepted == extension)
        .map(|(_, mime)| (MediaKind::Image, *mime))
}

fn accepts_extension(extension: &str) -> bool {
    audio_mime(extension).is_some()
}

fn accepted_extensions() -> String {
    ACCEPTED_AUDIO
        .iter()
        .chain(ACCEPTED_IMAGE.iter())
        .map(|(extension, _)| *extension)
        .collect::<Vec<_>>()
        .join(", ")
}

fn wire_accepts(wire: Wire, name: &str) -> bool {
    let extension = extension(Path::new(name));
    match wire {
        Wire::GeminiGenerate | Wire::GeminiInteractions => accepts_extension(&extension),
        // OpenAI documents only wav and mp3 for `input_audio`.
        Wire::OpenAiChatCompletions | Wire::OpenAiResponses => {
            matches!(extension.as_str(), "wav" | "mp3")
        }
        Wire::AnthropicMessages | Wire::Unknown => false,
    }
}

fn wire_extensions(wire: Wire) -> String {
    match wire {
        Wire::GeminiGenerate | Wire::GeminiInteractions => accepted_extensions(),
        Wire::OpenAiChatCompletions | Wire::OpenAiResponses => "wav or mp3".into(),
        Wire::AnthropicMessages | Wire::Unknown => "no audio".into(),
    }
}

fn wire_label(wire: Wire) -> &'static str {
    match wire {
        Wire::OpenAiChatCompletions => "OpenAI Chat Completions",
        Wire::OpenAiResponses => "OpenAI Responses",
        Wire::AnthropicMessages => "Anthropic Messages",
        Wire::GeminiInteractions => "Gemini Interactions",
        Wire::GeminiGenerate => "Gemini generateContent",
        Wire::Unknown => "this protocol",
    }
}

fn model_label(entry: &AiProviderEntry) -> String {
    match catalog::model(entry.wire(), &entry.model) {
        Ok(model) => format!("{} ({})", model.name, model.id),
        Err(_) => format!("\"{}\"", entry.model),
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn base64_len(bytes: u64) -> usize {
    bytes.div_ceil(3) as usize * 4
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Reads the container duration without keeping decoded samples in memory.
fn probe_duration(path: &Path) -> Option<f64> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;
    let track = format.default_track()?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    if let (Some(frames), Some(rate)) = (params.n_frames, params.sample_rate)
        && frames > 0
        && rate > 0
    {
        return Some(frames as f64 / f64::from(rate));
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .ok()?;
    let mut rate = params.sample_rate.unwrap_or(0);
    let mut frames = 0u64;
    loop {
        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }
                match decoder.decode(&packet) {
                    Ok(decoded) => {
                        if rate == 0 {
                            rate = decoded.spec().rate;
                        }
                        frames += decoded.frames() as u64;
                    }
                    Err(SymphoniaError::DecodeError(_)) => continue,
                    Err(SymphoniaError::IoError(error))
                        if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        break;
                    }
                    Err(_) => break,
                }
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    (rate > 0 && frames > 0).then(|| frames as f64 / f64::from(rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::auth::{self, Credential};
    use crate::ai::chat_completions::ChatCompletions;
    use crate::ai::gemini::GeminiInteractions;
    use crate::ai::gemini_generate::GeminiGenerate;
    use crate::ai::responses::Responses;
    use crate::ai::test_support::canned;
    use crate::ai::{Provider, StreamEvent};
    use crate::commands::{AiCapabilities, AuthKind, ProviderKind};
    use serde_json::{Value, json};
    use std::net::TcpListener;

    fn entry(wire: Wire, model: &str) -> AiProviderEntry {
        wire_entry(wire, model, "http://127.0.0.1:9/")
    }

    fn wire_entry(wire: Wire, model: &str, endpoint: &str) -> AiProviderEntry {
        let kind = wire.kind().unwrap_or(ProviderKind::OpenAiChatCompletions);
        AiProviderEntry {
            id: "fixture".into(),
            name: "Fixture".into(),
            kind,
            wire: Some(wire),
            auth: AuthKind::ApiKey,
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: endpoint.into(),
            model: model.into(),
            models: Vec::new(),
            context_length: 128_000,
            max_tokens: 1024,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: AiCapabilities {
                text: true,
                audio_in: true,
                audio_out: false,
                native_tool_calling: true,
                streaming: true,
            },
        }
    }

    fn plan(entry: &AiProviderEntry) -> auth::AuthPlan {
        auth::plan(entry, &Credential::ApiKey(Some("fixture-secret".into()))).unwrap()
    }

    /// Sends the request to a one-shot listener and returns the posted JSON body.
    async fn posted(
        provider: &dyn Provider,
        request: &ProviderRequest,
        server: std::thread::JoinHandle<String>,
    ) -> serde_json::Value {
        let mut sink = |_: StreamEvent| {};
        let _ = provider.stream(request, &mut sink).await;
        let raw = server.join().unwrap();
        let body = raw
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or_default();
        serde_json::from_str(body).unwrap()
    }

    /// Builds the request for one wire and returns the body its adapter posted.
    async fn posted_for(
        wire: Wire,
        model: &str,
        input: serde_json::Value,
    ) -> (serde_json::Value, Vec<MediaNotice>) {
        let (base, server) = canned(200, "");
        posted_entry(wire_entry(wire, model, &base), wire, input, server).await
    }

    /// The same for a model whose limits declare image input.
    async fn posted_image(
        wire: Wire,
        input: serde_json::Value,
    ) -> (serde_json::Value, Vec<MediaNotice>) {
        let (base, server) = canned(200, "");
        posted_entry(image_entry(wire, &base), wire, input, server).await
    }

    async fn posted_entry(
        entry: AiProviderEntry,
        wire: Wire,
        input: serde_json::Value,
        server: std::thread::JoinHandle<String>,
    ) -> (serde_json::Value, Vec<MediaNotice>) {
        let (request, notices) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider: Box<dyn Provider> = match wire {
            Wire::OpenAiChatCompletions => {
                Box::new(ChatCompletions::for_test(entry.clone(), auth_plan))
            }
            Wire::OpenAiResponses => Box::new(Responses::for_test(entry.clone(), auth_plan)),
            Wire::GeminiInteractions => {
                Box::new(GeminiInteractions::for_test(entry.clone(), auth_plan))
            }
            Wire::GeminiGenerate => Box::new(GeminiGenerate::for_test(entry.clone(), auth_plan)),
            Wire::AnthropicMessages => Box::new(crate::ai::anthropic::AnthropicMessages::for_test(
                entry.clone(),
                auth_plan,
            )),
            Wire::Unknown => unreachable!("fixture wires are all known"),
        };
        (posted(provider.as_ref(), &request, server).await, notices)
    }

    /// The user content parts of the one message these fixtures post.
    fn content_parts(body: &serde_json::Value, wire: Wire) -> Vec<serde_json::Value> {
        let content = match wire {
            Wire::OpenAiChatCompletions | Wire::AnthropicMessages => {
                &body["messages"][0]["content"]
            }
            Wire::OpenAiResponses | Wire::GeminiInteractions => &body["input"][0]["content"],
            Wire::GeminiGenerate => &body["contents"][0]["parts"],
            Wire::Unknown => unreachable!("fixture wires are all known"),
        };
        content
            .as_array()
            .cloned()
            .expect("a turn with media is a content-part array")
    }

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oc-media-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wav(path: &Path, seconds: f64) {
        let rate = 16_000u32;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let frames = (rate as f64 * seconds) as u32;
        for frame in 0..frames {
            let t = frame as f64 / rate as f64;
            let amplitude = (t * 440.0 * std::f64::consts::TAU).sin() * 0.2;
            writer
                .write_sample((amplitude * f64::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn messages(path: &Path) -> serde_json::Value {
        let attached = attach(&path.to_string_lossy()).unwrap();
        json!([{"role":"user","content":"what do you hear?","media":[attached]}])
    }

    fn image_messages(path: &Path) -> serde_json::Value {
        let attached = attach(&path.to_string_lossy()).unwrap();
        json!([{"role":"user","content":"what is this?","media":[attached]}])
    }

    #[test]
    fn attach_reads_duration_size_and_hash() {
        let dir = fixture("attach");
        let path = dir.join("take.wav");
        wav(&path, 1.5);
        let AttachedMedia { kind, media } = attach(&path.to_string_lossy()).unwrap();
        assert_eq!(kind, MediaKind::Audio);
        assert_eq!(media.name, "take.wav");
        assert_eq!(media.mime, "audio/wav");
        assert!(media.bytes > 40_000);
        assert!((media.duration_secs.unwrap() - 1.5).abs() < 0.05);
        assert_eq!(media.sha256.len(), 64);
        assert_eq!(media.sha256, digest(&std::fs::read(&path).unwrap()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attach_accepts_the_three_image_formats_and_keeps_a_null_duration() {
        let dir = fixture("image-attach");
        for (name, mime) in [
            ("shot.png", "image/png"),
            ("shot.jpg", "image/jpeg"),
            ("shot.webp", "image/webp"),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, b"not a decoded image, only a reference").unwrap();
            let AttachedMedia { kind, media } = attach(&path.to_string_lossy()).unwrap();
            assert_eq!(kind, MediaKind::Image, "{name}");
            assert_eq!(media.mime, mime, "{name}");
            assert!(media.duration_secs.is_none(), "{name}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attach_refuses_unsupported_and_oversized_files() {
        let dir = fixture("refuse");
        let text = dir.join("take.txt");
        std::fs::write(&text, b"not audio").unwrap();
        let error = attach(&text.to_string_lossy()).unwrap_err();
        assert!(
            error.contains("take.txt") && error.contains("accepted audio or image format"),
            "{error}"
        );

        let big = dir.join("big.png");
        std::fs::write(&big, vec![0u8; MAX_FILE_BYTES as usize + 1]).unwrap();
        let error = attach(&big.to_string_lossy()).unwrap_err();
        assert!(error.contains("inline media limit"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_long_local_audio_reference_reaches_a_text_model_as_metadata_only() {
        let dir = fixture("long-local-reference");
        let path = dir.join("song.wav");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_BYTES + 64).unwrap();
        let attached = attach(&path.to_string_lossy()).unwrap();
        assert!(attached.media.local_reference);
        let wire = to_wire(attached.kind, &attached.media);
        assert_eq!(from_wire(&wire).unwrap().1, attached.media);
        let input = json!([{"role":"user","content":"Analyze the arrangement","media":[wire]}]);
        let deepseek = entry(Wire::OpenAiChatCompletions, "deepseek-flash");
        let (request, notices) =
            crate::ai::request_from_wire(&deepseek, input.clone(), false, None).unwrap();
        assert!(notices.is_empty());
        assert!(
            request
                .turns
                .iter()
                .flat_map(|turn| &turn.blocks)
                .all(|block| !matches!(block, Block::Media { .. }))
        );
        let text = crate::ai::block_text(&request.turns[0].blocks);
        assert!(text.contains("No audio was sent"));
        assert!(text.contains("song.wav"));
        std::fs::remove_file(path).unwrap();
        let (_, notices) = crate::ai::request_from_wire(&deepseek, input, false, None).unwrap();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].missing);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn catalog_capability_gates_audio_before_any_request() {
        let dir = fixture("gate");
        let path = dir.join("take.wav");
        wav(&path, 0.5);
        let input = messages(&path);

        let deepseek = entry(Wire::OpenAiChatCompletions, "deepseek-flash");
        let error =
            super::validate(&deepseek, &crate::ai::turns_from_wire(&input).unwrap()).unwrap_err();
        assert!(error.message.contains("deepseek-flash"), "{error}");
        assert!(error.message.contains("audio_in = false"), "{error}");

        let anthropic = entry(Wire::AnthropicMessages, "claude-opus-4-6");
        let error =
            super::validate(&anthropic, &crate::ai::turns_from_wire(&input).unwrap()).unwrap_err();
        assert!(error.message.contains("Claude Opus 4.6"), "{error}");
        assert!(error.message.contains("audio_in = false"), "{error}");

        // A model the catalog does not know can reach the wire-specific refusal.
        let custom = entry(Wire::AnthropicMessages, "fixture-model");
        let error =
            super::validate(&custom, &crate::ai::turns_from_wire(&input).unwrap()).unwrap_err();
        assert!(error.message.contains("no audio content block"), "{error}");

        let gemini = entry(Wire::GeminiGenerate, "gemini-4-argon");
        assert!(super::validate(&gemini, &crate::ai::turns_from_wire(&input).unwrap()).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wire_format_rules_are_specific() {
        let dir = fixture("format");
        let path = dir.join("take.flac");
        std::fs::write(&path, b"fLaC").unwrap();
        let input = messages(&path);
        let gemini = entry(Wire::GeminiGenerate, "gemini-4-argon");
        assert!(super::validate(&gemini, &crate::ai::turns_from_wire(&input).unwrap()).is_ok());
        let chat = entry(Wire::OpenAiChatCompletions, "fixture-model");
        let error =
            super::validate(&chat, &crate::ai::turns_from_wire(&input).unwrap()).unwrap_err();
        assert!(error.message.contains("wav or mp3"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A model whose limits declare image input: the bundled catalog asks the
    /// user to opt in, so the test installs that layer in memory.
    fn image_entry(wire: Wire, endpoint: &str) -> AiProviderEntry {
        use crate::ai::limits::{InputModalities, ModelLimits};
        crate::ai::limits::install_for_test(
            "image-fixture",
            "image-model",
            ModelLimits {
                context: 128_000,
                max_output: 1024,
                input: InputModalities {
                    text: true,
                    image: true,
                    video: false,
                    audio: true,
                },
                price: None,
            },
        );
        let mut entry = wire_entry(wire, "image-model", endpoint);
        entry.id = "image-fixture".into();
        entry
    }

    #[tokio::test]
    async fn images_lower_to_each_wires_shape() {
        let dir = fixture("image");
        let path = dir.join("shot.png");
        let png = b"\x89PNG\r\n\x1a\nnot-a-real-png".to_vec();
        std::fs::write(&path, &png).unwrap();
        let input = image_messages(&path);
        let data = STANDARD.encode(&png);

        let (base, server) = canned(200, "");
        let entry = image_entry(Wire::OpenAiChatCompletions, &base);
        let (request, _) =
            crate::ai::request_from_wire(&entry, input.clone(), false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = ChatCompletions::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["messages"][0]["content"][2]["type"], "image_url");
        assert_eq!(
            body["messages"][0]["content"][2]["image_url"]["url"],
            format!("data:image/png;base64,{data}")
        );

        let (base, server) = canned(200, "");
        let entry = image_entry(Wire::OpenAiResponses, &base);
        let (request, _) =
            crate::ai::request_from_wire(&entry, input.clone(), false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = Responses::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["input"][0]["content"][2]["type"], "input_image");
        assert_eq!(
            body["input"][0]["content"][2]["image_url"],
            format!("data:image/png;base64,{data}")
        );

        let (base, server) = canned(200, "");
        let entry = image_entry(Wire::AnthropicMessages, &base);
        let (request, _) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = crate::ai::anthropic::AnthropicMessages::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["messages"][0]["content"][2]["type"], "image");
        assert_eq!(
            body["messages"][0]["content"][2]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(body["messages"][0]["content"][2]["source"]["data"], data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_request_builder_refuses_an_image_on_a_text_only_model() {
        let dir = fixture("image-gate");
        let path = dir.join("shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nnot-a-real-png").unwrap();
        let input = image_messages(&path);

        let text_only = entry(Wire::OpenAiChatCompletions, "deepseek-flash");
        let error =
            crate::ai::request_from_wire(&text_only, input.clone(), false, None).unwrap_err();
        assert!(
            error.message.contains("cannot accept image input"),
            "{error}"
        );
        assert!(error.message.contains("DeepSeek Flash"), "{error}");
        assert!(error.message.contains("declare text"), "{error}");

        let capable = image_entry(Wire::OpenAiChatCompletions, "http://127.0.0.1:9/");
        assert!(
            crate::ai::request_from_wire(&capable, input, false, None).is_ok(),
            "an image-capable model passes the gate"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolution_skips_media_free_turns() {
        let mut request = crate::ai::request_from_wire(
            &entry(Wire::GeminiGenerate, "gemini-4-argon"),
            json!([{"role":"user","content":"hello"}]),
            false,
            None,
        )
        .unwrap();
        let (request, notices) = &mut request;
        assert!(notices.is_empty());
        assert!(super::resolve(request).unwrap().is_empty());
    }

    #[test]
    fn saved_conversation_keeps_the_path_and_hash_but_never_base64() {
        let dir = fixture("saved");
        let path = dir.join("take.wav");
        wav(&path, 0.5);
        let input = messages(&path);
        let turns = crate::ai::turns_from_wire(&input).unwrap();
        let saved = conversation_wire(&turns).to_string();
        let media = attach(&path.to_string_lossy()).unwrap().media;
        assert!(saved.contains(&media.path), "{saved}");
        assert!(saved.contains(&media.sha256), "{saved}");
        let raw = std::fs::read(&path).unwrap();
        let base64 = STANDARD.encode(&raw);
        assert!(
            !saved.contains(&base64[..64]),
            "saved conversation carried base64"
        );
        assert!(!saved.contains("inline"), "{saved}");

        let mut request = crate::ai::request_from_wire(
            &entry(Wire::GeminiGenerate, "gemini-4-argon"),
            input,
            false,
            None,
        )
        .unwrap()
        .0;
        assert!(super::resolve(&mut request).is_ok());
        let resolved = conversation_wire(&request.turns).to_string();
        assert!(!resolved.contains(&base64[..64]));
        assert!(resolved.contains(&media.sha256));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_file_is_detected_and_the_fresh_bytes_are_sent() {
        let dir = fixture("changed");
        let path = dir.join("take.wav");
        wav(&path, 0.5);
        let input = messages(&path);
        let (_, notices) = crate::ai::request_from_wire(
            &entry(Wire::GeminiGenerate, "gemini-4-argon"),
            input.clone(),
            false,
            None,
        )
        .unwrap();
        assert!(notices.is_empty(), "an unchanged file is not a notice");

        // Rebuild from the same conversation (stale hash) after the file changed.
        wav(&path, 1.0);
        let (request, notices) = crate::ai::request_from_wire(
            &entry(Wire::GeminiGenerate, "gemini-4-argon"),
            input,
            false,
            None,
        )
        .unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].name, "take.wav");
        assert_eq!(notices[0].sha256, digest(&std::fs::read(&path).unwrap()));
        let data = request.turns[0]
            .blocks
            .iter()
            .find_map(|block| match block {
                Block::Media { data, .. } => Some(data),
                _ => None,
            })
            .expect("media block missing");
        let MediaData::Inline { base64, source } = data else {
            panic!("media was not resolved")
        };
        assert_eq!(
            STANDARD.decode(base64).unwrap(),
            std::fs::read(&path).unwrap()
        );
        assert!(source.duration_secs.unwrap() > 0.9);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_skipped_and_the_text_still_sends() {
        let dir = fixture("missing");
        let path = dir.join("gone.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        std::fs::remove_file(&path).unwrap();
        let (request, notes) = crate::ai::request_from_wire(
            &entry(Wire::GeminiGenerate, "gemini-4-argon"),
            input,
            false,
            None,
        )
        .expect("a deleted attachment does not fail the turn");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].missing, "the notice names the missing file");
        assert_eq!(notes[0].name, "gone.wav");
        // The user's text is still there; the media block became one notice line.
        assert!(request.turns[0].blocks.iter().any(|block| matches!(
            block,
            Block::Text(text) if text.contains("what do you hear?")
        )));
        assert!(request.turns[0].blocks.iter().any(|block| matches!(
            block,
            Block::Text(text) if text.contains("is missing") && text.contains("gone.wav")
        )));
        assert!(
            !request.turns[0]
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Media { .. })),
            "the missing file was not sent"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wire_entries_reject_malformed_descriptors() {
        let valid = json!({
            "kind":"audio","name":"a.wav","path":"/tmp/a.wav","mime":"audio/wav",
            "bytes":10,"duration_secs":1.0,"sha256":"0".repeat(64)
        });
        assert!(from_wire(&valid).is_ok());
        for broken in [
            json!({"kind":"audio","name":"a.wav","path":"/tmp/a.wav","mime":"audio/wav","bytes":10,"sha256":"zz"}),
            json!({"kind":"audio","name":"","path":"/tmp/a.wav","mime":"audio/wav","bytes":10,"sha256":"0".repeat(64)}),
            json!({"kind":"video","name":"a.mp4","path":"/tmp/a.mp4","mime":"video/mp4","bytes":10,"sha256":"0".repeat(64)}),
            json!({"kind":"audio","name":"a.wav","path":"/tmp/a.wav","mime":"audio/wav","bytes":10}),
            json!({"kind":"audio","name":"a.wav","path":"/tmp/a.wav","mime":"audio/wav","bytes":10,
                   "sha256":"0".repeat(64),"duration_secs":-1.0}),
        ] {
            assert!(from_wire(&broken).is_err(), "{broken}");
        }
        let (kind, _) = from_wire(&json!({
            "kind":"image","name":"a.png","path":"/tmp/a.png","mime":"image/png","bytes":10,"sha256":"0".repeat(64)
        }))
        .unwrap();
        assert_eq!(kind, MediaKind::Image);
    }

    #[tokio::test]
    async fn gemini_generate_posts_audio_as_inline_data() {
        let dir = fixture("wire-generate");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        let data = STANDARD.encode(std::fs::read(&path).unwrap());
        let (base, server) = canned(200, "");
        let entry = wire_entry(Wire::GeminiGenerate, "gemini-4-argon", &base);
        let (request, notices) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        assert!(notices.is_empty());
        let auth_plan = plan(&entry);
        let provider = GeminiGenerate::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "what do you hear?");
        let described = body["contents"][0]["parts"][1]["text"]
            .as_str()
            .expect("the attachment is described before the media part");
        assert!(
            described.contains(&path.to_string_lossy().into_owned()),
            "{described}"
        );
        assert_eq!(
            body["contents"][0]["parts"][2]["inlineData"],
            json!({"mimeType": "audio/wav", "data": data})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_inline_audio_part_is_media_that_never_touches_disk() {
        let data = STANDARD.encode(b"RIFF-inline-wav");
        let block = inline_audio(&json!({"data": data, "format": "wav"})).unwrap();
        let Block::Media {
            kind,
            mime,
            data: MediaData::Inline { source, base64 },
        } = block
        else {
            panic!("inline audio must lower to an inline media block")
        };
        assert_eq!(kind, MediaKind::Audio);
        assert_eq!(mime, "audio/wav");
        assert_eq!(base64, data);
        assert_eq!(source.bytes, 15);
        assert_eq!(source.name, "audition.wav");
        assert!(source.path.is_empty());

        for broken in [
            json!({"data": data, "format": "mp3"}),
            json!({"data": "not-base64!!", "format": "wav"}),
            json!({"format": "wav"}),
            json!({"data": "", "format": "wav"}),
        ] {
            assert!(inline_audio(&broken).is_err(), "{broken}");
        }

        // A model without audio input refuses it before the body is built.
        let mut entry = entry(Wire::OpenAiChatCompletions, "fixture-model");
        entry.capabilities.audio_in = false;
        let input = json!([{"role":"user","content":"hear this","input_audio":{"data":data,"format":"wav"}}]);
        let error = crate::ai::request_from_wire(&entry, input, false, None).unwrap_err();
        assert!(error.message.contains("audio_in = false"), "{error}");
    }

    #[tokio::test]
    async fn an_inline_audio_part_posts_as_input_audio() {
        let data = STANDARD.encode(b"RIFF-inline-wav");
        let input = json!([{
            "role": "user",
            "content": "You asked to hear 0–2 s: this is that render.",
            "input_audio": {"data": data, "format": "wav"}
        }]);
        let (base, server) = canned(200, "");
        let entry = wire_entry(Wire::OpenAiChatCompletions, "fixture-model", &base);
        let (request, _) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let body = posted(
            &ChatCompletions::for_test(entry.clone(), plan(&entry)),
            &request,
            server,
        )
        .await;
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "You asked to hear 0–2 s: this is that render."
        );
        assert_eq!(body["messages"][0]["content"][1]["type"], "input_audio");
        assert_eq!(
            body["messages"][0]["content"][1]["input_audio"],
            json!({"data": data, "format": "wav"})
        );
    }

    #[tokio::test]
    async fn chat_completions_posts_audio_as_input_audio() {
        let dir = fixture("wire-chat");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        let data = STANDARD.encode(std::fs::read(&path).unwrap());
        let (base, server) = canned(200, "");
        let entry = wire_entry(Wire::OpenAiChatCompletions, "fixture-model", &base);
        let (request, _) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = ChatCompletions::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][2]["type"], "input_audio");
        assert_eq!(
            body["messages"][0]["content"][2]["input_audio"]["format"],
            "wav"
        );
        assert_eq!(
            body["messages"][0]["content"][2]["input_audio"]["data"],
            data
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn responses_posts_audio_as_input_audio_content() {
        let dir = fixture("wire-responses");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        let data = STANDARD.encode(std::fs::read(&path).unwrap());
        let (base, server) = canned(200, "");
        let entry = wire_entry(Wire::OpenAiResponses, "fixture-model", &base);
        let (request, _) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = Responses::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["content"][2]["type"], "input_audio");
        assert_eq!(body["input"][0]["content"][2]["input_audio"]["data"], data);
        assert_eq!(
            body["input"][0]["content"][2]["input_audio"]["format"],
            "wav"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn gemini_interactions_posts_audio_in_a_user_input_step() {
        let dir = fixture("wire-interactions");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        let data = STANDARD.encode(std::fs::read(&path).unwrap());
        let (base, server) = canned(200, "");
        let entry = wire_entry(Wire::GeminiInteractions, "gemini-4-argon", &base);
        let (request, _) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        let auth_plan = plan(&entry);
        let provider = GeminiInteractions::for_test(entry, auth_plan);
        let body = posted(&provider, &request, server).await;
        assert_eq!(body["input"][0]["type"], "user_input");
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
        assert_eq!(
            body["input"][0]["content"][2],
            json!({"type": "audio", "data": data, "mime_type": "audio/wav"})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn anthropic_and_capability_refusals_make_no_network_call() {
        let dir = fixture("wire-refusal");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());

        let anthropic = wire_entry(Wire::AnthropicMessages, "claude-opus-4-6", &base);
        let error =
            crate::ai::request_from_wire(&anthropic, input.clone(), false, None).unwrap_err();
        assert!(error.message.contains("Claude Opus 4.6"), "{error}");
        assert!(error.message.contains("audio"), "{error}");

        let deepseek = wire_entry(Wire::OpenAiChatCompletions, "deepseek-flash", &base);
        let error = crate::ai::request_from_wire(&deepseek, input, false, None).unwrap_err();
        assert!(error.message.contains("deepseek-flash"), "{error}");
        assert!(error.message.contains("audio_in = false"), "{error}");

        assert!(
            listener.accept().is_err(),
            "a refused attachment must not open a connection"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn catalog_audio_flags_only_sit_on_wires_that_lower_audio() {
        for provider in &catalog::catalog().providers {
            for model in &provider.models {
                if !model.capabilities.audio_in {
                    continue;
                }
                assert!(
                    matches!(
                        provider.wire(),
                        Wire::GeminiGenerate
                            | Wire::GeminiInteractions
                            | Wire::OpenAiChatCompletions
                            | Wire::OpenAiResponses
                    ),
                    "{} declares audio_in on {:?}, which has no audio lowering",
                    model.id,
                    provider.wire()
                );
            }
        }
    }

    /// One seam, every wire: the same describing text rides right before the
    /// media part, which keeps its exact per-wire shape.
    #[tokio::test]
    async fn every_wire_describes_the_attachment_before_the_media_part() {
        let dir = fixture("describe");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let absolute = path.to_string_lossy().into_owned();
        let input = messages(&path);
        let data = STANDARD.encode(std::fs::read(&path).unwrap());

        let (body, _) =
            posted_for(Wire::OpenAiChatCompletions, "fixture-model", input.clone()).await;
        let parts = content_parts(&body, Wire::OpenAiChatCompletions);
        assert_eq!(parts.len(), 3, "{body}");
        assert_eq!(parts[0]["text"], "what do you hear?");
        let described = parts[1]["text"]
            .as_str()
            .expect("a text part names the file");
        assert_eq!(
            described,
            format!(
                "Attached audio \"take.wav\" is at {absolute} (0.20 s, 6.3 KiB); the analysis \
                 tools (audio_analyze, analyze_notes, separate_stems, audio_compare) accept this \
                 path directly."
            ),
            "the sentence the model reads"
        );
        assert_eq!(
            parts[2],
            json!({"type": "input_audio", "input_audio": {"data": data.clone(), "format": "wav"}})
        );

        let (body, _) = posted_for(Wire::OpenAiResponses, "fixture-model", input.clone()).await;
        let parts = content_parts(&body, Wire::OpenAiResponses);
        assert_eq!(parts.len(), 3, "{body}");
        assert!(
            parts[1]["text"].as_str().unwrap().contains(&absolute),
            "{body}"
        );
        assert_eq!(
            parts[2],
            json!({"type": "input_audio", "input_audio": {"data": data.clone(), "format": "wav"}})
        );

        let (body, _) = posted_for(Wire::GeminiInteractions, "gemini-4-argon", input.clone()).await;
        let parts = content_parts(&body, Wire::GeminiInteractions);
        assert_eq!(parts.len(), 3, "{body}");
        assert!(
            parts[1]["text"].as_str().unwrap().contains(&absolute),
            "{body}"
        );
        assert_eq!(
            parts[2],
            json!({"type": "audio", "data": data.clone(), "mime_type": "audio/wav"})
        );

        let (body, _) = posted_for(Wire::GeminiGenerate, "gemini-4-argon", input).await;
        let parts = content_parts(&body, Wire::GeminiGenerate);
        assert_eq!(parts.len(), 3, "{body}");
        assert!(
            parts[1]["text"].as_str().unwrap().contains(&absolute),
            "{body}"
        );
        assert_eq!(
            parts[2],
            json!({"inlineData": {"mimeType": "audio/wav", "data": data}})
        );

        // Anthropic lowers no audio, so its image path proves the same seam.
        let image = dir.join("shot.png");
        let png = b"\x89PNG\r\n\x1a\nnot-a-real-png".to_vec();
        std::fs::write(&image, &png).unwrap();
        let (body, _) = posted_image(Wire::AnthropicMessages, image_messages(&image)).await;
        let parts = content_parts(&body, Wire::AnthropicMessages);
        assert_eq!(parts.len(), 3, "{body}");
        let described = parts[1]["text"]
            .as_str()
            .expect("a text part names the image");
        assert!(described.contains("shot.png"), "{described}");
        assert!(
            described.contains(&image.to_string_lossy().into_owned()),
            "{described}"
        );
        assert!(!described.contains("audio_analyze"), "{described}");
        assert_eq!(parts[2]["type"], "image");
        assert_eq!(parts[2]["source"]["media_type"], "image/png");
        assert_eq!(parts[2]["source"]["data"], STANDARD.encode(&png));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_changed_attachment_says_earlier_measurements_may_be_stale() {
        let dir = fixture("describe-changed");
        let path = dir.join("take.wav");
        wav(&path, 0.2);
        let input = messages(&path);
        wav(&path, 0.4);
        let (body, notices) = posted_for(Wire::GeminiGenerate, "gemini-4-argon", input).await;
        assert_eq!(notices.len(), 1);
        let parts = content_parts(&body, Wire::GeminiGenerate);
        let described = parts[1]["text"]
            .as_str()
            .expect("a text part names the file");
        assert!(
            described.contains(&path.to_string_lossy().into_owned()),
            "{described}"
        );
        assert!(
            described.contains(
                "It changed on disk since it was attached, so earlier measurements may be stale."
            ),
            "{described}"
        );
        assert_eq!(parts[2]["inlineData"]["mimeType"], "audio/wav");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_inline_audio_part_without_a_path_gets_no_sentence() {
        // The inline audition clip has no file behind it, so there is nothing
        // to point a tool at: the turn keeps its original text and media parts.
        let data = STANDARD.encode(b"RIFF-inline-wav");
        let input = json!([{
            "role": "user",
            "content": "this is the render",
            "input_audio": {"data": data, "format": "wav"}
        }]);
        let (body, notices) = posted_for(Wire::OpenAiChatCompletions, "fixture-model", input).await;
        assert!(notices.is_empty());
        let parts = content_parts(&body, Wire::OpenAiChatCompletions);
        assert_eq!(parts.len(), 2, "{body}");
        assert_eq!(parts[0]["text"], "this is the render");
        assert_eq!(parts[1]["type"], "input_audio");
        assert!(!body.to_string().contains(" is at "), "{body}");
    }

    #[test]
    fn describe_refuses_a_blank_path() {
        let blank = MediaRef {
            local_reference: false,
            name: "audition.wav".into(),
            path: String::new(),
            mime: "audio/wav".into(),
            bytes: 15,
            duration_secs: None,
            sha256: String::new(),
        };
        assert!(describe(MediaKind::Audio, &blank, false).is_none());
        assert!(describe(MediaKind::Image, &blank, true).is_none());
    }

    #[tokio::test]
    async fn a_text_only_turn_still_posts_a_plain_string() {
        let (body, _) = posted_for(
            Wire::OpenAiChatCompletions,
            "fixture-model",
            json!([{"role": "user", "content": "hello"}]),
        )
        .await;
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    /// Recognition check using only generated audio and the explicit test key.
    #[tokio::test]
    #[ignore = "live provider; requires OPENCOMPOSE_AI_KEY and OPENCOMPOSE_TEST_AI_ENDPOINT"]
    async fn live_argon_identifies_ordered_audio_excerpts() {
        let key = std::env::var("OPENCOMPOSE_AI_KEY").expect("set the authorized test key");
        assert!(!key.trim().is_empty(), "the test key is empty");
        let endpoint = std::env::var("OPENCOMPOSE_TEST_AI_ENDPOINT")
            .expect("set OPENCOMPOSE_TEST_AI_ENDPOINT for the authorized test provider");
        let mut entry = wire_entry(Wire::GeminiGenerate, "gemini-4-argon", &endpoint);
        entry.id = "gateway-google".into();
        entry.thinking_level = "low".into();
        let auth = auth::plan(&entry, &Credential::ApiKey(Some(key.trim().into()))).unwrap();
        let adapter = GeminiGenerate::for_test(entry.clone(), auth);

        // The opening is a three-pulse distractor. Only the requested late
        // intervals contain the distinct two- and four-pulse fixtures.
        let write_fixture = |path: &Path, start: f32, pulses: &[f32], frequency: f32| {
            let rate = 16_000;
            let mut samples = vec![0.0; ((start + 4.0) * rate as f32) as usize];
            for (origin, starts, pitch) in [
                (0.0, &[0.4, 1.4, 2.4][..], 440.0),
                (start, pulses, frequency),
            ] {
                for pulse in starts {
                    let first = ((origin + pulse) * rate as f32).round() as usize;
                    for (frame, sample) in samples[first..first + 4_800].iter_mut().enumerate() {
                        let local = frame as f32 / rate as f32;
                        let gain = (local / 0.01).min((0.30 - local) / 0.01).min(1.0);
                        *sample = 0.4 * gain * (std::f32::consts::TAU * pitch * local).sin();
                    }
                }
            }
            let bytes = oc_engine::wav::mono_wav_bytes(&samples, rate).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("a.wav");
        let candidate = dir.path().join("b.wav");
        write_fixture(&reference, 78.25, &[0.4, 2.1], 330.0);
        write_fixture(&candidate, 81.75, &[0.4, 1.2, 2.0, 2.8], 660.0);
        let comparison = crate::agent_api::audio_compare_tool(&json!({
            "reference": reference,
            "candidate": candidate,
            "reference_start": 78.25,
            "candidate_start": 81.75,
            "duration": 4.0,
            "listen": true,
            "channel": "mono",
            "clip_sample_rate": 16_000
        }))
        .unwrap();
        assert_eq!(comparison["window"]["reference_start_s"], 78.25);
        assert_eq!(comparison["window"]["candidate_start_s"], 81.75);
        let listening = &comparison["listening"];
        assert_eq!(listening["status"], "matched");
        let delta = listening["delta_lu"].as_f64().unwrap();
        assert!(delta.abs() < 0.1);
        eprintln!("late comparison excerpts matched to {delta:+.4} LU");
        let low = listening["reference_audio"]["base64"].as_str().unwrap();
        let high = listening["candidate_audio"]["base64"].as_str().unwrap();
        let prompt = "Count the separated tone pulses in each audio clip. Return only JSON with integer keys first and second. If there is no audio, set both values to null.";
        let mut observations = Vec::new();
        for (label, pair) in [
            ("forward", Some((low, high))),
            ("reversed", Some((high, low))),
            ("text-only", None),
        ] {
            let mut messages = vec![json!({"role":"user","content":prompt})];
            if let Some((first, second)) = pair {
                messages.push(json!({"role":"user","content":"First audio clip.","input_audio":{"format":"wav","data":first}}));
                messages.push(json!({"role":"user","content":"Second audio clip.","input_audio":{"format":"wav","data":second}}));
            }
            let (request, notices) =
                crate::ai::request_from_wire(&entry, Value::Array(messages), false, None).unwrap();
            assert!(notices.is_empty());
            let mut sink = |_: StreamEvent| {};
            let turn = adapter
                .stream(&request, &mut sink)
                .await
                .unwrap_or_else(|error| {
                    panic!("{label} live request failed ({})", error.kind.as_str())
                });
            let reply = crate::ai::block_text(&turn.blocks);
            let answer = reply
                .find('{')
                .zip(reply.rfind('}'))
                .and_then(|(start, end)| reply.get(start..=end))
                .and_then(|body| serde_json::from_str::<Value>(body).ok())
                .unwrap_or(Value::Null);
            let counts = (
                answer.get("first").and_then(Value::as_u64),
                answer.get("second").and_then(Value::as_u64),
            );
            let no_audio = answer.get("first") == Some(&Value::Null)
                && answer.get("second") == Some(&Value::Null);
            eprintln!("{label}: pulse counts {counts:?}, explicit no audio {no_audio}");
            observations.push((counts, no_audio));
        }
        assert_eq!(
            observations,
            [
                ((Some(2), Some(4)), false),
                ((Some(4), Some(2)), false),
                ((None, None), true),
            ],
        );
    }

    /// Live listening check for an operator-supplied, authorized dry/wet pair.
    #[tokio::test]
    #[ignore = "live provider; requires OPENCOMPOSE_AI_KEY, OPENCOMPOSE_TEST_AI_ENDPOINT and explicit audio files"]
    async fn live_argon_compares_vocal_reverb_tails() {
        let key = std::env::var("OPENCOMPOSE_AI_KEY").expect("set the authorized test key");
        let dry = std::env::var("OCP_TEST_VOCAL_DRY").expect("set the dry excerpt path");
        let wet = std::env::var("OCP_TEST_VOCAL_WET").expect("set the wet excerpt path");
        let prepared_at = std::time::Instant::now();
        let comparison = crate::agent_api::audio_compare_tool(&json!({
            "reference": dry, "candidate": wet, "duration": 6.0, "listen": true, "listening_only": true
        }))
        .unwrap();
        let listening = &comparison["listening"];
        assert_eq!(listening["status"], "matched");
        let delta = listening["delta_lu"].as_f64().unwrap();
        assert!(delta.abs() < 0.1);
        eprintln!(
            "Prepared pair in {:?}, matched to {delta:+.4} LU",
            prepared_at.elapsed()
        );
        let first = listening["reference_audio"]["base64"].as_str().unwrap();
        let second = listening["candidate_audio"]["base64"].as_str().unwrap();

        let endpoint = std::env::var("OPENCOMPOSE_TEST_AI_ENDPOINT")
            .expect("set OPENCOMPOSE_TEST_AI_ENDPOINT for the authorized test provider");
        let mut entry = wire_entry(Wire::GeminiGenerate, "gemini-4-argon", &endpoint);
        entry.id = "gateway-google".into();
        entry.thinking_level = "low".into();
        let auth = auth::plan(&entry, &Credential::ApiKey(Some(key.trim().into()))).unwrap();
        let adapter = GeminiGenerate::for_test(entry.clone(), auth);
        for (order, a, b) in [("dry/wet", first, second), ("wet/dry", second, first)] {
            let messages = json!([
                {"role":"user","content":"Compare these two clips of the same vocal. Which has the more sustained ambience between words? If you cannot tell, say so. Give one audible observation."},
                {"role":"user","content":"Clip A","input_audio":{"format":"wav","data":a}},
                {"role":"user","content":"Clip B","input_audio":{"format":"wav","data":b}}
            ]);
            let (request, notices) =
                crate::ai::request_from_wire(&entry, messages, false, None).unwrap();
            assert!(notices.is_empty());
            let mut sink = |_: StreamEvent| {};
            let turn = adapter
                .stream(&request, &mut sink)
                .await
                .unwrap_or_else(|error| {
                    panic!("{order} live request failed ({})", error.kind.as_str())
                });
            let reply = crate::ai::block_text(&turn.blocks);
            assert!(!reply.trim().is_empty());
            eprintln!("{order}: {}", reply.chars().take(700).collect::<String>());
        }
    }

    /// Live plumbing check: needs `OPENCODE_API_KEY`. Writes the answer to a
    /// temporary file; the tone is not speech, so this is not a recognition test.
    #[tokio::test]
    #[ignore = "live network; needs OPENCODE_API_KEY and sends ~10 s of audio"]
    async fn live_gemini_argon_accepts_a_ten_second_wav() {
        let note = std::env::var("AI_AUDIO_SMOKE_NOTE").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("opencompose-aiaudio-smoke.md")
                .to_string_lossy()
                .into_owned()
        });
        let Some(key) = std::env::var("OPENCODE_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
        else {
            std::fs::write(
                &note,
                "# Audio live smoke\n\nSkipped: `OPENCODE_API_KEY` is not set.\n\nRun:\n\n```sh\n\
                 export OPENCODE_API_KEY=...\n\
                 cargo test -p oc-gui ai::media::tests::live_gemini_argon_accepts_a_ten_second_wav \
                 -- --ignored --nocapture\n```\n",
            )
            .unwrap();
            return;
        };
        let dir = fixture("live");
        let path = dir.join("ten-seconds.wav");
        wav(&path, 10.0);
        let media = attach(&path.to_string_lossy()).unwrap().media;
        let provider = catalog::provider("opencode-go").unwrap();
        let mut entry = wire_entry(
            provider.wire(),
            "gemini-4-argon",
            provider.base_url.as_deref().unwrap_or_default(),
        );
        entry.id = provider.id.clone();
        entry.name = provider.name.clone();
        entry.max_tokens = 512;
        let input = json!([{
            "role": "user",
            "content": "Describe this audio in one sentence. What do you hear?",
            "media": [{"kind":"audio","name":media.name,"path":media.path,"mime":media.mime,
                       "bytes":media.bytes,"duration_secs":media.duration_secs,"sha256":media.sha256}]
        }]);
        let (request, notices) = crate::ai::request_from_wire(&entry, input, false, None).unwrap();
        assert!(notices.is_empty());
        let plan = auth::plan(&entry, &Credential::ApiKey(Some(key.trim().into()))).unwrap();
        let adapter = GeminiGenerate::for_test(entry.clone(), plan);
        let mut sink = |_: StreamEvent| {};
        let result = adapter.stream(&request, &mut sink).await;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_secs())
            .unwrap_or_default();
        let mut text = format!(
            "# Audio live smoke — gemini-4-argon\n\n\
             - endpoint: `{}`\n- wire: `gemini-generate`\n- file: `{}` ({} bytes, {:.2} s, sha256 `{}`)\n\
             - sent: {} s of synthesized 440 Hz tone (not speech recognition)\n- unix time: {stamp}\n\n",
            entry.endpoint,
            media.path,
            media.bytes,
            media.duration_secs.unwrap_or_default(),
            media.sha256,
            media.duration_secs.unwrap_or_default()
        );
        match &result {
            Ok(turn) => {
                text.push_str("## Answer\n\n");
                text.push_str(&crate::ai::block_text(&turn.blocks));
                text.push('\n');
            }
            Err(error) => {
                text.push_str("## Failed\n\n");
                text.push_str(&error.message);
                text.push('\n');
            }
        }
        std::fs::write(&note, text).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        if let Err(error) = &result {
            panic!("live audio request failed: {}", error.message);
        }
    }
}
