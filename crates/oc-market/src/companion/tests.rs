use super::extraction::extract_content;
use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut directory = Vec::new();
    for (name, contents) in files {
        let offset = data.len() as u32;
        data.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        data.extend_from_slice(&20u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        data.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        data.extend_from_slice(&(name.len() as u16).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(contents);

        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_offset = data.len() as u32;
    let directory_size = directory.len() as u32;
    data.extend_from_slice(&directory);
    data.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&(files.len() as u16).to_le_bytes());
    data.extend_from_slice(&(files.len() as u16).to_le_bytes());
    data.extend_from_slice(&directory_size.to_le_bytes());
    data.extend_from_slice(&directory_offset.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data
}

fn archive(files: &[(&str, &[u8], u8)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, data, kind) in files {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[148..156].fill(b' ');
        h[156] = *kind;
        let sum: u64 = h.iter().map(|b| *b as u64).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        tar.extend_from_slice(&h);
        tar.extend_from_slice(data);
        tar.resize(tar.len() + (512 - data.len() % 512) % 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

struct Fixture {
    _root: TempDir,
    entry: Entry,
    plugin: Vec<u8>,
    content: Vec<u8>,
    paths: CompanionPaths,
}
impl Fixture {
    fn new() -> Self {
        let root =
            crate::temporary::directory_in(&std::env::temp_dir(), "companion-test-").unwrap();
        let plugin = stored_zip(&[("Fixture.vst3/Contents/Info.plist", b"plugin")]);
        let content = archive(&[
            ("factory/patches_factory/patch.fxp", b"patch", b'0'),
            ("factory/wavetables/wave.wav", b"wave", b'0'),
            ("factory/skins/skin.xml", b"skin", b'0'),
        ]);
        let install: Install = serde_json::from_value(serde_json::json!({
            "kind":"direct", "platform":"macos-aarch64", "format":"vst3", "archive":"zip",
            "url":"https://example.com/plugin.zip", "sha256":crate::sha256_hex(&plugin), "bytes":plugin.len(),
            "companion_content": {"target":"surge-xt-factory", "archive":"tar-gz", "url":"https://example.com/data.tar.gz",
                "sha256":crate::sha256_hex(&content), "bytes":content.len(), "strip_prefix":"factory"}
        })).unwrap();
        let entry: Entry = serde_json::from_value(serde_json::json!({
            "id":"fixture.synth", "name":"Fixture", "vendor":"Fixture", "version":"1.0", "installs":[install]
        })).unwrap();
        let dirs = HostDirs {
            home: Some(root.path().join("home")),
            common_program_files: None,
            local_app_data: None,
        };
        let paths = CompanionPaths::resolve(
            &entry.installs[0],
            HostOs::Macos,
            &dirs,
            &root.path().join("custom-plugins"),
        )
        .unwrap();
        Self {
            _root: root,
            entry,
            plugin,
            content,
            paths,
        }
    }
    fn prepare(&self) -> Result<PreparedCompanion, Error> {
        prepare(Prepare {
            entry: &self.entry,
            install: &self.entry.installs[0],
            plugin: &self.plugin,
            content: &self.content,
            paths: self.paths.clone(),
        })
    }
    fn seed_previous(&self) {
        fs::create_dir_all(self.paths.plugin_dir.join("Fixture.vst3")).unwrap();
        fs::write(
            self.paths.plugin_dir.join("Fixture.vst3/previous"),
            b"old plugin",
        )
        .unwrap();
        fs::create_dir_all(&self.paths.content_dir).unwrap();
        fs::write(self.paths.content_dir.join("previous"), b"old content").unwrap();
    }
    fn assert_previous(&self) {
        assert_eq!(
            fs::read(self.paths.plugin_dir.join("Fixture.vst3/previous")).unwrap(),
            b"old plugin"
        );
        assert_eq!(
            fs::read(self.paths.content_dir.join("previous")).unwrap(),
            b"old content"
        );
        assert!(!self.paths.content_dir.join(RECEIPT).exists());
    }
}

#[test]
fn both_archives_publish_to_custom_plugin_and_fixed_content_destinations() {
    let f = Fixture::new();
    assert!(!ready(&f.entry.installs[0], &f.paths).unwrap());
    let result = f.prepare().unwrap().publish(false).unwrap();
    assert!(
        matches!(result, InstallOutcome::Installed {path,..} if path==f.paths.plugin_dir.display().to_string())
    );
    assert_eq!(
        fs::read(f.paths.plugin_dir.join("Fixture.vst3/Contents/Info.plist")).unwrap(),
        b"plugin"
    );
    assert_eq!(
        fs::read(f.paths.content_dir.join("patches_factory/patch.fxp")).unwrap(),
        b"patch"
    );
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
}

#[test]
fn bad_primary_or_companion_hash_and_size_preserve_prior_install() {
    for field in 0..4 {
        let mut f = Fixture::new();
        f.seed_previous();
        let install = &mut f.entry.installs[0];
        match field {
            0 => install.sha256 = Some("a".repeat(64)),
            1 => install.bytes = Some(install.bytes.unwrap() + 1),
            2 => install.companion_content.as_mut().unwrap().sha256 = "b".repeat(64),
            _ => install.companion_content.as_mut().unwrap().bytes += 1,
        }
        assert_eq!(f.prepare().err().unwrap().code, ErrorCode::Integrity);
        f.assert_previous();
    }
}

#[test]
fn overwrite_confirmation_leaves_both_destinations_untouched() {
    let f = Fixture::new();
    f.seed_previous();
    assert!(matches!(
        f.prepare().unwrap().publish(false).unwrap(),
        InstallOutcome::NeedsConfirmation { .. }
    ));
    f.assert_previous();
}

#[test]
fn second_destination_failure_restores_primary_and_content() {
    let f = Fixture::new();
    f.seed_previous();
    let calls = AtomicUsize::new(0);
    let error = f
        .prepare()
        .unwrap()
        .publish_with(true, |source, destination| {
            if calls.fetch_add(1, Ordering::Relaxed) == 3 {
                return Err(std::io::Error::other(
                    "injected content publication failure",
                ));
            }
            fs::rename(source, destination)
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Publication);
    f.assert_previous();
}

#[test]
fn failed_rollback_retains_the_original_content_backup() {
    let f = Fixture::new();
    f.seed_previous();
    let calls = AtomicUsize::new(0);
    let error = f
        .prepare()
        .unwrap()
        .publish_with(true, |source, destination| {
            let call = calls.fetch_add(1, Ordering::Relaxed);
            if call == 3 || call == 4 {
                return Err(std::io::Error::other("injected rename failure"));
            }
            fs::rename(source, destination)
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Rollback);
    let parent = f.paths.content_dir.parent().unwrap();
    let backup = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".ocp-factory-backup-")
        })
        .unwrap();
    assert_eq!(
        fs::read(backup.join("content/previous")).unwrap(),
        b"old content"
    );
}

#[test]
fn readiness_uses_selected_content_hash_and_allows_identical_content_across_versions() {
    let f = Fixture::new();
    f.prepare().unwrap().publish(false).unwrap();
    let mut other = f.entry.installs[0].clone();
    other.format = InstallFormat::Clap;
    other.companion_content.as_mut().unwrap().sha256 = "a".repeat(64);
    assert!(!ready(&other, &f.paths).unwrap());
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
    let mut receipt: Receipt =
        serde_json::from_slice(&fs::read(f.paths.content_dir.join(RECEIPT)).unwrap()).unwrap();
    receipt.version = Some("old-but-compatible".into());
    fs::write(
        f.paths.content_dir.join(RECEIPT),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
}

#[test]
fn actual_receipt_bytes_and_required_directories_are_bounded_and_checked() {
    let f = Fixture::new();
    f.prepare().unwrap().publish(false).unwrap();
    let receipt = fs::read(f.paths.content_dir.join(RECEIPT)).unwrap();
    fs::write(
        f.paths.content_dir.join(RECEIPT),
        vec![b' '; RECEIPT_BYTES as usize + 1],
    )
    .unwrap();
    assert!(!ready(&f.entry.installs[0], &f.paths).unwrap());
    fs::write(f.paths.content_dir.join(RECEIPT), receipt).unwrap();
    fs::remove_dir_all(f.paths.content_dir.join("skins")).unwrap();
    assert!(!ready(&f.entry.installs[0], &f.paths).unwrap());
}

#[test]
fn unsupported_hosts_and_catalog_paths_are_rejected() {
    let f = Fixture::new();
    let dirs = HostDirs {
        home: Some(f._root.path().join("unused")),
        common_program_files: None,
        local_app_data: None,
    };
    assert_eq!(
        CompanionPaths::resolve(
            &f.entry.installs[0],
            HostOs::Linux,
            &dirs,
            Path::new("unused")
        )
        .unwrap_err()
        .code,
        ErrorCode::UnsupportedHost
    );
    let mut install = f.entry.installs[0].clone();
    install.companion_content.as_mut().unwrap().target = "../../escape".into();
    assert_eq!(
        CompanionPaths::resolve(&install, HostOs::Macos, &dirs, Path::new("unused"))
            .unwrap_err()
            .code,
        ErrorCode::Descriptor
    );
}

#[test]
fn bounded_gnu_names_and_prefixes_preserve_extraction_policy() {
    let root = crate::temporary::directory_in(&std::env::temp_dir(), "companion-tar-").unwrap();
    let long = format!("./factory/patches_factory/{}.fxp", "x".repeat(120));
    let bytes = archive(&[
        ("./", b"", b'5'),
        ("./outside", b"outside", b'0'),
        ("././@LongLink", long.as_bytes(), b'L'),
        ("truncated", b"patch", b'0'),
    ]);
    extract_content(&bytes, root.path(), "factory").unwrap();
    assert_eq!(
        fs::read(
            root.path()
                .join(format!("patches_factory/{}.fxp", "x".repeat(120)))
        )
        .unwrap(),
        b"patch"
    );
    assert!(!root.path().join("outside").exists());
    for (path, kind) in [
        ("../escape", b'0'),
        ("factory/link", b'2'),
        ("factory/hard", b'1'),
        ("factory/pax", b'x'),
        ("factory/.opencompose-content.json", b'0'),
    ] {
        assert!(extract_content(&archive(&[(path, b"x", kind)]), root.path(), "factory").is_err());
    }
}

#[test]
fn cooperating_readers_and_publishers_do_not_observe_directory_swaps() {
    let f = Fixture::new();
    f.prepare().unwrap().publish(false).unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for _ in 0..12 {
                assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
            }
        });
        barrier.wait();
        for _ in 0..6 {
            f.prepare().unwrap().publish(true).unwrap();
        }
    });
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
}

#[test]
fn corrupted_tar_header_is_rejected_before_any_payload_is_written() {
    let root = crate::temporary::directory_in(&std::env::temp_dir(), "companion-corrupt-").unwrap();
    let bytes = archive(&[("factory/file", b"content", b'0')]);
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    raw[0] ^= 1;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw).unwrap();
    assert!(extract_content(&encoder.finish().unwrap(), root.path(), "factory").is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn missing_required_content_directories_never_publish_the_plugin() {
    let mut f = Fixture::new();
    f.content = archive(&[("factory/patches_factory/p", b"patch", b'0')]);
    let descriptor = f.entry.installs[0].companion_content.as_mut().unwrap();
    descriptor.sha256 = crate::sha256_hex(&f.content);
    descriptor.bytes = f.content.len() as u64;
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::Archive);
    assert!(!f.paths.plugin_dir.join("Fixture.vst3").exists());
    assert!(!f.paths.content_dir.exists());
}

#[test]
fn concurrent_publishers_are_serialized_and_clean_successful_stages() {
    let f = Fixture::new();
    let a = f.prepare().unwrap();
    let b = f.prepare().unwrap();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| a.publish(true));
        let b = scope.spawn(|| b.publish(true));
        assert!(a.join().unwrap().is_ok());
        assert!(b.join().unwrap().is_ok());
    });
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
    assert_eq!(fs::read_dir(&f.paths.plugin_dir).unwrap().count(), 1);
    let parent = f.paths.content_dir.parent().unwrap();
    assert_eq!(
        fs::read_dir(parent).unwrap().count(),
        2,
        "only the installed content and persistent scope lock remain"
    );
}

#[test]
fn legacy_receipt_readiness_never_creates_a_lock_or_changes_user_files() {
    let f = Fixture::new();
    for dir in REQUIRED {
        fs::create_dir_all(f.paths.content_dir.join(dir)).unwrap();
    }
    let receipt = Receipt {
        id: f.entry.id.clone(),
        version: f.entry.version.clone(),
        sha256: f.entry.installs[0]
            .companion_content
            .as_ref()
            .unwrap()
            .sha256
            .clone(),
    };
    let bytes = serde_json::to_vec(&receipt).unwrap();
    fs::write(f.paths.content_dir.join(RECEIPT), &bytes).unwrap();
    assert!(ready(&f.entry.installs[0], &f.paths).unwrap());
    assert!(!f.paths.content_dir.parent().unwrap().join(LOCK).exists());
    assert_eq!(fs::read(f.paths.content_dir.join(RECEIPT)).unwrap(), bytes);
}

#[test]
fn parent_components_cannot_hide_an_overlap_but_can_escape_to_an_independent_destination() {
    let f = Fixture::new();
    let dirs = HostDirs {
        home: Some(f._root.path().join("home")),
        common_program_files: None,
        local_app_data: None,
    };
    let parent = f.paths.content_dir.parent().unwrap();
    let hidden = parent.join("unused/../Surge XT/plugins");
    let error =
        CompanionPaths::resolve(&f.entry.installs[0], HostOs::Macos, &dirs, &hidden).unwrap_err();
    assert_eq!(error.code, ErrorCode::Destination);
    assert!(
        !dirs.home.as_ref().unwrap().exists(),
        "resolution must not create missing directories"
    );
    let independent = f.paths.content_dir.join("../separate/plugins");
    let paths =
        CompanionPaths::resolve(&f.entry.installs[0], HostOs::Macos, &dirs, &independent).unwrap();
    assert_eq!(paths.plugin_dir, parent.join("separate/plugins"));
    assert!(!paths.plugin_dir.starts_with(&paths.content_dir));
}

#[test]
#[cfg(unix)]
fn a_relative_missing_custom_destination_resolves_without_changing_the_working_directory() {
    use std::path::Component;
    let f = Fixture::new();
    let cwd = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    for component in cwd.components() {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    relative.push(f._root.path().strip_prefix("/").unwrap());
    relative.push("relative/not-created/plugins");
    let dirs = HostDirs {
        home: Some(f._root.path().join("home")),
        common_program_files: None,
        local_app_data: None,
    };
    let paths =
        CompanionPaths::resolve(&f.entry.installs[0], HostOs::Macos, &dirs, &relative).unwrap();
    assert_eq!(
        paths.plugin_dir,
        f._root
            .path()
            .canonicalize()
            .unwrap()
            .join("relative/not-created/plugins")
    );
    assert!(!paths.plugin_dir.exists());
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[test]
#[cfg(unix)]
fn existing_symlink_aliases_cannot_place_plugin_staging_inside_factory_data() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    fs::create_dir_all(&f.paths.content_dir).unwrap();
    let alias = f._root.path().join("content-alias");
    symlink(&f.paths.content_dir, &alias).unwrap();
    let dirs = HostDirs {
        home: Some(f._root.path().join("home")),
        common_program_files: None,
        local_app_data: None,
    };
    assert_eq!(
        CompanionPaths::resolve(
            &f.entry.installs[0],
            HostOs::Macos,
            &dirs,
            &alias.join("plugins")
        )
        .unwrap_err()
        .code,
        ErrorCode::Destination
    );
    assert_eq!(fs::read_dir(&f.paths.content_dir).unwrap().count(), 0);
}

#[test]
#[cfg(unix)]
fn symlinks_are_resolved_before_following_parent_components() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let dirs = HostDirs {
        home: Some(f._root.path().join("home")),
        common_program_files: None,
        local_app_data: None,
    };
    let nested = f.paths.content_dir.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let inside = f._root.path().join("inside-alias");
    symlink(&nested, &inside).unwrap();
    assert_eq!(
        CompanionPaths::resolve(
            &f.entry.installs[0],
            HostOs::Macos,
            &dirs,
            &inside.join("../plugins")
        )
        .unwrap_err()
        .code,
        ErrorCode::Destination
    );
    let outside = f._root.path().join("independent/nested");
    fs::create_dir_all(&outside).unwrap();
    let external = f._root.path().join("external-alias");
    symlink(&outside, &external).unwrap();
    let paths = CompanionPaths::resolve(
        &f.entry.installs[0],
        HostOs::Macos,
        &dirs,
        &external.join("../plugins"),
    )
    .unwrap();
    assert_eq!(
        paths.plugin_dir,
        outside
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("plugins")
    );
}

#[test]
#[cfg(unix)]
fn a_destination_alias_introduced_after_resolution_is_rejected_before_staging() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    fs::create_dir_all(&f.paths.content_dir).unwrap();
    fs::write(f.paths.content_dir.join("previous"), b"untouched").unwrap();
    symlink(&f.paths.content_dir, &f.paths.plugin_dir).unwrap();
    assert_eq!(f.prepare().err().unwrap().code, ErrorCode::Destination);
    assert_eq!(fs::read_dir(&f.paths.content_dir).unwrap().count(), 1);
    assert_eq!(
        fs::read(f.paths.content_dir.join("previous")).unwrap(),
        b"untouched"
    );
}

#[test]
#[cfg(unix)]
fn a_destination_alias_introduced_after_preparation_is_rejected_before_publication() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let prepared = f.prepare().unwrap();
    fs::create_dir_all(&f.paths.content_dir).unwrap();
    fs::write(f.paths.content_dir.join("previous"), b"untouched").unwrap();
    fs::rename(&f.paths.plugin_dir, f._root.path().join("retired-plugins")).unwrap();
    symlink(&f.paths.content_dir, &f.paths.plugin_dir).unwrap();
    assert_eq!(
        prepared.publish(true).unwrap_err().code,
        ErrorCode::Destination
    );
    assert_eq!(fs::read_dir(&f.paths.content_dir).unwrap().count(), 1);
    assert_eq!(
        fs::read(f.paths.content_dir.join("previous")).unwrap(),
        b"untouched"
    );
}

#[test]
fn host_readiness_resolves_only_content_for_a_custom_plugin_install() {
    let f = Fixture::new();
    f.prepare().unwrap().publish(false).unwrap();
    let dirs = HostDirs {
        home: Some(f._root.path().join("home")),
        common_program_files: None,
        local_app_data: None,
    };
    assert!(ready_for_host(&f.entry.installs[0], HostOs::Macos, &dirs).unwrap());
    assert!(
        f.paths
            .plugin_dir
            .join("Fixture.vst3/Contents/Info.plist")
            .is_file()
    );
    assert_eq!(
        ready_for_host(&f.entry.installs[0], HostOs::Linux, &dirs)
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedHost
    );
    let requests_before = fs::read_dir(f.paths.content_dir.parent().unwrap())
        .unwrap()
        .count();
    assert!(ready_for_host(&f.entry.installs[0], HostOs::Macos, &dirs).unwrap());
    assert_eq!(
        fs::read_dir(f.paths.content_dir.parent().unwrap())
            .unwrap()
            .count(),
        requests_before
    );
}
