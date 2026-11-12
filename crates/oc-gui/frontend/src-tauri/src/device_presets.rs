//! Device preset library: a factory file shipped with the app and a user file
//! under `~/.opencompose`, both in the same versioned schema.
//!
//! A preset names one built-in device and values for some of its parameters.
//! Every entry is checked against the device catalog before it is listed, saved
//! or applied: an unknown device, an unknown parameter or a value outside the
//! declared range is refused with the offending field named, never clamped.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use oc_dsl::ast;
use oc_engine::dsp::schema::{DeviceSchema, find_device};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::commands::set_effect_params_inner;
use crate::commands::shared::{ProjectSnapshot, snapshot};
use crate::private_fs::{config_dir, write_atomic};
use crate::state::{AppState, AppStateInner};

/// The only schema version this module reads or writes.
pub const SCHEMA: &str = "opencompose.device_presets.v1";

/// The factory presets shipped with the binary.
const FACTORY_DOCUMENT: &str = include_str!("../device-presets.json");

/// Where the user library lives; the audio-IO preferences use the same directory.
pub fn user_path() -> PathBuf {
    config_dir().join("device-presets.json")
}

/// One preset as a file stores it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetEntry {
    pub id: String,
    pub device: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// One authored sentence saying what the preset is for. Authored data, like
    /// the name: it is stored as written and never translated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
}

/// The document wrapper; `schema` is checked before the list is read.
#[derive(Debug, Serialize)]
struct PresetDocument<'a> {
    schema: &'a str,
    presets: &'a [PresetEntry],
}

/// One preset as the panel sees it, with its origin and shadowing state.
#[derive(Debug, Clone, Serialize)]
pub struct DevicePreset {
    pub id: String,
    pub device: String,
    pub name: String,
    pub category: String,
    /// `None` when the entry carries no one-line purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub params: BTreeMap<String, Value>,
    /// `factory` for the shipped set, `user` for `~/.opencompose`.
    pub source: String,
    /// True when a user preset replaced a factory one with the same id.
    pub shadows: bool,
    /// False for factory rows; the panel disables Delete with a reason.
    pub deletable: bool,
}

/// The merged library. A broken user file is reported, never guessed at.
#[derive(Debug, Clone, Serialize)]
pub struct PresetListing {
    pub presets: Vec<DevicePreset>,
    pub warnings: Vec<String>,
    /// Set by a save: whether an entry with that name was replaced.
    pub replaced: bool,
}

/// Parse and validate one document. `source` names the file in every message so
/// a refusal says where the offending field is.
fn parse_document(text: &str, source: &str) -> Result<Vec<PresetEntry>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("{source}: not valid JSON: {error}"))?;
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{source}: schema field missing; expected \"{SCHEMA}\""))?;
    if schema != SCHEMA {
        return Err(format!(
            "{source}: schema '{schema}' is not supported; expected \"{SCHEMA}\""
        ));
    }
    let items = value
        .get("presets")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{source}: presets field missing or not a list"))?;
    let mut entries = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let mut entry: PresetEntry = serde_json::from_value(item.clone())
            .map_err(|error| format!("{source}: presets[{index}]: {error}"))?;
        // An empty or whitespace-only intent says nothing, so it is absent.
        entry.intent = entry
            .intent
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        validate_entry(&entry, index, source)?;
        entries.push(entry);
    }
    Ok(entries)
}

fn validate_entry(entry: &PresetEntry, index: usize, source: &str) -> Result<(), String> {
    if entry.id.trim().is_empty() {
        return Err(format!("{source}: presets[{index}].id is empty"));
    }
    if entry.name.trim().is_empty() {
        return Err(format!("{source}: presets[{index}].name is empty"));
    }
    if entry.category.trim().is_empty() {
        return Err(format!("{source}: presets[{index}].category is empty"));
    }
    let device = find_device(entry.device.trim()).ok_or_else(|| {
        format!(
            "{source}: presets[{index}].device = '{}' is not a device in the catalog",
            entry.device
        )
    })?;
    for (param, value) in &entry.params {
        validate_param(
            device,
            param,
            value,
            &format!("{source}: presets[{index}].params.{param}"),
        )?;
    }
    Ok(())
}

fn validate_param(
    device: &DeviceSchema,
    param: &str,
    value: &Value,
    path: &str,
) -> Result<(), String> {
    if device.param(param).is_none() {
        return Err(format!(
            "{path}: device '{}' has no parameter '{param}'",
            device.id
        ));
    }
    let value = param_value(value, path)?;
    oc_engine::dsp::validate_static_param(device, param, &value)
        .map(|_| ())
        .map_err(|error| format!("{path}: {error}"))
}

fn param_value(value: &Value, path: &str) -> Result<ast::ParamValue, String> {
    match value {
        Value::Bool(flag) => Ok(ast::ParamValue::Bool(*flag)),
        Value::Number(number) => number
            .as_f64()
            .map(ast::ParamValue::Number)
            .ok_or_else(|| format!("{path}: {number} has no numeric value")),
        Value::String(text) => Ok(ast::ParamValue::String(text.clone())),
        other => Err(format!(
            "{path}: expects a number, string or bool; got {other}"
        )),
    }
}

/// The shipped presets, validated against this binary's catalog.
pub fn factory() -> Result<Vec<PresetEntry>, String> {
    parse_document(FACTORY_DOCUMENT, "factory device presets")
}

fn load_user(path: &Path) -> Result<Vec<PresetEntry>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_document(&text, "user device presets"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!(
            "user device presets: cannot read {}: {error}",
            path.display()
        )),
    }
}

fn write_user(path: &Path, entries: &[PresetEntry]) -> Result<(), String> {
    let document = PresetDocument {
        schema: SCHEMA,
        presets: entries,
    };
    let mut text = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("user device presets: cannot serialize: {error}"))?;
    text.push('\n');
    write_atomic(path, text.as_bytes()).map_err(|error| {
        format!(
            "user device presets: cannot write {}: {error}",
            path.display()
        )
    })
}

/// Merge both sets. A user entry with a factory entry's id replaces it.
fn merge(
    factory: Vec<PresetEntry>,
    user: Vec<PresetEntry>,
    warnings: Vec<String>,
) -> PresetListing {
    let shadowed: HashSet<&str> = user.iter().map(|entry| entry.id.as_str()).collect();
    let mut presets = Vec::with_capacity(factory.len() + user.len());
    for entry in &factory {
        if shadowed.contains(entry.id.as_str()) {
            continue;
        }
        presets.push(device_preset(entry, "factory", false));
    }
    for entry in &user {
        let shadows = factory.iter().any(|other| other.id == entry.id);
        presets.push(device_preset(entry, "user", shadows));
    }
    PresetListing {
        presets,
        warnings,
        replaced: false,
    }
}

fn device_preset(entry: &PresetEntry, source: &str, shadows: bool) -> DevicePreset {
    DevicePreset {
        id: entry.id.clone(),
        device: entry.device.clone(),
        name: entry.name.clone(),
        category: entry.category.clone(),
        intent: entry.intent.clone(),
        params: entry.params.clone(),
        source: source.to_string(),
        shadows,
        deletable: source == "user",
    }
}

/// List the factory set merged with the user file at `path`.
pub fn list(path: &Path) -> Result<PresetListing, String> {
    let factory = factory()?;
    let mut warnings = Vec::new();
    let user = match load_user(path) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(error);
            Vec::new()
        }
    };
    Ok(merge(factory, user, warnings))
}

/// `user.<device>.<slug>`, the stable id a panel save gives an entry.
fn user_id(device: &str, name: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for character in name.trim().to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character);
            gap = false;
        } else {
            gap = true;
        }
    }
    format!("user.{device}.{slug}")
}

fn fresh_id(base: &str, entries: &[PresetEntry]) -> String {
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while entries.iter().any(|entry| entry.id == candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

/// The direct control of each `reverb-fdn` axis and the derived alias it wins
/// over. `decay` is the millisecond view of `space`; `damping`, `modulation`
/// and `early_late` are the inverted or duplicate readings of `brightness`,
/// `character` and `distance`.
const REVERB_AXIS_ALIASES: [(&str, &str); 4] = [
    ("space", "decay"),
    ("brightness", "damping"),
    ("character", "modulation"),
    ("distance", "early_late"),
];

/// Drop an alias when its canonical control is also present, so one preset
/// cannot carry two values for one field. An alias on its own is kept: it is
/// then the only value the document gives that axis.
fn drop_conflicting_aliases(device: &str, params: &mut BTreeMap<String, Value>) {
    if device != "reverb-fdn" {
        return;
    }
    for (canonical, alias) in REVERB_AXIS_ALIASES {
        if params.contains_key(canonical) {
            params.remove(alias);
        }
    }
}

/// Save one user preset. Refuses an unknown device or parameter, a value out of
/// range, and a duplicate name unless `replace` names the existing entry.
pub fn save(
    path: &Path,
    device: &str,
    name: &str,
    params: &BTreeMap<String, Value>,
    replace: bool,
) -> Result<PresetListing, String> {
    let factory = factory()?;
    let mut user = load_user(path)?;
    let name = name.trim();
    if name.is_empty() {
        return Err("A device preset needs a name".into());
    }
    let device = device.trim();
    let schema = find_device(device).ok_or_else(|| {
        format!("'{device}' is not a device in the catalog, so it cannot take a preset")
    })?;
    let mut params = params.clone();
    drop_conflicting_aliases(device, &mut params);
    for (param, value) in &params {
        validate_param(schema, param, value, &format!("params.{param}"))?;
    }
    if let Some(existing) = factory
        .iter()
        .find(|entry| entry.device == device && entry.name.eq_ignore_ascii_case(name))
    {
        return Err(format!(
            "The factory preset '{}' already uses the name '{name}' for {}; choose another name",
            existing.name, schema.name
        ));
    }
    let index = user
        .iter()
        .position(|entry| entry.device == device && entry.name.eq_ignore_ascii_case(name));
    if index.is_some() && !replace {
        return Err(format!(
            "A user preset named '{name}' already exists for {}; replace it or choose another name",
            schema.name
        ));
    }
    let entry = PresetEntry {
        id: match index {
            // Replacing keeps the id, and with it any shadowing of a factory row.
            Some(index) => user[index].id.clone(),
            None => fresh_id(&user_id(device, name), &user),
        },
        device: device.to_string(),
        name: name.to_string(),
        category: schema.category.as_str().to_string(),
        // A save does not author a purpose, but replacing keeps the one the
        // entry already carried.
        intent: index.and_then(|index| user[index].intent.clone()),
        params: params.clone(),
    };
    match index {
        Some(index) => user[index] = entry,
        None => user.push(entry),
    }
    write_user(path, &user)?;
    let mut listing = list(path)?;
    listing.replaced = index.is_some();
    Ok(listing)
}

/// Delete one user preset. A factory preset cannot be deleted, which the
/// listing reports as `deletable: false` with this refusal as its reason.
pub fn delete(path: &Path, id: &str) -> Result<PresetListing, String> {
    let mut user = load_user(path)?;
    let Some(index) = user.iter().position(|entry| entry.id == id) else {
        let factory = factory()?;
        return Err(if factory.iter().any(|entry| entry.id == id) {
            format!("'{id}' is a factory preset and cannot be deleted")
        } else {
            format!("No user device preset has the id '{id}'")
        });
    };
    user.remove(index);
    write_user(path, &user)?;
    list(path)
}

fn device_name(id: &str) -> String {
    find_device(id)
        .map(|device| device.name.to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Apply one preset to a chain entry as a single undoable edit.
pub(crate) fn apply_with(
    inner: &mut AppStateInner,
    path: &Path,
    id: &str,
    target: &str,
    effect_index: usize,
    device: &str,
) -> Result<(), String> {
    let listing = list(path)?;
    let preset = listing
        .presets
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("No device preset has the id '{id}'"))?;
    if preset.device != device {
        return Err(format!(
            "'{}' is a preset for {} and cannot be applied to {}",
            preset.name,
            device_name(&preset.device),
            device_name(device)
        ));
    }
    let mut params = preset.params.clone();
    drop_conflicting_aliases(&preset.device, &mut params);
    let params: Vec<(String, String)> = params
        .iter()
        .map(|(param, value)| (param.clone(), value_to_string(value)))
        .collect();
    set_effect_params_inner(inner, target, effect_index, &params)
}

/// The agent listing: the merged bank, optionally narrowed to one device. Every
/// row keeps its `source`, `shadows` and `deletable` flags.
pub(crate) fn list_value(path: &Path, device: Option<&str>) -> Result<Value, String> {
    let listing = list(path)?;
    let wanted = match device {
        Some(id) => {
            let schema = find_device(id.trim())
                .ok_or_else(|| format!("'{id}' is not a device in the catalog"))?;
            Some(schema.id)
        }
        None => None,
    };
    let presets: Vec<&DevicePreset> = listing
        .presets
        .iter()
        .filter(|entry| {
            wanted.is_none_or(|id| {
                find_device(&entry.device).is_some_and(|candidate| candidate.id == id)
            })
        })
        .collect();
    Ok(json!({ "presets": presets, "warnings": listing.warnings }))
}

/// The agent apply: the command's own path, so the refusal sentences match.
pub(crate) fn apply_to(
    state: &AppState,
    id: &str,
    target: &str,
    effect_index: usize,
    device: &str,
) -> Result<ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    apply_with(&mut inner, &user_path(), id, target, effect_index, device)?;
    Ok(snapshot(&inner))
}

/// The string form `set_effect_params` parses; validated values only.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// List user device presets.
#[tauri::command]
pub fn device_presets() -> Result<PresetListing, String> {
    list(&user_path())
}

/// Save a device preset.
#[tauri::command]
pub fn save_device_preset(
    device: String,
    name: String,
    params: BTreeMap<String, Value>,
    replace: Option<bool>,
) -> Result<PresetListing, String> {
    save(
        &user_path(),
        &device,
        &name,
        &params,
        replace.unwrap_or(false),
    )
}

/// Delete a device preset.
#[tauri::command]
pub fn delete_device_preset(id: String) -> Result<PresetListing, String> {
    delete(&user_path(), &id)
}

/// Apply a device preset to an effect slot.
#[tauri::command]
pub fn apply_device_preset(
    id: String,
    target: String,
    effect_index: usize,
    device: String,
    state: State<AppState>,
) -> Result<ProjectSnapshot, String> {
    apply_to(&state, &id, &target, effect_index, &device)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ocp-device-presets-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("device-presets.json")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn document(json: &str) -> String {
        format!("{{\"schema\":\"{SCHEMA}\",\"presets\":[{json}]}}")
    }

    fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn factory_file_parses_and_validates() {
        let entries = factory().expect("factory presets");
        assert_eq!(entries.len(), 25);
        let ids: std::collections::HashSet<&str> =
            entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids.len(), entries.len(), "factory ids are unique");
        for entry in &entries {
            let device = find_device(&entry.device).expect("a catalog device");
            for param in entry.params.keys() {
                assert!(
                    device.param(param).is_some(),
                    "{} has no parameter {param}",
                    entry.device
                );
            }
        }
    }

    #[test]
    fn every_factory_preset_carries_a_one_line_intent() {
        for entry in factory().expect("factory presets") {
            let intent = entry
                .intent
                .as_deref()
                .unwrap_or_else(|| panic!("{} has no intent", entry.id));
            assert!(
                !intent.trim().is_empty(),
                "{} has an empty intent",
                entry.id
            );
            assert!(
                !intent.contains('\n'),
                "{} intent is not one line",
                entry.id
            );
            assert!(
                intent.len() > 20,
                "{} intent is too short: {intent}",
                entry.id
            );
            assert!(
                intent.ends_with('.'),
                "{} intent is not a sentence",
                entry.id
            );
        }
    }

    #[test]
    fn an_intent_round_trips_through_the_document() {
        let entries = parse_document(
            &document(
                r#"{"id":"a","device":"eq","name":"X","category":"eq","intent":"Tames a peak.","params":{}}"#,
            ),
            "test",
        )
        .expect("an entry with an intent parses");
        assert_eq!(entries[0].intent.as_deref(), Some("Tames a peak."));

        let text = serde_json::to_string(&PresetDocument {
            schema: SCHEMA,
            presets: &entries,
        })
        .expect("the document serializes");
        let reopened = parse_document(&text, "test").expect("the serialized document parses");
        assert_eq!(reopened[0].intent, entries[0].intent);
    }

    #[test]
    fn an_empty_intent_is_treated_as_absent() {
        for blank in ["", "   ", "\t\n"] {
            let text = serde_json::to_string(&json!({
                "schema": SCHEMA,
                "presets": [{
                    "id": "a",
                    "device": "eq",
                    "name": "X",
                    "category": "eq",
                    "intent": blank,
                    "params": {}
                }]
            }))
            .expect("the blank-intent document serializes");
            let entries = parse_document(&text, "test").expect("a blank intent is not an error");
            assert_eq!(entries[0].intent, None, "{blank:?} must be absent");
        }
    }

    #[test]
    fn a_user_preset_without_an_intent_writes_no_intent_key() {
        let dir = TempDir::new("no-intent");
        let path = dir.file();
        save(
            &path,
            "eq",
            "Plain",
            &params(&[("low_gain", Value::from(2))]),
            false,
        )
        .expect("saved");

        let text = std::fs::read_to_string(&path).expect("the user file");
        assert!(!text.contains("intent"), "the user file wrote {text}");
        let row = list(&path)
            .expect("listing")
            .presets
            .into_iter()
            .find(|entry| entry.name == "Plain")
            .expect("the saved row");
        assert_eq!(row.intent, None);
    }

    #[test]
    fn an_unknown_device_is_refused_by_field_name() {
        let error = parse_document(
            &document(r#"{"id":"a","device":"reverb9","name":"X","category":"eq","params":{}}"#),
            "test",
        )
        .unwrap_err();
        assert!(error.contains("presets[0].device"), "{error}");
        assert!(error.contains("reverb9"), "{error}");
    }

    #[test]
    fn an_unknown_param_is_refused_by_field_name() {
        let error = parse_document(
            &document(
                r#"{"id":"a","device":"eq","name":"X","category":"eq","params":{"bogus":1}}"#,
            ),
            "test",
        )
        .unwrap_err();
        assert!(error.contains("presets[0].params.bogus"), "{error}");
        assert!(error.contains("has no parameter"), "{error}");
    }

    #[test]
    fn an_out_of_range_value_is_refused_by_field_name() {
        let error = parse_document(
            &document(
                r#"{"id":"a","device":"eq","name":"X","category":"eq","params":{"low_gain":30}}"#,
            ),
            "test",
        )
        .unwrap_err();
        assert!(error.contains("presets[0].params.low_gain"), "{error}");
        assert!(error.contains("outside"), "{error}");
        assert!(error.contains("30"), "{error}");
    }

    #[test]
    fn an_unknown_schema_is_refused() {
        let error = parse_document(
            r#"{"schema":"opencompose.device_presets.v2","presets":[]}"#,
            "test",
        )
        .unwrap_err();
        assert!(error.contains("not supported"), "{error}");
        assert!(error.contains("opencompose.device_presets.v2"), "{error}");
        let missing = parse_document(r#"{"presets":[]}"#, "test").unwrap_err();
        assert!(missing.contains("schema"), "{missing}");
    }

    #[test]
    fn a_user_preset_round_trips() {
        let dir = TempDir::new("round-trip");
        let path = dir.file();
        let listing = save(
            &path,
            "eq",
            "My Shape",
            &params(&[("mid_gain", Value::from(3.5)), ("q", Value::from(1.2))]),
            false,
        )
        .expect("saved");
        assert!(!listing.replaced);
        let row = listing
            .presets
            .iter()
            .find(|entry| entry.name == "My Shape")
            .expect("the saved row");
        assert_eq!(row.source, "user");
        assert_eq!(row.device, "eq");
        assert_eq!(row.category, "eq");
        assert!(!row.shadows);
        assert!(row.deletable);

        let reopened = list(&path).expect("listing");
        let row = reopened
            .presets
            .iter()
            .find(|entry| entry.name == "My Shape")
            .expect("the saved row after reload");
        assert_eq!(row.params["mid_gain"], Value::from(3.5));
        assert_eq!(row.params["q"], Value::from(1.2));
    }

    #[test]
    fn a_user_preset_shadows_a_factory_preset() {
        let dir = TempDir::new("shadow");
        let path = dir.file();
        let factory_entry = factory().expect("factory").remove(0);
        let shadow = format!(
            r#"{{"schema":"{SCHEMA}","presets":[{{"id":"{}","device":"{}","name":"Mine","category":"eq","params":{{"low_gain":2}}}}]}}"#,
            factory_entry.id, factory_entry.device
        );
        std::fs::write(&path, shadow).expect("write user file");
        let listing = list(&path).expect("listing");
        let rows: Vec<&DevicePreset> = listing
            .presets
            .iter()
            .filter(|entry| entry.id == factory_entry.id)
            .collect();
        assert_eq!(rows.len(), 1, "the factory row is replaced, not doubled");
        assert_eq!(rows[0].source, "user");
        assert!(rows[0].shadows);
        assert_eq!(rows[0].name, "Mine");
    }

    #[test]
    fn delete_refuses_a_factory_preset_and_removes_a_user_one() {
        let dir = TempDir::new("delete");
        let path = dir.file();
        let factory_entry = factory().expect("factory").remove(0);
        let error = delete(&path, &factory_entry.id).unwrap_err();
        assert!(error.contains("factory"), "{error}");
        assert!(error.contains("cannot be deleted"), "{error}");

        let listing = save(
            &path,
            "eq",
            "Mine",
            &params(&[("low_gain", Value::from(2))]),
            false,
        )
        .expect("saved");
        let id = listing
            .presets
            .iter()
            .find(|entry| entry.name == "Mine")
            .expect("row")
            .id
            .clone();
        let listing = delete(&path, &id).expect("deleted");
        assert!(!listing.presets.iter().any(|entry| entry.id == id));
        assert!(
            listing
                .presets
                .iter()
                .any(|entry| entry.id == factory_entry.id)
        );
    }

    #[test]
    fn save_refuses_a_duplicate_name_without_replace() {
        let dir = TempDir::new("duplicate");
        let path = dir.file();
        save(
            &path,
            "eq",
            "Mine",
            &params(&[("low_gain", Value::from(2))]),
            false,
        )
        .expect("first save");
        let error = save(
            &path,
            "eq",
            "Mine",
            &params(&[("low_gain", Value::from(3))]),
            false,
        )
        .unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert!(error.contains("Mine"), "{error}");

        let listing = save(
            &path,
            "eq",
            "Mine",
            &params(&[("low_gain", Value::from(3))]),
            true,
        )
        .expect("replace");
        assert!(listing.replaced);
        let rows: Vec<&DevicePreset> = listing
            .presets
            .iter()
            .filter(|entry| entry.name == "Mine")
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].params["low_gain"], Value::from(3));
    }

    #[test]
    fn save_refuses_a_factory_name_and_an_unknown_device() {
        let dir = TempDir::new("refuse");
        let path = dir.file();
        let factory_entry = factory().expect("factory").remove(0);
        let error = save(
            &path,
            &factory_entry.device,
            &factory_entry.name,
            &BTreeMap::new(),
            true,
        )
        .unwrap_err();
        assert!(error.contains("factory preset"), "{error}");

        let error = save(&path, "reverb9", "Mine", &BTreeMap::new(), false).unwrap_err();
        assert!(error.contains("reverb9"), "{error}");
        assert!(error.contains("not a device"), "{error}");
    }

    #[test]
    fn save_refuses_a_value_out_of_range_and_names_the_param() {
        let dir = TempDir::new("range");
        let path = dir.file();
        let error = save(
            &path,
            "eq",
            "Too Loud",
            &params(&[("low_gain", Value::from(99))]),
            false,
        )
        .unwrap_err();
        assert!(error.contains("params.low_gain"), "{error}");
        assert!(error.contains("outside"), "{error}");
    }

    #[test]
    fn a_broken_user_file_is_reported_and_never_overwritten() {
        let dir = TempDir::new("broken");
        let path = dir.file();
        std::fs::write(&path, "{\"schema\":\"nope\",\"presets\":[]}").expect("write");
        let listing = list(&path).expect("factory still lists");
        assert_eq!(listing.presets.len(), 25);
        assert_eq!(listing.warnings.len(), 1);
        assert!(listing.warnings[0].contains("not supported"));

        let error = save(&path, "eq", "Mine", &BTreeMap::new(), false).unwrap_err();
        assert!(error.contains("not supported"), "{error}");
        let on_disk = std::fs::read_to_string(&path).expect("file kept");
        assert!(
            on_disk.contains("\"nope\""),
            "the original file is untouched"
        );
    }
}
