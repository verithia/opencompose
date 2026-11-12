//! Network and host adapters for the shared companion-content operation.
use super::*;

pub use oc_market::catalog_wire::CompanionContent;

pub(super) async fn install(
    authority: &CatalogAuthority,
    entries: &[CatalogEntry],
    entry: &CatalogEntry,
    install: &CatalogInstall,
    plugin_bytes: &[u8],
    overwrite: bool,
    into: Option<&Path>,
) -> Result<InstallOutcome, String> {
    install_with_dirs(
        authority,
        entries,
        Request {
            entry,
            install,
            plugin_bytes,
            overwrite,
            into,
            dirs: &HostDirs::from_env(),
        },
    )
    .await
}

struct Request<'a> {
    entry: &'a CatalogEntry,
    install: &'a CatalogInstall,
    plugin_bytes: &'a [u8],
    overwrite: bool,
    into: Option<&'a Path>,
    dirs: &'a HostDirs,
}

async fn install_with_dirs(
    authority: &CatalogAuthority,
    entries: &[CatalogEntry],
    request: Request<'_>,
) -> Result<InstallOutcome, String> {
    authority.check()?;
    let Request {
        entry,
        install,
        plugin_bytes,
        overwrite,
        into,
        dirs,
    } = request;
    if host_os() != HostOs::Macos || install.platform != host_platform() {
        return Err(
            "Companion factory content is only supported for the selected macOS host".into(),
        );
    }
    let format = bundle_format(
        effective_install_kind(install).ok_or("Unsupported companion plugin format")?,
    )
    .ok_or("Unsupported companion plugin format")?;
    let target = match into {
        Some(dir) => InstallTarget {
            dir: dir.to_path_buf(),
            needs_admin: false,
        },
        None => select_install_target(
            host_os(),
            bundle_kind(format),
            format,
            dirs,
            path_is_writable,
        )?,
    };
    if target.needs_admin {
        return Err("Companion installation requires a writable user plugin directory".into());
    }
    let paths =
        oc_market::companion::CompanionPaths::resolve(install, host_os(), dirs, &target.dir)
            .map_err(|error| error.to_string())?;
    let descriptor = install
        .companion_content
        .as_ref()
        .ok_or("Missing companion content")?;
    let content_bytes = fetch_bytes(&descriptor.url, entries).await?;
    let prepared = oc_market::companion::prepare(oc_market::companion::Prepare {
        entry: &entry.document,
        install,
        plugin: plugin_bytes,
        content: &content_bytes,
        paths,
    })
    .map_err(|error| error.to_string())?;
    let _publication = authority.publication_guard()?;
    prepared
        .publish(overwrite)
        .map_err(|error| error.to_string())
}

/// Readiness belongs to the scanned format's selected catalog descriptor.
pub(super) fn install_ready(install: &CatalogInstall, dirs: &HostDirs) -> bool {
    if install.companion_content.is_none() {
        return true;
    }
    let Ok(os) = platform_os(install.platform.as_str()) else {
        return false;
    };
    oc_market::companion::ready_for_host(install, os, dirs).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        _root: tempfile::TempDir,
        server: oc_market::loopback::FixtureServer,
        entries: Vec<CatalogEntry>,
        dirs: HostDirs,
        plugins: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = oc_market::temporary::directory_in(&std::env::temp_dir(), "gui-companion-")
                .unwrap();
            let server = oc_market::loopback::FixtureServer::start();
            let plugin = stored_zip(&[("Fixture.vst3/Contents/Info.plist", b"plugin")]);
            let content = archive(&[
                ("factory/patches_factory/p", b"patch", b'0'),
                ("factory/wavetables/w", b"wave", b'0'),
                ("factory/skins/s", b"skin", b'0'),
            ]);
            server.publish("/plugin.zip", plugin.clone());
            server.publish("/factory.tar.gz", content.clone());
            let platform = if host_os() == HostOs::Macos {
                host_platform()
            } else {
                "macos-aarch64".into()
            };
            let entry:CatalogEntry=serde_json::from_value(serde_json::json!({
                "id":"fixture.synth","name":"Fixture","vendor":"Fixture","version":"1.0",
                "installs":[{"kind":"direct","platform":platform,"format":"vst3","archive":"zip",
                    "url":server.url("/plugin.zip"),"sha256":oc_market::sha256_hex(&plugin),"bytes":plugin.len(),
                    "companion_content":{"target":"surge-xt-factory","archive":"tar-gz","url":server.url("/factory.tar.gz"),"sha256":oc_market::sha256_hex(&content),"bytes":content.len(),"strip_prefix":"factory"}}]
            })).unwrap();
            let dirs = HostDirs {
                home: Some(root.path().join("home")),
                common_program_files: None,
                local_app_data: None,
            };
            let plugins = root.path().join("custom-plugins");
            Self {
                _root: root,
                server,
                entries: vec![entry],
                dirs,
                plugins,
            }
        }
        fn content_dir(&self) -> PathBuf {
            self.dirs
                .home
                .as_ref()
                .unwrap()
                .join("Library/Application Support/Surge XT")
        }
        #[cfg(target_os = "macos")]
        async fn install(&self) -> Result<InstallOutcome, String> {
            let entry = &self.entries[0];
            // Exercise the actual primary downloader and thin companion adapter.
            let bytes = download_verified(&entry.installs[0], &self.entries).await?;
            install_with_dirs(
                &CatalogAuthority::Bundled,
                &self.entries,
                Request {
                    entry,
                    install: &entry.installs[0],
                    plugin_bytes: &bytes,
                    overwrite: false,
                    into: Some(&self.plugins),
                    dirs: &self.dirs,
                },
            )
            .await
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn gui_one_call_fetches_both_artifacts_and_honors_custom_plugin_directory() {
        let f = Fixture::new();
        let outcome = tauri::async_runtime::block_on(f.install()).unwrap();
        assert!(
            matches!(outcome,InstallOutcome::Installed{path,..} if Path::new(&path).canonicalize().unwrap()==f.plugins.canonicalize().unwrap())
        );
        assert!(f.plugins.join("Fixture.vst3/Contents/Info.plist").is_file());
        assert!(install_ready(&f.entries[0].installs[0], &f.dirs));
        let inventory = [InstalledPlugin {
            name: "Fixture".into(),
            format: "vst3".into(),
            path: f.plugins.join("Fixture.vst3").display().to_string(),
        }];
        assert!(installed_match_at(&f.entries[0], &inventory, &host_platform(), &f.dirs).is_some());

        assert_eq!(
            fs::read(f.content_dir().join("patches_factory/p")).unwrap(),
            b"patch"
        );
        let requests = f.server.requests();
        assert!(requests.iter().any(|path| path == "/plugin.zip"));
        assert!(requests.iter().any(|path| path == "/factory.tar.gz"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn gui_bad_companion_preserves_primary_and_data_destinations() {
        let mut f = Fixture::new();
        f.entries[0].installs[0]
            .companion_content
            .as_mut()
            .unwrap()
            .sha256 = "a".repeat(64);
        let error = tauri::async_runtime::block_on(f.install()).unwrap_err();
        assert!(error.starts_with("Integrity:"), "{error}");
        assert!(!f.plugins.exists());
        assert!(!f.content_dir().exists());
    }

    #[test]
    fn selected_format_readiness_is_independent_of_catalog_and_inventory_order() {
        let mut f = Fixture::new();
        let mut clap = f.entries[0].installs[0].clone();
        clap.format = InstallFormat::Clap;
        f.entries[0].installs[0]
            .companion_content
            .as_mut()
            .unwrap()
            .sha256 = "a".repeat(64);
        f.entries[0].installs.push(clap.clone());
        for dir in ["patches_factory", "wavetables", "skins"] {
            fs::create_dir_all(f.content_dir().join(dir)).unwrap();
        }
        fs::write(f.content_dir().join(".opencompose-content.json"),serde_json::to_vec(&serde_json::json!({"id":"fixture.synth","version":"1.0","sha256":clap.companion_content.unwrap().sha256})).unwrap()).unwrap();
        let mut inventory = vec![
            InstalledPlugin {
                name: "Fixture".into(),
                format: "vst3".into(),
                path: "fixture.vst3".into(),
            },
            InstalledPlugin {
                name: "Fixture".into(),
                format: "clap".into(),
                path: "fixture.clap".into(),
            },
        ];
        let platform = f.entries[0].installs[0].platform.to_string();
        for _ in 0..2 {
            for _ in 0..2 {
                let matched =
                    installed_match_at(&f.entries[0], &inventory, &platform, &f.dirs).unwrap();
                assert_eq!(matched.format, "clap");
                inventory.reverse();
            }
            f.entries[0].installs.reverse();
        }
        assert!(!f.plugins.exists(), "readiness never installs a plugin");
        assert!(
            f.server.requests().is_empty(),
            "readiness never downloads content"
        );
    }
}
