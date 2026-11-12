//! The workspace commands through their inner functions: which directory a view
//! names, what it says about a folder that is gone, how many project files it
//! found, and the two ways a workspace can be chosen.

use std::path::{Path, PathBuf};

use crate::ai::sessions;
use crate::commands::workspace::{MAX_PROJECTS, current_for, detached_for, open_for};
use crate::commands::{list_for, new_for};
use crate::state::AppState;
use crate::workspaces::{self, FILE_NAME, Registry};

struct Fixture {
    dir: PathBuf,
    _guard: sessions::TestRootGuard,
    _workspaces: workspaces::TestRootGuard,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(label: &str) -> Fixture {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "oc-command-workspace-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = sessions::test_root(dir.join("sessions"));
    let workspace_guard = workspaces::test_root(dir.join("state"));
    Fixture {
        dir,
        _guard: guard,
        _workspaces: workspace_guard,
    }
}

const PROJECT: &str = "@ocp \"0.2.0\"\n\n@project \"fixture\"\n  tempo: 128\n";

fn folder(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, PROJECT).unwrap();
    path
}

fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().to_string_lossy().to_string()
}

fn open_project(state: &AppState, path: &Path) {
    let mut inner = state.lock_inner();
    inner.file_path = Some(path.to_path_buf());
}

#[test]
fn a_remembered_folder_that_is_gone_is_listed_as_missing() {
    let fixture = fixture("recents");
    let path = fixture.dir.join("state").join(FILE_NAME);
    let registry = Registry::new(&path);
    let songs = folder(&fixture.dir, "songs");
    registry
        .remember(&songs, "2026-10-30T08:00:00.000Z")
        .unwrap();
    let listed = registry.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].exists, "a folder that is there needs no marker");
    // A live entry does not carry the field, so the file format is unchanged.
    assert!(!std::fs::read_to_string(&path).unwrap().contains("exists"));

    std::fs::remove_dir_all(&songs).unwrap();
    let listed = registry.list().unwrap();
    assert_eq!(
        listed.len(),
        1,
        "a deleted folder stays listed so it can be forgotten"
    );
    assert!(!listed[0].exists);
    assert_eq!(listed[0].name, "songs");
    // Forgetting works by the path the list holds, even though the folder is gone;
    // that is the path the chooser's row passes back.
    let stored = PathBuf::from(&listed[0].path);
    registry.forget(&stored).unwrap();
    assert!(registry.list().unwrap().is_empty());
}

#[test]
fn a_chosen_workspace_is_not_the_open_projects_directory() {
    let fixture = fixture("chosen");
    let state = AppState::new();
    let a = folder(&fixture.dir, "a");
    let open_file = project(&a, "song.ocp");
    open_project(&state, &open_file);
    let b = folder(&fixture.dir, "b");
    project(&b, "other.ocp");

    // Choosing another directory answers with that directory, not with the one
    // the open file belongs to.
    let opened = open_for(&state, b.to_str().unwrap()).unwrap();
    assert_eq!(opened.path.as_deref(), Some(canonical(&b).as_str()));
    assert_eq!(opened.name.as_deref(), Some("b"));
    assert!(opened.chosen, "the answer names a chosen directory");
    assert!(!opened.detached);
    assert_eq!(
        opened
            .projects
            .iter()
            .map(|project| project.name.as_str())
            .collect::<Vec<_>>(),
        ["other.ocp"]
    );
    assert_eq!(opened.total_projects, 1);

    // Reading the current workspace must not fall back to the open file either.
    let current = current_for(&state);
    assert_eq!(current.path, opened.path);
    assert_eq!(current.name.as_deref(), Some("b"));

    // The conversation created now belongs to the chosen directory and records
    // the file the panel still has open.
    let session = new_for(&state, None, Some("in b"), None).unwrap();
    assert_eq!(
        session.workspace.as_deref(),
        Some(canonical(&b).as_str()),
        "the session is scoped to the chosen workspace"
    );
    assert_eq!(
        session.project_path.as_deref(),
        Some(canonical(&open_file).as_str())
    );
    let listed = list_for(&state, None, false).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, session.id);
}

#[test]
fn a_workspace_whose_folder_is_gone_says_so_and_lists_nothing() {
    let fixture = fixture("gone");
    let state = AppState::new();
    let dir = folder(&fixture.dir, "songs");
    project(&dir, "song.ocp");
    let opened = open_for(&state, dir.to_str().unwrap()).unwrap();
    assert!(opened.exists);
    let path = opened.path.clone().unwrap();

    std::fs::remove_dir_all(&dir).unwrap();

    // The answer still names the directory, so the chooser can show which
    // workspace was lost; it does not report project files it cannot read.
    let current = current_for(&state);
    assert_eq!(current.path.as_deref(), Some(path.as_str()));
    assert!(!current.exists);
    assert!(current.projects.is_empty());
    assert_eq!(current.total_projects, 0);
}

#[test]
fn a_workspace_past_the_list_cap_reports_the_total_it_cut() {
    let fixture = fixture("many");
    let state = AppState::new();
    let dir = folder(&fixture.dir, "many");
    for index in 0..MAX_PROJECTS + 5 {
        project(&dir, &format!("{index:03}.ocp"));
    }

    let opened = open_for(&state, dir.to_str().unwrap()).unwrap();
    assert_eq!(opened.projects.len(), MAX_PROJECTS);
    assert_eq!(opened.total_projects, MAX_PROJECTS + 5);
    // The list is the first names in order, so the count is the only thing that
    // tells the user files are missing.
    assert_eq!(opened.projects[0].name, "000.ocp");
    assert_eq!(
        opened.projects[MAX_PROJECTS - 1].name,
        format!("{:03}.ocp", MAX_PROJECTS - 1)
    );
}

#[test]
fn a_registry_that_cannot_be_written_still_opens_the_folder() {
    let fixture = fixture("registry");
    // A directory where the registry file belongs makes every write fail.
    std::fs::create_dir_all(fixture.dir.join("state").join(FILE_NAME)).unwrap();
    let registry = Registry::new(fixture.dir.join("state").join(FILE_NAME));
    assert!(registry.list().unwrap().is_empty());

    let state = AppState::new();
    let dir = folder(&fixture.dir, "songs");
    project(&dir, "song.ocp");

    let opened = open_for(&state, dir.to_str().unwrap()).unwrap();
    assert_eq!(opened.path.as_deref(), Some(canonical(&dir).as_str()));
    assert_eq!(opened.projects.len(), 1);
    // The recents are a convenience, so the failure stays a warning.
    assert!(registry.list().unwrap().is_empty());
}

#[test]
fn detached_still_follows_the_open_project() {
    let fixture = fixture("detached");
    let state = AppState::new();
    let a = folder(&fixture.dir, "a");
    let first = project(&a, "song.ocp");
    open_project(&state, &first);

    let detached = detached_for(&state);
    assert!(detached.detached);
    assert!(!detached.chosen, "no directory was chosen");
    assert_eq!(detached.path.as_deref(), Some(canonical(&a).as_str()));

    // Opening another project elsewhere moves the scope without another choice.
    let b = folder(&fixture.dir, "b");
    let second = project(&b, "other.ocp");
    open_project(&state, &second);
    assert_eq!(
        current_for(&state).path.as_deref(),
        Some(canonical(&b).as_str())
    );

    let session = new_for(&state, None, Some("no workspace"), None).unwrap();
    assert_ne!(session.workspace_key, sessions::DETACHED_KEY);
    assert_eq!(session.workspace.as_deref(), Some(canonical(&b).as_str()));
}
