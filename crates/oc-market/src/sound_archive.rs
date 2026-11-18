//! Bounded extraction for the instrument archives a signed catalog offers.
//!
//! Every reader counts the bytes it writes instead of trusting an entry header,
//! so a compression bomb fails at the declared expansion budget. Links, devices,
//! absolute paths and case-insensitive duplicates are refused before a file is
//! published into the caller's private staging directory.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::catalog_wire::SoundArchive;
use crate::tar::{TarEntry, TarKind, TarLimits, TarMetadata};

/// Most entries one archive may hold.
pub const MAX_ARCHIVE_ENTRIES: usize = 100_000;
/// Longest entry path in bytes.
pub const MAX_PATH_BYTES: usize = 4_096;
/// Most path components below the archive root.
pub const MAX_PATH_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy)]
/// Bounds an extraction enforces while streaming.
pub struct ExtractLimits {
    pub entries: usize,
    pub expanded_bytes: u64,
    pub path_bytes: usize,
    pub path_depth: usize,
}

impl ExtractLimits {
    /// Limits for a catalog pack with a declared expanded size.
    pub fn for_pack(unpacked_bytes: u64) -> Self {
        let allowance = unpacked_bytes.saturating_add(unpacked_bytes / 100);
        Self {
            entries: MAX_ARCHIVE_ENTRIES,
            expanded_bytes: allowance.min(crate::catalog_wire::MAX_PACK_UNPACKED_BYTES),
            path_bytes: MAX_PATH_BYTES,
            path_depth: MAX_PATH_DEPTH,
        }
    }
}

/// One archive to unpack and where the files belong.
pub struct ExtractRequest<'a> {
    pub format: SoundArchive,
    pub archive: &'a Path,
    pub destination: &'a Path,
    /// File name when the archive is a single file taken from the URL path.
    pub single_file_name: Option<&'a str>,
    pub limits: ExtractLimits,
    /// Reports bytes written so far and the expansion budget.
    pub progress: Option<&'a mut dyn FnMut(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What an extraction wrote.
pub struct ExtractOutcome {
    pub files: u64,
    pub bytes: u64,
}

struct Sink<'a> {
    root: PathBuf,
    limits: ExtractLimits,
    seen: HashMap<String, bool>,
    files: u64,
    bytes: u64,
    progress: Option<&'a mut dyn FnMut(u64, u64)>,
}

impl Sink<'_> {
    fn claim(&mut self, name: &str, is_dir: bool) -> Result<Option<PathBuf>, String> {
        if name.is_empty()
            || name.len() > self.limits.path_bytes
            || name.contains('\0')
            || name.starts_with('/')
            || name.contains('\\')
        {
            return Err("Sound archive entry has an unsafe path".into());
        }
        let depth = name
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .count();
        if depth > self.limits.path_depth {
            return Err("Sound archive path depth limit exceeded".into());
        }
        if is_dir && matches!(name, "." | "./") {
            return Ok(None);
        }
        let relative = crate::install::sanitize_zip_path(name)?;
        if !is_dir && relative.components().count() > self.limits.path_depth {
            return Err("Sound archive path depth limit exceeded".into());
        }
        let normalized = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("/");
        match self.seen.get(&normalized) {
            Some(true) if is_dir => return Ok(Some(relative)),
            Some(_) => return Err("Sound archive repeats a path with different case".into()),
            None => {
                self.seen.insert(normalized, is_dir);
            }
        }
        Ok(Some(relative))
    }

    fn add_dir(&mut self, name: &str) -> Result<(), String> {
        let Some(relative) = self.claim(name, true)? else {
            return Ok(());
        };
        let path = self.root.join(relative);
        fs::create_dir_all(&path)
            .map_err(|error| format!("Failed to create '{}': {error}", path.display()))
    }

    fn write_file(&mut self, name: &str, reader: &mut dyn Read) -> Result<(), String> {
        let relative = self
            .claim(name, false)?
            .ok_or_else(|| "Sound archive entry has an empty path".to_string())?;
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create '{}': {error}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("Failed to create '{}': {error}", path.display()))?;
        self.copy_file(&mut file, reader)
    }

    fn copy_file(&mut self, file: &mut File, reader: &mut dyn Read) -> Result<(), String> {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| format!("Failed to read a sound archive entry: {error}"))?;
            if count == 0 {
                break;
            }
            self.bytes = self
                .bytes
                .checked_add(count as u64)
                .ok_or("Sound archive size overflow")?;
            if self.bytes > self.limits.expanded_bytes {
                return Err("Sound archive expands beyond its declared size".into());
            }
            file.write_all(&buffer[..count])
                .map_err(|error| format!("Failed to write an unpacked file: {error}"))?;
            if let Some(callback) = self.progress.as_mut() {
                (*callback)(self.bytes, self.limits.expanded_bytes);
            }
        }
        self.files = self
            .files
            .checked_add(1)
            .ok_or("Sound archive file count overflow")?;
        Ok(())
    }
}

/// Unpack `request.format` into the caller's private staging directory.
pub fn extract(request: ExtractRequest<'_>) -> Result<ExtractOutcome, String> {
    let mut sink = Sink {
        root: request.destination.to_path_buf(),
        limits: request.limits,
        seen: HashMap::new(),
        files: 0,
        bytes: 0,
        progress: request.progress,
    };
    match request.format {
        SoundArchive::File => {
            let name = request
                .single_file_name
                .ok_or("Sound pack file has no file name")?;
            let relative = crate::install::sanitize_zip_path(name)?;
            if relative.components().count() != 1 {
                return Err("Sound pack file name must be a single path component".into());
            }
            let mut file = File::open(request.archive).map_err(|error| {
                format!("Failed to open '{}': {error}", request.archive.display())
            })?;
            sink.write_file(name, &mut file)?;
        }
        SoundArchive::Zip => extract_zip(request.archive, &mut sink)?,
        SoundArchive::TarGz => extract_tar(request.archive, &mut sink, false)?,
        SoundArchive::TarXz => extract_tar(request.archive, &mut sink, true)?,
        SoundArchive::SevenZ => extract_7z(request.archive, &mut sink)?,
    }
    Ok(ExtractOutcome {
        files: sink.files,
        bytes: sink.bytes,
    })
}

fn extract_zip(archive: &Path, sink: &mut Sink<'_>) -> Result<(), String> {
    let file = File::open(archive)
        .map_err(|error| format!("Failed to open '{}': {error}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| format!("Failed to read the zip archive: {error}"))?;
    if zip.len() > sink.limits.entries {
        return Err("Sound archive entry count limit exceeded".into());
    }
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("Failed to read a zip entry: {error}"))?;
        if entry.is_symlink() {
            return Err("Sound archive contains a link or device".into());
        }
        let name = entry
            .name()
            .map_err(|error| format!("Zip entry has no UTF-8 name: {error}"))?
            .into_owned();
        if entry.is_dir() {
            sink.add_dir(&name)?;
        } else {
            sink.write_file(&name, &mut entry)?;
        }
    }
    Ok(())
}

fn extract_tar(archive: &Path, sink: &mut Sink<'_>, xz: bool) -> Result<(), String> {
    let file = File::open(archive)
        .map_err(|error| format!("Failed to open '{}': {error}", archive.display()))?;
    let limits = TarLimits {
        compressed_bytes: sink.limits.expanded_bytes,
        entries: sink.limits.entries,
        entry_bytes: sink.limits.expanded_bytes,
        payload_bytes: sink.limits.expanded_bytes,
        expanded_bytes: sink.limits.expanded_bytes,
        path_bytes: sink.limits.path_bytes,
        path_depth: sink.limits.path_depth,
    };
    let mut failure = None;
    let visit = |entry: &TarEntry, payload: &mut dyn Read| {
        let result = match entry.kind {
            TarKind::Directory => sink.add_dir(&entry.name),
            TarKind::File => sink.write_file(&entry.name, payload),
            TarKind::Link | TarKind::Other => {
                Err("TAR contains links, devices or unsupported metadata".to_string())
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                failure = Some(error.clone());
                Err(error)
            }
        }
    };
    let result = if xz {
        crate::tar::visit_tar_xz(file, limits, TarMetadata::GnuLongName, visit)
    } else {
        crate::tar::visit_tar_gz(file, limits, TarMetadata::GnuLongName, visit)
    };
    if let Some(error) = failure {
        return Err(error);
    }
    result
}

fn extract_7z(archive: &Path, sink: &mut Sink<'_>) -> Result<(), String> {
    let file = File::open(archive)
        .map_err(|error| format!("Failed to open '{}': {error}", archive.display()))?;
    let mut reader = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty())
        .map_err(|error| format!("Failed to read the 7z archive: {error}"))?;
    reader.set_thread_count(1);
    let mut failure = None;
    let mut entries = 0usize;
    let result = reader.for_each_entries(|entry, data| {
        entries = entries.saturating_add(1);
        if entries > sink.limits.entries {
            let message = "Sound archive entry count limit exceeded".to_string();
            failure = Some(message.clone());
            return Err(sevenz_error(message));
        }
        if entry.is_anti_item() {
            let message = "Sound archive contains an anti-item".to_string();
            failure = Some(message.clone());
            return Err(sevenz_error(message));
        }
        let result = if entry.is_directory() {
            sink.add_dir(entry.name())
        } else {
            if entry.windows_attributes() & 0x400 != 0 {
                Err("Sound archive contains a link or device".to_string())
            } else {
                sink.write_file(entry.name(), data)
            }
        };
        match result {
            Ok(()) => Ok(true),
            Err(error) => {
                failure = Some(error.clone());
                Err(sevenz_error(error))
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    result.map_err(|error| format!("Failed to unpack the 7z archive: {error}"))
}

fn sevenz_error(message: String) -> sevenz_rust2::Error {
    sevenz_rust2::Error::Other(Cow::Owned(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ocp-sound-archive-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits() -> ExtractLimits {
        ExtractLimits {
            entries: 8,
            expanded_bytes: 4_096,
            path_bytes: 4_096,
            path_depth: 64,
        }
    }

    fn extract_path(
        archive: &Path,
        format: SoundArchive,
        destination: &Path,
    ) -> Result<ExtractOutcome, String> {
        super::extract(ExtractRequest {
            format,
            archive,
            destination,
            single_file_name: Some("song.sf2"),
            limits: limits(),
            progress: None,
        })
    }

    fn tar_bytes(entries: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (name, payload, kind) in entries {
            let mut header = [0; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000755\0");
            header[124..136].copy_from_slice(format!("{:011o}\0", payload.len()).as_bytes());
            header[156] = *kind;
            header[148..156].fill(b' ');
            let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            raw.extend_from_slice(&header);
            raw.extend_from_slice(payload);
            raw.resize(raw.len().div_ceil(512) * 512, 0);
        }
        raw.resize(raw.len() + 1024, 0);
        raw
    }

    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap()
    }

    fn xz(raw: &[u8]) -> Vec<u8> {
        let mut encoder =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap()
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, payload) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(payload).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn seven_zip(source: &Path, archive: &Path) {
        sevenz_rust2::compress_to_path(source, archive).unwrap();
    }

    /// Publishers commonly compress 7z packs with BZip2 rather than LZMA.
    #[test]
    fn a_bzip2_compressed_7z_archive_extracts() {
        let dir = TempDir::new("seven-bzip2");
        let archive = dir.0.join("pack.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        writer.set_content_methods(vec![
            sevenz_rust2::encoder_options::Bzip2Options::default().into(),
        ]);
        writer
            .push_archive_entry(
                sevenz_rust2::ArchiveEntry::new_file("Kit/kit.sf2"),
                Some(&b"RIFF\0\0\0\0sfbk"[..]),
            )
            .unwrap();
        writer.finish().unwrap();
        let target = dir.0.join("out");
        fs::create_dir(&target).unwrap();
        extract_path(&archive, SoundArchive::SevenZ, &target).unwrap();
        assert_eq!(
            fs::read(target.join("Kit/kit.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );
    }

    #[test]
    fn every_supported_archive_extracts_its_layout() {
        let dir = TempDir::new("formats");
        let source = dir.0.join("source");
        fs::create_dir_all(source.join("Piano/samples")).unwrap();
        fs::write(source.join("Piano/piano.sf2"), b"RIFF\0\0\0\0sfbk").unwrap();
        fs::write(source.join("Piano/samples/note.wav"), b"wave").unwrap();

        let single = dir.0.join("single.sf2");
        fs::write(&single, b"RIFF\0\0\0\0sfbk").unwrap();
        let target = dir.0.join("single-out");
        fs::create_dir(&target).unwrap();
        let outcome = extract_path(&single, SoundArchive::File, &target).unwrap();
        assert_eq!(outcome.files, 1);
        assert_eq!(
            fs::read(target.join("song.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );

        let zip = dir.0.join("pack.zip");
        fs::write(&zip, zip_bytes(&[("Piano/piano.sf2", b"RIFF\0\0\0\0sfbk")])).unwrap();
        let target = dir.0.join("zip-out");
        fs::create_dir(&target).unwrap();
        extract_path(&zip, SoundArchive::Zip, &target).unwrap();
        assert_eq!(
            fs::read(target.join("Piano/piano.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );

        let tar_gz = dir.0.join("pack.tar.gz");
        let raw = tar_bytes(&[
            ("Piano/", b"", b'5'),
            ("Piano/piano.sf2", b"RIFF\0\0\0\0sfbk", b'0'),
        ]);
        fs::write(&tar_gz, gzip(&raw)).unwrap();
        let target = dir.0.join("tar-gz-out");
        fs::create_dir(&target).unwrap();
        extract_path(&tar_gz, SoundArchive::TarGz, &target).unwrap();
        assert_eq!(
            fs::read(target.join("Piano/piano.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );

        let tar_xz = dir.0.join("pack.tar.xz");
        fs::write(&tar_xz, xz(&raw)).unwrap();
        let target = dir.0.join("tar-xz-out");
        fs::create_dir(&target).unwrap();
        extract_path(&tar_xz, SoundArchive::TarXz, &target).unwrap();
        assert_eq!(
            fs::read(target.join("Piano/piano.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );

        let seven = dir.0.join("pack.7z");
        seven_zip(&source, &seven);
        let target = dir.0.join("7z-out");
        fs::create_dir(&target).unwrap();
        extract_path(&seven, SoundArchive::SevenZ, &target).unwrap();
        assert_eq!(
            fs::read(target.join("Piano/piano.sf2")).unwrap(),
            b"RIFF\0\0\0\0sfbk"
        );
        assert_eq!(
            fs::read(target.join("Piano/samples/note.wav")).unwrap(),
            b"wave"
        );
    }

    #[test]
    fn traversal_absolute_paths_and_links_are_refused() {
        let dir = TempDir::new("unsafe");
        for (tag, name) in [
            ("parent", "../escape.sf2"),
            ("absolute", "/escape.sf2"),
            ("drive", "C:/escape.sf2"),
            ("nul", "bad\0name.sf2"),
        ] {
            let archive = dir.0.join(format!("{tag}.zip"));
            fs::write(&archive, zip_bytes(&[(name, b"x"), ("safe.sf2", b"x")])).unwrap();
            let target = dir.0.join(format!("{tag}-out"));
            fs::create_dir(&target).unwrap();
            assert!(
                extract_path(&archive, SoundArchive::Zip, &target).is_err(),
                "{tag} path was accepted"
            );
        }

        let archive = dir.0.join("link.zip");
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "link.sf2",
                "/tmp/outside.sf2",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        fs::write(&archive, writer.finish().unwrap().into_inner()).unwrap();
        let target = dir.0.join("link-out");
        fs::create_dir(&target).unwrap();
        assert!(extract_path(&archive, SoundArchive::Zip, &target).is_err());

        for kind in [b'1', b'2'] {
            let mut raw = tar_bytes(&[("link.sf2", b"", kind)]);
            raw.truncate(1024 + 512);
            let archive = dir.0.join(format!("link-{}.tar.gz", kind as char));
            fs::write(&archive, gzip(&raw)).unwrap();
            let target = dir.0.join(format!("tar-link-{}-out", kind as char));
            fs::create_dir(&target).unwrap();
            assert!(extract_path(&archive, SoundArchive::TarGz, &target).is_err());
        }
    }

    #[test]
    fn expanded_bytes_entry_cap_and_case_duplicates_are_bounded() {
        let dir = TempDir::new("limits");
        let archive = dir.0.join("large.zip");
        fs::write(&archive, zip_bytes(&[("a.sf2", &[0; 64])])).unwrap();
        let target = dir.0.join("large-out");
        fs::create_dir(&target).unwrap();
        let mut request = ExtractRequest {
            format: SoundArchive::Zip,
            archive: &archive,
            destination: &target,
            single_file_name: None,
            limits: ExtractLimits {
                expanded_bytes: 10,
                ..limits()
            },
            progress: None,
        };
        assert!(super::extract(request).is_err());

        let archive = dir.0.join("many.zip");
        fs::write(
            &archive,
            zip_bytes(&[("a.sf2", b"a"), ("b.sf2", b"b"), ("c.sf2", b"c")]),
        )
        .unwrap();
        let target = dir.0.join("many-out");
        fs::create_dir(&target).unwrap();
        request = ExtractRequest {
            format: SoundArchive::Zip,
            archive: &archive,
            destination: &target,
            single_file_name: None,
            limits: ExtractLimits {
                entries: 2,
                ..limits()
            },
            progress: None,
        };
        assert!(super::extract(request).is_err());

        let archive = dir.0.join("case.zip");
        fs::write(
            &archive,
            zip_bytes(&[("Piano.sf2", b"a"), ("piano.sf2", b"b")]),
        )
        .unwrap();
        let target = dir.0.join("case-out");
        fs::create_dir(&target).unwrap();
        request = ExtractRequest {
            format: SoundArchive::Zip,
            archive: &archive,
            destination: &target,
            single_file_name: None,
            limits: limits(),
            progress: None,
        };
        assert!(super::extract(request).is_err());
    }
}
