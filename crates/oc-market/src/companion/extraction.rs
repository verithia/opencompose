use crate::catalog_wire::MAX_DOWNLOAD_BYTES;
use crate::{MAX_ZIP_ENTRIES, MAX_ZIP_ENTRY_BYTES, MAX_ZIP_TOTAL_BYTES, sanitize_zip_path};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn extract_content(bytes: &[u8], dest: &Path, prefix: &str) -> Result<(), String> {
    use crate::tar::{TarKind, TarLimits, TarMetadata, visit_tar_gz};
    let prefix = if prefix.is_empty() {
        PathBuf::new()
    } else {
        sanitize_zip_path(prefix)?
    };
    let mut seen = HashSet::new();
    let limits = TarLimits {
        compressed_bytes: MAX_DOWNLOAD_BYTES,
        entries: MAX_ZIP_ENTRIES,
        entry_bytes: MAX_ZIP_ENTRY_BYTES,
        payload_bytes: MAX_ZIP_TOTAL_BYTES,
        expanded_bytes: MAX_ZIP_TOTAL_BYTES,
        path_bytes: 4096,
        path_depth: 64,
    };
    visit_tar_gz(bytes, limits, TarMetadata::GnuLongName, |entry, payload| {
        if matches!(entry.name.as_str(), "." | "./") {
            return Ok(());
        }
        let safe = sanitize_zip_path(&entry.name)?;
        let relative = match safe.strip_prefix(&prefix) {
            Ok(path) if !path.as_os_str().is_empty() => path,
            _ => return Ok(()),
        };
        if !seen.insert(relative.to_path_buf())
            || relative == Path::new(".opencompose-content.json")
        {
            return Err("Duplicate or reserved companion path".into());
        }
        let target = dest.join(relative);
        if entry.kind == TarKind::Directory {
            fs::create_dir_all(&target).map_err(|error| error.to_string())?;
        } else {
            fs::create_dir_all(target.parent().ok_or("Invalid content path")?)
                .map_err(|error| error.to_string())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| error.to_string())?;
            if std::io::copy(payload, &mut output).map_err(|error| error.to_string())? != entry.size
            {
                return Err("Truncated companion file".into());
            }
        }
        Ok(())
    })
}
