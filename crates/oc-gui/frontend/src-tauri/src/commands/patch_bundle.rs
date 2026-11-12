//! Share an OpenSynth preset as a single bounded archive.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use flate2::{Compression, write::GzEncoder};
use oc_dsl::ast::ParamValue;
use oc_engine::preset::{LibraryEntry, PresetBackend, PresetLibrary, SoundPreset};
use oc_synth::synth::{PROJECT_ASSET_KEYS, document::PatchDocument};
use tauri::Manager;

use crate::state::AppState;

use super::patch::{preset_library, sound_preset_from_project};
use super::shared::{project_root, run_blocking};

mod agent;
mod dependencies;
pub(crate) use agent::{export_bundle_for_agent, import_bundle_for_agent};

const RECORD: &str = "patch.ocp-preset";
const MAX_RECORD: u64 = 4 * 1024 * 1024;
const MAX_ASSET: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD: u64 = 292 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 300 * 1024 * 1024;
const MAX_ENTRIES: usize = 1026;

/// Export a preset bundle.
#[tauri::command]
pub async fn export_patch_bundle(
    preset_name: String,
    category: Option<String>,
    path: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (preset, root) = {
        let state = app.state::<AppState>();
        let inner = state.lock_inner();
        (
            sound_preset_from_project(
                &inner.project,
                &preset_name,
                &preset_name,
                category.as_deref(),
                vec![],
            )?,
            project_root(&inner),
        )
    };
    run_blocking(move || export(&preset, root.as_deref(), Path::new(&path))).await
}

/// Import a preset bundle.
#[tauri::command]
pub async fn import_patch_bundle(
    path: String,
    app: tauri::AppHandle,
) -> Result<LibraryEntry, String> {
    run_blocking(move || {
        let imported = import(Path::new(&path), &preset_library())?;
        app.state::<AppState>()
            .lock_inner()
            .presets
            .reload_library();
        Ok(imported)
    })
    .await
}

fn export(
    preset: &SoundPreset,
    project_dir: Option<&Path>,
    destination: &Path,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .filter(|parent| destination.is_absolute() && parent.is_dir())
        .ok_or("Choose an existing destination folder for the preset bundle")?;
    stage_export(preset, project_dir, parent)?
        .persist(destination)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn stage_export(
    preset: &SoundPreset,
    project_dir: Option<&Path>,
    parent: &Path,
) -> Result<tempfile::NamedTempFile, String> {
    dependencies::validate(preset)?;
    let staged = tempfile::tempdir().map_err(|error| error.to_string())?;
    let library = PresetLibrary::at(staged.path());
    library.save_project_patch(preset, project_dir, false)?;

    let record = library.file_for(preset.name.trim());
    if record.metadata().map_err(|error| error.to_string())?.len() > MAX_RECORD {
        return Err("The preset record exceeds the sharing limit".into());
    }
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    let compressed = GzEncoder::new(output.as_file_mut(), Compression::fast());
    let mut archive = tar::Builder::new(compressed);
    archive
        .append_path_with_name(record, RECORD)
        .map_err(|error| error.to_string())?;
    let assets = staged.path().join("assets");
    if assets.is_dir() {
        let mut entries = fs::read_dir(assets)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            archive
                .append_path_with_name(entry.path(), Path::new("assets").join(entry.file_name()))
                .map_err(|error| error.to_string())?;
        }
    }
    archive
        .into_inner()
        .and_then(GzEncoder::finish)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(output)
}

fn import(source: &Path, destination: &PresetLibrary) -> Result<LibraryEntry, String> {
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_ARCHIVE {
        return Err("Choose a preset bundle file up to 300 MiB".into());
    }
    let file = File::open(source).map_err(|error| error.to_string())?;
    let staged = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = staged.path();
    fs::create_dir(root.join("assets")).map_err(|error| error.to_string())?;
    unpack(file.take(MAX_ARCHIVE), root)?;
    let scan = PresetLibrary::at(root).scan();
    if scan.presets.len() != 1 || !scan.warnings.is_empty() {
        return Err("The bundle does not contain a complete preset".into());
    }
    let preset = scan.presets.into_iter().next().unwrap();
    dependencies::validate(&preset)?;
    if !matches!(&preset.backend, PresetBackend::BuiltinPlugin { plugin_id } if plugin_id == oc_synth::synth::PLUGIN_ID)
    {
        return Err("The bundle is not an OpenSynth preset".into());
    }
    for (key, value) in &preset.params {
        if !PROJECT_ASSET_KEYS.contains(&key.as_str()) {
            continue;
        }
        let ParamValue::String(value) = value else {
            return Err("Invalid bundled patch state".into());
        };
        let path = Path::new(value);
        if key != "open_synth_state_file" || path.parent() != Some(root.join("assets").as_path()) {
            return Err("The bundle refers to an external instrument file".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|error| error.to_string())?;
        if PatchDocument::from_json(&bytes)?.version != PatchDocument::PORTABLE_VERSION {
            return Err("The bundle contains an unsupported patch state".into());
        }
    }
    destination.import_project_patch(preset)
}

fn unpack(input: impl Read, root: &Path) -> Result<(), String> {
    use oc_market::tar::{TarKind, TarLimits, TarMetadata, visit_tar_gz};
    let mut seen = HashSet::new();
    let limits = TarLimits {
        compressed_bytes: MAX_ARCHIVE,
        entries: MAX_ENTRIES,
        entry_bytes: MAX_ASSET,
        payload_bytes: MAX_PAYLOAD,
        expanded_bytes: MAX_ARCHIVE,
        path_bytes: 4096,
        path_depth: 64,
    };
    visit_tar_gz(input, limits, TarMetadata::None, |entry, payload| {
        let name = entry.name.as_str();
        let limit = match name {
            RECORD => MAX_RECORD,
            _ if name.strip_prefix("assets/").is_some_and(asset_name) => MAX_ASSET,
            _ => return Err("Unexpected file in preset bundle".into()),
        };
        if entry.kind != TarKind::File || entry.size > limit || !seen.insert(name.to_owned()) {
            return Err("Invalid or oversized preset bundle entry".into());
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(name))
            .map_err(|error| error.to_string())?;
        if std::io::copy(payload, &mut output).map_err(|error| error.to_string())? != entry.size {
            return Err("Truncated preset bundle".into());
        }
        output.flush().map_err(|error| error.to_string())?;
        Ok(())
    })?;
    if !seen.contains(RECORD) {
        return Err("The preset bundle has no patch record".into());
    }
    Ok(())
}

fn asset_name(name: &str) -> bool {
    name.split_once('.').is_some_and(|(digest, extension)| {
        digest.len() == 32
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !extension.is_empty()
            && extension.len() <= 8
            && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oc_synth::sampler::SamplerPatch;
    use oc_synth::synth::{OpenSynth, SynthPatch, load_project_patch};

    #[test]
    fn shared_patch_bundle_reopens_after_the_original_samples_are_removed() {
        let source = tempfile::tempdir().unwrap();
        let exchange = tempfile::tempdir().unwrap();
        let wave = source.path().join("source.wav");
        let samples: Vec<_> = (0..4_800).map(|i| (i as f32 * 0.26).sin() * 0.25).collect();
        fs::write(
            &wave,
            oc_engine::wav::mono_wav_bytes(&samples, 48_000).unwrap(),
        )
        .unwrap();
        let document = PatchDocument {
            version: 1,
            name: "Shared".into(),
            author: "Fixture".into(),
            category: "Synth".into(),
            use_policy: "Test fixture".into(),
            patch: SynthPatch {
                oscillator_a_level: 0.0,
                sampler: Some(SamplerPatch {
                    regions: vec![
                        serde_json::from_value(serde_json::json!({"sample":wave})).unwrap(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        fs::write(
            source.path().join("voice.json"),
            document.to_json().unwrap(),
        )
        .unwrap();
        let project = oc_dsl::parse(
            "@project\n@preset Shared\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {open_synth_state_file: \"voice.json\", output.gain: 0.75}\n  effects:\n    - gain: factor=0.8\n"
        ).unwrap();
        let preset = sound_preset_from_project(&project, "Shared", "Shared", None, vec![]).unwrap();
        let render = |params: &[(String, ParamValue)], root: Option<&Path>| {
            let mut synth = OpenSynth::new(load_project_patch(params, root).unwrap());
            synth.note_on(0, 60, 100);
            let mut pcm = vec![0.0; 4_096];
            synth.render_interleaved(&mut pcm, 48_000);
            pcm
        };
        let reference = render(&preset.params, Some(source.path()));
        assert!(reference.iter().any(|sample| sample.abs() > 0.01));
        let archive = exchange.path().join("shared.ocppatch");
        export(&preset, Some(source.path()), &archive).unwrap();
        let sender = AppState::new();
        {
            let mut inner = sender.lock_inner();
            inner.project = project.clone();
            inner.dsl_source = oc_dsl::serialize(&project);
            inner.file_path = Some(source.path().join("sender.ocp"));
        }
        let shared = export_bundle_for_agent(&sender, "Shared", 0).unwrap();
        let shared: serde_json::Value = serde_json::to_value(shared).unwrap();
        let handoff = exchange.path().join("handoff");
        fs::create_dir(&handoff).unwrap();
        fs::copy(
            source.path().join(shared["path"].as_str().unwrap()),
            handoff.join("voice.ocppatch"),
        )
        .unwrap();
        drop(source);

        let recipient = PresetLibrary::at(exchange.path().join("recipient"));
        let first = import(&archive, &recipient).unwrap();
        let second = import(&archive, &recipient).unwrap();
        fs::remove_file(&archive).unwrap();
        assert_eq!(first.name, "Shared");
        assert_eq!(second.name, "Shared (2)");
        for patch in recipient.scan().presets {
            assert_eq!(patch.effects, preset.effects);
            assert_eq!(reference, render(&patch.params, None));
        }

        let receiver = AppState::new();
        let old = oc_dsl::parse("@project\n@preset Receiver\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {osc.a.level: 0.15}\n").unwrap();
        {
            let mut inner = receiver.lock_inner();
            inner.project = old.clone();
            inner.dsl_source = oc_dsl::serialize(&old);
            inner.file_path = Some(exchange.path().join("receiver.ocp"));
        }
        import_bundle_for_agent(
            &receiver,
            Path::new("handoff/voice.ocppatch"),
            "Receiver",
            0,
            "panel",
        )
        .unwrap();
        fs::remove_file(handoff.join("voice.ocppatch")).unwrap();
        {
            let inner = receiver.lock_inner();
            let loaded = &inner.project.presets[0];
            assert_eq!(loaded.name, "Receiver");
            assert_eq!(loaded.effects, preset.effects);
            assert_eq!(reference, render(&loaded.params, None));
            assert_eq!(
                oc_dsl::parse(&inner.dsl_source).unwrap().presets,
                inner.project.presets
            );
        }
        receiver.lock_inner().undo_step().unwrap();
        assert_eq!(receiver.lock_inner().project.presets, old.presets);
    }

    #[test]
    fn patch_bundle_refuses_project_sidechains_before_publishing() {
        let temp = tempfile::tempdir().unwrap();
        let project = oc_dsl::parse(
            "@project\n@preset Shared\n  source: builtin\n  plugin: \"builtin.open_synth\"\n",
        )
        .unwrap();
        let mut preset =
            sound_preset_from_project(&project, "Shared", "Shared", None, vec![]).unwrap();
        preset.effects.push(oc_dsl::ast::Effect {
            effect_type: oc_dsl::ast::EffectType::Compression,
            params: vec![],
            bypass: false,
            sidechain_source: Some("kick".into()),
        });
        let path = temp.path().join("blocked.ocppatch");
        assert!(
            export(&preset, None, &path)
                .unwrap_err()
                .contains("sidechain")
        );
        assert!(!path.exists());
        let mut archive = tar::Builder::new(GzEncoder::new(
            File::create(&path).unwrap(),
            Compression::fast(),
        ));
        let record = serde_json::to_vec(&preset).unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_size(record.len() as u64);
        header.set_mode(0o600);
        archive
            .append_data(&mut header, RECORD, record.as_slice())
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let library = PresetLibrary::at(temp.path().join("library"));
        assert!(import(&path, &library).unwrap_err().contains("sidechain"));
        assert!(!library.dir().exists());
    }

    #[test]
    fn patch_bundle_corrupted_checksum_never_publishes_a_preset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupted.ocppatch");
        let mut header = tar::Header::new_ustar();
        header.set_path(RECORD).unwrap();
        header.set_size(2);
        header.set_mode(0o600);
        header.set_cksum();
        let mut raw = header.as_bytes().to_vec();
        raw.extend_from_slice(b"{}");
        raw.resize(2048, 0);
        raw[0] ^= 1;
        let mut encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::fast());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap();
        let library = PresetLibrary::at(dir.path().join("library"));
        let error = import(&path, &library).unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(!library.dir().exists());
    }

    #[test]
    fn patch_bundle_rejects_traversal_and_external_state_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ocppatch");
        let archive = || {
            tar::Builder::new(GzEncoder::new(
                File::create(&path).unwrap(),
                Compression::fast(),
            ))
        };
        let mut malicious = archive();
        let mut header = tar::Header::new_ustar();
        let traversal = b"../outside.wav";
        header.as_mut_bytes()[..traversal.len()].copy_from_slice(traversal);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_cksum();
        malicious.append(&header, std::io::empty()).unwrap();
        malicious.into_inner().unwrap().finish().unwrap();
        let library = PresetLibrary::at(dir.path().join("library"));
        assert!(
            import(&path, &library)
                .unwrap_err()
                .contains("escapes the install directory")
        );
        assert!(!library.dir().exists());

        let project = oc_dsl::parse(
            "@project\n@preset Outside\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {open_synth_state_file: \"/outside.json\"}\n"
        ).unwrap();
        let preset =
            sound_preset_from_project(&project, "Outside", "Outside", None, vec![]).unwrap();
        let bytes = serde_json::to_vec(&preset).unwrap();
        let mut malicious = archive();
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        malicious
            .append_data(&mut header, RECORD, bytes.as_slice())
            .unwrap();
        malicious.into_inner().unwrap().finish().unwrap();
        assert!(import(&path, &library).is_err());
        assert!(!library.dir().exists());
    }
}
