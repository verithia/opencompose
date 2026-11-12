//! Bounded, fail-closed collection of project-relative resources for Save As.
//! Plugin binaries and opaque plugin-internal external libraries are not collected.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use oc_dsl::ast::Project;
use oc_dsl::media::{self, MediaDeclaration, MediaKind};

/// Per-file and per-project bounds, checked before anything is written.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_FILES: usize = 1024;

/// Files a Save As copied, and what it left in the source directory.
#[derive(Debug, Default)]
pub struct Collected {
    /// Newly created files, removed again if publishing the DSL fails.
    pub created: Vec<PathBuf>,
    /// Resources that stayed behind, one line each, with the reason.
    pub warnings: Vec<String>,
}

/// Publish DSL last, so a failed Save As does not replace a valid saved project.
pub fn save(project: &Project, source: &str, old: Option<&Path>, new: &Path) -> Result<(), String> {
    save_with_assets(project, source, old, new, None).map(|_| ())
}

/// Save a project and copy its assets.
pub fn save_with_assets(
    project: &Project,
    source: &str,
    old: Option<&Path>,
    new: &Path,
    library: Option<&Path>,
) -> Result<Option<(Project, String)>, String> {
    if std::fs::symlink_metadata(new).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Refusing to save through a project symlink".into());
    }
    let (destination, mut collection) = prepare_collection(project, old, new)?;
    let moved = old
        .and_then(Path::parent)
        .and_then(|parent| parent.canonicalize().ok())
        .as_deref()
        != Some(destination.as_path());
    let rewritten = if moved && let Some(library) = library {
        let paths = collection.add_library_patches(project, library)?;
        if paths.is_empty() {
            None
        } else {
            let rewritten = oc_dsl::parser::rewrite_synth_state_paths(source, &paths)
                .map_err(|error| error.to_string())?;
            if rewritten == source {
                return Err("Cannot locate the collected patch paths in the project text".into());
            }
            let mut next = project.clone();
            let rewrite = |params: &mut Vec<(String, oc_dsl::ast::ParamValue)>| {
                for (key, value) in params {
                    if key == "open_synth_state_file"
                        && let oc_dsl::ast::ParamValue::String(path) = value
                        && let Some(relative) = paths.get(path)
                    {
                        *path = relative.clone();
                    }
                }
            };
            for preset in &mut next.presets {
                rewrite(&mut preset.params);
            }
            for track in &mut next.tracks {
                if let Some(params) = &mut track.engine_params {
                    rewrite(params);
                }
            }
            Some((next, rewritten))
        }
    } else {
        None
    };
    let collected = publish_collection(collection, &destination, new)?;
    let source = rewritten.as_ref().map_or(source, |(_, text)| text);
    let parent = new
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temporary = parent.join(format!(".ocp-save-{}-{stamp}.tmp", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(source.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, new).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            for warning in &collected.warnings {
                log::warn!("save as: {warning}");
            }
        }
        Err(_) => {
            let _ = std::fs::remove_file(&temporary);
            rollback(&collected.created);
        }
    }
    result.map(|()| rewritten)
}

fn relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(format!(
            "Save As requires a confined relative resource: {}",
            path.display()
        ));
    }
    Ok(())
}

fn is_sfz(path: &Path) -> bool {
    path.extension()
        .is_some_and(|value| value.eq_ignore_ascii_case("sfz"))
}

/// The media a Save As carries, gathered before anything is written.
struct Collection {
    root: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
    warnings: Vec<String>,
}

impl Collection {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    fn bytes(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }

    /// Collect one declaration, dispatching on how the engine reads it.
    fn declare(&mut self, declaration: &MediaDeclaration) -> Result<(), String> {
        let path = PathBuf::from(&declaration.path);
        let kind = declaration.kind;
        if declaration.optional && !self.present(&path) {
            self.warnings.push(format!(
                "{}: optional sample '{}' is not on disk, so it is not copied",
                declaration.owner, declaration.path
            ));
            return Ok(());
        }
        match kind {
            MediaKind::Directory => self.add_directory(&path, &declaration.owner),
            MediaKind::Document => {
                self.add(&path, &declaration.owner)?;
                // An absolute document keeps its own meaning; only a
                // project-relative one carries media into the new directory.
                if path.is_absolute() {
                    return Ok(());
                }
                let Some(bytes) = self.files.get(&path).cloned() else {
                    return Ok(());
                };
                let document: oc_synth::audio_region::RegionDocument =
                    serde_json::from_slice(&bytes)
                        .map_err(|e| format!("Region document {}: {e}", path.display()))?;
                for referenced in document.media_files()? {
                    self.add(
                        &path.parent().unwrap_or(Path::new("")).join(referenced),
                        &declaration.owner,
                    )?;
                }
                Ok(())
            }
            MediaKind::SynthDocument => self
                .add_synth_document(&path, &declaration.owner)
                .map(|_| ()),
            MediaKind::File => self.add(&path, &declaration.owner),
        }
    }

    /// Collect a native state and report whether it is portable.
    fn add_synth_document(&mut self, path: &Path, owner: &str) -> Result<bool, String> {
        use oc_synth::synth::document::PatchDocument;
        self.add(path, owner)?;
        let Some(bytes) = self.files.get(path) else {
            return Ok(false);
        };
        let document = PatchDocument::from_json(bytes)
            .map_err(|error| format!("OpenSynth state {}: {error}", path.display()))?;
        if document.version != PatchDocument::PORTABLE_VERSION {
            return Ok(false);
        }
        let parent = path.parent().unwrap_or(Path::new(""));
        for sample in document.sample_paths() {
            self.add(&parent.join(sample), owner)?;
        }
        Ok(true)
    }

    /// True when a declaration's file is on disk where the engine would look.
    fn present(&self, path: &Path) -> bool {
        if path.is_absolute() {
            path.is_file()
        } else {
            self.root.join(path).is_file()
        }
    }

    fn add(&mut self, path: &Path, owner: &str) -> Result<(), String> {
        // Absolute references retain their meaning; they are not advertised as portable.
        if path.is_absolute() {
            if !path.is_file() {
                self.warnings.push(format!(
                    "{owner}: '{}' is not on disk, so it is not copied",
                    path.display()
                ));
            }
            return Ok(());
        }
        relative(path)?;
        if self.files.contains_key(path) {
            return Ok(());
        }
        if is_sfz(path) {
            return self.add_sfz(path, owner);
        }
        self.insert(path, owner)
    }

    /// Copy one file into the collection.
    fn insert(&mut self, path: &Path, owner: &str) -> Result<(), String> {
        let source = self
            .root
            .join(path)
            .canonicalize()
            .map_err(|e| format!("{owner}: cannot read '{}': {e}", path.display()))?;
        if !source.starts_with(&self.root) {
            return Err(format!(
                "{owner}: '{}' escapes the project directory",
                path.display()
            ));
        }
        if !source.is_file() {
            return Err(format!("{owner}: '{}' is not a file", path.display()));
        }
        let size = source.metadata().map_err(|e| e.to_string())?.len();
        if size > MAX_FILE_BYTES
            || self.files.len() >= MAX_FILES
            || self.bytes() + size as usize > MAX_TOTAL_BYTES
        {
            return Err("Save As collection exceeds 64MiB/file or 256MiB/project bound".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&source)
            .map_err(|e| e.to_string())?
            .take(size + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != size {
            return Err("Resource changed during collection".into());
        }
        self.files.insert(path.into(), bytes);
        Ok(())
    }

    /// Copy a directory the engine searches (`sampler.root`), following the same bounds.
    fn add_directory(&mut self, path: &Path, owner: &str) -> Result<(), String> {
        if path.is_absolute() {
            if !path.is_dir() {
                self.warnings.push(format!(
                    "{owner}: '{}' is not on disk, so it is not copied",
                    path.display()
                ));
            }
            return Ok(());
        }
        relative(path)?;
        let source = self
            .root
            .join(path)
            .canonicalize()
            .map_err(|e| format!("{owner}: cannot read '{}': {e}", path.display()))?;
        if !source.starts_with(&self.root) || !source.is_dir() {
            return Err(format!(
                "{owner}: '{}' escapes the project directory",
                path.display()
            ));
        }
        let mut pending = vec![source];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(directory) = pending.pop() {
            if !visited.insert(directory.clone()) {
                continue;
            }
            for entry in std::fs::read_dir(&directory).map_err(|e| format!("{owner}: {e}"))? {
                let entry = entry.map_err(|e| format!("{owner}: {e}"))?;
                let entry = entry.path();
                let Ok(resolved) = entry.canonicalize() else {
                    continue;
                };
                if !resolved.starts_with(&self.root) {
                    self.warnings.push(format!(
                        "{owner}: '{}' resolves outside the project directory, so it is not copied",
                        entry.display()
                    ));
                    continue;
                }
                if resolved.is_dir() {
                    pending.push(resolved);
                } else if resolved.is_file() {
                    let relative = resolved
                        .strip_prefix(&self.root)
                        .map_err(|_| format!("{owner}: '{}' escapes", entry.display()))?
                        .to_path_buf();
                    self.insert(&relative, owner)?;
                }
            }
        }
        Ok(())
    }

    /// Copy an `.sfz` together with the samples it references. A partial copy
    /// would leave the copied instrument pointing at nothing, so the group
    /// either moves whole or stays where it is with a warning.
    fn add_sfz(&mut self, path: &Path, owner: &str) -> Result<(), String> {
        let mut group = Collection::new(&self.root);
        let staged = (|| -> Result<(), String> {
            group.insert(path, owner)?;
            let document = self
                .root
                .join(path)
                .canonicalize()
                .map_err(|e| format!("{owner}: cannot read '{}': {e}", path.display()))?;
            let root = self.root.clone();
            let patch =
                oc_synth::sampler::patch_from_sfz_file(&document, std::slice::from_ref(&root))
                    .map_err(|e| format!("{owner}: {e}"))?;
            for region in &patch.regions {
                let relative = region.sample.strip_prefix(&self.root).map_err(|_| {
                    format!(
                        "{owner}: sample '{}' is outside the project directory",
                        region.sample.display()
                    )
                })?;
                group.insert(relative, owner)?;
            }
            Ok(())
        })();
        let outcome = staged.and_then(|()| self.merge(group));
        if let Err(reason) = outcome {
            self.warnings.push(format!(
                "Save As left '{}' ({owner}) in '{}': {reason}",
                path.display(),
                self.root.display()
            ));
        }
        Ok(())
    }

    fn add_library_patches(
        &mut self,
        project: &Project,
        library: &Path,
    ) -> Result<BTreeMap<String, String>, String> {
        let root =
            std::path::absolute(library.join("assets")).map_err(|error| error.to_string())?;
        let mut paths = BTreeMap::new();
        let mut group = Collection::new(&root);
        let params = project
            .presets
            .iter()
            .map(|preset| preset.params.as_slice())
            .chain(
                project
                    .tracks
                    .iter()
                    .filter_map(|track| track.engine_params.as_deref()),
            )
            .flatten();
        for (key, value) in params {
            let oc_dsl::ast::ParamValue::String(original) = value else {
                continue;
            };
            let path = Path::new(original);
            if key != "open_synth_state_file"
                || path.parent() != Some(&root)
                || paths.contains_key(original)
            {
                continue;
            }
            let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(digest) = file.strip_suffix(".json") else {
                continue;
            };
            if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            if !std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir())
                || !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
            {
                return Err(format!(
                    "Cannot collect OpenSynth library state: {}",
                    path.display()
                ));
            }
            group.root = root.canonicalize().map_err(|error| error.to_string())?;
            if !group
                .add_synth_document(Path::new(file), &format!("OpenSynth library state {file}"))?
            {
                group.files.remove(Path::new(file));
                continue;
            }
            paths.insert(original.clone(), format!("media/patches/library/{file}"));
        }
        group.files = group
            .files
            .into_iter()
            .map(|(path, bytes)| (Path::new("media/patches/library").join(path), bytes))
            .collect();
        for (path, bytes) in &group.files {
            if let Some(existing) = self.files.get(path)
                && existing != bytes
            {
                return Err(format!("Save As resource collision: {}", path.display()));
            }
        }
        group.files.retain(|path, _| !self.files.contains_key(path));
        self.merge(group)?;
        Ok(paths)
    }

    /// Merge a staged group, re-checking the project bounds against everything
    /// collected so far.
    fn merge(&mut self, group: Collection) -> Result<(), String> {
        if self.files.len() + group.files.len() > MAX_FILES
            || self.bytes() + group.bytes() > MAX_TOTAL_BYTES
        {
            return Err("Save As collection exceeds 64MiB/file or 256MiB/project bound".into());
        }
        self.files.extend(group.files);
        self.warnings.extend(group.warnings);
        Ok(())
    }
}

/// Returns the files it created for rollback, plus what it left behind.
#[cfg(test)]
pub fn collect(project: &Project, old: Option<&Path>, new: &Path) -> Result<Collected, String> {
    let (destination, collection) = prepare_collection(project, old, new)?;
    publish_collection(collection, &destination, new)
}

fn prepare_collection(
    project: &Project,
    old: Option<&Path>,
    new: &Path,
) -> Result<(PathBuf, Collection), String> {
    let destination = new
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| format!("Save directory: {e}"))?;
    let source = old
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut collection = Collection::new(&source);
    if source != destination {
        for declaration in media::declarations(project) {
            collection.declare(&declaration)?;
        }
    }
    Ok((destination, collection))
}

fn publish_collection(
    collection: Collection,
    destination: &Path,
    new: &Path,
) -> Result<Collected, String> {
    let Collection {
        files, warnings, ..
    } = collection;
    // Validate every target before any writes. Never overwrite a resource or follow a symlink.
    for (path, bytes) in &files {
        let target = destination.join(path);
        if Some(path.file_name().unwrap()) == new.file_name() && path.components().count() == 1 {
            return Err("Project filename collides with a resource".into());
        }
        let mut cursor = destination.to_path_buf();
        for part in path.components() {
            cursor.push(part);
            if let Ok(meta) = std::fs::symlink_metadata(&cursor)
                && meta.file_type().is_symlink()
            {
                return Err("Save destination contains a symlink".into());
            }
        }
        if target.exists()
            && (target.metadata().map_err(|e| e.to_string())?.len() != bytes.len() as u64
                || std::fs::read(&target).map_err(|e| e.to_string())? != *bytes)
        {
            return Err(format!("Save As resource collision: {}", target.display()));
        }
    }
    let mut created = vec![];
    let result = (|| -> Result<(), String> {
        for (path, bytes) in files {
            let target = destination.join(path);
            if target.exists() {
                continue;
            }
            std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| e.to_string())?;
            created.push(target);
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        rollback(&created);
        return Err(error);
    }
    Ok(Collected { created, warnings })
}

pub fn rollback(files: &[PathBuf]) {
    for path in files {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ocp-save-{}-{}-{}",
                std::process::id(),
                FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir_all(root.join("old")).unwrap();
            std::fs::create_dir_all(root.join("new")).unwrap();
            Self(root)
        }
        fn old(&self) -> PathBuf {
            self.0.join("old")
        }
        fn new_dir(&self) -> PathBuf {
            self.0.join("new")
        }
        fn source(&self) -> PathBuf {
            self.old().join("song.ocp")
        }
        fn target(&self) -> PathBuf {
            self.new_dir().join("song.ocp")
        }
        fn run(&self, project: &Project) -> Result<Collected, String> {
            collect(project, Some(&self.source()), &self.target())
        }
        /// Save As, then reopen the copy from its own directory.
        fn save_as(&self, project: &Project) -> Project {
            save(
                project,
                &oc_dsl::serialize(project),
                Some(&self.source()),
                &self.target(),
            )
            .unwrap();
            oc_dsl::parse(&std::fs::read_to_string(self.target()).unwrap()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn concurrent_fixtures_have_independent_directories() {
        let handles: Vec<_> = (0..32).map(|_| std::thread::spawn(Fixture::new)).collect();
        let fixtures: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let paths: std::collections::HashSet<_> = fixtures.iter().map(|f| &f.0).collect();
        assert_eq!(paths.len(), fixtures.len());
        assert!(fixtures.iter().all(|f| f.0.join("old").is_dir()));
    }
    fn project(key: &str, path: &str) -> Project {
        oc_dsl::parse(&format!("@project \"Save\"\n  tempo: 120\n  time_signature: 4/4\n@preset voice\n  source: clap\n  plugin: \"test\"\n  params: {{{key}: \"{path}\"}}\n@track audio | preset: voice\n")).unwrap()
    }
    fn render(project: &Project, root: PathBuf) -> Vec<f32> {
        let mut cache = oc_engine::render_cache::RenderCache::new();
        cache.set_resource_root(Some(root));
        cache
            .render_full(
                project,
                Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new(),
            )
            .unwrap()
    }
    fn write_wavetable(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..2048 {
            writer
                .write_sample((std::f32::consts::TAU * frame as f32 / 2048.0).sin())
                .unwrap();
        }
        writer.finalize().unwrap();
    }
    fn write_sample(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for frame in 0..8000 {
            writer
                .write_sample((std::f32::consts::TAU * 220.0 * frame as f32 / 8000.0).sin() * 0.5)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn collects_declared_state_only_and_reuses_identical_files() {
        let f = Fixture::new();
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        std::fs::write(f.old().join("unrelated.bin"), b"do not copy").unwrap();
        let p = project("clap_state_file", "voice.bin");
        assert_eq!(f.run(&p).unwrap().created.len(), 1);
        assert_eq!(
            std::fs::read(f.new_dir().join("voice.bin")).unwrap(),
            b"state"
        );
        assert!(!f.new_dir().join("unrelated.bin").exists());
        assert!(f.run(&p).unwrap().created.is_empty());
    }

    #[test]
    fn collision_missing_and_escape_resources_fail_without_copying() {
        let f = Fixture::new();
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        std::fs::write(f.new_dir().join("voice.bin"), b"keep").unwrap();
        assert!(
            f.run(&project("state_file", "voice.bin"))
                .unwrap_err()
                .contains("collision")
        );
        assert_eq!(
            std::fs::read(f.new_dir().join("voice.bin")).unwrap(),
            b"keep"
        );
        for (key, path) in [
            ("state_file", "missing.bin"),
            ("state_file", "../outside.bin"),
        ] {
            assert!(f.run(&project(key, path)).is_err());
        }
        assert_eq!(std::fs::read_dir(f.new_dir()).unwrap().count(), 1);
    }

    #[test]
    fn a_parameter_the_engine_never_reads_is_skipped_not_refused() {
        let f = Fixture::new();
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        let collected = f.run(&project("unreviewed_file", "voice.bin")).unwrap();
        assert!(collected.created.is_empty());
        assert!(collected.warnings.is_empty());
        assert!(!f.new_dir().join("voice.bin").exists());
    }

    #[test]
    fn a_plugin_presets_stale_file_is_skipped_and_kept() {
        let f = Fixture::new();
        let p = oc_dsl::parse(
            "@project \"Stale\"\n  tempo: 120\n@preset lead\n  source: clap\n  plugin: \"com.acme.lead\"\n  file: \"gone.wav\"\n@track lead | preset: lead\n",
        )
        .unwrap();
        assert!(f.run(&p).unwrap().created.is_empty());
        let reopened = f.save_as(&p);
        assert_eq!(reopened.presets[0].file.as_deref(), Some("gone.wav"));
    }

    #[test]
    fn collects_region_media_relative_to_document_and_rolls_back_only_new_files() {
        let f = Fixture::new();
        std::fs::create_dir(f.old().join("regions")).unwrap();
        std::fs::write(f.old().join("regions/sample.wav"), b"audio").unwrap();
        let doc = serde_json::json!({"sample_rate":8000,"regions":[{"file":"sample.wav",
            "region":{"timeline_start":0,"source_start":0,"length":400,"gain":1.0,"envelope":[]}}]});
        std::fs::write(f.old().join("regions/state.json"), doc.to_string()).unwrap();
        let p = project("audio_region_state_file", "regions/state.json");
        let collected = f.run(&p).unwrap();
        assert_eq!(collected.created.len(), 2);
        assert_eq!(
            std::fs::read(f.new_dir().join("regions/sample.wav")).unwrap(),
            b"audio"
        );
        rollback(&collected.created);
        assert!(!f.new_dir().join("regions/sample.wav").exists());
        assert!(f.old().join("regions/sample.wav").exists());
    }

    #[cfg(unix)]
    #[test]
    fn destination_symlinks_are_rejected() {
        let f = Fixture::new();
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        std::os::unix::fs::symlink(f.old().join("voice.bin"), f.new_dir().join("voice.bin"))
            .unwrap();
        assert!(
            f.run(&project("state_file", "voice.bin"))
                .unwrap_err()
                .contains("symlink")
        );
    }

    #[test]
    fn failed_project_publication_rolls_back_resources_and_success_reopens() {
        let f = Fixture::new();
        let p = project("state_file", "voice.bin");
        let source = oc_dsl::serialize(&p);
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        let old = f.source();
        let new = f.target();
        std::fs::create_dir(&new).unwrap();
        assert!(save(&p, &source, Some(&old), &new).is_err());
        assert!(!f.new_dir().join("voice.bin").exists());
        assert!(new.is_dir());
        std::fs::remove_dir(&new).unwrap();
        save(&p, &source, Some(&old), &new).unwrap();
        assert_eq!(
            oc_dsl::parse(&std::fs::read_to_string(&new).unwrap()).unwrap(),
            p
        );
        assert_eq!(
            std::fs::read(f.new_dir().join("voice.bin")).unwrap(),
            b"state"
        );
    }

    #[test]
    fn saved_native_project_renders_identically_after_original_resources_are_removed() {
        use oc_synth::synth::{SynthPatch, document::PatchDocument};
        let f = Fixture::new();
        let document = PatchDocument {
            version: 1,
            name: "Save As".into(),
            author: "OpenCompose".into(),
            category: "test".into(),
            use_policy: "diagnostic_only".into(),
            patch: SynthPatch::default(),
        };
        std::fs::write(f.old().join("voice.json"), document.to_json().unwrap()).unwrap();
        let example = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../examples/opensynth_showcase.ocp");
        let mut p = oc_dsl::parse(&std::fs::read_to_string(example).unwrap()).unwrap();
        p.tracks.truncate(1);
        let preset = p.tracks[0].preset.as_ref().unwrap();
        p.presets.retain(|v| &v.name == preset);
        p.presets[0].params = vec![(
            "open_synth_state_file".into(),
            oc_dsl::ast::ParamValue::String("voice.json".into()),
        )];
        let before = render(&p, f.old());
        assert!(before.iter().any(|v| v.abs() > 0.001));
        save(&p, &oc_dsl::serialize(&p), Some(&f.source()), &f.target()).unwrap();
        std::fs::remove_file(f.old().join("voice.json")).unwrap();
        let reopened = oc_dsl::parse(&std::fs::read_to_string(f.target()).unwrap()).unwrap();
        assert_eq!(before, render(&reopened, f.new_dir()));
    }

    #[test]
    fn save_as_collects_portable_native_state_and_its_sibling_sample() {
        use oc_synth::sampler::SamplerPatch;
        use oc_synth::synth::{SynthPatch, document::PatchDocument};

        let f = Fixture::new();
        let media = f.old().join("media");
        std::fs::create_dir_all(&media).unwrap();
        let audio: Vec<_> = (0..4_000)
            .map(|index| (index as f32 * 0.32).sin() * 0.3)
            .collect();
        let sample = oc_engine::wav::mono_wav_bytes(&audio, 8_000).unwrap();
        std::fs::write(media.join("tone.wav"), &sample).unwrap();
        let doc = PatchDocument {
            version: PatchDocument::PORTABLE_VERSION,
            name: "Sampled voice".into(),
            author: "OpenCompose".into(),
            category: "Test".into(),
            use_policy: "Fixture".into(),
            patch: SynthPatch {
                oscillator_a_level: 0.0,
                sampler: Some(SamplerPatch {
                    regions: vec![
                        serde_json::from_value(serde_json::json!({"sample":"tone.wav"})).unwrap(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        std::fs::write(media.join("voice.json"), doc.to_json().unwrap()).unwrap();
        let project = oc_dsl::parse(
            "@project \"Portable voice\"\n  tempo: 120\n@preset voice\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {open_synth_state_file: \"media/voice.json\"}\n@track lead | preset: voice, instrument: synth/lead_square\n  @phrase note [1:1 - 2:1]\n    C4 q\n"
        ).unwrap();
        let before = render(&project, f.old());
        assert!(before.iter().any(|sample| sample.abs() > 0.01));
        let reopened = f.save_as(&project);
        assert_eq!(
            std::fs::read(f.new_dir().join("media/tone.wav")).unwrap(),
            sample
        );
        std::fs::remove_dir_all(media).unwrap();
        assert_eq!(before, render(&reopened, f.new_dir()));
    }

    #[test]
    fn a_relative_wavetable_moves_and_still_renders_from_the_new_directory() {
        const SOURCE: &str = "@project \"Table\"\n  tempo: 120\n  time_signature: 4/4\n@preset table\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {wavetable.file: \"wt.wav\", osc.a.waveform: 4}\n@track table | preset: table\n  @phrase p [1:1 - 1:2]\n    @0 C4 q v=100\n";
        let f = Fixture::new();
        write_wavetable(&f.old().join("wt.wav"));
        let p = oc_dsl::parse(SOURCE).unwrap();
        let before = render(&p, f.old());
        assert!(
            before.iter().any(|v| v.abs() > 0.001),
            "wavetable is silent"
        );
        save(&p, SOURCE, Some(&f.source()), &f.target()).unwrap();
        // The resource moves; the path in the document is not rewritten.
        assert_eq!(std::fs::read_to_string(f.target()).unwrap(), SOURCE);
        assert_eq!(
            std::fs::read(f.new_dir().join("wt.wav")).unwrap(),
            std::fs::read(f.old().join("wt.wav")).unwrap()
        );
        let reopened = oc_dsl::parse(&std::fs::read_to_string(f.target()).unwrap()).unwrap();
        assert!(oc_dsl::media::missing(&reopened, &f.new_dir()).is_empty());
        std::fs::remove_file(f.old().join("wt.wav")).unwrap();
        assert_eq!(before, render(&reopened, f.new_dir()));
    }

    #[test]
    fn a_relative_sampler_sample_moves_and_still_renders_from_the_new_directory() {
        let f = Fixture::new();
        write_sample(&f.old().join("tone.wav"));
        let p = oc_dsl::parse(
            "@project \"Sampler\"\n  tempo: 120\n  time_signature: 4/4\n@preset sampler\n  source: builtin\n  plugin: \"builtin.sampler\"\n  params: {sampler.sample_file: \"tone.wav\", sampler.root_note: 60}\n@track sampler | preset: sampler\n  @phrase p [1:1 - 1:2]\n    @0 C4 q v=100\n",
        )
        .unwrap();
        let before = render(&p, f.old());
        let peak = before.iter().fold(0.0f32, |peak, v| peak.max(v.abs()));
        assert!(peak > 0.01, "sampler is silent");
        let reopened = f.save_as(&p);
        assert_eq!(
            std::fs::read(f.new_dir().join("tone.wav")).unwrap(),
            std::fs::read(f.old().join("tone.wav")).unwrap()
        );
        assert!(oc_dsl::media::missing(&reopened, &f.new_dir()).is_empty());
        std::fs::remove_file(f.old().join("tone.wav")).unwrap();
        assert_eq!(before, render(&reopened, f.new_dir()));
    }

    #[test]
    fn an_sfz_moves_with_the_samples_it_references() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.old().join("media/samples")).unwrap();
        std::fs::write(f.old().join("media/samples/kick.wav"), b"kick").unwrap();
        std::fs::write(
            f.old().join("media/kit.sfz"),
            "<control> default_path=samples\n<region> sample=kick.wav\n",
        )
        .unwrap();
        let p = oc_dsl::parse(
            "@project \"Kit\"\n  tempo: 120\n@preset kit\n  source: builtin\n  plugin: \"builtin.sampler\"\n  params: {sampler.sfz_file: \"media/kit.sfz\"}\n@track kit | preset: kit\n",
        )
        .unwrap();
        let collected = f.run(&p).unwrap();
        assert!(collected.warnings.is_empty(), "{:?}", collected.warnings);
        assert_eq!(collected.created.len(), 2);
        assert_eq!(
            std::fs::read(f.new_dir().join("media/kit.sfz")).unwrap(),
            std::fs::read(f.old().join("media/kit.sfz")).unwrap()
        );
        assert_eq!(
            std::fs::read(f.new_dir().join("media/samples/kick.wav")).unwrap(),
            b"kick"
        );
        let reopened = f.save_as(&p);
        assert!(oc_dsl::media::missing(&reopened, &f.new_dir()).is_empty());
    }

    #[test]
    fn an_sfz_whose_samples_leave_the_project_stays_behind_with_a_warning() {
        let f = Fixture::new();
        std::fs::create_dir(f.old().join("media")).unwrap();
        let outside = f.0.join("outside.wav");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::write(
            f.old().join("media/kit.sfz"),
            format!(
                "<region> sample={}\n",
                outside.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let p = oc_dsl::parse(
            "@project \"Kit\"\n  tempo: 120\n@preset kit\n  source: builtin\n  plugin: \"builtin.sampler\"\n  params: {sampler.sfz_file: \"media/kit.sfz\"}\n@track kit | preset: kit\n",
        )
        .unwrap();
        let collected = f.run(&p).unwrap();
        assert!(collected.created.is_empty());
        assert_eq!(collected.warnings.len(), 1, "{:?}", collected.warnings);
        assert!(collected.warnings[0].contains("media/kit.sfz"));
        assert!(
            collected.warnings[0].contains(&f.old().canonicalize().unwrap().display().to_string()),
            "{:?}",
            collected.warnings
        );
        assert!(!f.new_dir().join("media/kit.sfz").exists());
    }

    #[test]
    fn a_missing_absolute_resource_is_skipped_with_a_warning() {
        let f = Fixture::new();
        let missing = f.0.join("gone.bin");
        let collected = f
            .run(&project("state_file", &missing.display().to_string()))
            .unwrap();
        assert!(collected.created.is_empty());
        assert_eq!(collected.warnings.len(), 1, "{:?}", collected.warnings);
        assert!(collected.warnings[0].contains("gone.bin"));
    }

    #[test]
    fn a_plain_save_collects_nothing() {
        let f = Fixture::new();
        write_wavetable(&f.old().join("wt.wav"));
        let p = oc_dsl::parse(
            "@project \"Table\"\n  tempo: 120\n@preset table\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {wavetable.file: \"wt.wav\"}\n@track table | preset: table\n",
        )
        .unwrap();
        let collected = collect(&p, Some(&f.source()), &f.source()).unwrap();
        assert!(collected.created.is_empty());
        assert!(collected.warnings.is_empty());
    }

    const KIT_SOURCE: &str = "@project \"Kit\"\n  tempo: 120\n  time_signature: 4/4\n@kit basic\n  pad kick | note: C2\n  pad snare | note: D2\n  layer kick | low: 1, high: 127, sample: \"media/kick.wav\"\n  layer snare | low: 1, high: 127, sample: \"media/snare.wav\"\n@track drums | instrument: drums/drumkit, preset: basic\n  @pattern beat [1:1 - 1:2]\n    kick 1:0 ff\n    snare 1:1 ff\n";

    #[test]
    fn a_relative_kit_moves_its_layer_samples_and_still_renders() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.old().join("media")).unwrap();
        write_sample(&f.old().join("media/kick.wav"));
        write_sample(&f.old().join("media/snare.wav"));
        let p = oc_dsl::parse(KIT_SOURCE).unwrap();
        let before = render(&p, f.old());
        assert!(
            before.iter().any(|value| value.abs() > 0.001),
            "the kit is silent"
        );
        save(&p, KIT_SOURCE, Some(&f.source()), &f.target()).unwrap();
        // The document keeps its relative paths; both samples move beside it.
        assert_eq!(std::fs::read_to_string(f.target()).unwrap(), KIT_SOURCE);
        for name in ["kick.wav", "snare.wav"] {
            assert_eq!(
                std::fs::read(f.new_dir().join("media").join(name)).unwrap(),
                std::fs::read(f.old().join("media").join(name)).unwrap()
            );
        }
        let reopened = oc_dsl::parse(&std::fs::read_to_string(f.target()).unwrap()).unwrap();
        assert!(oc_dsl::media::missing(&reopened, &f.new_dir()).is_empty());
        std::fs::remove_dir_all(f.old().join("media")).unwrap();
        assert_eq!(before, render(&reopened, f.new_dir()));
        // A plain save in the same directory still collects nothing.
        let plain = collect(&reopened, Some(&f.target()), &f.target()).unwrap();
        assert!(plain.created.is_empty());
        assert!(plain.warnings.is_empty());
    }

    #[test]
    fn a_missing_kit_layer_is_skipped_with_a_warning() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.old().join("media")).unwrap();
        write_sample(&f.old().join("media/kick.wav"));
        write_sample(&f.old().join("media/snare.wav"));
        let p = oc_dsl::parse(KIT_SOURCE).unwrap();
        std::fs::remove_file(f.old().join("media/snare.wav")).unwrap();
        let collected = f.run(&p).unwrap();
        assert_eq!(collected.created.len(), 1, "{:?}", collected.created);
        assert!(f.new_dir().join("media/kick.wav").is_file());
        assert!(!f.new_dir().join("media/snare.wav").exists());
        assert_eq!(collected.warnings.len(), 1, "{:?}", collected.warnings);
        assert!(
            collected.warnings[0].contains("media/snare.wav"),
            "{:?}",
            collected.warnings
        );
        assert!(
            !collected.warnings[0].contains("kick.wav"),
            "{:?}",
            collected.warnings
        );
        // The saved document still loads; the absent layer stays optional.
        let reopened = f.save_as(&p);
        assert!(oc_dsl::media::missing(&reopened, &f.new_dir()).is_empty());
    }

    #[test]
    fn the_project_bounds_refuse_before_anything_is_written() {
        let f = Fixture::new();
        std::fs::write(f.old().join("voice.bin"), b"state").unwrap();
        let root = f.old().canonicalize().unwrap();
        let mut collection = Collection::new(&root);
        for index in 0..MAX_FILES {
            collection
                .files
                .insert(PathBuf::from(format!("f{index}")), Vec::new());
        }
        let error = collection
            .insert(Path::new("voice.bin"), "preset 'x'")
            .unwrap_err();
        assert!(error.contains("256MiB/project bound"));
        assert!(!f.new_dir().join("voice.bin").exists());
        let mut group = Collection::new(&root);
        group
            .files
            .insert(PathBuf::from("kit.sfz"), b"sfz".to_vec());
        assert!(collection.merge(group).is_err());
    }
}
