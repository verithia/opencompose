//! Samples already on this machine: the first section of the sample panel.
//!
//! A search walks the sound folders [`crate::local_content`] counts, matches
//! every query word against the file's path below its folder, and reads a
//! duration from the file header only when a duration bound asks for one.
//! Preview and add accept a path only when it is an audio file inside one of
//! those folders, so the webview cannot name an arbitrary file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oc_sample::model::{MAX_QUERY_CHARS, SampleKind, classify};
use serde::{Deserialize, Serialize};

/// Results one search returns when the caller names no limit.
pub const DEFAULT_LIMIT: usize = 50;
/// Most results one search returns.
pub const MAX_LIMIT: usize = 200;
/// Query words a search matches; more words only narrow an empty list.
const MAX_WORDS: usize = 8;
/// File headers one search may open for a duration bound.
const MAX_DURATION_PROBES: usize = 400;
/// How long a walk of the sound folders answers repeated searches.
const CATALOGUE_TTL: Duration = Duration::from_secs(30);
/// Largest file an add copies into the project.
pub const MAX_COPY_BYTES: u64 = 512 * 1024 * 1024;
/// Where an added local sample is copied, relative to the project folder. The
/// sample-market cache under `media/samples` can be cleared; these files
/// cannot, because the user's original may move.
pub const PROJECT_DIR: &str = "media/local-samples";

/// Why a local sample command refused. `code` is the wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSampleRefusal {
    NotFound,
    OutsideFolders,
    NotAudio,
    TooLarge,
    InvalidKind,
    ProjectUnsaved,
}

impl LocalSampleRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::OutsideFolders => "outside_folders",
            Self::NotAudio => "not_audio",
            Self::TooLarge => "too_large",
            Self::InvalidKind => "invalid_kind",
            Self::ProjectUnsaved => "project_unsaved",
        }
    }
}

/// A refusal with the sentence the panel shows when it has no translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSampleError {
    pub refusal: Option<LocalSampleRefusal>,
    pub message: String,
}

impl LocalSampleError {
    pub fn refused(refusal: LocalSampleRefusal, message: impl Into<String>) -> Self {
        Self {
            refusal: Some(refusal),
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            refusal: None,
            message: message.into(),
        }
    }
}

/// The panel's search, as it arrives over IPC.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalSearchArgs {
    pub query: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub min_seconds: Option<f64>,
    #[serde(default)]
    pub max_seconds: Option<f64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One bounded search.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalQuery {
    words: Vec<String>,
    kind: Option<SampleKind>,
    min_seconds: Option<f64>,
    max_seconds: Option<f64>,
    limit: usize,
}

impl LocalQuery {
    pub fn from_args(args: &LocalSearchArgs) -> Result<Self, LocalSampleError> {
        let kind = match args.kind.as_deref() {
            None => None,
            Some(text) => match SampleKind::parse(text) {
                Some(SampleKind::Unknown) => None,
                Some(kind) => Some(kind),
                None => {
                    return Err(LocalSampleError::refused(
                        LocalSampleRefusal::InvalidKind,
                        format!("'{text}' is not a sample kind."),
                    ));
                }
            },
        };
        let text: String = args.query.chars().take(MAX_QUERY_CHARS).collect();
        let mut words: Vec<String> = Vec::new();
        for word in searchable(&text).split_whitespace() {
            if words.len() == MAX_WORDS {
                break;
            }
            if !words.iter().any(|seen| seen == word) {
                words.push(word.to_string());
            }
        }
        let bound =
            |value: Option<f64>| value.filter(|seconds| seconds.is_finite() && *seconds >= 0.0);
        Ok(Self {
            words,
            kind,
            min_seconds: bound(args.min_seconds),
            max_seconds: bound(args.max_seconds),
            limit: args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        })
    }

    fn bounds_duration(&self) -> bool {
        self.min_seconds.is_some() || self.max_seconds.is_some()
    }

    fn accepts_duration(&self, seconds: f64) -> bool {
        self.min_seconds.is_none_or(|min| seconds >= min)
            && self.max_seconds.is_none_or(|max| seconds <= max)
    }
}

/// Lowercase text with path separators and word joiners turned into spaces,
/// so `Drums/Snare_Tight-01.wav` matches "snare tight".
fn searchable(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '/' | '\\' | '_' | '-' | '.' | '(' | ')' | '[' | ']' => ' ',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// One local result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalSample {
    /// The absolute path preview and add pass back.
    pub path: String,
    /// The file name without its extension.
    pub name: String,
    /// The sound folder the file was found in.
    pub folder: String,
    /// The path below that folder, which is what the search matched.
    pub relative: String,
    pub format: String,
    pub kind: SampleKind,
    /// From the file header; `None` when no duration bound asked for it or the
    /// container does not state one.
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalSearchResult {
    pub items: Vec<LocalSample>,
    /// More files match than `items` lists, or the duration reads stopped
    /// before every candidate was checked.
    pub truncated: bool,
    /// Sound folders that exist and were searched.
    pub folders: usize,
}

/// One audio file the walk found.
#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    root: usize,
    relative: String,
    name: String,
    format: String,
    /// `relative` in [`searchable`] form.
    haystack: String,
    /// `name` in [`searchable`] form.
    name_words: String,
}

/// Every audio file under the searched folders, each listed once.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    roots: Vec<PathBuf>,
    entries: Vec<Entry>,
}

impl Catalogue {
    /// Walk `roots` with `files`, which lists the audio files under one root.
    /// Roots are canonical first, so a folder nested in another yields the
    /// same file path from both and is kept once.
    pub fn build(roots: &[PathBuf], files: &dyn Fn(&Path) -> Vec<PathBuf>) -> Self {
        let mut canonical_roots: Vec<PathBuf> = Vec::new();
        for root in roots {
            let Ok(root) = root.canonicalize() else {
                continue;
            };
            if root.is_dir() && !canonical_roots.contains(&root) {
                canonical_roots.push(root);
            }
        }
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for (index, root) in canonical_roots.iter().enumerate() {
            for path in files(root) {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let format = path
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                entries.push(Entry {
                    haystack: searchable(&relative),
                    name_words: searchable(&name),
                    path,
                    root: index,
                    relative,
                    name,
                    format,
                });
            }
        }
        Self {
            roots: canonical_roots,
            entries,
        }
    }

    /// Files whose path holds every query word. A file whose name holds them
    /// all ranks before one that matches only through its folders; shorter
    /// names rank first within each group, then the path breaks ties.
    pub fn search(&self, query: &LocalQuery) -> LocalSearchResult {
        let mut matches: Vec<(bool, &Entry)> = self
            .entries
            .iter()
            .filter(|entry| {
                !query.words.is_empty()
                    && query
                        .words
                        .iter()
                        .all(|word| entry.haystack.contains(word.as_str()))
            })
            .filter(|entry| {
                query
                    .kind
                    .is_none_or(|kind| classify(&entry.relative, &[]) == kind)
            })
            .map(|entry| {
                let in_name = query
                    .words
                    .iter()
                    .all(|word| entry.name_words.contains(word.as_str()));
                (in_name, entry)
            })
            .collect();
        matches.sort_by(|(left_named, left), (right_named, right)| {
            right_named
                .cmp(left_named)
                .then_with(|| left.name.chars().count().cmp(&right.name.chars().count()))
                .then_with(|| {
                    left.relative
                        .to_lowercase()
                        .cmp(&right.relative.to_lowercase())
                })
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut items = Vec::new();
        let mut truncated = false;
        let mut probes = 0;
        for (_, entry) in matches {
            if items.len() == query.limit {
                truncated = true;
                break;
            }
            let duration_seconds = if query.bounds_duration() {
                if probes == MAX_DURATION_PROBES {
                    truncated = true;
                    break;
                }
                probes += 1;
                // A file without a stated length fails a bound rather than
                // passing on a guess, as a market result does.
                match oc_engine::audio_clip::probe_seconds(&entry.path) {
                    Some(seconds) if query.accepts_duration(seconds) => Some(seconds),
                    _ => continue,
                }
            } else {
                None
            };
            items.push(LocalSample {
                path: entry.path.display().to_string(),
                name: entry.name.clone(),
                folder: self.roots[entry.root].display().to_string(),
                relative: entry.relative.clone(),
                format: entry.format.clone(),
                kind: classify(&entry.relative, &[]),
                duration_seconds,
            });
        }
        LocalSearchResult {
            items,
            truncated,
            folders: self.roots.len(),
        }
    }
}

/// The canonical file `path` names, when it is an audio file inside one of
/// `roots`, which need not be canonical.
pub fn resolve_in(roots: &[PathBuf], path: &str) -> Result<PathBuf, LocalSampleError> {
    let requested = Path::new(path);
    let file = requested.canonicalize().map_err(|_| {
        LocalSampleError::refused(
            LocalSampleRefusal::NotFound,
            format!("'{path}' is no longer on this computer."),
        )
    })?;
    if !file.is_file() {
        return Err(LocalSampleError::refused(
            LocalSampleRefusal::NotFound,
            format!("'{path}' is not a file."),
        ));
    }
    if !crate::local_content::is_sample_file(&file) {
        return Err(LocalSampleError::refused(
            LocalSampleRefusal::NotAudio,
            format!("'{path}' is not a sample file."),
        ));
    }
    let inside = roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| file.starts_with(&root))
    });
    if !inside {
        return Err(LocalSampleError::refused(
            LocalSampleRefusal::OutsideFolders,
            format!("'{path}' is not in a sound folder the app searches."),
        ));
    }
    Ok(file)
}

/// The walk of the app's sound folders, reused for [`CATALOGUE_TTL`] so
/// typing a few searches in a row walks the disk once.
pub fn catalogue() -> Arc<Catalogue> {
    static CACHE: Mutex<Option<(Instant, Arc<Catalogue>)>> = Mutex::new(None);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((built, catalogue)) = cache.as_ref()
        && built.elapsed() < CATALOGUE_TTL
    {
        return Arc::clone(catalogue);
    }
    let catalogue = Arc::new(Catalogue::build(
        &crate::local_content::sample_search_roots(),
        &crate::local_content::sample_files,
    ));
    *cache = Some((Instant::now(), Arc::clone(&catalogue)));
    catalogue
}

/// [`resolve_in`] against the folders a search walks. It lists no
/// directory, so a preview never waits for a walk.
pub fn resolve(path: &str) -> Result<PathBuf, LocalSampleError> {
    resolve_in(&crate::local_content::sample_search_roots(), path)
}

/// A copy of a local sample inside a project folder. Dropping it before
/// [`ProjectCopy::keep`] removes a file the copy created, so a refused
/// placement leaves the media folder as it was.
#[derive(Debug)]
pub struct ProjectCopy {
    pub path: PathBuf,
    created: bool,
}

impl ProjectCopy {
    pub fn keep(mut self) {
        self.created = false;
    }
}

impl Drop for ProjectCopy {
    fn drop(&mut self) {
        if self.created {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A file name that is safe in every file system and in an `.ocp` path.
fn safe_stem(source: &Path) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    let mut gap = false;
    for character in stem.chars() {
        if character.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            out.push(character);
            gap = false;
        } else {
            gap = true;
        }
        if out.chars().count() >= 80 {
            break;
        }
    }
    if out.is_empty() {
        "sample".to_string()
    } else {
        out
    }
}

/// Whether two files hold the same bytes, read in chunks so a large sample is
/// never held in memory twice.
fn same_bytes(left: &Path, right: &Path) -> bool {
    use std::io::Read;
    let size = |path: &Path| std::fs::metadata(path).map(|meta| meta.len()).ok();
    if size(left).is_none() || size(left) != size(right) {
        return false;
    }
    let (Ok(mut left), Ok(mut right)) = (std::fs::File::open(left), std::fs::File::open(right))
    else {
        return false;
    };
    let mut left_chunk = vec![0u8; 64 * 1024];
    let mut right_chunk = vec![0u8; 64 * 1024];
    loop {
        let Ok(read) = left.read(&mut left_chunk) else {
            return false;
        };
        if read == 0 {
            return true;
        }
        if right.read_exact(&mut right_chunk[..read]).is_err()
            || left_chunk[..read] != right_chunk[..read]
        {
            return false;
        }
    }
}

/// Copy `source` into [`PROJECT_DIR`] under `project_root`. The copy's path
/// starts with `project_root` as given, so a clip path can be made relative to
/// it. A file with the same name and bytes is reused, so adding one sample
/// twice keeps one copy; a different file with that name gets a number.
pub fn copy_into_project(
    source: &Path,
    project_root: &Path,
) -> Result<ProjectCopy, LocalSampleError> {
    let bytes = std::fs::metadata(source)
        .map_err(|error| {
            LocalSampleError::failed(format!("Cannot read '{}': {error}", source.display()))
        })?
        .len();
    if bytes > MAX_COPY_BYTES {
        return Err(LocalSampleError::refused(
            LocalSampleRefusal::TooLarge,
            format!(
                "'{}' is larger than {} MiB.",
                source.display(),
                MAX_COPY_BYTES / (1024 * 1024)
            ),
        ));
    }
    let directory = project_root.join(PROJECT_DIR);
    std::fs::create_dir_all(&directory).map_err(|error| {
        LocalSampleError::failed(format!("Cannot create the project media folder: {error}"))
    })?;
    let canonical_root = project_root.canonicalize().map_err(|error| {
        LocalSampleError::failed(format!("Cannot resolve the project folder: {error}"))
    })?;
    let canonical_directory = directory.canonicalize().map_err(|error| {
        LocalSampleError::failed(format!("Cannot resolve the project media folder: {error}"))
    })?;
    if !canonical_directory.starts_with(&canonical_root) {
        return Err(LocalSampleError::failed(
            "The project media folder resolves outside the project folder.",
        ));
    }
    let stem = safe_stem(source);
    let extension = source
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "wav".to_string());
    let mut staged = tempfile::Builder::new()
        .prefix(".local-sample-")
        .tempfile_in(&canonical_directory)
        .map_err(|error| LocalSampleError::failed(format!("Cannot stage the sample: {error}")))?;
    let mut input = std::fs::File::open(source).map_err(|error| {
        LocalSampleError::failed(format!("Cannot read '{}': {error}", source.display()))
    })?;
    let copied = std::io::copy(
        &mut std::io::Read::take(&mut input, MAX_COPY_BYTES + 1),
        staged.as_file_mut(),
    )
    .map_err(|error| {
        LocalSampleError::failed(format!("Cannot copy '{}': {error}", source.display()))
    })?;
    if copied > MAX_COPY_BYTES {
        return Err(LocalSampleError::refused(
            LocalSampleRefusal::TooLarge,
            format!(
                "'{}' grew past the copy limit while it was read.",
                source.display()
            ),
        ));
    }
    for index in 1..=1_000 {
        let name = if index == 1 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{index}.{extension}")
        };
        let target = canonical_directory.join(&name);
        if target.symlink_metadata().is_ok() {
            if same_bytes(staged.path(), &target) {
                return Ok(ProjectCopy {
                    path: directory.join(&name),
                    created: false,
                });
            }
            continue;
        }
        match staged.persist_noclobber(&target) {
            Ok(_) => {
                return Ok(ProjectCopy {
                    path: directory.join(&name),
                    created: true,
                });
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staged = error.file;
            }
            Err(error) => {
                return Err(LocalSampleError::failed(format!(
                    "Cannot publish the sample copy: {}",
                    error.error
                )));
            }
        }
    }
    Err(LocalSampleError::failed(format!(
        "Every name for '{stem}' is taken in the project media folder."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(path: &Path, seconds: f64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rate = 8_000;
        let frames = (seconds * f64::from(rate)) as usize;
        std::fs::write(
            path,
            oc_engine::wav::pcm_wav_bytes(&vec![0.1; frames * 2], rate, 2).unwrap(),
        )
        .unwrap();
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        crate::local_content::sample_files(root)
    }

    fn query(text: &str) -> LocalQuery {
        LocalQuery::from_args(&LocalSearchArgs {
            query: text.into(),
            ..Default::default()
        })
        .unwrap()
    }

    fn names(result: &LocalSearchResult) -> Vec<&str> {
        result
            .items
            .iter()
            .map(|item| item.relative.as_str())
            .collect()
    }

    #[test]
    fn every_word_must_match_and_a_name_match_ranks_first() {
        let root = tempfile::tempdir().unwrap();
        wav(&root.path().join("Snares/Tight.wav"), 0.2);
        wav(&root.path().join("Kit/Snare_Tight_Long.wav"), 0.2);
        wav(&root.path().join("Kit/Snare_Tight.wav"), 0.2);
        wav(&root.path().join("Kit/Snare_Loose.wav"), 0.2);
        std::fs::write(root.path().join("Kit/snare tight notes.txt"), b"x").unwrap();
        let catalogue = Catalogue::build(&[root.path().to_path_buf()], &walk);

        let result = catalogue.search(&query("SNARE  tight"));
        assert_eq!(
            names(&result),
            [
                "Kit/Snare_Tight.wav",
                "Kit/Snare_Tight_Long.wav",
                "Snares/Tight.wav"
            ]
        );
        assert!(!result.truncated);
        assert_eq!(result.folders, 1);
        assert_eq!(result.items[0].name, "Snare_Tight");
        assert_eq!(result.items[0].format, "wav");
        assert_eq!(result.items[0].kind, SampleKind::DrumOneShot);
        assert_eq!(result.items[0].duration_seconds, None);

        assert!(catalogue.search(&query("")).items.is_empty());
        assert!(catalogue.search(&query("cowbell")).items.is_empty());
    }

    #[test]
    fn a_nested_folder_lists_each_file_once_and_a_limit_truncates() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..4 {
            wav(&root.path().join(format!("Pack/kick {index}.wav")), 0.1);
        }
        let catalogue = Catalogue::build(
            &[
                root.path().to_path_buf(),
                root.path().join("Pack"),
                root.path().join("missing"),
            ],
            &walk,
        );
        assert_eq!(catalogue.search(&query("kick")).items.len(), 4);
        let limited = catalogue.search(
            &LocalQuery::from_args(&LocalSearchArgs {
                query: "kick".into(),
                limit: Some(3),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(limited.items.len(), 3);
        assert!(limited.truncated);
        assert_eq!(limited.folders, 2);
    }

    #[test]
    fn kind_and_duration_filters_apply_to_local_files() {
        let root = tempfile::tempdir().unwrap();
        wav(&root.path().join("Drums/snare hit.wav"), 0.5);
        wav(&root.path().join("Loops/snare loop.wav"), 4.0);
        std::fs::write(root.path().join("Drums/snare broken.wav"), b"RIFF").unwrap();
        let catalogue = Catalogue::build(&[root.path().to_path_buf()], &walk);

        let loops = catalogue.search(
            &LocalQuery::from_args(&LocalSearchArgs {
                query: "snare".into(),
                kind: Some("loop".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(names(&loops), ["Loops/snare loop.wav"]);

        let short = catalogue.search(
            &LocalQuery::from_args(&LocalSearchArgs {
                query: "snare".into(),
                max_seconds: Some(2.0),
                ..Default::default()
            })
            .unwrap(),
        );
        // The unreadable file has no stated length and fails the bound.
        assert_eq!(names(&short), ["Drums/snare hit.wav"]);
        assert!((short.items[0].duration_seconds.unwrap() - 0.5).abs() < 1e-6);

        let refused = LocalQuery::from_args(&LocalSearchArgs {
            query: "snare".into(),
            kind: Some("banjo".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(refused.refusal, Some(LocalSampleRefusal::InvalidKind));
    }

    #[test]
    fn resolve_accepts_only_audio_inside_the_searched_folders() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = root.path().join("a/clap.wav");
        wav(&inside, 0.1);
        wav(&outside.path().join("clap.wav"), 0.1);
        std::fs::write(root.path().join("a/readme.txt"), b"x").unwrap();
        let roots = [root.path().to_path_buf()];

        let resolved = resolve_in(&roots, &inside.display().to_string()).unwrap();
        assert_eq!(resolved, inside.canonicalize().unwrap());
        let code = |path: PathBuf| {
            resolve_in(&roots, &path.display().to_string())
                .unwrap_err()
                .refusal
        };
        assert_eq!(
            code(outside.path().join("clap.wav")),
            Some(LocalSampleRefusal::OutsideFolders)
        );
        assert_eq!(
            code(root.path().join("a/readme.txt")),
            Some(LocalSampleRefusal::NotAudio)
        );
        assert_eq!(
            code(root.path().join("a/gone.wav")),
            Some(LocalSampleRefusal::NotFound)
        );
        // A path that climbs out through `..` is judged by where it lands.
        assert_eq!(
            code(
                root.path()
                    .join("a/../../")
                    .join(outside.path().file_name().unwrap())
                    .join("clap.wav")
            ),
            Some(LocalSampleRefusal::OutsideFolders)
        );
    }

    #[test]
    fn same_bytes_compares_content_not_only_size() {
        let dir = tempfile::tempdir().unwrap();
        let [a, b, c, d] = ["a", "b", "c", "d"].map(|name| dir.path().join(name));
        let big = vec![7u8; 200 * 1024];
        let mut changed = big.clone();
        *changed.last_mut().unwrap() = 8;
        std::fs::write(&a, &big).unwrap();
        std::fs::write(&b, &big).unwrap();
        std::fs::write(&c, &changed).unwrap();
        std::fs::write(&d, &big[..10]).unwrap();
        assert!(same_bytes(&a, &b));
        assert!(!same_bytes(&a, &c));
        assert!(!same_bytes(&a, &d));
        assert!(!same_bytes(&a, &dir.path().join("missing")));
    }

    #[test]
    fn a_copy_reuses_identical_bytes_numbers_a_clash_and_drops_an_unkept_file() {
        let library = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let first = library.path().join("one/Snare (Tight).wav");
        let second = library.path().join("two/Snare (Tight).wav");
        wav(&first, 0.1);
        wav(&second, 0.3);

        let copy = copy_into_project(&first, project.path()).unwrap();
        let expected = project.path().join(PROJECT_DIR).join("Snare-Tight.wav");
        assert_eq!(copy.path, expected);
        copy.keep();

        let again = copy_into_project(&first, project.path()).unwrap();
        assert_eq!(again.path, expected);
        drop(again);
        assert!(expected.exists(), "a reused copy is never removed");

        let clash = copy_into_project(&second, project.path()).unwrap();
        assert_eq!(clash.path.file_name().unwrap(), "Snare-Tight-2.wav");
        let clash_path = clash.path.clone();
        drop(clash);
        assert!(!clash_path.exists(), "an unkept new copy is removed");

        let leftovers: Vec<_> = std::fs::read_dir(project.path().join(PROJECT_DIR))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["Snare-Tight.wav"]);
    }
}
