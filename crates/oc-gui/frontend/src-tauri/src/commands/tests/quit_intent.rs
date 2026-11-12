//! The quit prompt's answer decides what reaches the file.
//!
//! Don't Save drops the document: nothing is written, the project file keeps its
//! bytes and its timestamp, and the recovery copies stay. Save and an edit after
//! an answer publish as they always did, so the row-111 guarantee (an agent edit
//! is not lost when the process leaves) still holds. The Tauri event wiring
//! itself is not exercised here; these cases drive the bodies it calls.

use std::path::{Path, PathBuf};

use crate::commands::shared::resync_with_undo;
use crate::project_persist::{backup_path, flush_exit, flush_on_exit_state, flush_state};
use crate::quit_intent::discard;
use crate::state::{AppState, UndoEdit, UndoScope};

use super::support::{TempDir, state_with_project};

const SONG: &str = r#"@ocp "0.2.0"

@project "quit song"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf

@track keys | instrument: piano/acoustic_grand
  effects:
    - gain: factor=0.5
  clip: riff [1:1 - 3:1]
"#;

struct Fixture {
    dir: TempDir,
    state: AppState,
    path: PathBuf,
}

impl Fixture {
    /// An open dirty project whose file already holds the baseline document.
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        std::fs::create_dir_all(dir.path()).unwrap();
        let state = state_with_project(SONG, dir.path());
        let baseline = {
            let mut inner = state.lock_inner();
            oc_dsl::instruments::apply_defaults(&mut inner.project);
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            inner.dirty = true;
            inner.dsl_source.clone()
        };
        let path = dir.path().join("song.ocp");
        std::fs::write(&path, &baseline).unwrap();
        Self { dir, state, path }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }

    fn modified(&self) -> std::time::SystemTime {
        std::fs::metadata(&self.path).unwrap().modified().unwrap()
    }

    fn backup(&self) -> PathBuf {
        backup_path(&self.path)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// One document edit with no write behind it, as a fader move makes it.
    fn edit(&self, mute: bool) -> String {
        self.resync("mute", UndoScope::MixerLevels, |project| {
            project.tracks[0].mix.mute = mute;
        })
    }

    /// A second, different edit, so the document cannot equal the file by luck.
    fn set_tempo(&self, tempo: f64) -> String {
        self.resync("tempo", UndoScope::Project, |project| {
            project.properties.tempo = tempo;
        })
    }

    fn resync(
        &self,
        label: &str,
        scope: UndoScope,
        change: impl FnOnce(&mut oc_dsl::ast::Project),
    ) -> String {
        let mut inner = self.state.lock_inner();
        let old = inner.project.clone();
        change(&mut inner.project);
        resync_with_undo(&mut inner, old, UndoEdit::new(label, scope)).unwrap();
        inner.dsl_source.clone()
    }

    fn discard(&self) {
        let mut inner = self.state.lock_inner();
        discard(&mut inner);
    }
}

fn copies_in(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .flat_map(|entry| {
            std::fs::read_dir(entry.path())
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|copy| copy.path())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_prompt_and_a_discard_both_leave_the_file_alone() {
    let fixture = Fixture::new("quit-discard");
    let baseline = fixture.bytes();
    let modified = fixture.modified();
    // The edit is in the document only: nothing published it.
    let discarded = fixture.edit(true);
    assert_ne!(discarded.as_bytes(), baseline.as_slice());
    assert_eq!(fixture.bytes(), baseline);

    // While the prompt is open the exit event writes nothing: no answer has
    // armed the write yet.
    flush_on_exit_state(&fixture.state);
    assert_eq!(fixture.bytes(), baseline, "the prompt wrote the file");
    assert_eq!(fixture.modified(), modified, "the file was not touched");

    fixture.discard();
    assert!(!fixture.state.lock_inner().dirty, "the document is dropped");
    // Even an armed exit may not publish a document the operator dropped.
    fixture.state.lock_inner().exit_ready = true;
    flush_on_exit_state(&fixture.state);
    flush_exit(&fixture.state);
    assert_eq!(
        fixture.bytes(),
        baseline,
        "the discarded document is not the file"
    );
    assert_eq!(fixture.modified(), modified, "the file was not touched");
    assert!(flush_state(&fixture.state).unwrap().is_none());
    assert!(
        !fixture.backup().exists(),
        "no backup of a file the write never replaced: {:?}",
        fixture.names()
    );
    assert!(
        fixture.names().iter().all(|name| !name.ends_with(".tmp")),
        "{:?}",
        fixture.names()
    );
}

#[test]
fn a_dirty_document_the_operator_kept_still_writes_on_exit() {
    let fixture = Fixture::new("quit-kept");
    let baseline = fixture.bytes();
    let edited = fixture.edit(true);
    assert_eq!(fixture.bytes(), baseline);

    // `quit_now` arms the last-resort write; the answer was Save.
    fixture.state.lock_inner().exit_ready = true;
    flush_on_exit_state(&fixture.state);

    assert_eq!(
        fixture.bytes(),
        edited.as_bytes(),
        "the kept edit is the file"
    );
    assert_eq!(std::fs::read(fixture.backup()).unwrap(), baseline);
    // The document was published, so a second exit event has nothing to do.
    assert!(flush_state(&fixture.state).unwrap().is_none());
}

#[test]
fn an_edit_after_a_discard_is_writable_again() {
    let fixture = Fixture::new("quit-again");
    let baseline = fixture.bytes();
    fixture.edit(true);
    fixture.discard();

    // A new edit is a new document: the answer no longer covers it.
    let edited = fixture.set_tempo(90.0);
    {
        let inner = fixture.state.lock_inner();
        assert!(inner.dirty, "an edit dirties the document");
        assert!(!inner.discard_pending, "the earlier answer is spent");
    }
    assert_ne!(edited.as_bytes(), baseline.as_slice());
    assert_eq!(flush_state(&fixture.state).unwrap(), Some(edited.len()));
    assert_eq!(fixture.bytes(), edited.as_bytes());
    assert_eq!(std::fs::read(fixture.backup()).unwrap(), baseline);
}

#[test]
fn a_discard_keeps_the_recovery_copies_and_a_save_retires_them() {
    let fixture = Fixture::new("quit-recovery");
    let baseline = fixture.bytes();
    fixture.edit(true);
    let recovery = TempDir::new("quit-copies");
    std::fs::create_dir_all(recovery.path()).unwrap();
    let copy = crate::autosave::write_copy(
        recovery.path(),
        Some(&fixture.path),
        Some("quit song"),
        "quit-session",
        std::str::from_utf8(&baseline).unwrap(),
        1_000,
    )
    .expect("the copy is written");
    let copy = PathBuf::from(copy.path);
    assert!(copy.exists());

    fixture.discard();
    assert_eq!(
        copies_in(recovery.path()),
        vec![copy.clone()],
        "a discard decides about the project file, not the crash net"
    );

    // A later save retires the copies of this project, as it always did.
    crate::autosave::clear_saved(recovery.path(), &fixture.path, false, "quit-session").unwrap();
    assert!(copies_in(recovery.path()).is_empty());
}
