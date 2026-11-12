//! Bounded streaming TAR framing shared by marketplace archive consumers.

use std::collections::HashSet;
use std::io::{self, Read};

use flate2::read::MultiGzDecoder;

#[derive(Debug, Clone, Copy)]
/// Bounds a tar reader enforces while streaming an archive.
pub struct TarLimits {
    pub compressed_bytes: u64,
    pub entries: usize,
    pub entry_bytes: u64,
    pub payload_bytes: u64,
    pub expanded_bytes: u64,
    pub path_bytes: usize,
    pub path_depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Tar metadata entry kind this reader understands.
pub enum TarMetadata {
    None,
    GnuLongName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Entry kind after applying metadata entries.
pub enum TarKind {
    File,
    Directory,
}

#[derive(Debug)]
/// One tar entry with its sanitized name and size.
pub struct TarEntry {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub kind: TarKind,
}

struct Expanded<R> {
    input: R,
    remaining: u64,
}

impl<R: Read> Read for Expanded<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0];
            return match self.input.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "TAR byte limit exceeded",
                )),
            };
        }
        let limit = output
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let count = self.input.read(&mut output[..limit])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

fn text(bytes: &[u8]) -> Result<&str, String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).map_err(|_| "Non-UTF8 TAR name or number".into())
}

/// Parse a tar octal number field, treating a blank field as zero.
pub fn octal(bytes: &[u8]) -> Result<u64, String> {
    let value = text(bytes)?.trim_matches(' ');
    if value.is_empty() {
        return Ok(0);
    }
    if !value.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
        return Err("Invalid TAR octal number".into());
    }
    u64::from_str_radix(value, 8).map_err(|_| "Invalid TAR octal number".into())
}

fn validate_name(name: &str, limits: TarLimits, kind: TarKind) -> Result<(), String> {
    if name.len() > limits.path_bytes {
        return Err("TAR path byte limit exceeded".into());
    }
    if name
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .count()
        > limits.path_depth
    {
        return Err("TAR path depth limit exceeded".into());
    }
    // Root directory records are harmless; individual consumers decide whether
    // they belong to their archive format. Every other name must be relative.
    if kind == TarKind::Directory && matches!(name, "." | "./") {
        return Ok(());
    }
    crate::install::sanitize_zip_path(name)?;
    Ok(())
}

/// Visit files and directories without buffering their payloads. The callback
/// reads at most the declared payload; any unconsumed bytes are drained and
/// verified before the next header. Callers write only into private staging.
pub fn visit_tar_gz(
    compressed: impl Read,
    limits: TarLimits,
    metadata: TarMetadata,
    mut visit: impl FnMut(&TarEntry, &mut dyn Read) -> Result<(), String>,
) -> Result<(), String> {
    let mut input = Expanded {
        input: MultiGzDecoder::new(Expanded {
            input: compressed,
            remaining: limits.compressed_bytes,
        }),
        remaining: limits.expanded_bytes,
    };
    let mut entries = 0usize;
    let mut total = 0u64;
    let mut pending = None;
    let mut seen = HashSet::new();
    loop {
        let mut header = [0u8; 512];
        input
            .read_exact(&mut header)
            .map_err(|error| format!("Truncated TAR header: {error}"))?;
        if header.iter().all(|byte| *byte == 0) {
            if pending.is_some() {
                return Err("Dangling GNU long name".into());
            }
            input
                .read_exact(&mut header)
                .map_err(|error| format!("Missing TAR end block: {error}"))?;
            if header.iter().any(|byte| *byte != 0) {
                return Err("TAR requires two zero end blocks".into());
            }
            // Drain the gzip trailer and all concatenated members under the
            // same expansion budget. Appended archives cannot hide after EOF.
            loop {
                let count = input.read(&mut header).map_err(|error| error.to_string())?;
                if count == 0 {
                    return Ok(());
                }
                if header[..count].iter().any(|byte| *byte != 0) {
                    return Err("Nonzero data follows the TAR end blocks".into());
                }
            }
        }
        entries = entries.checked_add(1).ok_or("TAR entry count overflow")?;
        if entries > limits.entries {
            return Err("TAR entry count limit exceeded".into());
        }
        let checksum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                if (148..156).contains(&i) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum();
        if octal(&header[148..156])? != checksum {
            return Err("Invalid TAR header checksum".into());
        }
        let size = octal(&header[124..136])?;
        total = total.checked_add(size).ok_or("TAR payload size overflow")?;
        if size > limits.entry_bytes || total > limits.payload_bytes {
            return Err("TAR payload byte limit exceeded".into());
        }
        let flag = header[156];
        if flag == b'L' {
            if metadata != TarMetadata::GnuLongName
                || pending.is_some()
                || size == 0
                || size > limits.path_bytes.saturating_add(1) as u64
            {
                return Err("Unsupported or invalid GNU long name record".into());
            }
            let length = usize::try_from(size).map_err(|_| "GNU long name is too large")?;
            let mut bytes = vec![0; length];
            input
                .read_exact(&mut bytes)
                .map_err(|error| format!("Truncated GNU long name: {error}"))?;
            while bytes.last() == Some(&0) {
                bytes.pop();
            }
            if bytes.contains(&0) {
                return Err("GNU long name contains an embedded NUL".into());
            }
            pending = Some(String::from_utf8(bytes).map_err(|_| "Non-UTF8 GNU long name")?);
        } else {
            let kind = match flag {
                0 | b'0' => TarKind::File,
                b'5' if size == 0 => TarKind::Directory,
                _ => return Err("TAR contains links, devices or unsupported metadata".into()),
            };
            let name = match pending.take() {
                Some(name) => name,
                None => {
                    // GNU headers store timestamps and sparse metadata where
                    // POSIX USTAR stores its pathname prefix.
                    let prefix = if &header[257..263] == b"ustar\0" {
                        text(&header[345..500])?
                    } else {
                        ""
                    };
                    let name = text(&header[..100])?;
                    if prefix.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{prefix}/{name}")
                    }
                }
            };
            validate_name(&name, limits, kind)?;
            let normalized = if matches!(name.as_str(), "." | "./") {
                std::path::PathBuf::new()
            } else {
                crate::install::sanitize_zip_path(&name)?
            };
            if !seen.insert(normalized) {
                return Err("Duplicate TAR entry path".into());
            }
            let entry = TarEntry {
                name,
                size,
                mode: u32::try_from(octal(&header[100..108])?)
                    .map_err(|_| "TAR mode is too large")?,
                kind,
            };
            let mut payload = input.by_ref().take(size);
            visit(&entry, &mut payload)?;
            io::copy(&mut payload, &mut io::sink()).map_err(|error| error.to_string())?;
            if payload.limit() != 0 {
                return Err("Truncated TAR payload".into());
            }
        }
        let mut padding = [0; 512];
        let padding = &mut padding[..((512 - size % 512) % 512) as usize];
        input
            .read_exact(padding)
            .map_err(|error| format!("Truncated TAR padding: {error}"))?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err("Nonzero TAR padding".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn limits() -> TarLimits {
        TarLimits {
            compressed_bytes: 1 << 20,
            entries: 16,
            entry_bytes: 1024,
            payload_bytes: 4096,
            expanded_bytes: 16 * 1024,
            path_bytes: 4096,
            path_depth: 64,
        }
    }

    fn checksum(header: &mut [u8]) {
        header[148..156].fill(b' ');
        let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    fn archive(entries: &[(&str, &[u8], u8)]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (name, payload, kind) in entries {
            let mut header = [0; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000755\0");
            header[124..136].copy_from_slice(format!("{:011o}\0", payload.len()).as_bytes());
            header[156] = *kind;
            checksum(&mut header);
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

    fn accept(
        raw: &[u8],
        limits: TarLimits,
        policy: TarMetadata,
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut files = Vec::new();
        visit_tar_gz(gzip(raw).as_slice(), limits, policy, |entry, payload| {
            assert_eq!(entry.mode, 0o755);
            let mut bytes = Vec::new();
            payload
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            files.push((entry.name.clone(), bytes));
            Ok(())
        })?;
        Ok(files)
    }

    #[test]
    fn tar_stream_frames_payloads_and_drains_skipped_files() {
        let raw = archive(&[
            ("folder/", b"", b'5'),
            ("folder/skip", b"ignored", b'0'),
            ("folder/read", b"kept", b'0'),
        ]);
        let mut seen = Vec::new();
        visit_tar_gz(
            gzip(&raw).as_slice(),
            limits(),
            TarMetadata::None,
            |entry, payload| {
                if entry.name.ends_with("/read") {
                    payload
                        .read_to_end(&mut seen)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, b"kept");
    }

    #[test]
    fn tar_checksum_is_verified_before_visiting_an_entry() {
        let mut raw = archive(&[("file", b"payload", b'0')]);
        raw[0] ^= 1;
        let mut visited = false;
        let error = visit_tar_gz(
            gzip(&raw).as_slice(),
            limits(),
            TarMetadata::None,
            |_, _| {
                visited = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(!visited);
    }

    #[test]
    fn tar_rejects_links_devices_and_unimplemented_metadata() {
        for kind in [b'1', b'2', b'3', b'4', b'6', b'7', b'x', b'g', b'K'] {
            assert!(
                accept(
                    &archive(&[("file", b"", kind)]),
                    limits(),
                    TarMetadata::GnuLongName
                )
                .is_err(),
                "{kind}"
            );
        }
        assert!(
            accept(
                &archive(&[("folder", b"not empty", b'5')]),
                limits(),
                TarMetadata::None
            )
            .is_err()
        );
    }

    #[test]
    fn tar_gnu_names_are_bounded_optional_and_attached_to_one_entry() {
        let name = format!("folder/{}/file", "a".repeat(120));
        let raw = archive(&[
            ("././@LongLink", name.as_bytes(), b'L'),
            ("short", b"payload", b'0'),
        ]);
        assert_eq!(
            accept(&raw, limits(), TarMetadata::GnuLongName).unwrap()[0],
            (name, b"payload".to_vec())
        );
        assert!(accept(&raw, limits(), TarMetadata::None).is_err());
        assert!(
            accept(
                &archive(&[("long", b"name", b'L')]),
                limits(),
                TarMetadata::GnuLongName
            )
            .is_err()
        );
        assert!(
            accept(
                &archive(&[("long", b"name", b'L'), ("long", b"next", b'L')]),
                limits(),
                TarMetadata::GnuLongName
            )
            .is_err()
        );
        assert!(
            accept(
                &archive(&[("long", b"../escape", b'L'), ("file", b"x", b'0')]),
                limits(),
                TarMetadata::GnuLongName
            )
            .is_err()
        );
    }

    #[test]
    fn tar_enforces_count_payload_depth_and_compressed_expanded_limits() {
        let raw = archive(&[("a/file", b"123", b'0'), ("other", b"456", b'0')]);
        for restricted in [
            TarLimits {
                entries: 1,
                ..limits()
            },
            TarLimits {
                entry_bytes: 2,
                ..limits()
            },
            TarLimits {
                payload_bytes: 5,
                ..limits()
            },
            TarLimits {
                path_depth: 1,
                ..limits()
            },
            TarLimits {
                path_bytes: 3,
                ..limits()
            },
            TarLimits {
                expanded_bytes: raw.len() as u64 - 1,
                ..limits()
            },
            TarLimits {
                compressed_bytes: gzip(&raw).len() as u64 - 1,
                ..limits()
            },
        ] {
            assert!(
                accept(&raw, restricted, TarMetadata::None).is_err(),
                "{restricted:?}"
            );
        }
    }

    #[test]
    fn tar_refuses_truncation_nonzero_padding_and_hidden_trailing_archives() {
        let raw = archive(&[("file", b"123", b'0')]);
        for cut in [1, 511, 512, 514, 515, 900, 1024, 1536] {
            assert!(
                accept(&raw[..cut], limits(), TarMetadata::None).is_err(),
                "cut {cut}"
            );
        }
        let mut padding = raw.clone();
        padding[515] = 1;
        assert!(accept(&padding, limits(), TarMetadata::None).is_err());
        let mut hidden = raw.clone();
        hidden.extend_from_slice(&archive(&[("hidden", b"payload", b'0')]));
        assert!(accept(&hidden, limits(), TarMetadata::None).is_err());
        let mut members = gzip(&raw);
        members.extend_from_slice(&gzip(&archive(&[("hidden", b"payload", b'0')])));
        assert!(
            visit_tar_gz(members.as_slice(), limits(), TarMetadata::None, |_, _| Ok(
                ()
            ))
            .is_err()
        );
        let mut trailer = gzip(&raw);
        let last = trailer.len() - 1;
        trailer[last] ^= 1;
        assert!(
            visit_tar_gz(trailer.as_slice(), limits(), TarMetadata::None, |_, _| Ok(
                ()
            ))
            .is_err()
        );
    }

    #[test]
    fn tar_names_are_utf8_relative_unique_and_preserve_spaces() {
        for name in [
            "../escape",
            "/absolute",
            "C:/drive",
            "back\\slash",
            "folder/../../escape",
        ] {
            assert!(accept(&archive(&[(name, b"", b'0')]), limits(), TarMetadata::None).is_err());
        }
        assert!(
            accept(
                &archive(&[("a/file", b"x", b'0'), ("a/./file", b"y", b'0')]),
                limits(),
                TarMetadata::None
            )
            .is_err()
        );
        assert_eq!(
            accept(
                &archive(&[(" folder ", b"x", b'0')]),
                limits(),
                TarMetadata::None
            )
            .unwrap()[0]
                .0,
            " folder "
        );
        let mut raw = archive(&[("file", b"", b'0')]);
        raw[0] = 0xff;
        checksum(&mut raw[..512]);
        assert!(accept(&raw, limits(), TarMetadata::None).is_err());
        let mut prefix = archive(&[("file", b"x", b'0')]);
        prefix[257..263].copy_from_slice(b"ustar\0");
        prefix[345..351].copy_from_slice(b"folder");
        checksum(&mut prefix[..512]);
        assert_eq!(
            accept(&prefix, limits(), TarMetadata::None).unwrap()[0].0,
            "folder/file"
        );
        let mut gnu = archive(&[("file", b"x", b'0')]);
        gnu[257..265].copy_from_slice(b"ustar  \0");
        gnu[345..357].copy_from_slice(b"00000002322\0");
        checksum(&mut gnu[..512]);
        assert_eq!(
            accept(&gnu, limits(), TarMetadata::None).unwrap()[0].0,
            "file"
        );
    }

    #[test]
    fn tar_numeric_fields_reject_non_octal_and_overflow() {
        assert!(octal(b"+1").is_err());
        assert!(octal(b"77777777777777777777777777777777").is_err());
        let mut raw = archive(&[("file", b"", b'0')]);
        raw[124] = 0x80;
        checksum(&mut raw[..512]);
        assert!(accept(&raw, limits(), TarMetadata::None).is_err());
    }
}
