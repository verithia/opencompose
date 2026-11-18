//! What this machine already has: plugin bundles and instrument files in the
//! folders the app searches, counted without loading any of them.
//!
//! The first-run guide shows this summary and lets the user add a plugin folder
//! or a sound folder. Plugin folders are owned by `oc_host::plugin_folders`
//! and sound folders by the content-library catalogue; this module only reads
//! both and adapts the result for the UI.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::content_library::{self, ContentLibrary};
use crate::soundfont_store::{self, RootOrigin, SearchRoot};

/// Why a folder is searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderOrigin {
    Standard,
    User,
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

/// One kind of content: distinct items, their formats, and the folders searched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentSummary {
    pub total: usize,
    pub formats: Vec<FormatCount>,
    pub folders: Vec<SearchedFolder>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalContentSummary {
    /// Plugin bundles; a bundle that exposes several plugins counts once.
    pub plugins: ContentSummary,
    /// SoundFont, SFZ and sampler-asset files.
    pub sounds: ContentSummary,
    /// A background scan is reading plugin details now.
    pub scanning: bool,
}

/// Format labels in the order the summary lists them.
const PLUGIN_FORMATS: [&str; 5] = ["vst3", "clap", "au", "lv2", "vst2"];
const SOUND_FORMATS: [&str; 4] = ["sf2", "sf3", "sfz", "sampler_asset"];

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
fn format_counts(found: &BTreeMap<PathBuf, &'static str>, order: &[&str]) -> Vec<FormatCount> {
    order
        .iter()
        .map(|format| FormatCount {
            format: (*format).to_string(),
            count: found.values().filter(|value| *value == format).count(),
        })
        .filter(|row| row.count > 0)
        .collect()
}

fn plugin_summary(folders: &[oc_host::PluginFolder]) -> ContentSummary {
    let mut found = BTreeMap::new();
    let mut searched = Vec::with_capacity(folders.len());
    for folder in folders {
        let path = Path::new(&folder.path);
        let bundles = oc_host::discover_plugin_files_in(path);
        let mut count = 0;
        for (bundle, format) in bundles {
            if let Some(format) = plugin_format(format) {
                count += 1;
                found.entry(bundle).or_insert(format);
            }
        }
        searched.push(SearchedFolder {
            path: folder.path.clone(),
            origin: match folder.origin {
                oc_host::FolderOrigin::Standard => FolderOrigin::Standard,
                oc_host::FolderOrigin::User => FolderOrigin::User,
            },
            exists: path.is_dir(),
            count,
            library_id: None,
        });
    }
    ContentSummary {
        total: found.len(),
        formats: format_counts(&found, &PLUGIN_FORMATS),
        folders: searched,
    }
}

fn sound_format(format: &str) -> Option<&'static str> {
    SOUND_FORMATS
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(format))
}

/// A library answers from its stored scan, which also lists SFZ files and
/// sampler assets; every other root is searched for SoundFonts. Banks that ship
/// with the app are not the machine's, so they are left out.
fn sound_summary(roots: &[SearchRoot], libraries: &[ContentLibrary]) -> ContentSummary {
    let mut found = BTreeMap::new();
    let mut searched: Vec<SearchedFolder> = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        if root.origin == RootOrigin::Bundled || !seen.insert(root.path.clone()) {
            continue;
        }
        let files: Vec<(PathBuf, &'static str)> = match &root.origin {
            RootOrigin::Library(id) => libraries
                .iter()
                .find(|library| &library.id == id)
                .map(|library| {
                    library
                        .instruments
                        .iter()
                        .filter_map(|item| {
                            sound_format(&item.format)
                                .map(|format| (PathBuf::from(&item.path), format))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => soundfont_store::discover_in(&root.path)
                .into_iter()
                .filter_map(|file| {
                    sound_format(&file.format).map(|format| (PathBuf::from(file.path), format))
                })
                .collect(),
        };
        let count = files.len();
        for (path, format) in files {
            found.entry(path).or_insert(format);
        }
        let library_id = match &root.origin {
            RootOrigin::Library(id) => Some(id.clone()),
            _ => None,
        };
        searched.push(SearchedFolder {
            path: root.path.display().to_string(),
            origin: if library_id.is_some() {
                FolderOrigin::User
            } else {
                FolderOrigin::Standard
            },
            exists: root.path.exists(),
            count,
            library_id,
        });
    }
    ContentSummary {
        total: found.len(),
        formats: format_counts(&found, &SOUND_FORMATS),
        folders: searched,
    }
}

/// Walk every searched folder now. Only directory entries and file headers are
/// read; no plugin binary is loaded.
pub fn summary() -> LocalContentSummary {
    let libraries = content_library::load(&content_library::default_catalogue_path())
        .map(|catalogue| catalogue.libraries)
        .unwrap_or_default();
    LocalContentSummary {
        plugins: plugin_summary(&oc_host::plugin_search_folders()),
        sounds: sound_summary(
            &soundfont_store::search_roots(&soundfont_store::dir()),
            &libraries,
        ),
        scanning: crate::plugin_inventory::refreshing(),
    }
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

    fn folder(path: &Path, origin: oc_host::FolderOrigin) -> oc_host::PluginFolder {
        oc_host::PluginFolder {
            path: path.display().to_string(),
            origin,
        }
    }

    #[test]
    fn plugin_bundles_are_counted_once_per_bundle_and_format() {
        let scratch = Scratch::new("plugins");
        let standard = scratch.dir("VST3");
        scratch.dir("VST3/Synth.vst3/Contents");
        scratch.dir("VST3/Vendor/Reverb.vst3");
        scratch.file("VST3/readme.txt", b"not a plugin");
        let user = scratch.dir("Mine");
        scratch.file("Mine/Delay.clap", b"clap");
        scratch.dir("Mine/Comp.lv2");

        let summary = plugin_summary(&[
            folder(&standard, oc_host::FolderOrigin::Standard),
            folder(&user, oc_host::FolderOrigin::User),
            // An overlapping folder names the same bundles again.
            folder(&standard, oc_host::FolderOrigin::Standard),
            folder(&scratch.0.join("Missing"), oc_host::FolderOrigin::Standard),
        ]);

        assert_eq!(summary.total, 4);
        assert_eq!(
            summary.formats,
            vec![
                FormatCount {
                    format: "vst3".into(),
                    count: 2
                },
                FormatCount {
                    format: "clap".into(),
                    count: 1
                },
                FormatCount {
                    format: "lv2".into(),
                    count: 1
                },
            ]
        );
        assert_eq!(summary.folders[0].count, 2);
        assert_eq!(summary.folders[1].origin, FolderOrigin::User);
        assert!(!summary.folders[3].exists);
    }

    #[test]
    fn sounds_join_soundfonts_and_library_instruments_without_bundled_banks() {
        let scratch = Scratch::new("sounds");
        let bank = b"RIFF\0\0\0\0sfbkrest";
        let standard = scratch.dir("SoundFonts");
        let piano = scratch.file("SoundFonts/Piano.sf2", bank);
        scratch.file("SoundFonts/fake.sf2", b"not a soundfont");
        let bundled = scratch.dir("App");
        scratch.file("App/General.sf3", bank);
        let library_root = scratch.dir("Library");
        let library = ContentLibrary {
            id: "lib-1".into(),
            name: "Library".into(),
            path: library_root.display().to_string(),
            kind: "directory".into(),
            added_at: 0,
            scanned_at: 0,
            missing: false,
            instruments: vec![
                content_library::ContentInstrument {
                    name: "Strings".into(),
                    path: library_root.join("Strings.sfz").display().to_string(),
                    format: "sfz".into(),
                    size_bytes: 1,
                },
                // The same file found through a standard root counts once.
                content_library::ContentInstrument {
                    name: "Piano".into(),
                    path: piano.canonicalize().unwrap().display().to_string(),
                    format: "sf2".into(),
                    size_bytes: 1,
                },
            ],
        };
        let roots = vec![
            SearchRoot {
                path: standard.clone(),
                origin: RootOrigin::Standard,
            },
            SearchRoot {
                path: library_root,
                origin: RootOrigin::Library("lib-1".into()),
            },
            SearchRoot {
                path: bundled,
                origin: RootOrigin::Bundled,
            },
        ];

        let summary = sound_summary(&roots, &[library]);

        assert_eq!(summary.total, 2);
        assert_eq!(
            summary.formats,
            vec![
                FormatCount {
                    format: "sf2".into(),
                    count: 1
                },
                FormatCount {
                    format: "sfz".into(),
                    count: 1
                },
            ]
        );
        assert_eq!(summary.folders.len(), 2, "bundled banks are not listed");
        assert_eq!(summary.folders[1].library_id.as_deref(), Some("lib-1"));
        assert_eq!(summary.folders[1].origin, FolderOrigin::User);
    }
}
