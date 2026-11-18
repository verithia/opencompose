//! What this machine already has: plugins, instrument files and samples in the
//! folders the app searches.
//!
//! The first-run scan page runs [`local_content_scan`], which waits for the
//! plugin inventory scan and then walks the sound folders, reporting progress
//! as it goes. Plugin folders are owned by `oc_host::plugin_folders` and sound
//! folders by the content-library catalogue; this module only reads both and
//! adapts the result for the UI.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;

use crate::content_library::{self, ContentLibrary};
use crate::soundfont_store::{self, RootOrigin, SearchRoot};

/// Emitted while [`local_content_scan`] runs.
pub const PROGRESS_EVENT: &str = "local_content_scan_progress";
/// Items each list returns at most; the counts stay exact.
const MAX_ITEMS: usize = 2_000;
/// Directory entries one sound folder may cost.
const MAX_WALK_ENTRIES: usize = 20_000;
/// Folder levels below a sound folder that are searched.
const MAX_WALK_DEPTH: usize = 8;
/// How often a waiting scan reports plugin progress.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Why a folder is searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderOrigin {
    Standard,
    Convention,
    Environment,
    HostSettings,
    User,
    /// The managed sounds folder that installed sound packs fill.
    Managed,
}

impl From<oc_host::FolderOrigin> for FolderOrigin {
    fn from(origin: oc_host::FolderOrigin) -> Self {
        match origin {
            oc_host::FolderOrigin::Standard => Self::Standard,
            oc_host::FolderOrigin::Convention => Self::Convention,
            oc_host::FolderOrigin::Environment => Self::Environment,
            oc_host::FolderOrigin::HostSettings => Self::HostSettings,
            oc_host::FolderOrigin::User => Self::User,
        }
    }
}

/// One searched folder and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchedFolder {
    pub path: String,
    pub origin: FolderOrigin,
    pub exists: bool,
    /// Items found under this folder, before de-duplication across folders.
    pub count: usize,
    /// The content-library id that removes a user sound folder.
    pub library_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatCount {
    pub format: String,
    pub count: usize,
}

/// One plugin or instrument file in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundItem {
    pub name: String,
    /// The vendor of a plugin, or the folder that holds an instrument file.
    pub detail: String,
    pub format: String,
    pub path: String,
    /// False for a plugin bundle the scan could not read.
    pub readable: bool,
}

/// One kind of content: distinct items, their formats, the folders searched,
/// and the items themselves up to [`MAX_ITEMS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentSummary {
    pub total: usize,
    pub formats: Vec<FormatCount>,
    pub folders: Vec<SearchedFolder>,
    pub items: Vec<FoundItem>,
    /// `items` stops at [`MAX_ITEMS`].
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalContentSummary {
    /// Plugins the inventory lists, plus bundles it could not read.
    pub plugins: ContentSummary,
    /// SoundFont, SFZ and sampler-asset files.
    pub instruments: ContentSummary,
    /// Audio files in sample folders; only counted, never listed.
    pub samples: ContentSummary,
    /// A background scan is reading plugin details now.
    pub scanning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Plugins,
    Sounds,
}

/// The payload of [`PROGRESS_EVENT`]. A zero `total` means the size of the
/// step is not known yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanProgressEvent {
    pub phase: ScanPhase,
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
}

/// Format labels in the order the summary lists them.
const PLUGIN_FORMATS: [&str; 5] = ["vst3", "clap", "au", "lv2", "vst2"];
const INSTRUMENT_FORMATS: [&str; 4] = ["sf2", "sf3", "sfz", "sampler_asset"];
const SAMPLE_EXTENSIONS: [&str; 6] = ["wav", "aif", "aiff", "flac", "ogg", "mp3"];

fn plugin_format(format: oc_host::PluginFormat) -> Option<&'static str> {
    match format {
        oc_host::PluginFormat::Vst3 => Some("vst3"),
        oc_host::PluginFormat::Clap => Some("clap"),
        oc_host::PluginFormat::Au => Some("au"),
        oc_host::PluginFormat::Lv2 => Some("lv2"),
        oc_host::PluginFormat::Vst2 => Some("vst2"),
        oc_host::PluginFormat::Internal => None,
    }
}

/// Count items per format in `order`, dropping formats with none.
fn format_counts<'a>(formats: impl Iterator<Item = &'a str>, order: &[&str]) -> Vec<FormatCount> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for format in formats {
        *counts.entry(format).or_default() += 1;
    }
    order
        .iter()
        .filter_map(|format| {
            counts.get(format).map(|count| FormatCount {
                format: (*format).to_string(),
                count: *count,
            })
        })
        .collect()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn by_name(items: &mut [FoundItem]) {
    items.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn bounded(mut items: Vec<FoundItem>) -> (Vec<FoundItem>, bool) {
    by_name(&mut items);
    let truncated = items.len() > MAX_ITEMS;
    items.truncate(MAX_ITEMS);
    (items, truncated)
}

/// Plugins in `folders`. With an inventory, every plugin it lists is an item
/// and a CLAP, VST3 or VST2 bundle it has no entry for is listed as unreadable;
/// before the first scan, each bundle is one item named after its file.
fn plugin_summary(
    folders: &[oc_host::PluginFolder],
    inventory: Option<&[oc_host::PluginInfo]>,
) -> ContentSummary {
    let mut bundles: BTreeMap<PathBuf, &'static str> = BTreeMap::new();
    let mut searched = Vec::with_capacity(folders.len());
    for folder in folders {
        let path = Path::new(&folder.path);
        let mut count = 0;
        for (bundle, format) in oc_host::discover_plugin_files_in(path) {
            if let Some(format) = plugin_format(format) {
                count += 1;
                bundles.entry(bundle).or_insert(format);
            }
        }
        searched.push(SearchedFolder {
            path: folder.path.clone(),
            origin: folder.origin.into(),
            exists: path.is_dir(),
            count,
            library_id: None,
        });
    }

    let items: Vec<FoundItem> = match inventory {
        Some(inventory) => {
            let mut seen = HashSet::new();
            let mut items: Vec<FoundItem> = inventory
                .iter()
                .filter_map(|info| {
                    let format = plugin_format(info.format)?;
                    seen.insert((info.path.clone(), info.name.clone()))
                        .then(|| FoundItem {
                            name: info.name.clone(),
                            detail: info.vendor.clone(),
                            format: format.to_string(),
                            path: info.path.clone(),
                            readable: true,
                        })
                })
                .collect();
            let listed: HashSet<&str> = inventory.iter().map(|info| info.path.as_str()).collect();
            items.extend(
                bundles
                    .iter()
                    .filter(|(bundle, format)| {
                        matches!(**format, "clap" | "vst3" | "vst2")
                            && !listed.contains(bundle.to_string_lossy().as_ref())
                    })
                    .map(|(bundle, format)| FoundItem {
                        name: file_stem(bundle),
                        detail: String::new(),
                        format: (*format).to_string(),
                        path: bundle.display().to_string(),
                        readable: false,
                    }),
            );
            items
        }
        None => bundles
            .iter()
            .map(|(bundle, format)| FoundItem {
                name: file_stem(bundle),
                detail: String::new(),
                format: (*format).to_string(),
                path: bundle.display().to_string(),
                readable: true,
            })
            .collect(),
    };
    let total = items.len();
    let formats = format_counts(
        items.iter().map(|item| item.format.as_str()),
        &PLUGIN_FORMATS,
    );
    let (items, truncated) = bounded(items);
    ContentSummary {
        total,
        formats,
        folders: searched,
        items,
        truncated,
    }
}

/// Folders where music tools and sample stores keep sample libraries. They
/// are walked for the summary only, never by the instrument picker.
fn sample_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(documents) = dirs::document_dir() {
        for relative in [
            "Samples",
            "Image-Line",
            "Ableton/User Library",
            "Ableton/Factory Packs",
            "lmms/samples",
            "Splice",
        ] {
            folders.push(documents.join(relative));
        }
    }
    if let Some(music) = dirs::audio_dir() {
        for relative in ["Samples", "Ableton/User Library", "Ableton/Factory Packs"] {
            folders.push(music.join(relative));
        }
    }
    if let Some(home) = dirs::home_dir() {
        folders.push(home.join("Splice"));
        folders.push(home.join("lmms/samples"));
    }
    folders
}

/// A folder the sound walk visits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SoundRoot {
    path: PathBuf,
    origin: FolderOrigin,
    library_id: Option<String>,
}

fn sound_roots(instrument_roots: Vec<SearchRoot>, samples: Vec<PathBuf>) -> Vec<SoundRoot> {
    let mut roots: Vec<SoundRoot> = Vec::new();
    let instrument = instrument_roots
        .into_iter()
        .filter_map(|root| match root.origin {
            RootOrigin::Bundled => None,
            RootOrigin::Standard => Some(SoundRoot {
                path: root.path,
                origin: FolderOrigin::Standard,
                library_id: None,
            }),
            RootOrigin::Managed => Some(SoundRoot {
                path: root.path,
                origin: FolderOrigin::Managed,
                library_id: None,
            }),
            RootOrigin::Library(id) => Some(SoundRoot {
                path: root.path,
                origin: FolderOrigin::User,
                library_id: Some(id),
            }),
        });
    let samples = samples.into_iter().map(|path| SoundRoot {
        path,
        origin: FolderOrigin::Convention,
        library_id: None,
    });
    // `SoundFonts` and `soundfonts` are one folder on a case-insensitive
    // disk, so existing folders are compared by their canonical path.
    let mut seen = HashSet::new();
    for root in instrument.chain(samples) {
        if seen.insert(canonical(root.path.clone())) {
            roots.push(root);
        }
    }
    roots
}

/// What one sound folder holds.
#[derive(Debug, Default)]
struct WalkResult {
    instruments: Vec<(PathBuf, &'static str)>,
    samples: Vec<PathBuf>,
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

/// Walk one folder for instrument files and samples. Symlinks and hidden
/// folders are skipped, and the walk stops after [`MAX_WALK_ENTRIES`]
/// entries, so one huge library cannot stall the scan.
fn walk_sounds(root: &Path) -> WalkResult {
    walk_sounds_within(root, MAX_WALK_ENTRIES)
}

fn walk_sounds_within(root: &Path, max_entries: usize) -> WalkResult {
    let mut result = WalkResult::default();
    let mut remaining = max_entries;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if remaining == 0 {
                return result;
            }
            remaining -= 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let hidden = entry.file_name().to_string_lossy().starts_with('.');
                if !hidden && depth + 1 < MAX_WALK_DEPTH {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(extension) = extension_of(&path) else {
                continue;
            };
            match extension.as_str() {
                "sf2" | "sf3" if soundfont_store::has_soundfont_header(&path) => {
                    let format = if extension == "sf2" { "sf2" } else { "sf3" };
                    result.instruments.push((path, format));
                }
                "sfz" => result.instruments.push((path, "sfz")),
                other if SAMPLE_EXTENSIONS.contains(&other) => result.samples.push(path),
                _ => {}
            }
        }
    }
    result
}

fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Instrument files and samples under `roots`, each file counted once. A
/// library also contributes the sampler assets its stored scan found.
fn sound_summaries(
    roots: &[SoundRoot],
    libraries: &[ContentLibrary],
    progress: &dyn Fn(ScanProgressEvent),
) -> (ContentSummary, ContentSummary) {
    let mut instruments: BTreeMap<PathBuf, &'static str> = BTreeMap::new();
    let mut samples: HashSet<PathBuf> = HashSet::new();
    let mut instrument_folders = Vec::with_capacity(roots.len());
    let mut sample_folders = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        progress(ScanProgressEvent {
            phase: ScanPhase::Sounds,
            done: index,
            total: roots.len(),
            current: Some(root.path.display().to_string()),
        });
        let mut walked = walk_sounds(&root.path);
        if let Some(id) = &root.library_id
            && let Some(library) = libraries.iter().find(|library| &library.id == id)
        {
            walked.instruments.extend(
                library
                    .instruments
                    .iter()
                    .filter(|item| item.format == "sampler_asset")
                    .map(|item| (PathBuf::from(&item.path), "sampler_asset")),
            );
        }
        let folder = |count| SearchedFolder {
            path: root.path.display().to_string(),
            origin: root.origin,
            exists: root.path.exists(),
            count,
            library_id: root.library_id.clone(),
        };
        instrument_folders.push(folder(walked.instruments.len()));
        sample_folders.push(folder(walked.samples.len()));
        for (path, format) in walked.instruments {
            instruments.entry(canonical(path)).or_insert(format);
        }
        samples.extend(walked.samples.into_iter().map(canonical));
    }
    progress(ScanProgressEvent {
        phase: ScanPhase::Sounds,
        done: roots.len(),
        total: roots.len(),
        current: None,
    });

    let items: Vec<FoundItem> = instruments
        .iter()
        .map(|(path, format)| FoundItem {
            name: file_stem(path),
            detail: path
                .parent()
                .map(|parent| parent.display().to_string())
                .unwrap_or_default(),
            format: (*format).to_string(),
            path: path.display().to_string(),
            readable: true,
        })
        .collect();
    let (items, truncated) = bounded(items);
    let instruments = ContentSummary {
        total: instruments.len(),
        formats: format_counts(instruments.values().copied(), &INSTRUMENT_FORMATS),
        folders: instrument_folders,
        items,
        truncated,
    };
    let samples = ContentSummary {
        total: samples.len(),
        formats: Vec::new(),
        folders: sample_folders,
        items: Vec::new(),
        truncated: false,
    };
    (instruments, samples)
}

fn libraries() -> Vec<ContentLibrary> {
    content_library::load(&content_library::default_catalogue_path())
        .map(|catalogue| catalogue.libraries)
        .unwrap_or_default()
}

/// Walk every searched folder now. Only directory entries and file headers are
/// read; no plugin binary is loaded, and the plugin list is whatever the
/// inventory last published.
pub fn summary_with(progress: &dyn Fn(ScanProgressEvent)) -> LocalContentSummary {
    let roots = sound_roots(
        soundfont_store::search_roots(&soundfont_store::dir()),
        sample_folders(),
    );
    let (instruments, samples) = sound_summaries(&roots, &libraries(), progress);
    LocalContentSummary {
        plugins: plugin_summary(
            &oc_host::plugin_search_folders(),
            oc_host::published_plugins(),
        ),
        instruments,
        samples,
        scanning: crate::plugin_inventory::refreshing(),
    }
}

pub fn summary() -> LocalContentSummary {
    summary_with(&|_| {})
}

fn plugin_progress(progress: Option<oc_host::scanner::ScanProgress>) -> ScanProgressEvent {
    let progress = progress.unwrap_or_default();
    ScanProgressEvent {
        phase: ScanPhase::Plugins,
        done: progress.done,
        total: progress.total,
        current: progress.current,
    }
}

/// Report the inventory scan's progress until `running` turns false.
fn follow_plugin_scan(running: &dyn Fn() -> bool, progress: &dyn Fn(ScanProgressEvent)) {
    while running() {
        progress(plugin_progress(oc_host::scanner::scan_progress()));
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Bring the plugin inventory up to date, then count everything. `rescan`
/// inspects every bundle again, including quarantined ones.
fn scan_with(rescan: bool, progress: &dyn Fn(ScanProgressEvent)) -> LocalContentSummary {
    progress(plugin_progress(None));
    if rescan {
        // One pass at a time: a background refresh finishes first.
        follow_plugin_scan(&crate::plugin_inventory::refreshing, progress);
        let pass = std::thread::spawn(crate::plugin_inventory::rescan);
        follow_plugin_scan(&|| !pass.is_finished(), progress);
        let _ = pass.join();
    } else {
        crate::plugin_inventory::refresh_if_stale();
        follow_plugin_scan(&crate::plugin_inventory::refreshing, progress);
    }
    summary_with(progress)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
}

/// Count what the searched folders hold, off the main thread.
#[tauri::command]
pub async fn local_content_summary() -> Result<LocalContentSummary, String> {
    blocking(|| Ok(summary())).await
}

/// Scan plugins and sounds, emitting [`PROGRESS_EVENT`] while it runs.
#[tauri::command]
pub async fn local_content_scan(
    app: tauri::AppHandle,
    rescan: Option<bool>,
) -> Result<LocalContentSummary, String> {
    blocking(move || {
        Ok(scan_with(rescan.unwrap_or(false), &|event| {
            let _ = app.emit(PROGRESS_EVENT, event);
        }))
    })
    .await
}

/// Search `path` for plugins from now on, then rescan in the background.
#[tauri::command]
pub async fn plugin_folder_add(path: String) -> Result<LocalContentSummary, String> {
    blocking(move || {
        oc_host::plugin_folders::add(Path::new(&path)).map_err(|error| error.to_string())?;
        crate::plugin_inventory::refresh_if_stale();
        Ok(summary())
    })
    .await
}

/// Stop searching `path` for plugins. Files on disk are never touched.
#[tauri::command]
pub async fn plugin_folder_remove(path: String) -> Result<LocalContentSummary, String> {
    blocking(move || {
        oc_host::plugin_folders::remove(Path::new(&path)).map_err(|error| error.to_string())?;
        crate::plugin_inventory::refresh_if_stale();
        Ok(summary())
    })
    .await
}

/// Register `path` as a content library so its instruments are found.
#[tauri::command]
pub async fn sound_folder_add(path: String) -> Result<LocalContentSummary, String> {
    blocking(move || {
        content_library::register(
            &content_library::default_catalogue_path(),
            Path::new(&path),
            None,
        )?;
        Ok(summary())
    })
    .await
}

/// Unregister a content library by id. Files on disk are never touched.
#[tauri::command]
pub async fn sound_folder_remove(id: String) -> Result<LocalContentSummary, String> {
    blocking(move || {
        content_library::remove(&content_library::default_catalogue_path(), &id)?;
        Ok(summary())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = crate::private_fs::config_dir()
                .join(format!("local-content-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BANK: &[u8] = b"RIFF\0\0\0\0sfbkrest";

    fn folder(path: &Path, origin: oc_host::FolderOrigin) -> oc_host::PluginFolder {
        oc_host::PluginFolder {
            path: path.display().to_string(),
            origin,
        }
    }

    fn info(path: &Path, name: &str, format: oc_host::PluginFormat) -> oc_host::PluginInfo {
        let mut info = oc_host::builtin::builtin_plugin_list().remove(0);
        info.id = format!("{}#{name}", path.display());
        info.name = name.into();
        info.vendor = "Vendor".into();
        info.path = path.display().to_string();
        info.format = format;
        info
    }

    #[test]
    fn before_a_scan_each_plugin_bundle_is_one_item() {
        let scratch = Scratch::new("plugins");
        let standard = scratch.dir("VST3");
        scratch.dir("VST3/Synth.vst3/Contents");
        scratch.dir("VST3/Vendor/Reverb.vst3");
        scratch.file("VST3/readme.txt", b"not a plugin");
        let user = scratch.dir("Mine");
        scratch.file("Mine/Delay.clap", b"clap");
        scratch.dir("Mine/Comp.lv2");

        let summary = plugin_summary(
            &[
                folder(&standard, oc_host::FolderOrigin::Standard),
                folder(&user, oc_host::FolderOrigin::User),
                // An overlapping folder names the same bundles again.
                folder(&standard, oc_host::FolderOrigin::HostSettings),
                folder(&scratch.0.join("Missing"), oc_host::FolderOrigin::Standard),
            ],
            None,
        );

        assert_eq!(summary.total, 4);
        let formats: Vec<(&str, usize)> = summary
            .formats
            .iter()
            .map(|row| (row.format.as_str(), row.count))
            .collect();
        assert_eq!(formats, vec![("vst3", 2), ("clap", 1), ("lv2", 1)]);
        let names: Vec<&str> = summary
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(names, vec!["Comp", "Delay", "Reverb", "Synth"]);
        assert_eq!(summary.folders[0].count, 2);
        assert_eq!(summary.folders[1].origin, FolderOrigin::User);
        assert_eq!(summary.folders[2].origin, FolderOrigin::HostSettings);
        assert!(!summary.folders[3].exists);
    }

    #[test]
    fn after_a_scan_plugins_are_listed_and_unread_bundles_are_flagged() {
        let scratch = Scratch::new("inventory");
        let root = scratch.dir("CLAP");
        let suite = scratch.file("CLAP/Suite.clap", b"clap");
        scratch.file("CLAP/Broken.clap", b"clap");
        let inventory = vec![
            info(&suite, "Suite Synth", oc_host::PluginFormat::Clap),
            info(&suite, "Suite Echo", oc_host::PluginFormat::Clap),
            // The same class reported twice is listed once.
            info(&suite, "Suite Echo", oc_host::PluginFormat::Clap),
            oc_host::builtin::builtin_plugin_list().remove(0),
        ];

        let summary = plugin_summary(
            &[folder(&root, oc_host::FolderOrigin::Standard)],
            Some(&inventory),
        );

        assert_eq!(summary.total, 3);
        let rows: Vec<(&str, bool)> = summary
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.readable))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Broken", false),
                ("Suite Echo", true),
                ("Suite Synth", true)
            ]
        );
        assert_eq!(summary.items[1].detail, "Vendor");
    }

    #[test]
    fn sound_folders_count_instruments_and_samples_once_each() {
        let scratch = Scratch::new("sounds");
        let fonts = scratch.dir("SoundFonts");
        scratch.file("SoundFonts/Piano.sf2", BANK);
        scratch.file("SoundFonts/fake.sf2", b"not a soundfont");
        scratch.file("SoundFonts/Strings/Strings.sfz", b"<region>");
        let packs = scratch.dir("Samples");
        scratch.file("Samples/Kit/kick.wav", b"RIFF");
        scratch.file("Samples/Kit/snare.FLAC", b"fLaC");
        scratch.file("Samples/Kit/notes.txt", b"text");
        scratch.file("Samples/.cache/hidden.wav", b"RIFF");
        let library_root = scratch.dir("Library");
        scratch.file("Library/Lead.sf3", BANK);
        let library = ContentLibrary {
            id: "lib-1".into(),
            name: "Library".into(),
            path: library_root.display().to_string(),
            kind: "directory".into(),
            added_at: 0,
            scanned_at: 0,
            missing: false,
            instruments: vec![content_library::ContentInstrument {
                name: "Choir".into(),
                path: library_root.join("Choir/asset.json").display().to_string(),
                format: "sampler_asset".into(),
                size_bytes: 1,
            }],
        };
        let roots = sound_roots(
            vec![
                SearchRoot {
                    path: fonts.clone(),
                    origin: RootOrigin::Standard,
                },
                SearchRoot {
                    path: library_root.clone(),
                    origin: RootOrigin::Library("lib-1".into()),
                },
                SearchRoot {
                    path: scratch.dir("App"),
                    origin: RootOrigin::Bundled,
                },
            ],
            // The SoundFont folder again, as a sample folder: still one root.
            vec![packs.clone(), fonts.clone()],
        );
        assert_eq!(roots.len(), 3, "bundled banks and repeats are dropped");

        let heard = Mutex::new(Vec::new());
        let (instruments, samples) = sound_summaries(&roots, &[library], &|event| {
            heard.lock().unwrap().push((event.done, event.total));
        });

        assert_eq!(instruments.total, 4);
        let formats: Vec<(&str, usize)> = instruments
            .formats
            .iter()
            .map(|row| (row.format.as_str(), row.count))
            .collect();
        assert_eq!(
            formats,
            vec![("sf2", 1), ("sf3", 1), ("sfz", 1), ("sampler_asset", 1)]
        );
        assert_eq!(
            samples.total, 2,
            "hidden folders and other files are skipped"
        );
        assert_eq!(samples.folders[2].count, 2);
        assert_eq!(instruments.folders[1].library_id.as_deref(), Some("lib-1"));
        assert_eq!(instruments.folders[1].origin, FolderOrigin::User);
        assert_eq!(instruments.folders[2].origin, FolderOrigin::Convention);
        assert_eq!(
            heard.into_inner().unwrap(),
            vec![(0, 3), (1, 3), (2, 3), (3, 3)]
        );
    }

    #[test]
    fn the_managed_sounds_folder_is_a_managed_search_root() {
        let scratch = Scratch::new("managed");
        let managed = scratch.dir("Sounds");
        let roots = sound_roots(
            vec![SearchRoot {
                path: managed.clone(),
                origin: RootOrigin::Managed,
            }],
            Vec::new(),
        );
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, managed);
        assert_eq!(roots[0].origin, FolderOrigin::Managed);
        assert_eq!(
            serde_json::to_value(FolderOrigin::Managed).unwrap(),
            serde_json::json!("managed")
        );
    }

    #[test]
    fn a_huge_sound_folder_costs_a_bounded_walk() {
        let scratch = Scratch::new("bounded");
        let root = scratch.dir("Big");
        for index in 0..40 {
            scratch.file(&format!("Big/{index}.wav"), b"");
        }
        assert_eq!(walk_sounds_within(&root, 25).samples.len(), 25);
    }

    #[test]
    fn plugin_progress_without_a_running_pass_is_an_unknown_size() {
        assert_eq!(
            plugin_progress(None),
            ScanProgressEvent {
                phase: ScanPhase::Plugins,
                done: 0,
                total: 0,
                current: None,
            }
        );
        let event = plugin_progress(Some(oc_host::scanner::ScanProgress {
            done: 3,
            total: 8,
            current: Some("Synth.vst3".into()),
        }));
        assert_eq!((event.done, event.total), (3, 8));
        assert_eq!(event.current.as_deref(), Some("Synth.vst3"));
    }

    /// Counts and timings on this machine, without starting a plugin scan:
    ///
    /// ```sh
    /// cargo test -p oc-gui --lib review_local_content_on_this_machine -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn review_local_content_on_this_machine() {
        let started = std::time::Instant::now();
        let folders = oc_host::plugin_search_folders();
        let plugins = plugin_summary(&folders, None);
        let plugin_time = started.elapsed();
        let started = std::time::Instant::now();
        let roots = sound_roots(
            soundfont_store::search_roots(&soundfont_store::dir()),
            sample_folders(),
        );
        let (instruments, samples) = sound_summaries(&roots, &libraries(), &|_| {});
        let sound_time = started.elapsed();
        for folder in folders
            .iter()
            .filter(|folder| Path::new(&folder.path).is_dir())
        {
            println!("plugin folder {:?} {}", folder.origin, folder.path);
        }
        for (index, root) in roots
            .iter()
            .enumerate()
            .filter(|(_, root)| root.path.is_dir())
        {
            println!(
                "sound folder {:?} {} instruments {} samples {}",
                root.origin,
                root.path.display(),
                instruments.folders[index].count,
                samples.folders[index].count
            );
        }
        println!(
            "plugin bundles {} in {plugin_time:?}; instruments {} samples {} in {sound_time:?}",
            plugins.total, instruments.total, samples.total
        );
    }
}
