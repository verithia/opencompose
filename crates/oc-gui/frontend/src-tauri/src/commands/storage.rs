//! The Storage page: what this build keeps on disk, and the one safe deletion.
//!
//! Every count is read from the filesystem, so the page reports what is there
//! rather than what a project claims to use. The clear action deletes files
//! inside the open project's `media/samples` directory only; it never walks
//! outside it and never touches the project file.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

use super::shared::run_blocking;

/// Installed SoundFont packs and where they live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SoundfontStorage {
    pub dir: String,
    /// Packs the pack index knows; a folder can hold files no index names.
    pub installed: usize,
    /// Every soundfont file under the directory.
    pub files: usize,
    pub bytes: u64,
}

/// One plugin format and how many bundles were found for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatStorage {
    pub format: String,
    pub count: usize,
    pub bytes: u64,
}

/// Plugin bundles found in one search path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginFolder {
    pub path: String,
    pub count: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Plugin storage usage.
pub struct PluginStorage {
    pub by_format: Vec<FormatStorage>,
    pub folders: Vec<PluginFolder>,
    pub bytes: u64,
}

/// The open project's sample cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SampleStorage {
    /// `null` until the project has a file, so it has no media folder yet.
    pub dir: Option<String>,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Clip PCM cache usage.
pub struct ClipPcmStorage {
    pub decoded: oc_engine::media_cache::ClipCacheUsage,
    pub processed: oc_engine::media_cache::ClipCacheUsage,
}

fn clip_pcm_report() -> ClipPcmStorage {
    ClipPcmStorage {
        decoded: oc_engine::media_cache::shared().disk_usage(),
        processed: oc_engine::derived_cache::shared().disk_usage(),
    }
}

/// Everything the Storage page shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageReport {
    pub soundfonts: SoundfontStorage,
    pub plugins: PluginStorage,
    pub samples: SampleStorage,
    pub clip_pcm: ClipPcmStorage,
    /// Conversations: messages, DSL versions and the archived audition audio.
    pub sessions: SessionStorage,
    /// The private file this build writes its own preferences to.
    pub preferences: String,
}

/// One workspace directory of conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceStorage {
    /// The directory a person recognises, e.g. `/Users/me/songs`.
    pub path: String,
    /// The hash directory the store groups it under.
    pub key: String,
    pub sessions: Vec<SessionEntry>,
    pub bytes: u64,
}

/// One conversation, with the size it occupies on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    pub updated: String,
}

/// Conversation storage, split by workspace plus the shared audition archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStorage {
    pub root: String,
    pub workspaces: Vec<WorkspaceStorage>,
    /// Every archived audition file: clips and renders, referenced or not.
    pub audio_bytes: u64,
    /// The archive's two directories, their sizes and their shared budget.
    pub auditions: crate::ai::sessions::AuditionArchive,
    pub total_bytes: u64,
}

/// Label for one plugin format, as the page shows it.
fn format_label(format: oc_audio::PluginFormat) -> &'static str {
    match format {
        oc_audio::PluginFormat::Vst3 => "VST3",
        oc_audio::PluginFormat::Clap => "CLAP",
        oc_audio::PluginFormat::Au => "AU",
        oc_audio::PluginFormat::Lv2 => "LV2",
        oc_audio::PluginFormat::Vst2 => "VST2",
        oc_audio::PluginFormat::Internal => "Built-in",
    }
}

/// The bytes a discovered plugin occupies: its bundle directory when the path
/// is inside one, otherwise the file itself. A bundle binary's own metadata is
/// a few kilobytes, which is why per-format totals read 0.0 MB.
fn plugin_bytes(path: &Path) -> u64 {
    let bundles = ["vst3", "clap", "component", "vst", "lv2"];
    let mut current = path;
    while let Some(parent) = current.parent() {
        let named = parent
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| bundles.contains(&ext.to_ascii_lowercase().as_str()));
        if named {
            let (_, bytes) = dir_usage(parent, 0);
            return bytes;
        }
        if parent.parent().is_none() {
            break;
        }
        current = parent;
    }
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Files and bytes under `dir`, following subdirectories to a fixed depth.
fn dir_usage(dir: &Path, depth: usize) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if depth < 3 {
                let (nested_files, nested_bytes) = dir_usage(&entry.path(), depth + 1);
                files += nested_files;
                bytes += nested_bytes;
            }
            continue;
        }
        files += 1;
        bytes += metadata.len();
    }
    (files, bytes)
}

/// The sample cache of the open project, or `None` before it has a file.
fn samples_dir(state: &AppState) -> Option<PathBuf> {
    let file = state.lock_inner().file_path.clone()?;
    let root = file.parent()?;
    Some(root.join(crate::samples::SAMPLES_SUBDIR))
}

fn samples_report_at(dir: Option<&Path>) -> SampleStorage {
    let Some(dir) = dir else {
        return SampleStorage {
            dir: None,
            files: 0,
            bytes: 0,
        };
    };
    let (files, bytes) = dir_usage(dir, 0);
    SampleStorage {
        dir: Some(dir.display().to_string()),
        files,
        bytes,
    }
}

/// Conversation storage: every workspace's messages and DSL versions, plus the
/// shared audition archive.
fn sessions_report() -> SessionStorage {
    sessions_report_at(crate::ai::sessions::Store::config().root())
}

/// The same report for an explicit store root, so tests do not touch the
/// operator's conversations.
fn sessions_report_at(root: &Path) -> SessionStorage {
    let root = root.to_path_buf();
    // Audition clips live in one shared archive; a session's size includes the
    // assets its own messages reference, or a conversation with a dozen
    // auditions would report a few kilobytes.
    let clips = shared_assets(&root.join("audition-media"));
    let renders = shared_assets(&root.join("audition-renders"));
    let auditions = crate::ai::sessions::audition_archive(&root);

    let mut groups: Vec<WorkspaceStorage> = Vec::new();
    let mut total = 0u64;
    let mut referenced = std::collections::HashSet::new();
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let workspace = entry.path();
        if !workspace.is_dir()
            || workspace
                .file_name()
                .is_some_and(|name| name == "audition-media" || name == "audition-renders")
        {
            continue;
        }
        let key = workspace
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let mut sessions: Vec<SessionEntry> = Vec::new();
        let mut workspace_bytes = 0u64;
        for session in std::fs::read_dir(&workspace)
            .into_iter()
            .flatten()
            .flatten()
        {
            let dir = session.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(meta) = crate::ai::sessions::read_meta_for_storage(&dir) else {
                continue;
            };
            let (_, own) = dir_usage(&dir, 0);
            let hashes = referenced_hashes(&dir);
            let bytes = own + asset_bytes(&hashes, &clips) + asset_bytes(&hashes, &renders);
            referenced.extend(hashes);
            workspace_bytes += bytes;
            sessions.push(SessionEntry {
                id: meta.id,
                title: meta.title,
                bytes,
                updated: meta.updated,
            });
        }
        if sessions.is_empty() {
            continue;
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.bytes));
        total += workspace_bytes;
        groups.push(WorkspaceStorage {
            path: workspace_path(&workspace),
            key,
            sessions,
            bytes: workspace_bytes,
        });
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.bytes));
    let archive_bytes = auditions.media.bytes + auditions.renders.bytes;
    // A session's own size already counts the assets it names, so only what no
    // conversation references is added on top.
    let named = asset_bytes(&referenced, &clips) + asset_bytes(&referenced, &renders);
    total += archive_bytes.saturating_sub(named);
    SessionStorage {
        root: root.display().to_string(),
        workspaces: groups,
        audio_bytes: archive_bytes,
        auditions,
        total_bytes: total,
    }
}

/// Every file in one archive directory, keyed by the content hash in its name.
fn shared_assets(dir: &Path) -> std::collections::HashMap<String, u64> {
    let mut assets = std::collections::HashMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        assets.insert(stem.to_ascii_lowercase(), bytes);
    }
    assets
}

/// The asset hashes one session's messages name.
fn referenced_hashes(dir: &Path) -> std::collections::HashSet<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("messages.jsonl")) else {
        return std::collections::HashSet::new();
    };
    let mut seen = std::collections::HashSet::new();
    for token in text.split(|c: char| !c.is_ascii_hexdigit()) {
        if token.len() == 64 {
            seen.insert(token.to_ascii_lowercase());
        }
    }
    seen
}

/// Bytes of shared audition assets these hashes name, counted once.
fn asset_bytes(
    hashes: &std::collections::HashSet<String>,
    assets: &std::collections::HashMap<String, u64>,
) -> u64 {
    hashes
        .iter()
        .filter_map(|hash| assets.get(hash))
        .sum::<u64>()
}

/// The directory a workspace hash stands for, read from one of its sessions.
fn workspace_path(workspace: &Path) -> String {
    for session in std::fs::read_dir(workspace).into_iter().flatten().flatten() {
        if let Some(meta) = crate::ai::sessions::read_meta_for_storage(&session.path())
            && let Some(path) = meta.workspace.as_deref().filter(|path| !path.is_empty())
        {
            return path.to_string();
        }
    }
    workspace.display().to_string()
}

fn soundfonts_report() -> SoundfontStorage {
    let dir = crate::soundfont_store::dir();
    let installed = crate::soundfont_store::installed(&dir)
        .map(|packs| packs.len())
        .unwrap_or(0);
    let (files, bytes) = dir_usage(&dir, 0);
    SoundfontStorage {
        dir: dir.display().to_string(),
        installed,
        files,
        bytes,
    }
}

fn plugins_report() -> PluginStorage {
    let mut counts: Vec<FormatStorage> = Vec::new();
    for (path, format) in oc_host::discover_plugin_files() {
        let label = format_label(format);
        let bytes = plugin_bytes(&path);
        match counts.iter_mut().find(|entry| entry.format == label) {
            Some(entry) => {
                entry.count += 1;
                entry.bytes += bytes;
            }
            None => counts.push(FormatStorage {
                format: label.to_string(),
                count: 1,
                bytes,
            }),
        }
    }
    counts.sort_by(|a, b| a.format.cmp(&b.format));
    let folders: Vec<PluginFolder> = oc_host::plugin_search_paths()
        .into_iter()
        .map(|path| {
            let (count, bytes) = std::fs::metadata(&path)
                .map(|_| dir_usage(Path::new(&path), 0))
                .unwrap_or((0, 0));
            PluginFolder { path, count, bytes }
        })
        .collect();
    PluginStorage {
        by_format: counts,
        bytes: folders.iter().map(|folder| folder.bytes).sum(),
        folders,
    }
}

fn report(state: &AppState) -> StorageReport {
    report_at(samples_dir(state).as_deref())
}

fn report_at(samples: Option<&Path>) -> StorageReport {
    StorageReport {
        soundfonts: soundfonts_report(),
        plugins: { plugins_report() },
        samples: samples_report_at(samples),
        clip_pcm: clip_pcm_report(),
        sessions: sessions_report(),
        preferences: crate::gui_settings::path().display().to_string(),
    }
}

/// Scan one block of the Storage page without walking the other sections. The
/// filesystem walk runs on the blocking pool, so the webview keeps painting.
#[tauri::command]
pub async fn storage_scan(
    state: State<'_, AppState>,
    section: String,
) -> Result<serde_json::Value, String> {
    let samples = samples_dir(&state);
    run_blocking(move || {
        let value = match section.as_str() {
            "clip_pcm" => serde_json::to_value(clip_pcm_report()).unwrap_or_default(),
            "sessions" => serde_json::to_value(sessions_report()).unwrap_or_default(),
            "soundfonts" => serde_json::to_value(soundfonts_report()).unwrap_or_default(),
            "plugins" => serde_json::to_value({
                let mut plugins = plugins_report();
                plugins.bytes = plugins.by_format.iter().map(|entry| entry.bytes).sum();
                plugins
            })
            .unwrap_or_default(),
            "samples" => {
                serde_json::to_value(samples_report_at(samples.as_deref())).unwrap_or_default()
            }
            other => serde_json::json!({ "error": format!("unknown storage section '{other}'") }),
        };
        Ok(value)
    })
    .await
}

/// Counts and paths; opening the page writes nothing.
#[tauri::command]
pub async fn storage_report(state: State<'_, AppState>) -> Result<StorageReport, String> {
    let samples = samples_dir(&state);
    run_blocking(move || Ok(report_at(samples.as_deref()))).await
}

/// Reclaim generated clip audio that no reader still needs.
#[tauri::command]
pub async fn clear_clip_pcm_cache() -> Result<ClipPcmStorage, String> {
    run_blocking(|| {
        oc_engine::media_cache::shared().clear();
        oc_engine::derived_cache::shared().clear();
        Ok(clip_pcm_report())
    })
    .await
}

/// Delete the open project's sample cache and report what is left.
#[tauri::command]
pub fn clear_sample_cache(state: State<AppState>) -> Result<StorageReport, String> {
    let Some(dir) = samples_dir(&state) else {
        return Err(
            "Save the project first: its media folder lives beside the project file.".into(),
        );
    };
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(error) = removed {
                    return Err(format!("cannot remove '{}': {error}", path.display()));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read '{}': {error}", dir.display())),
    }
    Ok(report(&state))
}

/// Delete every archived audition file, referenced or not, and report what is
/// left. Conversations keep the paths their messages name.
#[tauri::command]
pub fn clear_audition_archive(state: State<AppState>) -> Result<StorageReport, String> {
    crate::ai::sessions::Store::config()
        .clear_audition_archive()
        .map_err(|error| format!("cannot clear the audition archive: {error}"))?;
    Ok(report(&state))
}

/// Open one https address in the platform's browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    open_https_url(&url)
}

/// Open an https address in the platform's browser.
///
/// Every surface that opens a web page calls this function, so an address with
/// another scheme, or one carrying whitespace, is refused before a process
/// starts.
pub fn open_https_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.chars().any(char::is_whitespace) {
        return Err(format!("not an https address: {url}"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // No shell: cmd re-parses and expands the URL. rundll32 receives it as
        // one argument, the same path the plugin store uses for vendor pages.
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open {url}: {error}"))
}

/// Open a folder the page names in the platform's file manager.
#[tauri::command]
pub fn open_storage_folder(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(&target);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(&target);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(&target);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot open '{}': {error}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_addresses_reach_the_browser() {
        // Refused before a process starts: the vendor pages all arrive here.
        for url in [
            "http://vital.audio/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://vital.audio/ x",
            "",
        ] {
            let error = open_https_url(url).unwrap_err();
            assert!(error.contains("not an https address"), "{error}");
        }
    }

    #[test]
    fn a_missing_directory_counts_as_empty() {
        let dir = std::env::temp_dir().join("opencompose-storage-missing-does-not-exist");
        assert_eq!(dir_usage(&dir, 0), (0, 0));
    }

    #[test]
    fn the_report_counts_both_audition_directories_against_one_budget() {
        let root = std::env::temp_dir().join(format!(
            "opencompose-storage-auditions-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, hash, bytes) in [
            ("audition-media", "a".repeat(64), 4_096u64),
            ("audition-renders", "b".repeat(64), 8_192u64),
        ] {
            let path = root.join(dir).join(format!("{hash}.wav"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, vec![0u8; bytes as usize]).unwrap();
        }

        let report = sessions_report_at(&root);
        assert_eq!(report.auditions.media.files, 1);
        assert_eq!(report.auditions.media.bytes, 4_096);
        assert_eq!(report.auditions.renders.files, 1);
        assert_eq!(report.auditions.renders.bytes, 8_192);
        assert_eq!(
            report.auditions.media.dir,
            root.join("audition-media").display().to_string()
        );
        assert_eq!(
            report.auditions.renders.dir,
            root.join("audition-renders").display().to_string()
        );
        // Both directories share the cap, and both count as audition audio.
        assert_eq!(report.auditions.budget_bytes, 256 * 1024 * 1024);
        assert_eq!(report.audio_bytes, 4_096 + 8_192);
        // Neither archive directory is read as a workspace.
        assert!(report.workspaces.is_empty(), "{:?}", report.workspaces);
        // No conversation names them, so both are part of the total.
        assert_eq!(report.total_bytes, 4_096 + 8_192);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_session_counts_the_audio_its_messages_reference() {
        let root = std::env::temp_dir().join(format!(
            "opencompose-storage-sessions-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = root.join("ws-key").join("session-1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(root.join("audition-media")).unwrap();
        let referenced = "a".repeat(64);
        let orphan = "b".repeat(64);
        std::fs::write(
            session.join("meta.json"),
            r#"{"id":"001a18de8dfe80000","title":"a take","created":"2026-11-01T00:00:00Z","updated":"2026-11-01T00:00:00Z","model":"m","provider":"p","workspace_key":"ws-key","workspace":"/songs","turns":1,"tokens":0}"#,
        )
        .unwrap();
        std::fs::write(
            session.join("messages.jsonl"),
            format!(
                "{{\"v\":1,\"kind\":\"turn\",\"at\":\"2026-11-01T00:00:00Z\",\"messages\":[{{\"role\":\"user\",\"content\":\"{referenced}\"}}]}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("audition-media")
                .join(format!("{referenced}.wav")),
            vec![0u8; 4096],
        )
        .unwrap();
        std::fs::write(
            root.join("audition-media").join(format!("{orphan}.wav")),
            vec![0u8; 8192],
        )
        .unwrap();

        let report = sessions_report_at(&root);
        let entry = &report.workspaces[0].sessions[0];
        assert!(entry.bytes >= 4096, "{}", entry.bytes);
        assert_eq!(report.audio_bytes, 4096 + 8192);
        // The workspace path comes from the session, not its hash directory.
        assert_eq!(report.workspaces[0].path, "/songs");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn usage_walks_the_directory_it_is_given() {
        let dir = std::env::temp_dir().join(format!(
            "opencompose-storage-usage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("one.wav"), [0u8; 4]).unwrap();
        std::fs::write(dir.join("nested/two.wav"), [0u8; 6]).unwrap();
        assert_eq!(dir_usage(&dir, 0), (2, 10));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_plugin_format_has_a_label() {
        for format in [
            oc_audio::PluginFormat::Vst3,
            oc_audio::PluginFormat::Clap,
            oc_audio::PluginFormat::Au,
            oc_audio::PluginFormat::Lv2,
            oc_audio::PluginFormat::Vst2,
            oc_audio::PluginFormat::Internal,
        ] {
            assert!(!format_label(format).is_empty());
        }
    }
}

/// Select any path the panel names in the platform's file manager.
///
/// The plugin and sample rows show where a file lives; revealing it is the only
/// useful action a path can offer besides copying it.
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    crate::diagnostics::reveal_in_file_manager(&path)
}
