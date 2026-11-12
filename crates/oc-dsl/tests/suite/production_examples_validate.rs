//! Every shipped sample project must parse and pass semantic validation.
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn validate_file(path: &Path) {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let project =
        oc_dsl::parse(&source).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    if let Err(errors) = oc_dsl::validator::validate(&project) {
        let messages = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        panic!("{}:\n{messages}", path.display());
    }
    assert!(project.master.is_some(), "{} has no master", path.display());
}

#[test]
fn every_example_is_semantically_valid() {
    let dir = repo_root().join("examples");
    let mut paths = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("ocp"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(paths.len() >= 10, "expected the ten shipped examples");
    for path in &paths {
        validate_file(path);
    }
}

#[test]
fn demo_song_projects_are_semantically_valid() {
    let Some(dir) = std::env::var_os("OCP_DEMO_SONG_DIR").map(PathBuf::from) else {
        eprintln!("skipping demo song projects: set OCP_DEMO_SONG_DIR");
        return;
    };
    if !dir.is_dir() {
        panic!("OCP_DEMO_SONG_DIR is not a directory: {}", dir.display());
    }
    for name in ["compare.ocp", "opensynth.ocp", "opensynth_hot.ocp"] {
        validate_file(&dir.join(name));
    }
}
