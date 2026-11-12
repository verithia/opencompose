//! Diagnostics archive for bug reports: logs, project DSL, environment,
//! plugin scan and the last error, with per-section absence recorded.

use oc_diag::time::format_unix_seconds as timestamp;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::state::AppState;

#[derive(Debug, Clone, Deserialize, Serialize)]
/// One captured log line.
pub struct LogLine {
    pub time: u64,
    pub level: String,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
/// Runtime environment facts for diagnostics.
pub struct Environment {
    pub os: String,
    pub arch: String,
    pub os_version: String,
    pub app_version: String,
    pub source_revision: String,
    pub dsl_version: String,
    pub build_profile: String,
    pub project_path: Option<String>,
    pub app_log_path: Option<String>,
}

#[derive(Debug)]
/// Diagnostic archive contents.
pub struct Sections {
    pub logs: Vec<LogLine>,
    pub project: Option<String>,
    pub environment: Environment,
    pub plugins: Option<serde_json::Value>,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    generated_at: String,
    app_version: String,
    build_profile: String,
    contents: Vec<ManifestEntry>,
}

#[derive(Debug, Serialize)]
struct ManifestEntry {
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl ManifestEntry {
    fn included(path: &str) -> Self {
        Self {
            path: path.to_string(),
            status: "included",
            reason: None,
        }
    }

    fn missing(path: &str, reason: &str) -> Self {
        Self {
            path: path.to_string(),
            status: "missing",
            reason: Some(reason.to_string()),
        }
    }
}

/// The tail of the diagnostics session log, at most 4 MiB.
fn session_log() -> Option<Vec<u8>> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let path = oc_diag::log_path()?;
    let data = std::fs::read(&path).ok()?;
    if data.len() as u64 <= LIMIT {
        return Some(data);
    }
    let start = data.len() - LIMIT as usize;
    let mut tail = b"--- truncated to the last 4 MiB ---\n".to_vec();
    tail.extend_from_slice(&data[start..]);
    Some(tail)
}

fn now_iso() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    timestamp(seconds)
}

fn log_time(ms: u64) -> String {
    timestamp(ms / 1000)
}

fn format_logs(lines: &[LogLine]) -> Option<Vec<u8>> {
    if lines.is_empty() {
        return None;
    }
    let text = lines
        .iter()
        .map(|line| {
            format!(
                "{} [{}] {}: {}",
                log_time(line.time),
                line.level,
                line.source,
                line.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("{text}\n").into_bytes())
}

fn last_error_log(lines: &[LogLine]) -> Option<String> {
    lines.iter().rev().find(|line| line.level.eq_ignore_ascii_case("error")).map(|line| {
        format!(
            "{} [error] {}: {}\n\nBacktrace: unavailable (no panic was captured at export time).\n",
            log_time(line.time),
            line.source,
            line.message
        )
    })
}

fn plugin_summary() -> Option<serde_json::Value> {
    let plugins = std::panic::catch_unwind(|| oc_host::cached_plugins().to_vec()).ok()?;
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for plugin in &plugins {
        *formats.entry(format!("{:?}", plugin.format)).or_default() += 1;
    }
    let items = plugins
        .iter()
        .map(|plugin| {
            serde_json::json!({
                "name": plugin.name,
                "vendor": plugin.vendor,
                "format": format!("{:?}", plugin.format),
                "path": plugin.path,
            })
        })
        .collect::<Vec<_>>();
    Some(serde_json::json!({
        "state": "scanned",
        "total": plugins.len(),
        "formats": formats,
        "plugins": items,
    }))
}

fn environment(app: &tauri::AppHandle, project_path: Option<String>) -> Environment {
    Environment {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os_version: os_version(),
        app_version: app.package_info().version.to_string(),
        source_revision: crate::commands::app_info().source_revision,
        dsl_version: crate::commands::app_info().dsl_version,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .to_string(),
        project_path,
        app_log_path: app_log_path(app),
    }
}

/// The plugin-log file for this build, preferring the live file over rotations.
fn app_log_path(app: &tauri::AppHandle) -> Option<String> {
    let dir = app.path().app_log_dir().ok()?;
    let live = dir.join(format!("{}.log", app.package_info().name));
    if live.is_file() {
        return Some(live.display().to_string());
    }
    let mut rotated: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    rotated.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    });
    rotated.pop().map(|path| path.display().to_string())
}

fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return version;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/etc/os-release") {
            if let Some(line) = contents
                .lines()
                .find(|line| line.starts_with("PRETTY_NAME="))
            {
                return line
                    .trim_start_matches("PRETTY_NAME=")
                    .trim_matches('"')
                    .to_string();
            }
        }
    }
    String::new()
}

fn append_file<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<(), String> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, data)
        .map_err(|error| format!("archive {path}: {error}"))
}

/// Build the diagnostics ZIP bytes.
pub fn build_archive(sections: &Sections) -> Result<Vec<u8>, String> {
    let mut contents = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    match format_logs(&sections.logs) {
        Some(data) => {
            contents.push(ManifestEntry::included("logs/recent.log"));
            files.push(("logs/recent.log".to_string(), data));
        }
        None => contents.push(ManifestEntry::missing(
            "logs/recent.log",
            "no in-app log entries",
        )),
    }

    match sections
        .project
        .as_deref()
        .filter(|source| !source.trim().is_empty())
    {
        Some(source) => {
            contents.push(ManifestEntry::included("project/project.ocp"));
            files.push((
                "project/project.ocp".to_string(),
                source.as_bytes().to_vec(),
            ));
        }
        None => contents.push(ManifestEntry::missing(
            "project/project.ocp",
            "no project source available",
        )),
    }

    // The session log written by `oc_diag` (only present when the app was
    // started with --debug/--perf) carries the timings and host detail the
    // in-app log cannot see. Cap it so a long session stays sendable.
    match session_log() {
        Some(data) => {
            contents.push(ManifestEntry::included("logs/session.log"));
            files.push(("logs/session.log".to_string(), data));
        }
        None => contents.push(ManifestEntry::missing(
            "logs/session.log",
            "no diagnostics session log: start with --debug or --perf",
        )),
    }

    match serde_json::to_vec_pretty(&sections.environment) {
        Ok(data) => {
            contents.push(ManifestEntry::included("environment.json"));
            files.push(("environment.json".to_string(), data));
        }
        Err(error) => contents.push(ManifestEntry::missing(
            "environment.json",
            &error.to_string(),
        )),
    }

    match &sections.plugins {
        Some(plugins) => match serde_json::to_vec_pretty(plugins) {
            Ok(data) => {
                contents.push(ManifestEntry::included("plugins/scan-summary.json"));
                files.push(("plugins/scan-summary.json".to_string(), data));
            }
            Err(error) => contents.push(ManifestEntry::missing(
                "plugins/scan-summary.json",
                &error.to_string(),
            )),
        },
        None => contents.push(ManifestEntry::missing(
            "plugins/scan-summary.json",
            "plugin scan unavailable",
        )),
    }

    match sections.last_error.as_deref() {
        Some(error) => {
            contents.push(ManifestEntry::included("errors/last-error.txt"));
            files.push((
                "errors/last-error.txt".to_string(),
                error.as_bytes().to_vec(),
            ));
        }
        None => contents.push(ManifestEntry::missing(
            "errors/last-error.txt",
            "no error entries",
        )),
    }

    let manifest = Manifest {
        generated_at: now_iso(),
        app_version: sections.environment.app_version.clone(),
        build_profile: sections.environment.build_profile.clone(),
        contents,
    };
    let manifest_data = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    files.push(("manifest.json".to_string(), manifest_data));

    let buffer = Vec::new();
    let encoder = flate2::write::GzEncoder::new(buffer, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, data) in &files {
        append_file(&mut builder, path, data)?;
    }
    let encoder = builder.into_inner().map_err(|error| error.to_string())?;
    encoder.finish().map_err(|error| error.to_string())
}

#[tauri::command]
/// Reveal a diagnostics file in the file manager.
pub fn reveal_diagnostics(path: String) -> Result<(), String> {
    reveal_in_file_manager(&path)
}

/// Select `path` in the platform's file manager.
pub(crate) fn reveal_in_file_manager(path: &str) -> Result<(), String> {
    let target = PathBuf::from(path);
    if !target.exists() {
        return Err(format!("file not found: {path}"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(&target);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", target.display()));
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(target.parent().unwrap_or(&target));
        command
    };
    command
        .status()
        .map_err(|error| format!("reveal {}: {error}", target.display()))?;
    Ok(())
}

#[tauri::command]
/// Write a diagnostics archive and return its path.
pub fn export_diagnostics(
    dest: String,
    logs: Vec<LogLine>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let (project_path, project) = {
        let inner = state.lock_inner();
        let path = inner
            .file_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source = Some(inner.dsl_source.clone()).filter(|source| !source.trim().is_empty());
        (path, source)
    };
    let sections = Sections {
        last_error: last_error_log(&logs),
        plugins: plugin_summary(),
        environment: environment(&app, project_path),
        project,
        logs,
    };
    let archive = build_archive(&sections)?;
    let path = PathBuf::from(&dest);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, archive).map_err(|error| format!("write {}: {error}", path.display()))?;
    log::info!("[diagnostics] wrote {}", path.display());
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn environment() -> Environment {
        Environment {
            os: "macos".into(),
            arch: "aarch64".into(),
            os_version: "15.0".into(),
            app_version: "0.1.0".into(),
            source_revision: "test-revision".into(),
            dsl_version: oc_dsl::DSL_VERSION.into(),
            build_profile: "test".into(),
            project_path: Some("/tmp/song.ocp".into()),
            app_log_path: Some("/tmp/logs/opencompose.log".into()),
        }
    }

    fn read_archive(archive: &[u8]) -> BTreeMap<String, String> {
        let decoder = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(decoder);
        let mut files = BTreeMap::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().display().to_string();
            if entry.header().entry_type().is_file() {
                let mut text = String::new();
                entry.read_to_string(&mut text).unwrap();
                files.insert(path, text);
            }
        }
        files
    }

    #[test]
    fn archive_includes_every_collected_section() {
        let sections = Sections {
            logs: vec![
                LogLine {
                    time: 0,
                    level: "info".into(),
                    source: "app".into(),
                    message: "started".into(),
                },
                LogLine {
                    time: 1000,
                    level: "error".into(),
                    source: "render".into(),
                    message: "boom".into(),
                },
            ],
            project: Some("@project \"Test\"\n".into()),
            environment: environment(),
            plugins: Some(serde_json::json!({ "state": "scanned", "total": 2 })),
            last_error: Some("1000 [error] render: boom\n".into()),
        };

        let archive = build_archive(&sections).unwrap();
        let files = read_archive(&archive);

        let logs = files.get("logs/recent.log").unwrap();
        assert!(logs.contains("[info] app: started"));
        assert!(logs.contains("[error] render: boom"));
        assert_eq!(
            files.get("project/project.ocp").unwrap(),
            "@project \"Test\"\n"
        );
        assert!(
            files
                .get("environment.json")
                .unwrap()
                .contains("\"app_version\": \"0.1.0\"")
        );
        assert!(
            files
                .get("environment.json")
                .unwrap()
                .contains("\"app_log_path\": \"/tmp/logs/opencompose.log\"")
        );
        assert!(
            files
                .get("plugins/scan-summary.json")
                .unwrap()
                .contains("\"total\": 2")
        );
        assert!(files.get("errors/last-error.txt").unwrap().contains("boom"));

        let manifest = files.get("manifest.json").unwrap();
        assert!(manifest.contains("\"generated_at\""));
        for path in [
            "logs/recent.log",
            "project/project.ocp",
            "environment.json",
            "plugins/scan-summary.json",
            "errors/last-error.txt",
        ] {
            assert!(manifest.contains(path), "manifest should list {path}");
        }
        // `logs/session.log` only exists when the app runs with --debug/--perf,
        // so its absence is recorded rather than asserted away.
        let parsed: serde_json::Value = serde_json::from_str(manifest).unwrap();
        let missing: Vec<&str> = parsed["contents"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["status"] == "missing")
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert!(
            missing.iter().all(|path| *path == "logs/session.log"),
            "unexpected missing sections: {missing:?}"
        );
    }

    #[test]
    fn absent_sections_are_recorded_without_failing_the_export() {
        let sections = Sections {
            logs: Vec::new(),
            project: None,
            environment: environment(),
            plugins: None,
            last_error: None,
        };

        let archive = build_archive(&sections).unwrap();
        let files = read_archive(&archive);

        assert!(!files.contains_key("logs/recent.log"));
        assert!(!files.contains_key("project/project.ocp"));
        assert!(!files.contains_key("errors/last-error.txt"));
        let manifest = files.get("manifest.json").unwrap();
        assert!(manifest.contains("\"status\": \"missing\""));
        assert!(manifest.contains("no in-app log entries"));
        assert!(manifest.contains("no project source available"));
        assert!(manifest.contains("no error entries"));
        // Environment is always collected, so the archive still has value.
        assert!(files.contains_key("environment.json"));
    }

    #[test]
    fn log_timestamps_are_utc() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
