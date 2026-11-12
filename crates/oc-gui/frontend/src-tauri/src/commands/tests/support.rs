use oc_dsl::ast;

use crate::state::AppStateInner;

/// A full app state whose project came from `source` and is saved next to `dir`.
pub(crate) fn state_with_project(source: &str, dir: &std::path::Path) -> crate::state::AppState {
    let state = crate::state::AppState::new();
    {
        let mut inner = state.lock_inner();
        inner.project = oc_dsl::parse(source).unwrap();
        inner.dsl_source = oc_dsl::serialize(&inner.project);
        inner.file_path = Some(dir.join("song.ocp"));
        inner.audio.soundfont_path = None;
    }
    state
}

pub(crate) fn test_state() -> AppStateInner {
    let state = crate::state::AppState::new();
    state.inner.lock().unwrap().project = default_project();
    state.inner.into_inner().unwrap()
}

pub(crate) fn default_project() -> ast::Project {
    ast::Project {
        name: Some("Test".to_string()),
        ocp_version: Some("0.2.0".to_string()),
        properties: ast::ProjectProperties::default(),
        tracks: vec![],
        patterns: vec![],
        presets: vec![],
        buses: vec![],
        master: None,
        sections: vec![],
        markers: Vec::new(),
        groups: Vec::new(),
    }
}

/// A track that records the input instead of an instrument.
pub(crate) fn make_audio_track(name: &str) -> ast::Track {
    let mut track = make_track(name);
    track.instrument = ast::InstrumentPath::new("audio", "input");
    track.preset = None;
    track
}

pub(crate) fn make_track(name: &str) -> ast::Track {
    ast::Track {
        name: name.to_string(),
        instrument: ast::InstrumentPath::new("piano", "acoustic_grand"),
        engine: None,
        preset: None,
        mix: ast::MixState::default(),
        effects: vec![],
        phrases: vec![],
        patterns: vec![],
        inline_patterns: vec![],
        clips: oc_dsl::ast::ClipPool::new(),
        gain_automation: Vec::new(),
        engine_params: None,
        performance: None,
        channel: None,
        output_port: None,
        output_pair: None,
        input_routes: vec![],
    }
}

/// Temporary directory that removes itself when the test ends.
pub(crate) struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ocp-{label}-{}-{stamp}-{unique}",
            std::process::id()
        ));
        Self { path }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Build the LV2 fixture cdylib once per process and return its path.
pub(crate) fn lv2_fixture_library() -> std::path::PathBuf {
    static FIXTURE: std::sync::OnceLock<(std::path::PathBuf, std::path::PathBuf)> =
        std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let directory = oc_test_support::build_dir("lv2-fixture");
            let extension = if cfg!(target_os = "macos") {
                "dylib"
            } else if cfg!(windows) {
                "dll"
            } else {
                "so"
            };
            let output = directory.join(format!("lv2_fixture.{extension}"));
            let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../oc-host/tests/fixtures/build_lv2_fixture.sh");
            let status = std::process::Command::new("sh")
                .arg(&script)
                .arg(&output)
                .status()
                .expect("run the LV2 fixture build script");
            assert!(status.success(), "fixture build failed: {status}");
            (directory, output)
        })
        .1
        .clone()
}
