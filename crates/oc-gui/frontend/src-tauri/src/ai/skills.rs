//! Skills: short reference notes the model reads on demand.
//!
//! A skill is a Markdown file with YAML frontmatter (`id, title, when,
//! category, version, tools[]`). The bundled pack is compiled in with
//! `include_str!`; `~/.opencompose/ai/skills/<id>.md` adds user skills and may
//! not redefine a bundled id. The index is built once and never carries a body;
//! `read` loads one body. An unusable file is skipped with a warning, and a
//! skill never grants a tool: a name in `tools[]` that the panel does not offer
//! is dropped with a warning.
mod curriculum;

use crate::private_fs;
use crate::tool_registry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const DIR_NAME: &str = "skills";
const MAX_ID_CHARS: usize = 64;
const MAX_TITLE_CHARS: usize = 80;
const MAX_WHEN_CHARS: usize = 200;
const MAX_CATEGORY_CHARS: usize = 32;
const MAX_VERSION: u32 = 1_000_000;
const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: usize = MAX_BODY_BYTES + 4 * 1024;

/// One body-less index row, as `list_skills` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillEntry {
    pub id: String,
    pub title: String,
    pub when: String,
    pub category: String,
    pub version: u32,
    /// `bundled` when compiled into the binary, `user` when read from disk.
    pub source: &'static str,
}

/// One skill body, as `read_skill` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skill {
    pub id: String,
    pub title: String,
    pub version: u32,
    pub body: String,
    pub tools: Vec<String>,
}

/// The machine-read frontmatter. Unknown keys are ignored: user files are
/// hand-written, and a stray key must not hide the skill.
#[derive(Debug, Deserialize)]
struct Frontmatter {
    id: String,
    title: String,
    when: String,
    category: String,
    version: u32,
    #[serde(default)]
    tools: Option<Vec<String>>,
}

/// Where a body lives. The index keeps only this, never the body itself.
#[derive(Debug, Clone)]
enum Origin {
    Bundled(&'static str),
    User(PathBuf),
}

#[derive(Debug, Clone)]
struct Registered {
    entry: SkillEntry,
    tools: Vec<String>,
    origin: Origin,
}

/// Each entry embeds one file and uses its stem as the lookup ID.
macro_rules! bundled_skills {
    ($($id:literal),* $(,)?) => {
        &[$(($id, include_str!(concat!("skills/builtin/", $id, ".md")))),*]
    };
}

const BUNDLED: &[(&str, &str)] = bundled_skills![
    "align-and-stretch-audio",
    "align-layered-transients",
    "arrange-a-future-bass-drop",
    "arrange-afrobeats-guitar",
    "arrange-amapiano-keys-and-log-drum",
    "arrange-bossa-and-samba-grooves",
    "arrange-brass-sections",
    "arrange-plugg-and-rage-beats",
    "arrange-techno-variation",
    "audition-and-listen",
    "build-a-tempo-and-meter-map",
    "build-ambient-textures",
    "build-pop-chorus-transitions",
    "capture-algorithmic-patterns",
    "chop-jungle-breaks",
    "comp-vocal-takes",
    "design-and-verify-supersaw",
    "design-dnb-and-halftime-bass",
    "design-future-bass-wobble",
    "design-hard-dance-kicks",
    "design-wave-and-hardwave-layers",
    "dsl-subtree-paths",
    "edit-audio-clip-seams",
    "edit-hyperpop-vocals",
    "edit-vaporwave-sample-collage",
    "effects-and-devices",
    "fix-a-render-error",
    "flip-boom-bap-chops",
    "gain-stage-a-mix",
    "layer-cinematic-percussion",
    "map-a-multisampled-instrument",
    "master-for-delivery",
    "measure-and-compare",
    "mix-acoustic-folk",
    "mix-dub-techno-returns",
    "place-rap-vocal-throws",
    "playable-instruments",
    "prepare-a-stem-handoff",
    "prepare-loopable-score-stems",
    "prepare-project-media",
    "produce-intimate-indie-vocals",
    "produce-jazz-combos",
    "production-curriculum",
    "program-clave-and-montuno",
    "program-drill-808-slides",
    "program-house-groove",
    "program-independent-pitch-gestures",
    "program-jersey-and-footwork",
    "program-phonk-cowbell-and-bass",
    "program-playable-guitar",
    "program-reggaeton-dembow",
    "program-sparse-regional-club",
    "program-two-step-garage",
    "program-virtual-string-sections",
    "rebuild-a-reference-passage",
    "resample-glitch-edits",
    "route-groups-and-returns",
    "score-to-picture",
    "separated-vocal-mixing",
    "control-vocal-sibilance",
    "share-portable-synth-presets",
    "sequence-trance-and-psytrance",
    "shape-electroacoustic-depth",
    "shape-rock-guitar-tracks",
    "shape-transients-and-decay",
    "stack-vocal-harmonies",
    "transcribe-a-part",
    "triage-stem-artifacts",
    "tune-and-align-vocal-doubles",
    "unmask-kick-and-bass",
    "warp-drum-loops-without-flattening-groove",
    "write-mix-automation",
    "writing-notes",
];

/// Directory holding user skills.
pub fn path() -> PathBuf {
    private_fs::config_dir().join("ai").join(DIR_NAME)
}

/// The body-less index: bundled first, then valid user files in file-name order.
pub fn list() -> Vec<SkillEntry> {
    INDEX.iter().map(|skill| skill.entry.clone()).collect()
}

/// One body by exact id. Unknown ids and unreadable bodies are errors, never a
/// panic: the index can outlive the file it was built from.
pub fn read(id: &str) -> Result<Skill, String> {
    read_from(&INDEX, id)
}

/// Skills index as JSON.
pub fn list_value() -> serde_json::Value {
    serde_json::to_value(list()).unwrap_or(serde_json::Value::Null)
}

/// Read a skill or curriculum topic as JSON.
pub fn read_value(id: &str, topic: Option<&str>) -> Result<serde_json::Value, String> {
    let skill = match topic {
        None => read(id)?,
        Some(topic) if id == curriculum::ID => curriculum::read_topic(topic)?,
        Some(_) => return Err("topic is only supported by the production-curriculum skill".into()),
    };
    serde_json::to_value(skill).map_err(|error| error.to_string())
}

fn read_from(skills: &[Registered], id: &str) -> Result<Skill, String> {
    let registered = skills
        .iter()
        .find(|skill| skill.entry.id == id)
        .ok_or_else(|| format!("unknown skill '{id}'; call list_skills for the exact ids"))?;
    let text = match &registered.origin {
        Origin::Bundled(text) => (*text).to_owned(),
        Origin::User(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("skill '{id}': could not read {}: {error}", path.display()))?,
    };
    let (_, body) = split(text.as_bytes())
        .ok_or_else(|| format!("skill '{id}': the file no longer has YAML frontmatter"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "skill '{id}': the body is {} bytes; the limit is {MAX_BODY_BYTES}",
            body.len()
        ));
    }
    let body = std::str::from_utf8(body)
        .map_err(|_| format!("skill '{id}': the body is not valid UTF-8"))?;
    Ok(Skill {
        id: registered.entry.id.clone(),
        title: registered.entry.title.clone(),
        version: registered.entry.version,
        body: body.trim().to_owned(),
        tools: registered.tools.clone(),
    })
}

/// The panel's tool names, in registry order. A skill asks; the panel decides.
fn offered_tools(names: &[String], owner: &str, warnings: &mut Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        if !tool_registry::is_panel_tool(name) {
            warnings.push(format!(
                "skill '{owner}': tool '{name}' is not offered by the panel; the name was ignored"
            ));
        } else if !kept.contains(name) {
            kept.push(name.clone());
        }
    }
    kept
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let starts_lower = matches!(chars.next(), Some(c) if c.is_ascii_lowercase());
    starts_lower
        && (2..=MAX_ID_CHARS).contains(&id.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_text(kind: &str, value: &str, max_chars: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("the {kind} is empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("the {kind} is longer than {max_chars} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("the {kind} has a control character"));
    }
    Ok(())
}

/// Splits a file into frontmatter and body bytes. Only the frontmatter has to be
/// UTF-8, so a file with a broken body still lists; `read` reports the body.
fn split(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let tail = bytes
        .strip_prefix(b"---\n")
        .or_else(|| bytes.strip_prefix(b"---\r\n"))?;
    let mut start = 0;
    while start < tail.len() {
        let end = tail[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(tail.len(), |offset| start + offset);
        let mut line = &tail[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line == b"---" {
            let body = if end < tail.len() {
                &tail[end + 1..]
            } else {
                &tail[end..]
            };
            return Some((&tail[..start], body));
        }
        start = end + 1;
    }
    None
}

/// A parsed frontmatter: the index row, the panel tools it may refer to, and
/// the warning per tool name that was dropped.
struct Parsed {
    entry: SkillEntry,
    tools: Vec<String>,
    warnings: Vec<String>,
}

/// Parses one file's frontmatter. The body is not touched.
fn parse_file(bytes: &[u8], stem: &str, source: &'static str) -> Result<Parsed, String> {
    let (front, _) = split(bytes).ok_or("no YAML frontmatter (expected a leading '---' line)")?;
    let front = std::str::from_utf8(front).map_err(|_| "the frontmatter is not valid UTF-8")?;
    let meta: Frontmatter = serde_yaml::from_str(front)
        .map_err(|error| format!("the frontmatter is not YAML: {error}"))?;
    if meta.id != stem {
        return Err(format!(
            "id '{}' does not match the file name '{stem}.md'",
            meta.id
        ));
    }
    if !valid_id(&meta.id) {
        return Err(format!(
            "id '{}' is not lowercase letters, digits and '-' (2..{MAX_ID_CHARS} characters, starting with a letter)",
            meta.id
        ));
    }
    valid_text("title", &meta.title, MAX_TITLE_CHARS)?;
    valid_text("when", &meta.when, MAX_WHEN_CHARS)?;
    valid_text("category", &meta.category, MAX_CATEGORY_CHARS)?;
    if meta.version == 0 || meta.version > MAX_VERSION {
        return Err(format!(
            "version {} is outside 1..{MAX_VERSION}",
            meta.version
        ));
    }
    let mut warnings = Vec::new();
    let tools = offered_tools(&meta.tools.unwrap_or_default(), &meta.id, &mut warnings);
    Ok(Parsed {
        entry: SkillEntry {
            id: meta.id,
            title: meta.title,
            when: meta.when,
            category: meta.category,
            version: meta.version,
            source,
        },
        tools,
        warnings,
    })
}

fn bundled_registered() -> (Vec<Registered>, Vec<String>) {
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    for (id, text) in BUNDLED {
        match parse_file(text.as_bytes(), id, "bundled") {
            Ok(parsed) => {
                warnings.extend(parsed.warnings);
                skills.push(Registered {
                    entry: parsed.entry,
                    tools: parsed.tools,
                    origin: Origin::Bundled(text),
                });
            }
            Err(error) => warnings.push(format!("bundled skill '{id}' was ignored: {error}")),
        }
    }
    skills.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
    (skills, warnings)
}

/// Reads every `*.md` in `dir`. A file that cannot be used is skipped with a
/// warning; a regular file is the only kind accepted, so a symlink can never
/// point the loader at an arbitrary path.
fn user_registered(dir: &Path, taken: &mut BTreeSet<String>) -> (Vec<Registered>, Vec<String>) {
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (skills, warnings),
        Err(error) => {
            warnings.push(format!(
                "could not read {}: {error}; using the bundled skills only",
                dir.display()
            ));
            return (skills, warnings);
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => files.push(path),
            Ok(kind) if kind.is_symlink() => warnings.push(format!(
                "skill file '{}' is a symbolic link; only regular files are read",
                path.display()
            )),
            Ok(_) => {}
            Err(error) => warnings.push(format!("could not inspect '{}': {error}", path.display())),
        }
    }
    files.sort();
    for path in files {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            warnings.push(format!("'{}' has no usable file name", path.display()));
            continue;
        };
        let stem = stem.to_owned();
        let parsed = match load_user_file(&path, &stem) {
            Ok(parsed) => parsed,
            Err(error) => {
                warnings.push(format!("skill '{stem}' was ignored: {error}"));
                continue;
            }
        };
        if !taken.insert(parsed.entry.id.clone()) {
            warnings.push(format!(
                "skill id '{}' is already defined; '{}' was ignored",
                parsed.entry.id,
                path.display()
            ));
            continue;
        }
        warnings.extend(parsed.warnings);
        skills.push(Registered {
            entry: parsed.entry,
            tools: parsed.tools,
            origin: Origin::User(path),
        });
    }
    (skills, warnings)
}

fn load_user_file(path: &Path, stem: &str) -> Result<Parsed, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("could not read it: {error}"))?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(format!("the file is larger than {MAX_FILE_BYTES} bytes"));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("could not read it: {error}"))?;
    parse_file(&bytes, stem, "user")
}

/// Bundled pack plus the user directory. Never fails: a bad file is one
/// warning, not a startup error.
fn load_from(dir: &Path) -> (Vec<Registered>, Vec<String>) {
    let (mut skills, mut warnings) = bundled_registered();
    let mut taken: BTreeSet<String> = skills.iter().map(|s| s.entry.id.clone()).collect();
    let (user, user_warnings) = user_registered(dir, &mut taken);
    skills.extend(user);
    warnings.extend(user_warnings);
    (skills, warnings)
}

static INDEX: LazyLock<Vec<Registered>> = LazyLock::new(|| {
    // Tests install their own directory; never read the developer's home file.
    if cfg!(test) {
        return bundled_registered().0;
    }
    let (skills, warnings) = load_from(&path());
    for warning in &warnings {
        log::warn!("[ai] {warning}");
    }
    skills
});

#[cfg(test)]
mod tests {
    use super::*;

    fn frontmatter(id: &str, tools: &str, version: u32) -> String {
        [
            "---".to_owned(),
            format!("id: {id}"),
            "title: A test skill".to_owned(),
            "when: Use it when the test needs a skill.".to_owned(),
            "category: test".to_owned(),
            format!("version: {version}"),
            format!("tools: {tools}"),
            "---".to_owned(),
            format!("Body of {id}."),
            "BODY-MARKER".to_owned(),
            String::new(),
        ]
        .join("\n")
    }

    fn install(name: &str, files: &[(&str, String)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oc-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    fn ids(skills: &[Registered]) -> Vec<&str> {
        skills.iter().map(|skill| skill.entry.id.as_str()).collect()
    }

    fn user_ids(skills: &[Registered]) -> Vec<&str> {
        skills
            .iter()
            .filter(|skill| skill.entry.source == "user")
            .map(|skill| skill.entry.id.as_str())
            .collect()
    }

    fn bundled_body(id: &str) -> &'static str {
        BUNDLED
            .iter()
            .find(|(bundled, _)| *bundled == id)
            .map(|(_, text)| *text)
            .unwrap()
    }

    #[test]
    fn the_bundled_pack_parses_and_lists_every_bundled_skill() {
        let (skills, warnings) = bundled_registered();
        assert!(warnings.is_empty(), "{warnings:?}");
        let ids = ids(&skills);
        for expected in [
            "design-future-bass-wobble",
            "design-and-verify-supersaw",
            "separated-vocal-mixing",
            "audition-and-listen",
            "dsl-subtree-paths",
            "effects-and-devices",
            "fix-a-render-error",
            "measure-and-compare",
            "playable-instruments",
            "transcribe-a-part",
            "writing-notes",
        ] {
            assert!(ids.contains(&expected), "{expected} is missing: {ids:?}");
        }
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ai/skills/builtin");
        let mut file_ids: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        file_ids.sort_unstable();
        assert_eq!(
            ids, file_ids,
            "the bundled index must include every skill file"
        );
        assert_eq!(
            ids.len(),
            BUNDLED.len(),
            "the index dropped a bundled skill"
        );
        for skill in &skills {
            assert_eq!(skill.entry.source, "bundled");
            // A bundled body may be revised; what matters is that it declares one.
            assert!(skill.entry.version >= 1);
            assert!(!skill.entry.when.is_empty());
            assert!(!skill.tools.is_empty(), "{} names no tool", skill.entry.id);
            for name in &skill.tools {
                assert!(
                    tool_registry::is_panel_tool(name),
                    "{} names '{}' which the panel does not offer",
                    skill.entry.id,
                    name
                );
            }
            let (_, body) = split(bundled_body(&skill.entry.id).as_bytes()).unwrap();
            let body = std::str::from_utf8(body).unwrap();
            let lines = body.trim().lines().count();
            assert!(
                (20..=40).contains(&lines),
                "{} has {lines} body lines; the pack keeps 20-40",
                skill.entry.id
            );
            assert!(
                !body.contains("http"),
                "{} carries an external link",
                skill.entry.id
            );
            assert!(
                !body.contains("/Users/") && !body.contains("/home/"),
                "{} carries a machine path",
                skill.entry.id
            );
        }
    }

    #[test]
    fn musical_guidance_is_registered_and_separates_evidence_from_claims() {
        let (skills, warnings) = bundled_registered();
        assert!(warnings.is_empty(), "{warnings:?}");
        let saw = read_from(&skills, "design-and-verify-supersaw").unwrap();
        assert!(saw.body.contains("osc.a.waveform=1"));
        assert!(saw.body.contains("renaming"));
        assert!(saw.body.contains("16 kHz mono audition cannot certify"));
        let vocal = read_from(&skills, "separated-vocal-mixing").unwrap();
        assert!(vocal.body.contains("Source separation can leave reverb"));
        assert!(
            vocal
                .body
                .contains("Dedicated dereverb requires a restoration plug-in")
        );
        assert!(vocal.body.contains("listening acceptance remains pending"));
        let measure = read_from(&skills, "measure-and-compare").unwrap();
        assert!(
            measure
                .body
                .contains("Nearby scores do not prove inaudibility")
        );
        assert!(
            !measure
                .body
                .contains("the problem is arrangement or performance")
        );
    }

    #[test]
    fn wobble_example_uses_real_controls_and_loads_the_intended_route() {
        use oc_dsl::ast::ParamValue;
        use oc_engine::dsp::schema::synth;
        let (skills, warnings) = bundled_registered();
        assert!(warnings.is_empty(), "{warnings:?}");
        let skill = read_from(&skills, "design-future-bass-wobble").unwrap();
        let json = skill
            .body
            .split("```json\n")
            .nth(1)
            .unwrap()
            .split("```")
            .next()
            .unwrap();
        let values: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(json).unwrap();
        let params: Vec<_> = values
            .iter()
            .map(|(key, value)| {
                assert!(
                    synth::find_param(key).is_some() || synth::find_family_field(key).is_some(),
                    "unknown control {key}"
                );
                let value = ParamValue::Number(value.as_f64().unwrap());
                crate::device_catalog::validate_synth_value(key, &value).unwrap();
                (key.clone(), value)
            })
            .collect();
        let patch = oc_synth::synth::load_project_patch(&params, None).unwrap();
        assert!(patch.lfo_sync && patch.lfo_retrigger);
        assert_eq!(patch.lfo_sync_div, Some(25));
        assert_eq!(oc_synth::synth::LFO_SYNC_DIVISIONS[22], 1.0);
        assert_eq!(oc_synth::synth::LFO_SYNC_DIVISIONS[25], 0.5);
        assert_eq!(oc_synth::synth::LFO_SYNC_DIVISIONS[28], 0.25);
        assert_eq!(patch.mod_routes[0].source, oc_synth::synth::ModSource::Lfo);
        assert_eq!(
            patch.mod_routes[0].destination,
            oc_synth::synth::ModDestination::FilterCutoff
        );
        assert!((patch.mod_routes[0].amount - 0.12).abs() < 1e-6);
        assert!(patch.mod_routes[0].bipolar && patch.gain > 0.0);
    }

    #[test]
    fn read_returns_the_body_and_the_tools_and_charges_nothing_else() {
        let dir = install(
            "read",
            &[(
                "mine.md",
                frontmatter("mine", "[tracks_list, render_range]", 3),
            )],
        );
        let (skills, warnings) = load_from(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let loaded = read_from(&skills, "mine").unwrap();
        assert_eq!(loaded.body, "Body of mine.\nBODY-MARKER");
        assert_eq!(loaded.tools, vec!["tracks_list", "render_range"]);
        assert_eq!(loaded.version, 3);
        let value = serde_json::to_value(&loaded).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["body", "id", "title", "tools", "version"]);
        assert!(read_from(&skills, "nope").is_err());
        assert!(read_from(&skills, "../mine").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_user_skill_cannot_take_a_bundled_id() {
        let dir = install(
            "duplicate",
            &[(
                "dsl-subtree-paths.md",
                frontmatter("dsl-subtree-paths", "[tracks_list]", 1),
            )],
        );
        let (skills, warnings) = load_from(&dir);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("already defined")),
            "{warnings:?}"
        );
        let bundled = read_from(&skills, "dsl-subtree-paths").unwrap();
        assert!(!bundled.body.contains("Body of dsl-subtree-paths"));
        assert_eq!(
            skills
                .iter()
                .filter(|skill| skill.entry.id == "dsl-subtree-paths")
                .count(),
            1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unknown_tool_name_is_dropped_with_a_warning_not_fatal() {
        let dir = install(
            "unknown-tool",
            &[(
                "mine.md",
                frontmatter("mine", "[tracks_list, no_such_tool]", 1),
            )],
        );
        let (skills, warnings) = load_from(&dir);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no_such_tool"), "{warnings:?}");
        assert_eq!(
            read_from(&skills, "mine").unwrap().tools,
            vec!["tracks_list"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_skill_cannot_smuggle_a_panel_excluded_tool() {
        let dir = install(
            "smuggle",
            &[(
                "mine.md",
                frontmatter(
                    "mine",
                    "[render_offline, midi_import_apply, edit_subtree]",
                    1,
                ),
            )],
        );
        let (skills, warnings) = load_from(&dir);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("render_offline")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("midi_import_apply")),
            "{warnings:?}"
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("edit_subtree")),
            "{warnings:?}"
        );
        assert_eq!(
            read_from(&skills, "mine").unwrap().tools,
            vec!["edit_subtree"]
        );
        // The granted set stays inside the toolset surface, whatever a file says.
        let all: Vec<String> = crate::ai::toolsets::bundled()
            .into_iter()
            .map(|set| set.id)
            .collect();
        let offered = crate::ai::toolsets::resolve(&all);
        for skill in &skills {
            for name in &skill.tools {
                assert!(
                    offered.contains(name),
                    "{name} is outside the panel surface"
                );
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_never_reads_a_body() {
        let mut broken = frontmatter("broken", "[tracks_list]", 1).into_bytes();
        broken.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80]);
        let dir = std::env::temp_dir().join(format!("oc-skills-body-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.md"), &broken).unwrap();
        std::fs::write(dir.join("ok.md"), frontmatter("ok", "[tracks_list]", 1)).unwrap();

        let (skills, warnings) = load_from(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let listed = serde_json::to_string(&ids(&skills)).unwrap();
        assert!(listed.contains("broken") && listed.contains("ok"));
        assert!(!listed.contains("BODY-MARKER"), "the index carried a body");
        assert!(read_from(&skills, "ok").is_ok());
        let error = read_from(&skills, "broken").unwrap_err();
        assert!(error.contains("UTF-8"), "{error}");

        // An index built once still lists after the body becomes unreadable.
        std::fs::remove_file(dir.join("ok.md")).unwrap();
        assert!(ids(&skills).contains(&"ok"));
        assert!(read_from(&skills, "ok").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_file_is_skipped_with_a_warning_while_the_rest_list() {
        let dir = install(
            "broken",
            &[
                ("ok.md", frontmatter("ok", "[tracks_list]", 1)),
                ("nofront.md", "just prose, no frontmatter\n".to_owned()),
                ("badname.md", frontmatter("different", "[tracks_list]", 1)),
                ("Bad_ID.md", frontmatter("Bad_ID", "[tracks_list]", 1)),
                (
                    "badyaml.md",
                    "---\nid: badyaml\ntitle: [unclosed\n---\nbody\n".to_owned(),
                ),
                (
                    "badversion.md",
                    frontmatter("badversion", "[tracks_list]", 0),
                ),
            ],
        );
        let (skills, warnings) = load_from(&dir);
        assert_eq!(user_ids(&skills), vec!["ok"], "{:?}", user_ids(&skills));
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("frontmatter"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("does not match"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not lowercase"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("YAML")));
        assert!(warnings.iter().any(|warning| warning.contains("version 0")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcribe_a_part_teaches_the_payload_and_the_comma_form() {
        let skill = read("transcribe-a-part").expect("the new skill reads");
        assert_eq!(
            skill.tools,
            vec![
                "audition_range",
                "analyze_notes",
                "render_range",
                "edit_subtree",
                "separate_stems"
            ]
        );
        for name in &skill.tools {
            assert!(
                tool_registry::is_panel_tool(name),
                "{name} is not offered by the panel"
            );
        }
        for anchor in [
            "analyze_notes",
            "audition_range",
            "start_s",
            "midi",
            "cents",
            "confidence",
            "polyphonic",
            "warnings",
            "start_beat",
            "duration_beats",
            "bar",
            "suggested_duration",
            "quantize: true",
            "@0.0 G4 q mf, G5 q mf",
            "single-voice",
            "separate_stems",
        ] {
            assert!(skill.body.contains(anchor), "the body lost {anchor:?}");
        }
        assert!(
            skill.body.contains("do not invent a token"),
            "the body has to forbid inventing the duration token"
        );
        assert_eq!(skill.version, 7);
        assert!(
            skill.body.contains("after one full reading"),
            "the body lost the write-early rule"
        );
        let lines = skill.body.lines().count();
        assert!(
            (20..=40).contains(&lines),
            "transcribe-a-part has {lines} body lines; the pack keeps 20-40"
        );
    }

    #[test]
    fn measure_and_compare_points_note_questions_at_analyze_notes() {
        let skill = read("measure-and-compare").expect("the skill reads");
        assert!(
            skill.tools.iter().any(|tool| tool == "analyze_notes"),
            "the skill does not offer analyze_notes: {:?}",
            skill.tools
        );
        for anchor in ["analyze_notes", "estimates", "not authoritative MIDI"] {
            assert!(skill.body.contains(anchor), "the body lost {anchor:?}");
        }
        let lines = skill.body.lines().count();
        assert!(
            (20..=40).contains(&lines),
            "measure-and-compare has {lines} body lines; the pack keeps 20-40"
        );
    }

    #[test]
    fn the_automation_skill_teaches_the_lane_line() {
        let skill = read("write-mix-automation").expect("the bundled skill reads");
        assert_eq!(skill.version, 3);
        for anchor in [
            "automation: volume",
            "{4:1=-6dB, 4:4=0dB}",
            "bar:beat=value",
            "The braces are part of",
        ] {
            assert!(skill.body.contains(anchor), "the body lost {anchor:?}");
        }
        let lines = skill.body.lines().count();
        assert!(
            (20..=40).contains(&lines),
            "write-mix-automation has {lines} body lines; the pack keeps 20-40"
        );

        // The example is a lane the parser accepts, not prose shaped like one.
        let project = "@ocp \"0.4.0\"\n\n@project \"lane\"\n  tempo: 120\n  \
                       time_signature: 4/4\n\n@track keys | instrument: synth/keys\n  \
                       automation: volume [3:1 - 5:1] {4:1=-6dB, 4:4=0dB}\n";
        oc_dsl::parse(project).expect("the skill's lane example parses");
    }

    #[test]
    fn audition_and_listen_teaches_the_assisted_separation_path() {
        let skill = read("audition-and-listen").expect("the bundled skill reads");
        assert_eq!(skill.version, 7);
        assert!(
            skill.tools.contains(&"separate_stems".to_owned()),
            "{:?}",
            skill.tools
        );
        for name in &skill.tools {
            assert!(
                tool_registry::is_panel_tool(name),
                "{name} is not offered by the panel"
            );
        }
        for anchor in [
            "separate_stems",
            "method",
            "Band filtering is not source separation",
            "vocals/accompaniment",
            "neither promises a dry vocal",
            "16 kHz mono cannot certify",
            "failed stage explicitly",
        ] {
            assert!(skill.body.contains(anchor), "the body lost {anchor:?}");
        }
        let lines = skill.body.lines().count();
        assert!(
            (20..=40).contains(&lines),
            "audition-and-listen has {lines} body lines; the pack keeps 20-40"
        );
    }

    /// The one fenced example in a bundled body, without the fence lines.
    fn fenced_example(body: &str) -> &str {
        let opening = "```ocp\n";
        let start = body
            .find(opening)
            .unwrap_or_else(|| panic!("no ```ocp example in the body"))
            + opening.len();
        let rest = &body[start..];
        let end = rest
            .find("\n```")
            .unwrap_or_else(|| panic!("the example fence is not closed"));
        &rest[..end]
    }

    #[test]
    fn effects_and_devices_quotes_the_refusals_it_answers() {
        let skill = read("effects-and-devices").expect("the new skill reads");
        assert_eq!(
            skill.tools,
            vec![
                "project_extract",
                "edit_subtree",
                "project_validate",
                "devices_catalog",
                "effects_add",
                "effects_set_param"
            ]
        );
        for name in &skill.tools {
            assert!(
                tool_registry::is_panel_tool(name),
                "{name} is not offered by the panel"
            );
        }
        // Every phrase the hint table maps to this skill, and the sentence each
        // refusal prints, so the manual cannot drift from the parser or the table.
        for anchor in [
            "expected effect_entry",
            "expected effect_type",
            "unknown effect type:",
            "expected EOI, top_level_block, track_content, or effect_entry",
            "invalid typed effect parameter",
            "has no parameter",
            "devices_catalog",
            "effects_add",
            "effects_set_param",
            "high_cut_freq",
            "low_cut_freq",
            "high_cut_enabled",
            "low_cut_enabled",
            "`eq`",
            "`dynamic_eq`",
        ] {
            assert!(skill.body.contains(anchor), "the body lost {anchor:?}");
        }
        let lines = skill.body.lines().count();
        assert!(
            (20..=40).contains(&lines),
            "effects-and-devices has {lines} body lines; the pack keeps 20-40"
        );
    }

    #[test]
    fn the_effects_and_devices_example_parses_and_validates() {
        let (_, body) = split(bundled_body("effects-and-devices").as_bytes()).unwrap();
        let body = std::str::from_utf8(body).unwrap();
        let example = fenced_example(body);
        // The manual's own block, inside the smallest project it is meant for.
        let source = format!("@project\n  tempo: 120\n  time_signature: 4/4\n\n{example}\n");
        let project = match oc_dsl::parse(&source) {
            Ok(project) => project,
            Err(error) => panic!("the manual's example does not parse: {error}\n{source}"),
        };
        if let Err(errors) = oc_dsl::validate(&project) {
            panic!("the manual's example does not validate: {errors:?}\n{source}");
        }
        // Two tracks, one cut each: the low-pass (high cut) and the high-pass.
        let effects: Vec<_> = project
            .tracks
            .iter()
            .map(|track| (track.name.as_str(), track.effects.as_slice()))
            .collect();
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert_eq!(effects[0].0, "reference");
        assert_eq!(effects[1].0, "reference_top");
        for (track, control) in [
            ("reference", "high_cut_enabled"),
            ("reference_top", "low_cut_enabled"),
        ] {
            let (_, chain) = effects
                .iter()
                .find(|(name, _)| *name == track)
                .unwrap_or_else(|| panic!("{track} is missing"));
            assert_eq!(chain.len(), 1, "{track} carries {} effects", chain.len());
            assert_eq!(
                chain[0].effect_type,
                oc_dsl::ast::EffectType::DynamicEq,
                "{track} does not use the device the manual names"
            );
            assert!(
                chain[0].params.iter().any(|(name, _)| name == control),
                "{track} lost its {control} control: {:?}",
                chain[0].params
            );
        }
    }

    #[test]
    fn the_module_index_has_the_bundled_pack_and_no_user_file_in_tests() {
        // `INDEX` must never read the developer's home directory under test.
        let ids: Vec<String> = list().into_iter().map(|entry| entry.id).collect();
        assert!(ids.len() >= 6);
        assert!(
            ids.iter()
                .all(|id| BUNDLED.iter().any(|(bundled, _)| bundled == id))
        );
        for entry in list_value().as_array().unwrap() {
            let mut keys: Vec<&str> = entry
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["category", "id", "source", "title", "version", "when"]
            );
        }
    }

    #[test]
    fn writing_notes_lists_every_error_phrase_it_answers() {
        // The phrases the mapping table claims `writing-notes` answers, so the
        // manual cannot drift from the table.
        const PHRASES: &[&str] = &[
            "Parse error",
            "Syntax error at line",
            "trailing comma",
            "comma list",
            "chord",
            "rest is not a chord member",
            "Unknown pattern reference",
        ];
        let (_, body) = split(bundled_body("writing-notes").as_bytes()).unwrap();
        let body = std::str::from_utf8(body).unwrap();
        for phrase in PHRASES {
            assert!(
                body.contains(phrase),
                "writing-notes does not list {phrase:?}, so the mapping drifts"
            );
        }
        let lines = body.trim().lines().count();
        assert!(
            (20..=40).contains(&lines),
            "writing-notes has {lines} body lines; the pack keeps 20-40"
        );
        // The syntax the manual teaches stays in it.
        for anchor in [
            "@<beats>",
            "canonical",
            "clip:",
            "@pattern",
            "rest <duration>",
            "bar line",
            "[1:1:0 - 5:1:0]",
            "pedal down",
            "pedal up",
        ] {
            assert!(body.contains(anchor), "writing-notes lost {anchor:?}");
        }
    }

    #[test]
    fn the_render_and_path_skills_quote_the_classes_the_table_names() {
        for (skill, class) in [
            ("fix-a-render-error", "output directory "),
            ("fix-a-render-error", "cannot be used as an effect"),
            ("fix-a-render-error", "refused: "),
            ("fix-a-render-error", "unknown tool '"),
            ("playable-instruments", "needs a SoundFont"),
            ("playable-instruments", "preset '"),
            ("dsl-subtree-paths", "Unknown path segment: "),
            ("dsl-subtree-paths", "Unsupported replace path"),
            ("dsl-subtree-paths", "Invalid track child for replace"),
        ] {
            let (_, body) = split(bundled_body(skill).as_bytes()).unwrap();
            let body = std::str::from_utf8(body).unwrap();
            let class = class.trim_end();
            assert!(
                body.contains(class),
                "{skill} does not quote {class:?}, so the table drifts"
            );
        }
    }
}
