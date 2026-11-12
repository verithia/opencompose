//! The preferences bundle: one portable JSON file carrying the settings this
//! build keeps outside a project.
//!
//! The bundle is data, never code. Import validates every leaf, refuses names
//! that look like a credential, and answers the values it accepted; the
//! frontend applies each of them through the store that owns it. Export writes
//! the same shape atomically, so a failed write leaves the previous file alone.

use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bundle schema this build writes and reads.
pub const SCHEMA: &str = "opencompose.preferences.v1";

/// Largest bundle this build reads or writes, in bytes.
pub const MAX_BUNDLE_BYTES: u64 = 1024 * 1024;

/// Locales this build ships a dictionary for.
pub const LOCALES: [&str; 5] = ["en", "zh-Hans", "zh-Hant", "ja", "es"];

/// Interface scales [`ui_scale`] may carry.
pub const UI_SCALE_RANGE: (f64, f64) = (0.7, 1.5);

/// Output rates [`AudioBundle::sample_rate`] may carry.
pub const SAMPLE_RATE_RANGE: (u32, u32) = (8_000, 192_000);

/// Device periods [`AudioBundle::buffer_size`] may carry.
pub const BUFFER_SIZE_RANGE: (u32, u32) = (16, 8_192);

/// Containers [`ExportBundle::format`] may name.
pub const EXPORT_FORMATS: [&str; 3] = ["wav", "flac", "mp3"];

/// Integer depths [`ExportBundle::bit_depth`] may name, in bits.
pub const EXPORT_BIT_DEPTHS: [u8; 3] = [16, 24, 32];

/// FLAC compression levels [`ExportBundle::flac_level`] may carry.
pub const FLAC_LEVEL_RANGE: (u8, u8) = (0, 8);

/// MP3 CBR bitrates [`ExportBundle::mp3_bitrate`] may carry, in kbps.
pub const MP3_BITRATE_RANGE: (u16, u16) = (32, 320);

/// MP3 VBR quality indices [`ExportBundle::mp3_vbr`] may carry.
pub const MP3_VBR_RANGE: (u8, u8) = (0, 9);

/// Bitrate an MP3 export uses when the bundle names none, in kbps.
pub const DEFAULT_MP3_BITRATE: u16 = 192;

/// Word tokens that mark a field name as a credential.
const SECRET_TOKENS: [&str; 12] = [
    "key",
    "keys",
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "passwords",
    "credential",
    "credentials",
    "apikey",
    "authorization",
];

/// One portable bundle. Every leaf is optional, so a hand-written file may
/// carry one setting or all of them.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct PreferencesBundle {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub exported_at: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub theme: Option<ThemeBundle>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub ui_scale: Option<f64>,
    #[serde(default)]
    pub audio: Option<AudioBundle>,
    #[serde(default)]
    pub midi: Option<MidiBundle>,
    #[serde(default)]
    pub export: Option<ExportBundle>,
    #[serde(default)]
    pub grid: Option<GridBundle>,
    /// Carried opaque: the keymap store owns its shape and reads it at startup.
    #[serde(default)]
    pub keymap: Option<serde_json::Value>,
    #[serde(default)]
    pub workspace: Option<WorkspaceBundle>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted theme preferences.
pub struct ThemeBundle {
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted audio device preferences.
pub struct AudioBundle {
    /// Backend-stable device key; `None` is the system default.
    #[serde(default)]
    pub output_device: Option<String>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub buffer_size: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted MIDI input preferences.
pub struct MidiBundle {
    /// Input port name; `None` selects no port.
    #[serde(default)]
    pub input: Option<String>,
    /// 1-based channel; `None` keeps the controller's own channel.
    #[serde(default)]
    pub channel: Option<u8>,
    #[serde(default)]
    pub monitor: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted export defaults.
pub struct ExportBundle {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub bit_depth: Option<u8>,
    #[serde(default)]
    pub flac_level: Option<u8>,
    #[serde(default)]
    pub normalise: Option<bool>,
    /// MP3 CBR bitrate in kbps; the other containers ignore it.
    #[serde(default)]
    pub mp3_bitrate: Option<u16>,
    /// MP3 VBR quality index; `None` is a CBR encode.
    #[serde(default)]
    pub mp3_vbr: Option<u8>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted grid subdivision preferences.
pub struct GridBundle {
    /// Divisions per quarter note; the frontend checks it against its own list.
    #[serde(default)]
    pub subdivision: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
/// Persisted workspace preferences.
pub struct WorkspaceBundle {
    #[serde(default)]
    pub recent_directories: Option<Vec<String>>,
}

/// One leaf a validating reader refused, with the reason it gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedField {
    pub field: String,
    pub reason: String,
}

/// What import answers: the values to apply, the leaves it dropped, and the
/// errors that refuse the whole file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreferencesImport {
    pub bundle: PreferencesBundle,
    /// Leaf names this reader accepted; the frontend applies exactly these.
    pub applied: Vec<String>,
    pub skipped: Vec<SkippedField>,
    /// A non-empty list refuses the file; nothing is applied.
    pub errors: Vec<String>,
}

impl PreferencesImport {
    fn refused(error: String) -> Self {
        Self {
            bundle: PreferencesBundle::default(),
            applied: Vec::new(),
            skipped: Vec::new(),
            errors: vec![error],
        }
    }
}

/// Read a bundle and answer the values the frontend should apply.
#[tauri::command]
pub fn preferences_import(path: String) -> Result<PreferencesImport, String> {
    let text = read_bundle(&path)?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        format!("Not an OpenCompose preferences file: {path} is not JSON ({error}).")
    })?;
    if let Some(field) = secret_field(&value, "") {
        return Ok(PreferencesImport::refused(format!(
            "Refusing to import '{field}': the name looks like a credential, and this bundle never carries one."
        )));
    }
    let bundle = typed(&text, &path)?;
    Ok(validate(bundle))
}

/// Write a bundle to `path` atomically: a temporary file in the same directory
/// is renamed over the target, so a failure leaves the previous file intact.
#[tauri::command]
pub fn preferences_export(path: String, json: String) -> Result<(), String> {
    if json.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(oversize());
    }
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|error| {
        format!("Not an OpenCompose preferences file: {path} is not JSON ({error}).")
    })?;
    if let Some(field) = secret_field(&value, "") {
        return Err(format!(
            "Refusing to write '{field}': the name looks like a credential, and this bundle never carries one."
        ));
    }
    let bundle = typed(&json, &path)?;
    let import = validate(bundle);
    if let Some(first) = import.skipped.first() {
        return Err(format!(
            "Refusing to write {}: {} is invalid ({}).",
            path, first.field, first.reason
        ));
    }
    // The file always names the build that wrote it.
    let mut bundle = import.bundle;
    bundle.app_version = Some(env!("CARGO_PKG_VERSION").to_string());
    let mut text = serde_json::to_string_pretty(&bundle)
        .map_err(|error| format!("Could not encode the preferences bundle: {error}."))?;
    text.push('\n');
    write_atomic(Path::new(&path), text.as_bytes())
        .map_err(|error| format!("Could not write {path}: {error}."))
}

/// Read the file under the size cap, answering the text it holds.
fn read_bundle(path: &str) -> Result<String, String> {
    let target = Path::new(path);
    let metadata =
        std::fs::metadata(target).map_err(|error| format!("Could not read {path}: {error}."))?;
    if metadata.len() > MAX_BUNDLE_BYTES {
        return Err(oversize());
    }
    let bytes =
        std::fs::read(target).map_err(|error| format!("Could not read {path}: {error}."))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(oversize());
    }
    String::from_utf8(bytes)
        .map_err(|_| format!("Not an OpenCompose preferences file: {path} is not UTF-8 text."))
}

fn oversize() -> String {
    "The file is larger than 1 MiB.".to_string()
}

/// Parse the typed bundle and check the schema stamp.
fn typed(text: &str, path: &str) -> Result<PreferencesBundle, String> {
    let bundle: PreferencesBundle = serde_json::from_str(text).map_err(|error| {
        format!(
            "Not an OpenCompose preferences file: {path} has a field of the wrong type ({error})."
        )
    })?;
    if bundle.schema != SCHEMA {
        return Err(format!(
            "Not an OpenCompose preferences file: schema '{}' is not {SCHEMA}.",
            bundle.schema
        ));
    }
    Ok(bundle)
}

/// Drop leaves this build cannot use and name every one of them.
fn validate(mut bundle: PreferencesBundle) -> PreferencesImport {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    if let Some(theme) = bundle.theme.clone() {
        let family = theme.family.unwrap_or_default();
        let variant = theme.variant.unwrap_or_default();
        if family.trim().is_empty() || variant.trim().is_empty() {
            skipped.push(SkippedField {
                field: "theme".to_string(),
                reason: "a theme needs both a family and a variant".to_string(),
            });
            bundle.theme = None;
        } else {
            applied.push("theme".to_string());
        }
    }

    if let Some(locale) = bundle.locale.clone() {
        if LOCALES.contains(&locale.as_str()) {
            applied.push("locale".to_string());
        } else {
            skipped.push(SkippedField {
                field: "locale".to_string(),
                reason: format!("locale '{locale}' is not one of {}", LOCALES.join(", ")),
            });
            bundle.locale = None;
        }
    }

    if let Some(scale) = bundle.ui_scale {
        if scale.is_finite() && scale >= UI_SCALE_RANGE.0 && scale <= UI_SCALE_RANGE.1 {
            applied.push("ui_scale".to_string());
        } else {
            skipped.push(SkippedField {
                field: "ui_scale".to_string(),
                reason: format!(
                    "ui_scale {scale} is outside {}–{}",
                    UI_SCALE_RANGE.0, UI_SCALE_RANGE.1
                ),
            });
            bundle.ui_scale = None;
        }
    }

    if let Some(audio) = bundle.audio.clone() {
        let mut kept = audio.clone();
        if let Some(key) = audio.output_device.clone() {
            if key.trim().is_empty() {
                skipped.push(SkippedField {
                    field: "audio.output_device".to_string(),
                    reason: "the device key is empty".to_string(),
                });
                kept.output_device = None;
            } else {
                applied.push("audio.output_device".to_string());
            }
        }
        if let Some(rate) = audio.sample_rate {
            if (SAMPLE_RATE_RANGE.0..=SAMPLE_RATE_RANGE.1).contains(&rate) {
                applied.push("audio.sample_rate".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "audio.sample_rate".to_string(),
                    reason: format!(
                        "sample rate {rate} is outside {}–{} Hz",
                        SAMPLE_RATE_RANGE.0, SAMPLE_RATE_RANGE.1
                    ),
                });
                kept.sample_rate = None;
            }
        }
        if let Some(frames) = audio.buffer_size {
            if (BUFFER_SIZE_RANGE.0..=BUFFER_SIZE_RANGE.1).contains(&frames) {
                applied.push("audio.buffer_size".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "audio.buffer_size".to_string(),
                    reason: format!(
                        "buffer size {frames} is outside {}–{} frames",
                        BUFFER_SIZE_RANGE.0, BUFFER_SIZE_RANGE.1
                    ),
                });
                kept.buffer_size = None;
            }
        }
        bundle.audio = if kept == AudioBundle::default() {
            None
        } else {
            Some(kept)
        };
    }

    if let Some(midi) = bundle.midi.clone() {
        let mut kept = midi.clone();
        if let Some(input) = midi.input.clone() {
            if input.trim().is_empty() {
                // An explicit `null` selects no port; an empty name is a hole.
                skipped.push(SkippedField {
                    field: "midi.input".to_string(),
                    reason: "the port name is empty".to_string(),
                });
                kept.input = None;
            } else {
                applied.push("midi.input".to_string());
            }
        }
        if let Some(channel) = midi.channel {
            if (1..=16).contains(&channel) {
                applied.push("midi.channel".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "midi.channel".to_string(),
                    reason: format!("channel {channel} is outside 1–16"),
                });
                kept.channel = None;
            }
        }
        if midi.monitor.is_some() {
            applied.push("midi.monitor".to_string());
        }
        bundle.midi = if kept == MidiBundle::default() {
            None
        } else {
            Some(kept)
        };
    }

    if let Some(export) = bundle.export.clone() {
        let mut kept = export.clone();
        let mut mp3 = false;
        if let Some(format) = export.format.clone() {
            if EXPORT_FORMATS.contains(&format.as_str()) {
                mp3 = format == "mp3";
                applied.push("export.format".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "export.format".to_string(),
                    reason: format!(
                        "format '{format}' is not one of {}",
                        EXPORT_FORMATS.join(", ")
                    ),
                });
                kept.format = None;
            }
        }
        if let Some(depth) = export.bit_depth {
            if mp3 {
                // MP3 consumes 16-bit PCM, so a carried depth says nothing.
                kept.bit_depth = None;
            } else if EXPORT_BIT_DEPTHS.contains(&depth) {
                applied.push("export.bit_depth".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "export.bit_depth".to_string(),
                    reason: format!("{depth}-bit is not one of 16, 24, 32"),
                });
                kept.bit_depth = None;
            }
        }
        if let Some(level) = export.flac_level {
            if (FLAC_LEVEL_RANGE.0..=FLAC_LEVEL_RANGE.1).contains(&level) {
                applied.push("export.flac_level".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "export.flac_level".to_string(),
                    reason: format!(
                        "FLAC level {level} is outside {}–{}",
                        FLAC_LEVEL_RANGE.0, FLAC_LEVEL_RANGE.1
                    ),
                });
                kept.flac_level = None;
            }
        }
        if export.normalise.is_some() {
            applied.push("export.normalise".to_string());
        }
        match export.mp3_bitrate {
            Some(kbps) if (MP3_BITRATE_RANGE.0..=MP3_BITRATE_RANGE.1).contains(&kbps) => {
                applied.push("export.mp3_bitrate".to_string());
            }
            Some(kbps) => {
                skipped.push(SkippedField {
                    field: "export.mp3_bitrate".to_string(),
                    reason: format!(
                        "MP3 bitrate {kbps} kbps is outside {}–{}",
                        MP3_BITRATE_RANGE.0, MP3_BITRATE_RANGE.1
                    ),
                });
                kept.mp3_bitrate = None;
            }
            // An MP3 export always has a bitrate; a bundle that names none
            // keeps the default rather than leaving the dialog without one.
            None if mp3 => {
                kept.mp3_bitrate = Some(DEFAULT_MP3_BITRATE);
                applied.push("export.mp3_bitrate".to_string());
            }
            None => {}
        }
        if let Some(quality) = export.mp3_vbr {
            if (MP3_VBR_RANGE.0..=MP3_VBR_RANGE.1).contains(&quality) {
                applied.push("export.mp3_vbr".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "export.mp3_vbr".to_string(),
                    reason: format!(
                        "MP3 VBR quality {quality} is outside {}–{}",
                        MP3_VBR_RANGE.0, MP3_VBR_RANGE.1
                    ),
                });
                kept.mp3_vbr = None;
            }
        }
        bundle.export = if kept == ExportBundle::default() {
            None
        } else {
            Some(kept)
        };
    }

    if let Some(grid) = bundle.grid.clone() {
        if let Some(subdivision) = grid.subdivision {
            if subdivision.is_finite() && subdivision > 0.0 {
                applied.push("grid.subdivision".to_string());
            } else {
                skipped.push(SkippedField {
                    field: "grid.subdivision".to_string(),
                    reason: format!("subdivision {subdivision} is not a positive number"),
                });
                bundle.grid = None;
            }
        } else {
            bundle.grid = None;
        }
    }

    if let Some(keymap) = bundle.keymap.clone() {
        if keymap.is_object() {
            applied.push("keymap".to_string());
        } else {
            skipped.push(SkippedField {
                field: "keymap".to_string(),
                reason: "the keymap must be an object".to_string(),
            });
            bundle.keymap = None;
        }
    }

    if let Some(workspace) = bundle.workspace.clone()
        && workspace.recent_directories.is_some()
    {
        applied.push("workspace.recent_directories".to_string());
    }

    PreferencesImport {
        bundle,
        applied,
        skipped,
        errors: Vec::new(),
    }
}

/// The first object key whose name reads as a credential, as a dotted path.
///
/// The keymap is opaque: its entries are key bindings, not settings, so a
/// binding may legitimately be named after a key.
fn secret_field(value: &serde_json::Value, path: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                let next = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                if looks_like_secret(name) {
                    return Some(next);
                }
                if name == "keymap" {
                    continue;
                }
                if let Some(found) = secret_field(child, &next) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items.iter().find_map(|item| secret_field(item, path)),
        _ => None,
    }
}

/// Whether a field name carries a credential token, split on case and punctuation.
fn looks_like_secret(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut token = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
            continue;
        }
        if ch.is_uppercase() && !token.is_empty() {
            let previous_lower = chars
                .get(index.wrapping_sub(1))
                .map(|previous| previous.is_lowercase())
                .unwrap_or(false);
            let next_lower = chars
                .get(index + 1)
                .map(|next| next.is_lowercase())
                .unwrap_or(false);
            if previous_lower || next_lower {
                tokens.push(std::mem::take(&mut token));
            }
        }
        token.extend(ch.to_lowercase());
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
        .iter()
        .any(|token| SECRET_TOKENS.contains(&token.as_str()))
}

/// Write `bytes` to `path` through a temporary file in the same directory.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("opencompose-preferences.json");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = parent.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let write = (|| -> io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            // Windows cannot rename over an existing file.
            #[cfg(windows)]
            Err(_) => {
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                std::fs::rename(&temporary, path)
            }
            #[cfg(not(windows))]
            Err(error) => Err(error),
        }
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(label: &str) -> Fixture {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "oc-preferences-{label}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Fixture { dir }
    }

    fn write(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().to_string()
    }

    fn import(path: &str) -> PreferencesImport {
        preferences_import(path.to_string()).unwrap()
    }

    fn field_names(import: &PreferencesImport) -> Vec<&str> {
        import.applied.iter().map(String::as_str).collect()
    }

    const FULL: &str = r#"{
        "schema": "opencompose.preferences.v1",
        "exported_at": "2026-10-31T10:00:00Z",
        "app_version": "0.1.0",
        "theme": { "family": "catppuccin", "variant": "mocha" },
        "locale": "zh-Hans",
        "ui_scale": 1.2,
        "audio": { "output_device": "coreaudio:builtin", "sample_rate": 48000, "buffer_size": 512 },
        "midi": { "input": "Keystation 49", "channel": 2, "monitor": true },
        "export": { "format": "flac", "bit_depth": 24, "flac_level": 7, "normalise": true },
        "grid": { "subdivision": 6 },
        "keymap": { "save": "Cmd+S" },
        "workspace": { "recent_directories": ["/Users/me/songs"] }
    }"#;

    #[test]
    fn a_full_bundle_validates_every_leaf() {
        let fixture = fixture("full");
        let path = write(&fixture.dir, "prefs.json", FULL);
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(
            field_names(&import),
            vec![
                "theme",
                "locale",
                "ui_scale",
                "audio.output_device",
                "audio.sample_rate",
                "audio.buffer_size",
                "midi.input",
                "midi.channel",
                "midi.monitor",
                "export.format",
                "export.bit_depth",
                "export.flac_level",
                "export.normalise",
                "grid.subdivision",
                "keymap",
                "workspace.recent_directories",
            ]
        );
        let bundle = import.bundle;
        assert_eq!(bundle.theme.unwrap().variant.as_deref(), Some("mocha"));
        assert_eq!(bundle.audio.unwrap().sample_rate, Some(48_000));
        assert_eq!(bundle.midi.unwrap().input.as_deref(), Some("Keystation 49"));
        assert_eq!(bundle.export.unwrap().flac_level, Some(7));
        assert_eq!(bundle.keymap, Some(serde_json::json!({ "save": "Cmd+S" })));
    }

    #[test]
    fn every_field_is_independently_optional() {
        let fixture = fixture("minimal");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{ "schema": "opencompose.preferences.v1" }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.applied.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(
            import.bundle,
            PreferencesBundle {
                schema: SCHEMA.to_string(),
                ..PreferencesBundle::default()
            }
        );
    }

    #[test]
    fn a_wrong_schema_is_refused() {
        let fixture = fixture("schema");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{ "schema": "opencompose.preferences.v2", "locale": "en" }"#,
        );
        let error = preferences_import(path).unwrap_err();
        assert_eq!(
            error,
            "Not an OpenCompose preferences file: schema 'opencompose.preferences.v2' is not opencompose.preferences.v1."
        );
    }

    #[test]
    fn a_file_without_a_schema_is_refused() {
        let fixture = fixture("noschema");
        let path = write(&fixture.dir, "prefs.json", r#"{ "locale": "en" }"#);
        let error = preferences_import(path).unwrap_err();
        assert!(
            error.contains("schema '' is not opencompose.preferences.v1"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_field_is_ignored() {
        let fixture = fixture("unknown");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "window": { "width": 1400, "height": 900, "x": 10 },
                "locale": "ja",
                "future_setting": [1, 2, 3]
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(field_names(&import), vec!["locale"]);
    }

    #[test]
    fn a_secret_named_field_is_refused() {
        for (label, name) in [
            ("snake", "api_key"),
            ("camel", "openaiApiKey"),
            ("dashed", "access-token"),
            ("nested", "ai.token"),
            ("password", "password"),
        ] {
            let fixture = fixture(label);
            let body = if label == "nested" {
                r#"{ "schema": "opencompose.preferences.v1", "ai": { "token": "sk-live" } }"#
                    .to_string()
            } else {
                format!(r#"{{ "schema": "opencompose.preferences.v1", "{name}": "sk-live" }}"#)
            };
            let path = write(&fixture.dir, "prefs.json", &body);
            let import = import(&path);
            assert!(!import.errors.is_empty(), "{label}: expected a refusal");
            assert!(
                import.errors[0].contains("credential"),
                "{label}: {}",
                import.errors[0]
            );
            assert!(import.applied.is_empty(), "{label}: nothing may be applied");
            assert_eq!(import.bundle, PreferencesBundle::default());
        }
    }

    #[test]
    fn key_bindings_inside_the_keymap_are_not_secrets() {
        let fixture = fixture("keymap");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "keymap": { "toggle key": "K", "password_field": "P" }
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert_eq!(field_names(&import), vec!["keymap"]);
    }

    #[test]
    fn an_oversize_file_is_refused() {
        let fixture = fixture("oversize");
        let body = "x".repeat(MAX_BUNDLE_BYTES as usize + 1);
        let path = write(&fixture.dir, "prefs.json", &body);
        let error = preferences_import(path).unwrap_err();
        assert_eq!(error, "The file is larger than 1 MiB.");
    }

    #[test]
    fn out_of_range_values_are_skipped_with_a_reason() {
        let fixture = fixture("range");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "locale": "klingon",
                "ui_scale": 9.0,
                "audio": { "output_device": "coreaudio:builtin", "sample_rate": 1, "buffer_size": 4 },
                "midi": { "channel": 40, "monitor": false },
                "export": { "format": "ogg", "bit_depth": 12, "flac_level": 9 },
                "grid": { "subdivision": 0 }
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert_eq!(
            field_names(&import),
            vec!["audio.output_device", "midi.monitor",],
            "only the leaves that validate survive"
        );
        let skipped: Vec<(&str, &str)> = import
            .skipped
            .iter()
            .map(|entry| (entry.field.as_str(), entry.reason.as_str()))
            .collect();
        assert_eq!(skipped.len(), 9, "{skipped:?}");
        assert_eq!(skipped[0].0, "locale");
        assert!(skipped[0].1.contains("klingon"));
        assert_eq!(skipped[1].0, "ui_scale");
        assert!(skipped[1].1.contains("0.7"));
        assert_eq!(skipped[2].0, "audio.sample_rate");
        assert!(skipped[2].1.contains("1"));
        assert_eq!(skipped[3].0, "audio.buffer_size");
        assert_eq!(skipped[4].0, "midi.channel");
        assert_eq!(skipped[5].0, "export.format");
        assert_eq!(skipped[6].0, "export.bit_depth");
        assert_eq!(skipped[7].0, "export.flac_level");
        assert_eq!(skipped[8].0, "grid.subdivision");
        // A leaf that failed must not survive into the typed values.
        let audio = import.bundle.audio.unwrap();
        assert_eq!(audio.sample_rate, None);
        assert_eq!(audio.buffer_size, None);
        assert_eq!(audio.output_device.as_deref(), Some("coreaudio:builtin"));
    }

    #[test]
    fn an_mp3_export_bundle_keeps_format_bitrate_and_vbr() {
        let fixture = fixture("mp3");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "export": { "format": "mp3", "bit_depth": 24, "mp3_bitrate": 320, "mp3_vbr": 4 }
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(
            field_names(&import),
            vec!["export.format", "export.mp3_bitrate", "export.mp3_vbr"]
        );
        let export = import.bundle.export.unwrap();
        assert_eq!(export.format.as_deref(), Some("mp3"));
        assert_eq!(export.mp3_bitrate, Some(320));
        assert_eq!(export.mp3_vbr, Some(4));
        // MP3 consumes 16-bit PCM, so a carried depth is ignored, not kept.
        assert_eq!(export.bit_depth, None);
    }

    #[test]
    fn an_mp3_bundle_without_a_bitrate_keeps_the_default() {
        let fixture = fixture("mp3-default");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{ "schema": "opencompose.preferences.v1", "export": { "format": "mp3" } }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(
            field_names(&import),
            vec!["export.format", "export.mp3_bitrate"]
        );
        let export = import.bundle.export.unwrap();
        assert_eq!(export.mp3_bitrate, Some(DEFAULT_MP3_BITRATE));
        assert_eq!(export.mp3_vbr, None);
    }

    #[test]
    fn an_out_of_range_mp3_bitrate_or_vbr_is_skipped_with_a_reason() {
        let fixture = fixture("mp3-range");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "export": { "format": "mp3", "mp3_bitrate": 999, "mp3_vbr": 12 }
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty(), "a skip never refuses the bundle");
        assert_eq!(field_names(&import), vec!["export.format"]);
        let skipped: Vec<(&str, &str)> = import
            .skipped
            .iter()
            .map(|entry| (entry.field.as_str(), entry.reason.as_str()))
            .collect();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert_eq!(skipped[0].0, "export.mp3_bitrate");
        assert!(skipped[0].1.contains("999"), "{}", skipped[0].1);
        assert!(skipped[0].1.contains("32"), "{}", skipped[0].1);
        assert_eq!(skipped[1].0, "export.mp3_vbr");
        assert!(skipped[1].1.contains("12"), "{}", skipped[1].1);
        let export = import.bundle.export.unwrap();
        assert_eq!(export.mp3_bitrate, None);
        assert_eq!(export.mp3_vbr, None);
    }

    #[test]
    fn export_writes_a_bundle_after_an_mp3_export() {
        let fixture = fixture("mp3-export");
        let path = fixture.dir.join("prefs.json");
        // The shape `buildPreferencesBundle` writes for an MP3 last export,
        // which the previous build refused because mp3 was not a format.
        let body = r#"{
            "schema": "opencompose.preferences.v1",
            "exported_at": "2026-11-04T10:00:00.000Z",
            "app_version": "0.1.0",
            "locale": "ja",
            "export": {
                "format": "mp3",
                "bit_depth": 16,
                "flac_level": 5,
                "normalise": false,
                "mp3_bitrate": 320,
                "mp3_vbr": 4
            }
        }"#;
        preferences_export(path.to_string_lossy().to_string(), body.to_string()).unwrap();
        let written: PreferencesBundle =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let export = written.export.unwrap();
        assert_eq!(export.format.as_deref(), Some("mp3"));
        assert_eq!(export.mp3_bitrate, Some(320));
        assert_eq!(export.mp3_vbr, Some(4));
        assert_eq!(export.bit_depth, None);
        assert_eq!(
            written.app_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_bundle_without_the_mp3_fields_imports_unchanged() {
        let fixture = fixture("mp3-absent");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{
                "schema": "opencompose.preferences.v1",
                "export": { "format": "flac", "bit_depth": 24, "flac_level": 5, "normalise": true }
            }"#,
        );
        let import = import(&path);
        assert!(import.errors.is_empty());
        assert!(import.skipped.is_empty());
        assert_eq!(
            field_names(&import),
            vec![
                "export.format",
                "export.bit_depth",
                "export.flac_level",
                "export.normalise",
            ]
        );
        let export = import.bundle.export.unwrap();
        assert_eq!(export.mp3_bitrate, None);
        assert_eq!(export.mp3_vbr, None);
    }

    #[test]
    fn a_wrongly_typed_field_refuses_the_file() {
        let fixture = fixture("type");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{ "schema": "opencompose.preferences.v1", "ui_scale": "large" }"#,
        );
        let error = preferences_import(path).unwrap_err();
        assert!(error.contains("has a field of the wrong type"), "{error}");
    }

    #[test]
    fn a_non_object_keymap_is_a_skipped_field() {
        let fixture = fixture("keymap-array");
        let path = write(
            &fixture.dir,
            "prefs.json",
            r#"{ "schema": "opencompose.preferences.v1", "keymap": ["Cmd+S"] }"#,
        );
        let import = import(&path);
        assert_eq!(field_names(&import), Vec::<&str>::new());
        assert_eq!(import.skipped.len(), 1);
        assert_eq!(import.skipped[0].field, "keymap");
        assert!(import.skipped[0].reason.contains("object"));
    }

    #[test]
    fn export_writes_the_validated_bundle_and_stamps_the_build_version() {
        let fixture = fixture("export");
        let path = fixture.dir.join("prefs.json");
        let body = FULL.replace("\"0.1.0\"", "\"0.0.9\"");
        preferences_export(path.to_string_lossy().to_string(), body).unwrap();
        let written: PreferencesBundle =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.schema, SCHEMA);
        assert_eq!(
            written.app_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(written.locale.as_deref(), Some("zh-Hans"));
        assert_eq!(written.export.unwrap().format.as_deref(), Some("flac"));
        // One file, and no temporary left behind.
        let entries: Vec<_> = std::fs::read_dir(&fixture.dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn export_refuses_a_secret_or_an_invalid_value() {
        let fixture = fixture("export-refuse");
        let path = fixture.dir.join("prefs.json");
        let secret = r#"{ "schema": "opencompose.preferences.v1", "api_key": "sk-live" }"#;
        let error =
            preferences_export(path.to_string_lossy().to_string(), secret.to_string()).unwrap_err();
        assert!(error.contains("credential"), "{error}");
        let invalid = r#"{ "schema": "opencompose.preferences.v1", "ui_scale": 9.0 }"#;
        let error = preferences_export(path.to_string_lossy().to_string(), invalid.to_string())
            .unwrap_err();
        assert!(error.contains("Refusing to write"), "{error}");
        assert!(error.contains("ui_scale"), "{error}");
        assert!(!path.exists(), "a refused export writes nothing");
    }

    #[test]
    fn export_refuses_an_oversize_bundle() {
        let fixture = fixture("export-oversize");
        let path = fixture.dir.join("prefs.json");
        let body = format!(
            r#"{{ "schema": "opencompose.preferences.v1", "exported_at": "{}" }}"#,
            "x".repeat(MAX_BUNDLE_BYTES as usize)
        );
        let error = preferences_export(path.to_string_lossy().to_string(), body).unwrap_err();
        assert_eq!(error, "The file is larger than 1 MiB.");
        assert!(!path.exists());
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary_file_behind() {
        let fixture = fixture("atomic");
        let path = fixture.dir.join("prefs.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&fixture.dir).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_atomic_write_removes_its_temporary_file() {
        let fixture = fixture("atomic-fail");
        // Renaming a file onto an existing directory fails, after the temporary
        // file was written: nothing may be left in the fixture.
        let target = fixture.dir.join("occupied");
        std::fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"payload").is_err());
        let names: Vec<String> = std::fs::read_dir(&fixture.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["occupied".to_string()]);
    }

    #[test]
    fn a_missing_file_reports_the_path() {
        let fixture = fixture("missing");
        let path = fixture.dir.join("absent.json");
        let error = preferences_import(path.to_string_lossy().to_string()).unwrap_err();
        assert!(error.starts_with("Could not read "), "{error}");
        assert!(error.contains("absent.json"), "{error}");
    }
}
