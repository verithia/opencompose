//! Plugin scanning, marketplace index, sampler assets, and project compatibility.

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use oc_dsl::ast;

use crate::state::AppState;

/// Scan installed plugins through the crash-isolated per-bundle helper cache.
/// The inventory walk itself. The agent dispatcher and the async command both
/// call this; only the command has to hop off the main thread.
pub fn scan_plugins_blocking(rescan: Option<bool>) -> Vec<oc_host::PluginInfo> {
    let started = std::time::Instant::now();
    let plugins = inventory_for(
        rescan,
        &mut crate::plugin_inventory::plugins,
        &mut crate::plugin_inventory::rescan,
    );
    // A warm inventory answers from the disk cache; a cold scan walks every
    // bundle in a helper process. Keep both costs visible.
    log::debug!(
        "plugin inventory: {} entries in {:?}",
        plugins.len(),
        started.elapsed()
    );
    plugins
}

/// Which inventory a `scan_plugins` call serves. Only the explicit action scans:
/// every other call answers from the published list, so opening a project never
/// waits for the bundles.
fn inventory_for(
    rescan: Option<bool>,
    serve: &mut dyn FnMut() -> Vec<oc_host::PluginInfo>,
    scan: &mut dyn FnMut() -> Vec<oc_host::PluginInfo>,
) -> Vec<oc_host::PluginInfo> {
    if rescan.unwrap_or(false) {
        scan()
    } else {
        serve()
    }
}

#[tauri::command]
/// Scan installed plugins off the main thread.
pub async fn scan_plugins(rescan: Option<bool>) -> Vec<oc_host::PluginInfo> {
    // The inventory walks the plugin folders and inspects every bundle in a
    // helper process; it must never run on the webview's main thread.
    tauri::async_runtime::spawn_blocking(move || scan_plugins_blocking(rescan))
        .await
        .unwrap_or_default()
}

/// Return the active free-plugin catalog plus installed bundles for this OS.
#[tauri::command]
pub async fn plugin_catalog(
    client: State<'_, crate::plugin_index::IndexClient>,
) -> Result<crate::plugin_store::PluginCatalog, String> {
    let view = client.view()?;
    let status = client.status();
    let mut catalog = crate::plugin_store::catalog_response(&view.entries);
    catalog.source = status.source;
    catalog.generated_at = status.generated_at;
    catalog.fetched_at = status.fetched_at;
    catalog.stale_days = status.stale_days;
    catalog.last_error = status.last_error;
    Ok(catalog)
}

/// Read-only marketplace search for the built-in AI panel.
#[tauri::command]
pub fn plugin_search(
    client: State<'_, crate::plugin_index::IndexClient>,
    query: Option<String>,
    tags: Option<Vec<String>>,
    platform: Option<String>,
    format: Option<String>,
    installed_only: Option<bool>,
    limit: Option<usize>,
) -> Result<crate::plugin_store::PluginSearchResult, String> {
    let view = client.view()?;
    crate::plugin_store::search(
        view.entries.as_slice(),
        view.source.as_str(),
        query.as_deref(),
        tags.as_deref(),
        platform.as_deref(),
        format.as_deref(),
        installed_only.unwrap_or(false),
        limit,
    )
}

/// Open a catalog URL with the system browser. The URL must be allowlisted.
#[tauri::command]
pub fn plugin_store_open(
    client: State<'_, crate::plugin_index::IndexClient>,
    url: String,
) -> Result<(), String> {
    let view = client.view()?;
    crate::plugin_store::open_allowed_url(view.entries.as_slice(), &url)
}

/// Open the vendor page a page-only entry links, and return that address.
///
/// The address is resolved from the active catalog by id, so the page a person
/// opens is the page the catalog names. Only https reaches the browser.
#[tauri::command]
pub fn plugin_manual_page(
    client: State<'_, crate::plugin_index::IndexClient>,
    id: String,
) -> Result<String, String> {
    let view = client.view()?;
    let url = crate::plugin_store::manual_page_url(view.entries.as_slice(), &id)?;
    crate::commands::open_https_url(&url)?;
    Ok(url)
}

/// Re-scan the plugin folders and report whether a page-only entry is installed
/// now. Nothing is downloaded, and no vendor installer is launched.
#[tauri::command]
pub async fn plugin_installed_check(
    client: State<'_, crate::plugin_index::IndexClient>,
    id: String,
) -> Result<crate::plugin_store::InstalledCheck, String> {
    let entries = client.view()?.entries;
    // The walk reads the plugin directories, so keep it off the webview thread.
    tauri::async_runtime::spawn_blocking(move || {
        crate::plugin_store::installed_check(entries.as_slice(), &id)
    })
    .await
    .unwrap_or_else(|error| Err(format!("plugin scan failed: {error}")))
}

/// Ask a running install to stop at its next checkpoint. The id is the pack or
/// catalog entry the panel is installing.
#[tauri::command]
pub fn install_cancel(id: String) {
    crate::plugin_store::request_cancel(&id);
}

/// Install a catalog entry. The install URL always comes from the active catalog.
#[tauri::command]
pub async fn plugin_store_install(
    client: State<'_, crate::plugin_index::IndexClient>,
    id: String,
    overwrite: Option<bool>,
) -> Result<crate::plugin_store::InstallOutcome, String> {
    let view = client.view()?;
    crate::plugin_store::install(
        &view.authority,
        view.entries.as_slice(),
        &id,
        overwrite.unwrap_or(false),
        None,
    )
    .await
}

/// Report the active plugin-index source and any cached refresh failure.
#[tauri::command]
pub fn plugin_index_status(
    client: State<'_, crate::plugin_index::IndexClient>,
) -> crate::plugin_index::PluginIndexStatus {
    client.status()
}

/// Refresh the signed plugin index; `force` bypasses the 6-hour freshness
/// interval but never the 5-minute manual floor.
#[tauri::command]
pub async fn plugin_index_refresh(
    client: State<'_, crate::plugin_index::IndexClient>,
    force: Option<bool>,
) -> Result<crate::plugin_index::RefreshOutcome, crate::plugin_index::IndexError> {
    let fetch = crate::plugin_index::HttpFetch::new()
        .map_err(|message| crate::plugin_index::IndexError::new("network", message))?;
    client.refresh(&fetch, force.unwrap_or(false)).await
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// One qualified, relocation-verified sampler package.
pub struct SamplerAssetInfo {
    pub asset_id: String,
    pub entrypoint: String,
    pub source_url: String,
    pub license: String,
    pub attribution: String,
    pub file_count: usize,
    pub manifest_path: String,
}

/// List only qualified, relocation-verified sampler packages.
pub fn list_sampler_assets_blocking() -> Result<Vec<SamplerAssetInfo>, String> {
    // Asset packs ship outside the repo; the env var points at a shared library.
    let root = std::env::var_os("OPENCOMPOSE_SAMPLER_ASSETS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::private_fs::config_dir().join("sampler-assets"));
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut assets = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(|err| {
        format!(
            "failed to read sampler asset library '{}': {err}",
            root.display()
        )
    })? {
        let manifest_path = entry
            .map_err(|err| format!("failed to read sampler asset entry: {err}"))?
            .path()
            .join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .map_err(|err| format!("failed to read '{}': {err}", manifest_path.display()))?,
        )
        .map_err(|err| {
            format!(
                "invalid sampler manifest '{}': {err}",
                manifest_path.display()
            )
        })?;
        if value.get("schema").and_then(|v| v.as_str()) != Some("opencompose.pgen.sampler_asset.v1")
            || value.get("relocation_verified").and_then(|v| v.as_bool()) != Some(true)
        {
            continue;
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("sampler manifest missing string '{key}'"))
        };
        let entrypoint = manifest_path
            .parent()
            .unwrap_or(&root)
            .join(text("entrypoint")?)
            .canonicalize()
            .map_err(|err| format!("sampler entrypoint is missing: {err}"))?;
        assets.push(SamplerAssetInfo {
            asset_id: text("asset_id")?,
            entrypoint: entrypoint.to_string_lossy().into_owned(),
            source_url: text("source_url")?,
            license: text("license")?,
            attribution: text("attribution")?,
            file_count: value
                .get("file_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            manifest_path: manifest_path.to_string_lossy().into_owned(),
        });
    }
    assets.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
    Ok(assets)
}

#[tauri::command]
/// List qualified sampler packages off the main thread.
pub async fn list_sampler_assets() -> Result<Vec<SamplerAssetInfo>, String> {
    tauri::async_runtime::spawn_blocking(list_sampler_assets_blocking)
        .await
        .unwrap_or_else(|error| Err(format!("sampler asset scan failed: {error}")))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// One project compatibility problem.
pub struct CompatibilityIssue {
    pub kind: String,
    pub subject: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// External resources required to reopen and render a project.
pub struct ProjectCompatibilityReport {
    pub compatible: bool,
    pub installed_plugin_count: usize,
    pub resolved_plugins: Vec<ResolvedPlugin>,
    pub issues: Vec<CompatibilityIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// A project plugin requirement matched to an installed plugin.
pub struct ResolvedPlugin {
    pub required_id: String,
    pub installed_id: String,
    pub name: String,
    pub format: String,
    pub path: String,
    pub version: Option<String>,
    pub architectures: Vec<String>,
    pub state_abi: Option<String>,
}

/// Audit external resources required to reopen and render the current project.
/// Takes the app-state lock and may warm the plugin inventory, so the command
/// below runs it on the blocking pool; the agent dispatcher calls it directly.
pub fn project_compatibility_report_blocking(state: &AppState) -> ProjectCompatibilityReport {
    let inner = state.lock_inner();
    build_project_compatibility_report(
        &inner.project,
        inner.file_path.as_deref(),
        oc_host::cached_plugins(),
    )
}

#[tauri::command]
/// Audit project compatibility off the main thread.
pub async fn project_compatibility_report(app: tauri::AppHandle) -> ProjectCompatibilityReport {
    // The panel asks for this on mount; a cold inventory scan here froze the
    // webview thread before it could paint the loading skeleton.
    tauri::async_runtime::spawn_blocking(move || {
        project_compatibility_report_blocking(&app.state::<AppState>())
    })
    .await
    .unwrap_or_else(|error| ProjectCompatibilityReport {
        compatible: false,
        installed_plugin_count: 0,
        resolved_plugins: Vec::new(),
        issues: vec![CompatibilityIssue {
            kind: "report_failed".into(),
            subject: "compatibility".into(),
            message: format!("compatibility task failed: {error}"),
        }],
    })
}

pub(crate) fn build_project_compatibility_report(
    project: &ast::Project,
    project_path: Option<&std::path::Path>,
    plugins: &[oc_host::PluginInfo],
) -> ProjectCompatibilityReport {
    let mut issues = Vec::new();
    let mut resolved_plugins = Vec::new();
    let resolve = |raw: &str| {
        let path = std::path::PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            project_path
                .and_then(std::path::Path::parent)
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(path)
        }
    };

    for track in &project.tracks {
        if let Some(preset) = &track.preset
            && !project
                .presets
                .iter()
                .any(|candidate| &candidate.name == preset)
        {
            issues.push(CompatibilityIssue {
                kind: "missing_preset".into(),
                subject: track.name.clone(),
                message: format!("Preset '{}' is not defined", preset),
            });
        }
    }

    for preset in &project.presets {
        if matches!(
            preset.source.as_deref(),
            Some("clap" | "vst3" | "vst2" | "au" | "lv2")
        ) {
            match preset.plugin.as_deref() {
                None => issues.push(CompatibilityIssue {
                    kind: "missing_plugin_id".into(),
                    subject: preset.name.clone(),
                    message: "External plugin preset has no plugin identifier".into(),
                }),
                Some(required) => record_plugin_requirement(
                    required,
                    &preset.name,
                    plugins,
                    &mut resolved_plugins,
                    &mut issues,
                ),
            }
            if let Some(required) = preset.plugin.as_deref()
                && let Some(plugin) = resolve_plugin_requirement(required, plugins)
            {
                record_plugin_qualification(preset, &plugin, &mut issues);
            }
        }

        if preset.source.as_deref() == Some("vst3")
            && preset
                .plugin
                .as_deref()
                .is_some_and(|plugin| plugin.to_ascii_lowercase().contains("sitala"))
            && !oc_host::preset_has_declared_plugin_state(
                &preset.params,
                oc_host::PluginFormat::Vst3,
            )
        {
            issues.push(CompatibilityIssue {
                kind: "missing_sampler_state".into(),
                subject: preset.name.clone(),
                message: "Sitala has no captured kit state; add vst3_state_file or vst3_state_hex before rendering"
                    .into(),
            });
        }

        if let Some(file) = &preset.file
            && !resolve(file).is_file()
        {
            issues.push(CompatibilityIssue {
                kind: "missing_media".into(),
                subject: preset.name.clone(),
                message: format!("Sample/media file '{}' is missing", file),
            });
        }

        for (key, value) in &preset.params {
            let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
            if matches!(
                normalized.as_str(),
                "statefile"
                    | "pluginstatefile"
                    | "vst3statefile"
                    | "clapstatefile"
                    | "lv2statefile"
            ) && let ast::ParamValue::String(file) = value
                && !resolve(file).is_file()
            {
                issues.push(CompatibilityIssue {
                    kind: "missing_plugin_state".into(),
                    subject: preset.name.clone(),
                    message: format!("Plugin state file '{}' is missing", file),
                });
            }
        }
    }

    let mut effect_requirements = Vec::new();
    for track in &project.tracks {
        collect_effect_plugin_requirements(&track.effects, &track.name, &mut effect_requirements);
        for pattern in &track.inline_patterns {
            collect_effect_plugin_requirements(
                &pattern.effects,
                &pattern.name,
                &mut effect_requirements,
            );
        }
    }
    for pattern in &project.patterns {
        collect_effect_plugin_requirements(
            &pattern.effects,
            &pattern.name,
            &mut effect_requirements,
        );
    }
    for preset in &project.presets {
        collect_effect_plugin_requirements(&preset.effects, &preset.name, &mut effect_requirements);
    }
    for bus in &project.buses {
        collect_effect_plugin_requirements(&bus.effects, &bus.name, &mut effect_requirements);
    }
    if let Some(master) = &project.master {
        collect_effect_plugin_requirements(&master.effects, "master", &mut effect_requirements);
    }
    for (required, subject) in effect_requirements {
        record_plugin_requirement(
            &required,
            &subject,
            plugins,
            &mut resolved_plugins,
            &mut issues,
        );
    }

    resolved_plugins.sort_by(|a, b| a.required_id.cmp(&b.required_id));
    resolved_plugins.dedup_by(|a, b| a.required_id == b.required_id && a.path == b.path);

    ProjectCompatibilityReport {
        compatible: issues.is_empty(),
        installed_plugin_count: plugins.len(),
        resolved_plugins,
        issues,
    }
}

fn collect_effect_plugin_requirements(
    effects: &[ast::Effect],
    subject: &str,
    requirements: &mut Vec<(String, String)>,
) {
    for effect in effects {
        if let ast::EffectType::Plugin(plugin_id) = &effect.effect_type {
            requirements.push((plugin_id.clone(), subject.to_string()));
        }
    }
}

fn record_plugin_requirement(
    required: &str,
    subject: &str,
    plugins: &[oc_host::PluginInfo],
    resolved: &mut Vec<ResolvedPlugin>,
    issues: &mut Vec<CompatibilityIssue>,
) {
    if let Some(plugin) = resolve_plugin_requirement(required, plugins) {
        resolved.push(ResolvedPlugin {
            required_id: required.to_string(),
            installed_id: plugin.id.clone(),
            name: plugin.name.clone(),
            format: format!("{:?}", plugin.format),
            path: plugin.path.clone(),
            version: plugin.version.clone(),
            architectures: plugin.architectures.clone(),
            state_abi: plugin.state_abi.clone(),
        });
    } else {
        issues.push(CompatibilityIssue {
            kind: "missing_plugin".into(),
            subject: subject.to_string(),
            message: format!("Plugin '{}' is not installed", required),
        });
    }
}

pub(crate) fn find_plugin_requirement<'a>(
    required: &str,
    plugins: &'a [oc_host::PluginInfo],
) -> Option<&'a oc_host::PluginInfo> {
    plugins.iter().find(|plugin| names_plugin(plugin, required))
}

/// Split the format prefix the DSL keeps in an effect reference
/// (`vst3.`, `clap.`, `vst2.`, `au.`, `lv2.`) from the bare identifier.
fn split_format_prefix(required: &str) -> Option<(oc_host::PluginFormat, &str)> {
    let (prefix, bare) = required.split_once('.')?;
    let format = match prefix.to_ascii_lowercase().as_str() {
        "clap" => oc_host::PluginFormat::Clap,
        "vst3" => oc_host::PluginFormat::Vst3,
        "vst2" => oc_host::PluginFormat::Vst2,
        "au" => oc_host::PluginFormat::Au,
        "lv2" => oc_host::PluginFormat::Lv2,
        _ => return None,
    };
    (!bare.is_empty()).then_some((format, bare))
}

/// Resolve one plugin reference the way the render path does: an unprefixed
/// identifier names an inventory entry, and a prefixed one is stripped and
/// looked up in that format.
pub(crate) fn resolve_plugin_requirement(
    required: &str,
    plugins: &[oc_host::PluginInfo],
) -> Option<oc_host::PluginInfo> {
    if let Some(found) = find_plugin_requirement(required, plugins) {
        return Some(found.clone());
    }
    let (format, bare) = split_format_prefix(required)?;
    if let Some(found) = plugins
        .iter()
        .find(|plugin| plugin.format == format && names_plugin(plugin, bare))
    {
        return Some(found.clone());
    }
    // The scanner also reaches a bundle installed after the last scan.
    oc_host::find_plugin_info(bare, format)
}

fn names_plugin(plugin: &oc_host::PluginInfo, name: &str) -> bool {
    plugin.id.eq_ignore_ascii_case(name)
        || plugin.name.eq_ignore_ascii_case(name)
        || plugin.path.eq_ignore_ascii_case(name)
}

fn record_plugin_qualification(
    preset: &ast::PresetDef,
    plugin: &oc_host::PluginInfo,
    issues: &mut Vec<CompatibilityIssue>,
) {
    let declared = |wanted: &str| {
        preset.params.iter().find_map(|(name, value)| {
            let normalized: String = name
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            (normalized == wanted).then_some(match value {
                ast::ParamValue::String(value) => Some(value.as_str()),
                _ => None,
            })?
        })
    };
    if let Some(required) = declared("pluginversion")
        && plugin.version.as_deref() != Some(required)
    {
        issues.push(CompatibilityIssue {
            kind: "plugin_version_mismatch".into(),
            subject: preset.name.clone(),
            message: format!(
                "Plugin '{}' requires version '{}', installed version is '{}'",
                plugin.name,
                required,
                plugin.version.as_deref().unwrap_or("unknown")
            ),
        });
    }
    if let Some(required) = declared("pluginarchitecture")
        && !plugin.architectures.iter().any(|arch| arch == required)
    {
        issues.push(CompatibilityIssue {
            kind: "plugin_architecture_mismatch".into(),
            subject: preset.name.clone(),
            message: format!(
                "Plugin '{}' requires architecture '{}', installed bundle advertises {:?}",
                plugin.name, required, plugin.architectures
            ),
        });
    }
    if let Some(required) = declared("pluginstateabi")
        && plugin.state_abi.as_deref() != Some(required)
    {
        issues.push(CompatibilityIssue {
            kind: "plugin_state_abi_mismatch".into(),
            subject: preset.name.clone(),
            message: format!(
                "Plugin '{}' state ABI mismatch: required '{}', installed '{}'",
                plugin.name,
                required,
                plugin.state_abi.as_deref().unwrap_or("unknown")
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The explicit action is the only caller that walks the bundles; a panel
    /// mount or a project open answers from the published inventory.
    #[test]
    fn only_an_explicit_rescan_walks_the_bundles() {
        let served = std::cell::Cell::new(0);
        let scanned = std::cell::Cell::new(0);
        let mut serve = || {
            served.set(served.get() + 1);
            Vec::new()
        };
        let mut scan = || {
            scanned.set(scanned.get() + 1);
            Vec::new()
        };

        inventory_for(None, &mut serve, &mut scan);
        inventory_for(Some(false), &mut serve, &mut scan);
        assert_eq!(
            (served.get(), scanned.get()),
            (2, 0),
            "a cached read must not scan"
        );

        inventory_for(Some(true), &mut serve, &mut scan);
        assert_eq!(
            (served.get(), scanned.get()),
            (2, 1),
            "the explicit action must scan"
        );
    }
}
