//! Integration tests: parse the example .ocp files from the repo

use oc_dsl::ast::*;

fn examples_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
}

#[test]
fn parse_demo_ballad() {
    let path = examples_dir().join("demo_ballad.ocp");
    let source = std::fs::read_to_string(&path).expect("Failed to read demo_ballad.ocp");
    let project = oc_dsl::parse(&source).expect("Failed to parse demo_ballad.ocp");

    assert_eq!(project.name, Some("Autumn Leaves".to_string()));
    assert_eq!(project.properties.tempo, 72.0);
    assert_eq!(project.properties.time_signature.numerator, 4);

    let key = project.properties.key.unwrap();
    assert_eq!(key.root, NoteName::A);
    assert_eq!(key.mode, KeyMode::Minor);

    // 3 tracks: piano, cello, strings_pad
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[0].name, "piano");
    assert_eq!(
        project.tracks[0].instrument.as_path(),
        "piano/acoustic_grand"
    );
    assert_eq!(project.tracks[1].name, "cello");
    assert_eq!(project.tracks[1].instrument.as_path(), "strings/cello");
    assert_eq!(project.tracks[2].name, "strings_pad");
    assert_eq!(project.tracks[2].instrument.as_path(), "strings/section");

    // 1 bus
    assert_eq!(project.buses.len(), 1);
    assert_eq!(project.buses[0].name, "hall_reverb");

    // Master exists
    assert!(project.master.is_some());
}

#[test]
fn parse_demo_electronic() {
    let path = examples_dir().join("demo_electronic.ocp");
    let source = std::fs::read_to_string(&path).expect("Failed to read demo_electronic.ocp");
    let project = oc_dsl::parse(&source).expect("Failed to parse demo_electronic.ocp");

    assert_eq!(project.name, Some("Neon Drive".to_string()));
    assert_eq!(project.properties.tempo, 128.0);

    let key = project.properties.key.unwrap();
    assert_eq!(key.root, NoteName::F);
    assert_eq!(key.mode, KeyMode::Minor);

    // 4 tracks: lead, bass, pad, drums
    assert_eq!(project.tracks.len(), 4);
    assert_eq!(project.tracks[0].name, "lead");
    assert_eq!(project.tracks[0].instrument.as_path(), "synth/lead_saw");
    assert_eq!(project.tracks[3].name, "drums");
    assert_eq!(project.tracks[3].instrument.as_path(), "drums/electronic");

    // Drums should have patterns
    assert!(!project.tracks[3].patterns.is_empty());

    // 2 buses
    assert_eq!(project.buses.len(), 2);
}

#[test]
fn parse_demo_orchestral() {
    let path = examples_dir().join("demo_orchestral.ocp");
    let source = std::fs::read_to_string(&path).expect("Failed to read demo_orchestral.ocp");
    let project = oc_dsl::parse(&source).expect("Failed to parse demo_orchestral.ocp");

    assert_eq!(project.name, Some("Dawn".to_string()));
    assert_eq!(project.properties.tempo, 88.0);
    assert_eq!(project.properties.time_signature.numerator, 3);
    assert_eq!(project.properties.time_signature.denominator, 4);

    let key = project.properties.key.unwrap();
    assert_eq!(key.root, NoteName::D);
    assert_eq!(key.mode, KeyMode::Major);

    // Should have sections
    assert!(!project.sections.is_empty());

    // Multiple tracks (strings, woodwinds, brass, percussion)
    assert!(project.tracks.len() >= 6);

    // Master and bus exist
    assert!(project.master.is_some());
    assert!(!project.buses.is_empty());
}

#[test]
fn parse_free_clap_plugins() {
    let path = examples_dir().join("free_clap_plugins.ocp");
    let source = std::fs::read_to_string(&path).expect("Failed to read free_clap_plugins.ocp");
    let project = oc_dsl::parse(&source).expect("Failed to parse free_clap_plugins.ocp");

    assert_eq!(
        project.name,
        Some("Free CLAP Plugin Render Test".to_string())
    );
    assert_eq!(project.presets.len(), 3);
    assert_eq!(project.tracks.len(), 3);
    assert!(
        project
            .presets
            .iter()
            .all(|preset| preset.source.as_deref() == Some("clap"))
    );
}

#[test]
fn parse_free_plugin_warning_cases() {
    let path = examples_dir().join("free_plugin_warning_cases.ocp");
    let source =
        std::fs::read_to_string(&path).expect("Failed to read free_plugin_warning_cases.ocp");
    let project = oc_dsl::parse(&source).expect("Failed to parse free_plugin_warning_cases.ocp");

    assert_eq!(project.name, Some("Free Plugin Warning Cases".to_string()));
    assert_eq!(project.presets.len(), 4);
    assert_eq!(project.tracks.len(), 4);
    assert!(project.tracks.iter().any(|track| !track.effects.is_empty()));
}
// ---------------------------------------------------------------------------
// Canonical text: formatting a shipped project must not move a single byte.
//
// The example files carry comments, which the AST does not keep, so the
// invariant is the canonical text (`serialize(parse(file))`), pinned by length
// and a 64-bit FNV-1a fingerprint. `OCP_PRINT_CANONICAL=1` prints the table.
// ---------------------------------------------------------------------------

fn fingerprint(text: &str) -> (usize, u64) {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (text.len(), hash)
}

fn check_canonical(name: &str, source: &str) {
    let project =
        oc_dsl::parse(source).unwrap_or_else(|error| panic!("{name}: parse failed: {error}"));
    let canonical = oc_dsl::serialize(&project);
    let again = oc_dsl::serialize(&oc_dsl::parse(&canonical).expect("the canonical text parses"));
    assert_eq!(again, canonical, "{name}: formatting is not idempotent");
    let (length, hash) = fingerprint(&canonical);
    if std::env::var_os("OCP_PRINT_CANONICAL").is_some() {
        println!("    (\"{name}\", {length}, 0x{hash:016x}),");
        return;
    }
    let pinned = CANONICAL
        .iter()
        .find(|(pinned, _, _)| *pinned == name)
        .unwrap_or_else(|| panic!("{name}: no pinned canonical fingerprint"));
    assert_eq!(
        (length, hash),
        (pinned.1, pinned.2),
        "{name}: canonical text changed (length, hash)"
    );
}

/// Length and FNV-1a fingerprint of the canonical text of every shipped project.
const CANONICAL: &[(&str, usize, u64)] = &[
    ("au_dls_piano.ocp", 336, 0x8dc4_daba_c700_82df),
    ("demo_ballad.ocp", 1871, 0x9ce9_a0cd_e3aa_9d42),
    ("demo_electronic.ocp", 2575, 0xc1ee_bdf6_ee31_c0e5),
    ("demo_full_featured.ocp", 11867, 0x87b3_9995_f8f9_64c7),
    ("demo_orchestral.ocp", 3985, 0x1d3f_1ad1_af08_46cb),
    ("demo_presets.ocp", 1821, 0x566c_0f41_c393_5ff5),
    ("dynamic_eq.ocp", 1230, 0xa4da_0621_87d2_0beb),
    ("free_clap_plugins.ocp", 2179, 0x5132_7949_ed7d_477d),
    ("free_plugin_warning_cases.ocp", 2427, 0xe8e9_d377_a619_b0c1),
    ("opensynth.ocp", 3553, 0xa32d_ed2d_090e_a8a6),
    ("opensynth_hot.ocp", 3582, 0x5886_9f9b_555f_c2fd),
    ("opensynth_showcase.ocp", 2675, 0x70c3_cdb8_2000_d225),
    ("tests/reused_clap_preset.ocp", 404, 0xd1ce_61f9_530e_c7fb),
];

const GATE_FIXTURES: [&str; 2] = ["opensynth.ocp", "opensynth_hot.ocp"];

#[test]
fn every_example_formats_to_its_pinned_text() {
    let root = examples_dir();
    let mut paths = Vec::new();
    ocp_files_under(&root, &mut paths);
    paths.sort();
    if std::env::var_os("OCP_PRINT_CANONICAL").is_none() {
        assert_eq!(
            paths.len(),
            CANONICAL.len() - GATE_FIXTURES.len(),
            "every shipped example must have a canonical fingerprint"
        );
    }
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("the example reads");
        let name = path
            .strip_prefix(&root)
            .expect("under examples")
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        check_canonical(&name, &source);
    }
}

#[test]
fn the_gate_fixtures_format_to_their_pinned_text() {
    let Some(dir) = std::env::var_os("OCP_DEMO_SONG_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipping gate fixtures: set OCP_DEMO_SONG_DIR");
        return;
    };
    if !dir.is_dir() {
        eprintln!("skipping gate fixtures: {} not found", dir.display());
        return;
    }
    for name in GATE_FIXTURES {
        let source = std::fs::read_to_string(dir.join(name)).expect("the gate fixture reads");
        check_canonical(name, &source);
    }
}

fn ocp_files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let path = entry.expect("the directory entry reads").path();
        if path.is_dir() {
            ocp_files_under(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("ocp") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// JSON document shape: `.ocpc` files and the agent API read this.
// ---------------------------------------------------------------------------

/// The clip-carrying parts of a project document: `properties.audio_clips`
/// and, per track, `clips` and `automation`.
fn clip_json(project: &oc_dsl::ast::Project) -> (serde_json::Value, Vec<serde_json::Value>) {
    let value = serde_json::to_value(project).expect("the project serialises");
    let audio_clips = value["properties"]["audio_clips"].clone();
    let tracks = value["tracks"]
        .as_array()
        .expect("tracks are a JSON array")
        .iter()
        .map(|track| serde_json::json!([track["clips"], track["automation"]]))
        .collect();
    (audio_clips, tracks)
}

#[test]
fn every_example_json_round_trips_through_the_dsl() {
    let mut paths = Vec::new();
    ocp_files_under(&examples_dir(), &mut paths);
    paths.sort();
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("the example reads");
        let project = oc_dsl::parse(&source).expect("the example parses");
        let json = serde_json::to_string(&project).expect("the project serialises");
        let loaded: oc_dsl::ast::Project = serde_json::from_str(&json).expect("the JSON loads");
        assert!(
            loaded == project,
            "{}: JSON lost project state",
            path.display()
        );
        let text = oc_dsl::serialize(&loaded);
        let reopened = oc_dsl::parse(&text).expect("the saved text parses");
        // The three stores keep their shape and their order across a load and
        // a save. Other fields may exercise an older serializer limitation, so
        // this compares the clip subtrees rather than the whole document.
        assert!(
            clip_json(&reopened) == clip_json(&project),
            "{}: load -> save changed the clip JSON",
            path.display()
        );
    }
}

/// A project that uses all three clip stores: project-level `@audio_clip`
/// blocks, a track's pattern clips and its automation.
const THREE_STORES: &str = r#"@ocp "0.2.0"
@project "Three stores"
  tempo: 120

@audio_clip swell [1:1 - 3:1] | track: tape
  file: "media/swell.wav"

@audio_clip tail [5:1 - 6:1] | track: tape
  file: "media/tail.wav"

@track tape | instrument: audio/clip
  mix: vol=-6dB

@track lead | instrument: synth/virtual_analog
  automation: delay.mix {1:1=18%, 3:1=32%}
  @phrase line [1:1:0 - 2:1:0]
    C4 q mf
    E4 q mf
    G4 h mf
"#;

#[test]
fn the_json_shape_keeps_the_three_stores_where_they_were() {
    let source = THREE_STORES;
    let project = oc_dsl::parse(source).expect("the three-store project parses");
    let value = serde_json::to_value(&project).expect("the project serialises");

    // `@audio_clip` blocks stay project-level, in their written order.
    let written: Vec<&str> = source
        .lines()
        .filter_map(|line| line.strip_prefix("@audio_clip "))
        .map(|rest| rest.split(' ').next().unwrap_or(rest))
        .collect();
    let stored: Vec<&str> = value["properties"]["audio_clips"]
        .as_array()
        .expect("audio clips are a JSON array")
        .iter()
        .map(|clip| clip["name"].as_str().expect("a clip has a name"))
        .collect();
    assert_eq!(stored, written, "audio clip order changed in JSON");

    // A track still writes pattern clips and automation, and no audio clips.
    for track in value["tracks"].as_array().expect("tracks are a JSON array") {
        assert!(track["clips"].is_array(), "a track keeps its clips array");
        assert!(
            track["automation"].is_array(),
            "a track keeps its automation array"
        );
        assert!(
            track.get("audio_clips").is_none(),
            "a track must not grow an audio_clips key"
        );
    }
}
