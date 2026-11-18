//! Headless plugin installs from a verified macOS disk image.
//!
//! The image is attached read-only, without Finder, auto-open or a password
//! prompt, under a private mount root. Bundles of the requested format at the
//! top of the volume, or one folder below it, are copied with `ditto` into a
//! staging directory inside the target folder and then renamed into place.
//! `ditto` keeps code signatures, extended attributes and bundle symlinks.
//! A vendor installer inside the image is never run: an image that holds only
//! a `.pkg` reports [`DmgExtraction::InstallerOnly`] so the caller can take the
//! same hand-off path a bare disk image gets.

use std::path::Path;

use crate::install::{BundleFormat, InstallOutcome};

/// Most plugin bundles one disk image may install.
pub const MAX_DMG_BUNDLES: usize = 64;
/// Largest `hdiutil attach -plist` reply the installer reads.
pub const MAX_ATTACH_PLIST_BYTES: usize = 256 * 1024;

/// What a disk-image install needs.
#[derive(Debug, Clone, Copy)]
pub struct DmgRequest<'a> {
    /// The verified image bytes.
    pub image: &'a [u8],
    pub format: BundleFormat,
    pub target_dir: &'a Path,
    pub overwrite: bool,
}

/// The result of a disk-image install.
#[derive(Debug, PartialEq, Eq)]
pub enum DmgExtraction {
    /// Bundles were installed, or the caller must confirm a replacement.
    Bundles(InstallOutcome),
    /// The image holds a vendor installer and no bundle of the requested
    /// format. Nothing was written; the caller opens the image for a person.
    InstallerOnly,
}

/// Install the plugin bundles a verified disk image carries.
#[cfg(target_os = "macos")]
pub fn extract_dmg_bundles(request: DmgRequest<'_>) -> Result<DmgExtraction, String> {
    macos::extract(request)
}

/// Disk images are a macOS container; other hosts never plan this install.
#[cfg(not(target_os = "macos"))]
pub fn extract_dmg_bundles(request: DmgRequest<'_>) -> Result<DmgExtraction, String> {
    let _ = request;
    Err("Disk images can only be unpacked on macOS".into())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::{DmgExtraction, DmgRequest, MAX_ATTACH_PLIST_BYTES, MAX_DMG_BUNDLES};
    use crate::install::{
        BundleFormat, HostOs, InstallOutcome, MAX_ZIP_ENTRIES, MAX_ZIP_TOTAL_BYTES,
        publish_staged_bundles,
    };

    const HDIUTIL: &str = "/usr/bin/hdiutil";
    const DITTO: &str = "/usr/bin/ditto";
    /// Attaching verifies the image checksums, which reads the whole image.
    const ATTACH_TIMEOUT: Duration = Duration::from_secs(180);
    const ATTACH_ATTEMPTS: u32 = 3;
    const ATTACH_RETRY_DELAY: Duration = Duration::from_millis(500);
    /// Serializes this process's `hdiutil attach` and `detach` calls.
    static HDIUTIL_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const DETACH_TIMEOUT: Duration = Duration::from_secs(60);
    const COPY_TIMEOUT: Duration = Duration::from_secs(300);
    const MAX_STDERR_BYTES: usize = 16 * 1024;
    /// Deepest directory nesting accepted inside one bundle.
    const MAX_BUNDLE_DEPTH: usize = 64;

    /// Extensions of macOS bundle directories the scan never descends into.
    const BUNDLE_EXTENSIONS: [&str; 8] = [
        ".vst3",
        ".vst",
        ".clap",
        ".lv2",
        ".component",
        ".app",
        ".aaxplugin",
        ".bundle",
    ];

    pub(super) fn extract(request: DmgRequest<'_>) -> Result<DmgExtraction, String> {
        let workspace = crate::temporary::directory_in(&std::env::temp_dir(), "ocp-dmg-")
            .map_err(|error| format!("Failed to create a disk image workspace: {error}"))?;
        let image = workspace.path().join("image.dmg");
        write_image(&image, request.image)?;
        let mount_root = workspace.path().join("mount");
        fs::create_dir(&mount_root)
            .map_err(|error| format!("Failed to create a disk image mount root: {error}"))?;
        // Declared after the workspace so it drops first: the volume is
        // detached before its mount root is removed.
        let attached = Attached::attach(&image, &mount_root)?;
        install_from_volumes(&attached.mounts, request)
    }

    fn write_image(path: &Path, bytes: &[u8]) -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Failed to stage the disk image: {error}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("Failed to stage the disk image: {error}"))
    }

    /// An attached image. Dropping it detaches every volume it mounted, on
    /// success and on every failure path.
    pub(super) struct Attached {
        pub(super) mounts: Vec<PathBuf>,
        devices: Vec<String>,
        root: PathBuf,
    }

    impl Attached {
        pub(super) fn attach(image: &Path, root: &Path) -> Result<Self, String> {
            let root = fs::canonicalize(root)
                .map_err(|error| format!("Failed to resolve the mount root: {error}"))?;
            // The guard exists before hdiutil runs, so a timeout or a reply
            // that fails to parse still detaches whatever appeared under root.
            let mut attached = Self {
                mounts: Vec::new(),
                devices: Vec::new(),
                root,
            };
            // The disk image service refuses an attach while another attach
            // or detach in this process is in flight, so attaches take turns
            // and a refused one is tried again a bounded number of times.
            let _turn = HDIUTIL_TURN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut attempt = 1;
            let output = loop {
                let mut command = Command::new(HDIUTIL);
                command
                    .arg("attach")
                    .args(["-nobrowse", "-readonly", "-noautoopen"])
                    // Reads a passphrase from the closed stdin instead of
                    // showing a password dialog for an encrypted image.
                    .arg("-stdinpass")
                    .arg("-mountrandom")
                    .arg(&attached.root)
                    .arg("-plist")
                    .arg(image);
                let output = run_bounded(command, ATTACH_TIMEOUT, MAX_ATTACH_PLIST_BYTES)?;
                if output.success || attempt == ATTACH_ATTEMPTS {
                    break output;
                }
                attempt += 1;
                std::thread::sleep(ATTACH_RETRY_DELAY);
            };
            if !output.success {
                return Err(format!(
                    "Failed to attach the disk image: {}",
                    output.stderr.trim()
                ));
            }
            let reply = std::str::from_utf8(&output.stdout)
                .map_err(|_| "Disk image attach reply is not UTF-8".to_string())?;
            let parsed = parse_attach_plist(reply)?;
            attached.devices = parsed.devices;
            for mount in parsed.mounts {
                let resolved = fs::canonicalize(&mount)
                    .map_err(|error| format!("Failed to resolve a disk image volume: {error}"))?;
                if resolved.parent() != Some(attached.root.as_path()) {
                    return Err("Disk image mounted outside its private mount root".into());
                }
                attached.mounts.push(resolved);
            }
            if attached.mounts.is_empty() {
                return Err("Disk image has no mountable volume".into());
            }
            Ok(attached)
        }
    }

    impl Drop for Attached {
        fn drop(&mut self) {
            let mut mounts = self.mounts.clone();
            // A volume the reply did not name (timeout, malformed reply) is
            // still a mount point under the private root.
            if let (Ok(root), Ok(entries)) = (fs::metadata(&self.root), fs::read_dir(&self.root)) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.dev() != root.dev())
                        && !mounts.contains(&path)
                    {
                        mounts.push(path);
                    }
                }
            }
            for mount in &mounts {
                detach(mount.as_os_str());
            }
            // Detaching a volume ejects its image; a whole-disk device that
            // is still present is detached directly.
            for device in &self.devices {
                if Path::new(device).exists() {
                    detach(device.as_ref());
                }
            }
        }
    }

    fn detach(target: &std::ffi::OsStr) {
        let _turn = HDIUTIL_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut command = Command::new(HDIUTIL);
        command.args(["detach", "-force"]).arg(target);
        // A drop guard has no caller to report to; a second target (the
        // whole-disk device) follows when a volume refuses to detach.
        let _ = run_bounded(command, DETACH_TIMEOUT, MAX_STDERR_BYTES);
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    pub(super) struct AttachReply {
        pub(super) mounts: Vec<PathBuf>,
        pub(super) devices: Vec<String>,
    }

    /// Read the mount points and whole-disk devices from an attach reply.
    /// The reply is a flat list of `<key>`/`<string>` pairs per entity, so a
    /// scan for the two keys is enough; values are entity-decoded.
    pub(super) fn parse_attach_plist(reply: &str) -> Result<AttachReply, String> {
        if reply.len() > MAX_ATTACH_PLIST_BYTES {
            return Err("Disk image attach reply exceeds its size limit".into());
        }
        let mut parsed = AttachReply::default();
        let mut rest = reply;
        while let Some(start) = rest.find("<key>") {
            rest = &rest[start + "<key>".len()..];
            let end = rest
                .find("</key>")
                .ok_or_else(|| "Malformed disk image attach reply".to_string())?;
            let key = &rest[..end];
            rest = &rest[end + "</key>".len()..];
            if key != "mount-point" && key != "dev-entry" {
                continue;
            }
            let value = rest
                .trim_start()
                .strip_prefix("<string>")
                .ok_or_else(|| "Malformed disk image attach reply".to_string())?;
            let close = value
                .find("</string>")
                .ok_or_else(|| "Malformed disk image attach reply".to_string())?;
            let text = decode_entities(&value[..close])?;
            rest = &value[close + "</string>".len()..];
            if text.is_empty() || text.chars().any(char::is_control) {
                return Err("Malformed disk image attach reply".into());
            }
            if key == "mount-point" {
                if !text.starts_with('/') {
                    return Err("Disk image mount point is not absolute".into());
                }
                parsed.mounts.push(PathBuf::from(text));
            } else if is_whole_disk(&text) {
                parsed.devices.push(text);
            }
        }
        Ok(parsed)
    }

    fn is_whole_disk(device: &str) -> bool {
        device
            .strip_prefix("/dev/disk")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
    }

    fn decode_entities(text: &str) -> Result<String, String> {
        let mut decoded = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(index) = rest.find('&') {
            decoded.push_str(&rest[..index]);
            rest = &rest[index..];
            let end = rest
                .find(';')
                .ok_or_else(|| "Malformed entity in disk image attach reply".to_string())?;
            decoded.push(match &rest[..=end] {
                "&amp;" => '&',
                "&lt;" => '<',
                "&gt;" => '>',
                "&quot;" => '"',
                "&apos;" => '\'',
                _ => return Err("Unknown entity in disk image attach reply".into()),
            });
            rest = &rest[end + 1..];
        }
        decoded.push_str(rest);
        Ok(decoded)
    }

    struct BoundedOutput {
        success: bool,
        stdout: Vec<u8>,
        stderr: String,
    }

    /// Run a tool with a deadline. Output beyond `max_stdout` is an error, and
    /// a tool that outlives the deadline is killed.
    fn run_bounded(
        mut command: Command,
        timeout: Duration,
        max_stdout: usize,
    ) -> Result<BoundedOutput, String> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Failed to start {:?}: {error}", command.get_program()))?;
        let stdout = child.stdout.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let read = pipe.take(max_stdout as u64 + 1).read_to_end(&mut bytes);
                read.map(|_| bytes)
            })
        });
        let stderr = child.stderr.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.take(MAX_STDERR_BYTES as u64).read_to_end(&mut bytes);
                String::from_utf8_lossy(&bytes).into_owned()
            })
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{:?} did not finish within {} s",
                        command.get_program(),
                        timeout.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Failed to wait for {:?}: {error}",
                        command.get_program()
                    ));
                }
            }
        };
        let stdout = match stdout {
            Some(reader) => reader
                .join()
                .map_err(|_| "Output reader panicked".to_string())?
                .map_err(|error| format!("Failed to read tool output: {error}"))?,
            None => Vec::new(),
        };
        if stdout.len() > max_stdout {
            return Err(format!(
                "{:?} output exceeds its size limit",
                command.get_program()
            ));
        }
        let stderr = match stderr {
            Some(reader) => reader.join().unwrap_or_default(),
            None => String::new(),
        };
        Ok(BoundedOutput {
            success: status.success(),
            stdout,
            stderr,
        })
    }

    /// Bundles and vendor installers found near the top of the volumes.
    #[derive(Debug, Default)]
    pub(super) struct Found {
        pub(super) bundles: Vec<(String, PathBuf)>,
        pub(super) installers: usize,
    }

    /// Look for bundles of `format` at the top of each volume and one folder
    /// below it. Symlinks are skipped: images often carry links to the system
    /// plugin folders as drag targets, and those must never be followed.
    pub(super) fn discover(volumes: &[PathBuf], format: BundleFormat) -> Result<Found, String> {
        let suffix = format!(".{}", format.extension(HostOs::Macos));
        let mut found = Found::default();
        let mut visited = 0usize;
        for volume in volumes {
            let mut folders = Vec::new();
            scan_level(volume, &suffix, &mut found, &mut folders, &mut visited)?;
            for folder in folders {
                scan_level(&folder, &suffix, &mut found, &mut Vec::new(), &mut visited)?;
            }
        }
        Ok(found)
    }

    fn scan_level(
        directory: &Path,
        suffix: &str,
        found: &mut Found,
        folders: &mut Vec<PathBuf>,
        visited: &mut usize,
    ) -> Result<(), String> {
        let entries = fs::read_dir(directory)
            .map_err(|error| format!("Failed to read the disk image: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Failed to read the disk image: {error}"))?;
            *visited += 1;
            if *visited > MAX_ZIP_ENTRIES {
                return Err("Disk image lists too many entries".into());
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("Failed to inspect the disk image: {error}"))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".pkg") || lower.ends_with(".mpkg") {
                found.installers += 1;
            } else if metadata.is_dir() && lower.ends_with(suffix) {
                if found
                    .bundles
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
                {
                    return Err("Disk image contains conflicting bundle names".into());
                }
                if found.bundles.len() >= MAX_DMG_BUNDLES {
                    return Err("Disk image holds more bundles than the install limit".into());
                }
                found.bundles.push((name.to_string(), path));
            } else if metadata.is_dir()
                && !BUNDLE_EXTENSIONS
                    .iter()
                    .any(|extension| lower.ends_with(extension))
            {
                folders.push(path);
            }
        }
        Ok(())
    }

    /// Size of one bundle, refusing special files and symlinks that leave it.
    fn bundle_bytes(root: &Path, budget: &mut BundleBudget) -> Result<(), String> {
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        while let Some((directory, depth)) = stack.pop() {
            if depth > MAX_BUNDLE_DEPTH {
                return Err("Disk image bundle nests too deeply".into());
            }
            let entries = fs::read_dir(&directory)
                .map_err(|error| format!("Failed to read a disk image bundle: {error}"))?;
            for entry in entries {
                let entry = entry
                    .map_err(|error| format!("Failed to read a disk image bundle: {error}"))?;
                budget.entries += 1;
                if budget.entries > MAX_ZIP_ENTRIES {
                    return Err("Disk image bundles hold too many files".into());
                }
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|error| format!("Failed to inspect a disk image bundle: {error}"))?;
                let kind = metadata.file_type();
                if kind.is_symlink() {
                    let target = fs::read_link(&path)
                        .map_err(|error| format!("Failed to read a bundle symlink: {error}"))?;
                    let relative = path
                        .strip_prefix(root)
                        .map_err(|_| "Disk image bundle entry escapes its bundle".to_string())?;
                    if !link_stays_inside(relative, &target) {
                        return Err("Disk image bundle has a symlink that leaves the bundle".into());
                    }
                } else if kind.is_dir() {
                    stack.push((path, depth + 1));
                } else if kind.is_file() {
                    budget.bytes = budget
                        .bytes
                        .checked_add(metadata.len())
                        .filter(|total| *total <= MAX_ZIP_TOTAL_BYTES)
                        .ok_or_else(|| {
                            "Disk image bundles exceed the install size limit".to_string()
                        })?;
                } else {
                    return Err("Disk image bundle contains a special file".into());
                }
            }
        }
        Ok(())
    }

    #[derive(Default)]
    struct BundleBudget {
        entries: usize,
        bytes: u64,
    }

    /// Whether a relative symlink at `link` (relative to the bundle root)
    /// resolves inside the bundle. Frameworks inside bundles use links such as
    /// `Versions/Current`; an absolute target or one that climbs out is refused.
    pub(super) fn link_stays_inside(link: &Path, target: &Path) -> bool {
        let mut depth: usize = link
            .parent()
            .map(|parent| parent.components().count())
            .unwrap_or(0);
        for component in target.components() {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir => match depth.checked_sub(1) {
                    Some(value) => depth = value,
                    None => return false,
                },
                Component::RootDir | Component::Prefix(_) => return false,
            }
        }
        true
    }

    fn copy_bundle(source: &Path, destination: &Path) -> Result<(), String> {
        let mut command = Command::new(DITTO);
        command.arg(source).arg(destination);
        let output = run_bounded(command, COPY_TIMEOUT, MAX_STDERR_BYTES)?;
        if !output.success {
            return Err(format!(
                "Failed to copy a bundle from the disk image: {}",
                output.stderr.trim()
            ));
        }
        if !fs::symlink_metadata(destination).is_ok_and(|meta| meta.is_dir()) {
            return Err("Copied bundle is missing from the staging directory".into());
        }
        Ok(())
    }

    pub(super) fn install_from_volumes(
        volumes: &[PathBuf],
        request: DmgRequest<'_>,
    ) -> Result<DmgExtraction, String> {
        let found = discover(volumes, request.format)?;
        if found.bundles.is_empty() {
            if found.installers > 0 {
                return Ok(DmgExtraction::InstallerOnly);
            }
            return Err(format!(
                "Disk image does not contain a .{} bundle",
                request.format.extension(HostOs::Macos)
            ));
        }
        let mut budget = BundleBudget::default();
        for (_, source) in &found.bundles {
            bundle_bytes(source, &mut budget)?;
        }
        let target_dir = request.target_dir;
        if !request.overwrite
            && let Some((name, _)) = found
                .bundles
                .iter()
                .find(|(name, _)| target_dir.join(name).exists())
        {
            let path = target_dir.join(name);
            return Ok(DmgExtraction::Bundles(InstallOutcome::NeedsConfirmation {
                path: path.display().to_string(),
                message: format!("{} already exists. Replace it?", path.display()),
            }));
        }
        fs::create_dir_all(target_dir)
            .map_err(|error| format!("Failed to create '{}': {error}", target_dir.display()))?;
        let cleanup =
            crate::temporary::directory_in(target_dir, ".ocp-staging-").map_err(|error| {
                format!(
                    "Failed to create staging directory in '{}': {error}",
                    target_dir.display()
                )
            })?;
        for (name, source) in &found.bundles {
            copy_bundle(source, &cleanup.path().join(name))?;
        }
        let roots: Vec<String> = found.bundles.iter().map(|(name, _)| name.clone()).collect();
        publish_staged_bundles(cleanup.path(), target_dir, &roots)?;
        drop(cleanup);
        Ok(DmgExtraction::Bundles(InstallOutcome::Installed {
            path: target_dir.join(&roots[0]).display().to_string(),
            message: format!("Installed {} bundle(s).", roots.len()),
        }))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::macos::{Attached, discover, link_stays_inside, parse_attach_plist};
    use super::*;

    fn scratch(tag: &str) -> tempfile::TempDir {
        crate::temporary::directory_in(&std::env::temp_dir(), &format!("oc-market-dmg-{tag}-"))
            .unwrap()
    }

    fn fake_bundle(parent: &Path, name: &str) {
        let contents = parent.join(name).join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::write(contents.join("Info.plist"), b"<plist/>").unwrap();
        fs::write(contents.join("MacOS").join("binary"), b"code").unwrap();
    }

    /// Build a compressed image from `source` with the system tool.
    fn build_image(source: &Path, output: &Path) -> Vec<u8> {
        let status = Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-format", "UDZO", "-volname", "Fixture"])
            .arg("-srcfolder")
            .arg(source)
            .arg(output)
            .status()
            .unwrap();
        assert!(status.success());
        fs::read(output).unwrap()
    }

    fn image_is_attached(needle: &str) -> bool {
        let output = Command::new("/usr/bin/hdiutil")
            .args(["info"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(needle)
    }

    #[test]
    fn a_disk_image_with_bundles_installs_them_headlessly() {
        let work = scratch("bundles");
        let source = work.path().join("source");
        fake_bundle(&source, "Foo.vst3");
        fake_bundle(&source.join("Extras"), "Bar.vst3");
        fake_bundle(&source, "Foo.component");
        std::os::unix::fs::symlink("/Library/Audio/Plug-Ins/VST3", source.join("VST3")).unwrap();
        let image = build_image(&source, &work.path().join("fixture.dmg"));
        let target = work.path().join("Plug-Ins/VST3");

        let outcome = extract_dmg_bundles(DmgRequest {
            image: &image,
            format: BundleFormat::Vst3,
            target_dir: &target,
            overwrite: false,
        })
        .unwrap();
        assert!(matches!(
            outcome,
            DmgExtraction::Bundles(InstallOutcome::Installed { .. })
        ));
        assert!(target.join("Foo.vst3/Contents/Info.plist").is_file());
        assert!(target.join("Bar.vst3/Contents/MacOS/binary").is_file());
        assert!(!target.join("Foo.component").exists());
        let names: Vec<_> = fs::read_dir(&target)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "no staging left behind: {names:?}");

        let again = extract_dmg_bundles(DmgRequest {
            image: &image,
            format: BundleFormat::Vst3,
            target_dir: &target,
            overwrite: false,
        })
        .unwrap();
        assert!(matches!(
            again,
            DmgExtraction::Bundles(InstallOutcome::NeedsConfirmation { .. })
        ));
        fs::write(target.join("Foo.vst3/Contents/stale"), b"old").unwrap();
        extract_dmg_bundles(DmgRequest {
            image: &image,
            format: BundleFormat::Vst3,
            target_dir: &target,
            overwrite: true,
        })
        .unwrap();
        assert!(!target.join("Foo.vst3/Contents/stale").exists());
    }

    #[test]
    fn a_disk_image_holding_only_an_installer_is_not_extracted() {
        let work = scratch("pkg");
        let source = work.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Vendor Installer.pkg"), b"xar!").unwrap();
        let image = build_image(&source, &work.path().join("installer.dmg"));
        let target = work.path().join("VST3");

        let outcome = extract_dmg_bundles(DmgRequest {
            image: &image,
            format: BundleFormat::Vst3,
            target_dir: &target,
            overwrite: false,
        })
        .unwrap();
        assert_eq!(outcome, DmgExtraction::InstallerOnly);
        assert!(!target.exists());
    }

    #[test]
    fn a_failed_install_still_detaches_the_image() {
        let work = scratch("detach");
        let source = work.path().join("source");
        fake_bundle(&source, "Foo.vst3");
        let image_path = work.path().join("detach.dmg");
        build_image(&source, &image_path);
        let root = work.path().join("mount");
        fs::create_dir(&root).unwrap();

        let attached = Attached::attach(&image_path, &root).unwrap();
        let mount: PathBuf = attached.mounts[0].clone();
        assert!(mount.join("Foo.vst3").is_dir());
        assert!(image_is_attached(&mount.display().to_string()));
        // A target that is a regular file makes the install fail after the
        // volume is attached.
        let blocker = work.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        let error = super::macos::install_from_volumes(
            &attached.mounts,
            DmgRequest {
                image: &[],
                format: BundleFormat::Vst3,
                target_dir: &blocker,
                overwrite: false,
            },
        )
        .unwrap_err();
        assert!(error.contains("Failed to create"), "{error}");
        drop(attached);
        assert!(!image_is_attached(&mount.display().to_string()));
        assert!(!mount.exists());
    }

    #[test]
    fn bytes_that_are_not_an_image_fail_without_mounting() {
        let work = scratch("garbage");
        let target = work.path().join("VST3");
        let error = extract_dmg_bundles(DmgRequest {
            image: b"not a disk image",
            format: BundleFormat::Vst3,
            target_dir: &target,
            overwrite: false,
        })
        .unwrap_err();
        assert!(error.contains("Failed to attach"), "{error}");
        assert!(!target.exists());
    }

    #[test]
    fn discovery_stops_one_folder_below_the_volume_root() {
        let work = scratch("depth");
        let volume = work.path().join("volume");
        fake_bundle(&volume.join("Mac"), "Near.vst3");
        fake_bundle(&volume.join("Mac/Deeper"), "Far.vst3");
        fake_bundle(&volume.join("Host.app/Contents/PlugIns"), "Inside.vst3");
        let found = discover(&[volume], BundleFormat::Vst3).unwrap();
        let names: Vec<_> = found
            .bundles
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["Near.vst3"]);
    }

    #[test]
    fn the_attach_reply_names_mount_points_and_whole_disks() {
        let reply = "<plist><dict><key>system-entities</key><array>\
            <dict><key>dev-entry</key><string>/dev/disk4</string></dict>\
            <dict><key>dev-entry</key><string>/dev/disk4s1</string>\
            <key>mount-point</key>\n\t<string>/tmp/root/dmg.A &amp; B</string></dict>\
            </array></dict></plist>";
        let parsed = parse_attach_plist(reply).unwrap();
        assert_eq!(parsed.mounts, [PathBuf::from("/tmp/root/dmg.A & B")]);
        assert_eq!(parsed.devices, ["/dev/disk4"]);
        assert!(parse_attach_plist("<key>mount-point</key><string>relative</string>").is_err());
        assert!(parse_attach_plist("<key>mount-point</key><string>/a&bogus;</string>").is_err());
        let oversized = " ".repeat(MAX_ATTACH_PLIST_BYTES + 1);
        assert!(parse_attach_plist(&oversized).is_err());
    }

    #[test]
    fn bundle_symlinks_must_stay_inside_the_bundle() {
        let link = Path::new("Contents/Frameworks/Lib.framework/Lib");
        assert!(link_stays_inside(link, Path::new("Versions/Current/Lib")));
        assert!(link_stays_inside(link, Path::new("../../MacOS/binary")));
        assert!(!link_stays_inside(link, Path::new("../../../../outside")));
        assert!(!link_stays_inside(link, Path::new("/etc/passwd")));
    }
}
