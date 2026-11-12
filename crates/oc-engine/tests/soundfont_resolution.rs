//! Which engine a track's preset picks, and what a render says when the bank
//! it names is not there.
//!
//! The rule the renderer applies: a builtin preset plays without a SoundFont,
//! so a project made of builtin presets renders through every entry point with
//! an empty fallback path; a preset that names a `file:` renders from that
//! file, so a name that is not on disk is refused with the track, the preset
//! and the path, identically from every entry point.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::render_cache::{RangeRenderRequest, RenderCache};

const RATE: u32 = 48_000;

/// A builtin-preset project that still declares a `@pattern`: the pattern
/// preview pass must not make a project that needs no bank ask for one.
const BUILTIN_WITH_PATTERN: &str = r#"@project
  tempo: 100
@preset pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {osc.a.waveform: 1, osc.a.level: 0.5, filter.cutoff: 2200Hz}
@pattern chords | preset: pad, instrument: synth/pad
  events:
    @0 A3 w mp, C4 w mp, E4 w mp
    @4 F3 w mp, A3 w mp, C4 w mp
@track pads | instrument: synth/pad, preset: pad
  clip: chords [1:1 - 3:1]
"#;

/// The reported shape: the track's instrument names a SoundFont instrument and
/// the preset it plays is a `source: soundfont` bank whose file is not on disk.
const BROKEN_KIT: &str = r#"@project
  tempo: 110
@preset kit
  source: soundfont
  program: 0
  file: "media/missing.sf2"
@preset lead
  source: builtin
  plugin: "builtin.open_synth"
@pattern groove | instrument: drums/acoustic_kit, preset: kit
  hits:
    kick  1:0 ff
    snare 2:0 f
@track drums | instrument: drums/acoustic_kit, preset: kit
  clip: groove [1:1 - 3:1]
@track lead | instrument: synth/lead_square, preset: lead
  @phrase hook [1:1 - 3:1]
    C4 q mf
    E4 q mf
"#;

fn temp_root(name: &str) -> PathBuf {
    oc_test_support::temp_dir("soundfont-resolution", name)
}

fn parse(source: &str) -> Project {
    oc_dsl::parse(source).unwrap()
}

fn cache_with_root(root: &Path) -> RenderCache {
    let mut cache = RenderCache::new();
    cache.set_resource_root(Some(root.to_path_buf()));
    cache
}

/// The three entry points the app and the CLI use, over one project, with the
/// fallback bank path the caller would pass when no SoundFont is configured.
fn verdicts(project: &Project, root: &Path) -> [Result<Vec<f32>, String>; 3] {
    let soundfont = Path::new("");
    let mut full = cache_with_root(root);
    let full_audio = full.render_full(project, soundfont, RATE, &mut HashMap::new());
    let mut streaming = cache_with_root(root);
    let streaming_audio =
        streaming.render_incremental(project, soundfont, RATE, &mut HashMap::new());
    let mut ranged = cache_with_root(root);
    let range_audio = ranged
        .render_range(
            project,
            soundfont,
            RATE,
            &mut HashMap::new(),
            RangeRenderRequest {
                start_sample: 0,
                end_sample: RATE as u64 * 2,
                tracks: None,
                stems: false,
            },
        )
        .map(|output| output.audio);
    [full_audio, streaming_audio, range_audio]
}

#[test]
fn a_builtin_project_with_patterns_renders_through_every_entry_point() {
    let root = temp_root("builtin");
    let project = parse(BUILTIN_WITH_PATTERN);
    let [full, streaming, ranged] = verdicts(&project, &root);

    let full = full.expect("the export entry point renders a builtin project");
    let streaming = streaming.expect("the streaming entry point renders a builtin project");
    let ranged = ranged.expect("a full-mix range renders a builtin project");
    assert!(full.iter().any(|sample| *sample != 0.0), "silent render");
    assert_eq!(full, streaming, "export and streaming bytes differ");
    assert_eq!(ranged, full[..ranged.len()], "the range is not a slice");
}

#[test]
fn a_declared_bank_that_is_missing_is_refused_by_name_from_every_entry_point() {
    let root = temp_root("missing");
    let project = parse(BROKEN_KIT);
    let [full, streaming, ranged] = verdicts(&project, &root);

    let messages: Vec<String> = [full, streaming, ranged]
        .into_iter()
        .map(|verdict| verdict.expect_err("a missing bank must refuse"))
        .collect();
    assert_eq!(messages[0], messages[1], "entry points disagree");
    assert_eq!(messages[0], messages[2], "entry points disagree");
    let message = &messages[0];
    assert!(message.contains("Track 'drums'"), "{message}");
    assert!(message.contains("needs a SoundFont"), "{message}");
    assert!(message.contains("preset 'kit'"), "{message}");
    assert!(message.contains("media/missing.sf2"), "{message}");
    // An empty path reaching the loader would surface as `: No such file or
    // directory`, which names nothing the project can act on.
    assert!(!message.contains(": No such file"), "{message}");
    assert!(
        message.contains("builtin.open_synth"),
        "the refusal must name a way out: {message}"
    );
}

#[test]
fn a_bank_path_the_launcher_never_supplied_still_names_the_track() {
    // The legacy fallback path takes no preset file; an empty fallback is a
    // missing bank, not an I/O failure on a path the project named.
    let mut project = parse(BROKEN_KIT);
    for preset in &mut project.presets {
        preset.file = None;
    }
    let error = oc_engine::render::render_to_buffer(&project, Path::new(""), RATE)
        .expect_err("no bank and no builtin preset must refuse");
    assert!(error.contains("Track 'drums'"), "{error}");
    assert!(error.contains("needs a SoundFont"), "{error}");
}
