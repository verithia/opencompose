//! Device preset application: one undo entry per preset, a refusal when the
//! preset belongs to another device type, and the shipped bank itself: every
//! entry applies to its device and renders.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use oc_dsl::ast;
use oc_engine::dsp::schema::{ParamKind, find_device};
use oc_engine::dsp::try_create_effect;
use oc_engine::render_cache::RenderCache;
use serde_json::{Value, json};

use crate::device_presets::{PresetEntry, apply_with, factory, list_value, save};
use crate::state::{AppStateInner, UndoScope};

use super::support::{TempDir, make_track, test_state};

fn inner_with_eq() -> AppStateInner {
    let mut inner = test_state();
    let mut track = make_track("Bus");
    track.effects.push(ast::Effect {
        effect_type: ast::EffectType::Eq,
        params: vec![],
        bypass: false,
        sidechain_source: None,
    });
    inner.project.tracks.push(track);
    inner
}

fn eq_preset() -> PresetEntry {
    factory()
        .expect("factory presets")
        .into_iter()
        .find(|entry| entry.device == "eq")
        .expect("a seeded eq preset")
}

#[test]
fn applying_a_preset_writes_every_parameter_in_one_undo_entry() {
    let mut inner = inner_with_eq();
    let dir = TempDir::new("apply");
    let path = dir.path().join("device-presets.json");
    let preset = eq_preset();

    apply_with(&mut inner, &path, &preset.id, "track:0", 0, "eq").expect("applied");

    assert_eq!(inner.undo.undo.len(), 1, "one undo entry");
    let entry = &inner.undo.undo[0];
    assert_eq!(entry.label, "effect parameters");
    assert!(
        entry.scopes.iter().any(|scope| scope == UndoScope::Preset),
        "the edit belongs to the preset scope"
    );
    let effect = &inner.project.tracks[0].effects[0];
    assert_eq!(effect.params.len(), preset.params.len());
    for (param, value) in &preset.params {
        let (_, written) = effect
            .params
            .iter()
            .find(|(name, _)| name == param)
            .unwrap_or_else(|| panic!("{param} was not written"));
        let expected = value.as_f64().expect("a numeric seed value");
        match written {
            ast::ParamValue::Number(number) => assert_eq!(*number, expected, "{param}"),
            other => panic!("{param} was written as {other:?}"),
        }
    }
}

#[test]
fn applying_a_preset_to_another_device_type_is_refused() {
    let mut inner = inner_with_eq();
    let dir = TempDir::new("cross");
    let path = dir.path().join("device-presets.json");
    let preset = eq_preset();

    let error = apply_with(&mut inner, &path, &preset.id, "track:0", 0, "compression")
        .expect_err("a device mismatch is refused");

    // The same sentence the `apply_device_preset` command returns: both call
    // `apply_with`, so the agent tool cannot phrase this refusal differently.
    let expected = format!(
        "'{}' is a preset for {} and cannot be applied to {}",
        preset.name,
        find_device(&preset.device).expect("a catalog device").name,
        find_device("compression").expect("a catalog device").name,
    );
    assert_eq!(error, expected);
    assert!(inner.undo.undo.is_empty(), "a refusal writes nothing");
    assert!(inner.project.tracks[0].effects[0].params.is_empty());
}

#[test]
fn a_preset_id_that_does_not_exist_is_refused_by_name() {
    let mut inner = inner_with_eq();
    let dir = TempDir::new("missing-id");
    let path = dir.path().join("device-presets.json");

    let error = apply_with(&mut inner, &path, "factory.eq.nope", "track:0", 0, "eq")
        .expect_err("an unknown id is refused");
    assert_eq!(error, "No device preset has the id 'factory.eq.nope'");
    assert!(inner.undo.undo.is_empty());
}

#[test]
fn listing_answers_factory_and_user_rows_with_source_and_shadow_flags() {
    let dir = TempDir::new("listing");
    let path = dir.path().join("device-presets.json");
    let shadowed = factory().expect("factory presets").remove(0);
    let document = json!({
        "schema": crate::device_presets::SCHEMA,
        "presets": [
            {
                "id": shadowed.id,
                "device": shadowed.device,
                "name": "Shadow",
                "category": "eq",
                "params": {"low_gain": 2}
            },
            {
                "id": "user.eq.mine",
                "device": "eq",
                "name": "Mine",
                "category": "eq",
                "params": {"low_gain": 1}
            }
        ]
    });
    std::fs::create_dir_all(dir.path()).expect("temp dir");
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).expect("write");

    let listing = list_value(&path, None).expect("the library lists");
    let rows = listing["presets"].as_array().expect("a list of rows");
    assert_eq!(rows.len(), 26, "24 factory rows plus two user rows");
    assert_eq!(listing["warnings"], json!([]));

    let shadow = rows
        .iter()
        .find(|row| row["id"] == shadowed.id)
        .expect("the shadowing row");
    assert_eq!(shadow["source"], "user");
    assert_eq!(shadow["shadows"], true);
    assert_eq!(shadow["deletable"], true);

    let untouched = rows
        .iter()
        .find(|row| row["id"] == "factory.eq.unbox-the-kit")
        .expect("an unshadowed factory row");
    assert_eq!(untouched["source"], "factory");
    assert_eq!(untouched["shadows"], false);
    assert_eq!(untouched["deletable"], false);

    let mine = rows
        .iter()
        .find(|row| row["id"] == "user.eq.mine")
        .expect("the plain user row");
    assert_eq!(mine["source"], "user");
    assert_eq!(mine["shadows"], false);
    assert_eq!(mine["deletable"], true);

    let eq_only = list_value(&path, Some("eq")).expect("one device lists");
    let rows = eq_only["presets"].as_array().expect("a list of rows");
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row["device"] == "eq"));

    let error = list_value(&path, Some("reverb9")).expect_err("an unknown device is refused");
    assert!(error.contains("reverb9"), "{error}");
    assert!(error.contains("not a device"), "{error}");
}

// === The shipped bank =====================================================

/// The DSL lane keyword for each device the bank uses.
fn lane(device: &str) -> &'static str {
    match device {
        "eq" => "eq",
        "dynamic-eq" => "dynamic_eq",
        "compression" => "compression",
        "fet" => "fet",
        "opto" => "opto",
        "de-esser" => "de_esser",
        "multiband" => "multiband",
        "reverb-fdn" => "reverb_fdn",
        "limiter" => "limiter",
        other => panic!("the bank uses an unknown device: {other}"),
    }
}

/// One valid parameter the preset overwrites, so the fixture parses.
fn seed(device: &str) -> &'static str {
    match device {
        "eq" => "mid_gain=0dB",
        "dynamic-eq" => "band1_enabled=true, band1_gain=0dB",
        "compression" => "ratio=2:1",
        "fet" => "drive=0dB",
        "opto" => "peak_reduction=1dB",
        "de-esser" => "range=6dB",
        "multiband" => "band1_ratio=2:1",
        "reverb-fdn" => "mix=0",
        "limiter" => "ceiling=-1dB",
        other => panic!("the bank uses an unknown device: {other}"),
    }
}

fn preset_document() -> String {
    r#"@ocp "0.2.0"

@project "device preset probe"
  tempo: 120
  time_signature: 4/4

@preset probe
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

"#
    .to_string()
}

/// One track carrying one effect, with a two-second phrase so the render has
/// audio to process.
fn single_track(device: &str, effect_line: &str) -> AppStateInner {
    let source = format!(
        "{}@track probe | preset: probe, instrument: synth/lead_square\n  effects:\n    - {}: {effect_line}\n  @phrase phrase [1:1 - 2:1]\n    C4 q mf\n",
        preset_document(),
        lane(device)
    );
    let mut inner = test_state();
    inner.project = oc_dsl::parse(&source).expect("the probe project parses");
    oc_dsl::validate(&inner.project).expect("the probe project validates");
    inner
}

/// One project with a track per preset, so the whole bank renders once.
fn bank_project(presets: &[PresetEntry]) -> AppStateInner {
    let mut source = preset_document();
    for (index, preset) in presets.iter().enumerate() {
        source.push_str(&format!(
            "@track t{index} | preset: probe, instrument: synth/lead_square\n  effects:\n    - {}: {}\n  @phrase p{index} [1:1 - 2:1]\n    C4 q mf\n\n",
            lane(&preset.device),
            seed(&preset.device)
        ));
    }
    let mut inner = test_state();
    inner.project = oc_dsl::parse(&source).expect("the bank project parses");
    oc_dsl::validate(&inner.project).expect("the bank project validates");
    inner
}

fn render(project: &ast::Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache
        .render_full(
            project,
            Path::new("/nonexistent/device-preset-probe.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .expect("the project renders")
}

fn peak(audio: &[f32]) -> f32 {
    audio
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

fn params_of(effect: &ast::Effect) -> BTreeMap<&str, f64> {
    effect
        .params
        .iter()
        .filter_map(|(name, value)| value.as_f64().map(|number| (name.as_str(), number)))
        .collect()
}

#[test]
fn the_factory_bank_lists_twenty_five_unique_factory_ids() {
    let presets = factory().expect("the shipped bank parses and validates");
    assert_eq!(presets.len(), 25, "the bank carries 25 presets");

    let mut ids = HashSet::new();
    for preset in &presets {
        assert!(
            ids.insert(preset.id.clone()),
            "duplicate preset id {}",
            preset.id
        );
        let mut parts = preset.id.splitn(3, '.');
        assert_eq!(parts.next(), Some("factory"), "{}", preset.id);
        assert_eq!(parts.next(), Some(preset.device.as_str()), "{}", preset.id);
        let slug = parts.next().unwrap_or_default();
        assert!(!slug.is_empty(), "{} has no slug", preset.id);
        assert!(
            slug.chars().all(|character| character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'),
            "{} is not a lower-case slug",
            preset.id
        );
        assert!(!preset.params.is_empty(), "{} pins no control", preset.id);
    }
}

#[test]
fn every_factory_preset_applies_to_its_device_and_renders() {
    let presets = factory().expect("the shipped bank");
    let mut inner = bank_project(&presets);
    let dir = TempDir::new("bank");
    let path = dir.path().join("device-presets.json");

    for (index, preset) in presets.iter().enumerate() {
        apply_with(
            &mut inner,
            &path,
            &preset.id,
            &format!("track:{index}"),
            0,
            &preset.device,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", preset.id));
        assert!(
            !inner.project.tracks[index].effects[0].params.is_empty(),
            "{} wrote no parameter",
            preset.id
        );
    }

    assert!(
        peak(&render(&inner.project)) > 0.0,
        "the project carrying all 25 presets must produce audio"
    );
}

#[test]
fn a_saved_compression_preset_with_full_link_renders() {
    let mut inner = single_track("compression", "ratio=2:1");
    let dir = TempDir::new("stereo-link");
    let path = dir.path().join("device-presets.json");
    let params = BTreeMap::from([
        ("threshold".to_string(), json!(-20)),
        ("ratio".to_string(), json!(3)),
        // The value a GUI save writes: the schema's 0-100 % domain.
        ("stereo_link".to_string(), json!(100)),
    ]);
    save(&path, "compression", "Full Link Probe", &params, false).expect("saved");

    apply_with(
        &mut inner,
        &path,
        "user.compression.full-link-probe",
        "track:0",
        0,
        "compression",
    )
    .expect("applied");

    let effect = &inner.project.tracks[0].effects[0];
    let instance = try_create_effect(&effect.effect_type, &effect.params, 48_000)
        .expect("the preset's controls reach the DSP");
    assert!(
        (instance.get_param("stereo_link").unwrap() - 1.0).abs() < 1e-12,
        "100 % must reach the DSP as 1.0"
    );
    assert!(peak(&render(&inner.project)) > 0.0);
}

#[test]
fn the_dynamic_eq_forward_voice_preset_applies_and_groups_with_its_device() {
    let preset = factory()
        .expect("the shipped bank")
        .into_iter()
        .find(|entry| entry.id == "factory.dynamic-eq.forward-voice")
        .expect("the relocated presence job is in the bank");

    // Every control must be a declared dynamic-eq control, in range: the
    // catalog is the contract, so a renamed control fails here before the panel
    // ever offers the preset.
    let device = find_device("dynamic-eq").expect("a catalog device");
    for (name, value) in &preset.params {
        let parsed = match value {
            Value::Bool(flag) => ast::ParamValue::Bool(*flag),
            Value::Number(number) => ast::ParamValue::Number(number.as_f64().expect("a number")),
            Value::String(text) => ast::ParamValue::String(text.clone()),
            other => panic!("{name} is {other:?}"),
        };
        oc_engine::dsp::validate_param(device, name, &parsed)
            .unwrap_or_else(|error| panic!("{}: {name}: {error}", preset.id));
    }
    assert_eq!(preset.params["band2_freq"], json!(3200));
    assert_eq!(preset.params["band2_dynamic"], json!(true));
    assert_eq!(preset.params["band2_range"], json!(-2));

    let mut inner = single_track("dynamic-eq", "band1_enabled=true");
    let dir = TempDir::new("dynamic-eq-forward-voice");
    let path = dir.path().join("device-presets.json");
    apply_with(&mut inner, &path, &preset.id, "track:0", 0, "dynamic-eq")
        .expect("the new preset applies to a live dynamic-eq insert");

    let effect = &inner.project.tracks[0].effects[0];
    let instance = try_create_effect(&effect.effect_type, &effect.params, 48_000)
        .expect("the preset's controls reach the DSP");
    assert_eq!(instance.get_param("band2_range"), Some(-2.0));
    assert_eq!(instance.get_param("band2_dynamic"), Some(1.0));
    assert_eq!(instance.get_param("band2_threshold"), Some(-26.0));
    assert!(peak(&render(&inner.project)) > 0.0);

    // The panel's picker narrows the bank to the open device, so the row must
    // answer that listing with its own device and factory origin.
    let listing = list_value(&path, Some("dynamic-eq")).expect("the device lists");
    let rows = listing["presets"].as_array().expect("a list of rows");
    assert!(
        rows.iter().all(|row| row["device"] == "dynamic-eq"),
        "the device listing leaks another device: {rows:?}"
    );
    let row = rows
        .iter()
        .find(|row| row["id"] == json!(preset.id))
        .expect("the new preset sits in the dynamic-eq group");
    assert_eq!(row["source"], "factory");
    assert_eq!(row["shadows"], false);
    assert_eq!(row["deletable"], false);
    assert_eq!(row["name"], "Forward Voice");
}

// === Reverb aliases =======================================================

fn reverb_preset(id: &str, name: &str, params: Value) -> String {
    format!(
        r#"{{"schema":"opencompose.device_presets.v1","presets":[{{"id":"{id}","device":"reverb-fdn","name":"{name}","category":"time","params":{params}}}]}}"#
    )
}

#[test]
fn conflicting_reverb_aliases_keep_the_canonical_control() {
    let mut inner = single_track("reverb-fdn", "mix=0");
    let dir = TempDir::new("reverb-pair");
    std::fs::create_dir_all(dir.path()).expect("temp dir");
    let path = dir.path().join("device-presets.json");
    std::fs::write(
        &path,
        reverb_preset(
            "user.reverb-fdn.legacy-pair",
            "Legacy Pair",
            json!({ "space": 30, "decay": 2500, "mix": 30 }),
        ),
    )
    .expect("write the legacy document");

    apply_with(
        &mut inner,
        &path,
        "user.reverb-fdn.legacy-pair",
        "track:0",
        0,
        "reverb-fdn",
    )
    .expect("a document carrying both sides still applies");

    let effect = &inner.project.tracks[0].effects[0];
    let written = params_of(effect);
    assert_eq!(
        written.get("space"),
        Some(&30.0),
        "the canonical control wins"
    );
    assert!(
        !written.contains_key("decay"),
        "the conflicting alias is dropped: {written:?}"
    );

    let instance =
        try_create_effect(&effect.effect_type, &effect.params, 48_000).expect("the effect builds");
    assert_eq!(instance.get_param("space"), Some(30.0));
    let decay = instance.get_param("decay").unwrap();
    assert!(
        (decay - 910.28).abs() < 0.05,
        "space 30 % reads back as {decay} ms"
    );
    assert!(peak(&render(&inner.project)) > 0.0);
}

#[test]
fn an_alias_alone_still_sets_its_axis() {
    let mut inner = single_track("reverb-fdn", "mix=0");
    let dir = TempDir::new("reverb-alias");
    std::fs::create_dir_all(dir.path()).expect("temp dir");
    let path = dir.path().join("device-presets.json");
    std::fs::write(
        &path,
        reverb_preset(
            "user.reverb-fdn.alias-only",
            "Alias Only",
            json!({ "decay": 1200, "mix": 30 }),
        ),
    )
    .expect("write the alias-only document");

    apply_with(
        &mut inner,
        &path,
        "user.reverb-fdn.alias-only",
        "track:0",
        0,
        "reverb-fdn",
    )
    .expect("an alias without its partner applies");

    let effect = &inner.project.tracks[0].effects[0];
    let instance =
        try_create_effect(&effect.effect_type, &effect.params, 48_000).expect("the effect builds");
    assert_eq!(instance.get_param("decay"), Some(1200.0));
    assert!(peak(&render(&inner.project)) > 0.0);
}

/// Every control the device schema declares, in the JSON form a GUI save uses:
/// each value is the schema default until `overrides` names it.
fn every_control(device: &str, overrides: &[(&str, Value)]) -> BTreeMap<String, Value> {
    let schema = find_device(device).expect("a catalog device");
    let mut params = BTreeMap::new();
    for param in schema.params {
        let value = match param.kind {
            ParamKind::Bool => Value::Bool(param.default >= 0.5),
            ParamKind::Enum => Value::String(
                param
                    .options
                    .get(param.default as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            ),
            ParamKind::Continuous => Value::from(param.default),
        };
        params.insert(param.id.to_string(), value);
    }
    for (name, value) in overrides {
        params.insert((*name).to_string(), value.clone());
    }
    params
}

#[test]
fn a_reverb_preset_pinning_every_control_renders() {
    // A GUI save reads every control: the four aliases show up with their own
    // defaults while `space` is the value the user set, and that pair must
    // render.
    let mut inner = single_track("reverb-fdn", "mix=0");
    let dir = TempDir::new("reverb-every");
    let path = dir.path().join("device-presets.json");
    let params = every_control(
        "reverb-fdn",
        &[
            ("space", json!(30)),
            ("brightness", json!(55)),
            ("character", json!(20)),
            ("distance", json!(40)),
            ("mix", json!(30)),
        ],
    );
    save(&path, "reverb-fdn", "Every Control Probe", &params, false).expect("saved");

    // The document cannot carry both sides of an axis after a save.
    let document: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("valid JSON");
    let saved = document["presets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "Every Control Probe")
        .expect("the saved entry");
    for alias in ["decay", "damping", "modulation", "early_late"] {
        assert!(
            saved["params"].get(alias).is_none(),
            "a saved preset must not pin the alias {alias}"
        );
    }
    for canonical in ["space", "brightness", "character", "distance"] {
        assert!(
            saved["params"].get(canonical).is_some(),
            "the canonical control {canonical} must be pinned"
        );
    }

    apply_with(
        &mut inner,
        &path,
        "user.reverb-fdn.every-control-probe",
        "track:0",
        0,
        "reverb-fdn",
    )
    .expect("applied");

    let effect = &inner.project.tracks[0].effects[0];
    let instance =
        try_create_effect(&effect.effect_type, &effect.params, 48_000).expect("the effect builds");
    assert_eq!(instance.get_param("space"), Some(30.0));
    assert!((instance.get_param("decay").unwrap() - 910.28).abs() < 0.05);
    assert!((instance.get_param("damping").unwrap() - 45.0).abs() < 1e-9);
    assert!((instance.get_param("modulation").unwrap() - 20.0).abs() < 1e-9);
    assert!((instance.get_param("early_late").unwrap() - 60.0).abs() < 1e-9);
    assert!(peak(&render(&inner.project)) > 0.0);
}
