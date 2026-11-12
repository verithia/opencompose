use super::*;
use std::collections::HashSet;
use std::path::PathBuf;

/// Whether `id` is a bounded, filesystem-safe identifier.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
/// Whether `value` is a 64-character lowercase hex sha256.
pub fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
/// Whether `url` is an absolute https URL with a host.
pub fn is_https_url(url: &str) -> bool {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let host = url[8..].split('/').next().unwrap_or_default();
    !host.is_empty() && !host.starts_with(':') && host.contains('.')
}
/// Whether `url` is an absolute http(s) URL no longer than 2048 bytes.
pub fn is_absolute_http_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.len() > 2048 {
        return false;
    }
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    rest.split('/').next().is_some_and(|host| host.len() > 1)
}
/// Whether `value` is a safe single path component.
pub fn safe_component(value: &str) -> bool {
    is_valid_id(value) && !matches!(value, "." | "..")
}

/// Files the bundled separation model must provide.
pub const MODEL_FILES: &[&str] = &[
    "models/2stems/checkpoint",
    "models/2stems/model.data-00000-of-00001",
    "models/2stems/model.index",
    "models/2stems/.probe",
];
/// Executables a POSIX runtime must provide.
pub const POSIX_EXECUTABLES: &[&str] = &["bin/spleeter", "python/bin/python3"];
/// Bundled Python path on Windows.
pub const WINDOWS_PYTHON: &str = "python/python.exe";
/// Separation runner path on Windows.
pub const WINDOWS_RUNNER: &str = "runner/separate.py";
/// Files a Windows runtime must provide.
pub const WINDOWS_REQUIRED: &[&str] = &[WINDOWS_PYTHON, WINDOWS_RUNNER, "models/2stems/model.meta"];

/// Resolve a runtime file name under `name`, rejecting unsafe components.
pub fn runtime_path(name: &str, os: HostOs) -> Result<PathBuf, String> {
    let path = crate::sanitize_zip_path(name)?;
    if os == HostOs::Windows {
        for component in path.iter() {
            let name = component.to_string_lossy();
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_ascii_uppercase();
            if name.ends_with(['.', ' '])
                || name.contains(['<', '>', '"', '|', '?', '*'])
                || matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                )
                || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|n| {
                        matches!(
                            n,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                })
            {
                return Err("Runtime path is unsafe on Windows".into());
            }
        }
    }
    Ok(path)
}
impl RuntimeDescriptor {
    /// Check the descriptor against the files the OS needs.
    pub fn validate_layout(&self, os: HostOs) -> Result<(), CatalogError> {
        let path = "runtime";
        let fail = |message: &str| CatalogError::new(CatalogErrorCode::Runtime, path, message);
        let (version, launcher, required) = match os {
            HostOs::Windows => (2, WINDOWS_PYTHON, WINDOWS_REQUIRED),
            _ => (1, POSIX_EXECUTABLES[0], POSIX_EXECUTABLES),
        };
        if self.provider != "spleeter"
            || self.layout_version != version
            || self.launcher != launcher
            || self.required_files.len() > MAX_RUNTIME_FILES
        {
            return Err(fail("Invalid separator runtime layout for this platform"));
        }
        let mut paths = HashSet::new();
        for file in &self.required_files {
            runtime_path(&file.path, os).map_err(|message| fail(&message))?;
            if file
                .path
                .split('/')
                .any(|part| part.is_empty() || part == ".")
                || !is_lowercase_sha256(&file.sha256)
                || !paths.insert(file.path.as_str())
            {
                return Err(fail("Invalid or duplicate separator runtime required file"));
            }
        }
        if required
            .iter()
            .chain(MODEL_FILES)
            .any(|name| !paths.contains(name))
        {
            return Err(fail(
                "Separator runtime is missing required interpreter, launcher or model hashes",
            ));
        }
        Ok(())
    }
}
/// Validate an entry runtime when one is declared.
pub fn validate_runtime(entry: &Entry, install: &Install, path: &str) -> Result<(), CatalogError> {
    let Some(runtime) = &install.runtime else {
        return Ok(());
    };
    let fail = |message: &str| {
        CatalogError::new(
            CatalogErrorCode::Runtime,
            format!("{path}.runtime"),
            message,
        )
    };
    let os = crate::platform_os(install.platform.as_str()).map_err(|message| fail(&message))?;
    if install.format != Format::Standalone
        || install.archive != Some(Archive::TarGz)
        || install.kind == InstallKind::VendorPage
        || !safe_component(&entry.id)
        || !entry.version.as_deref().is_some_and(safe_component)
        || entry.license.as_deref().is_none_or(|s| s.trim().is_empty())
        || !install.sha256.as_deref().is_some_and(is_lowercase_sha256)
        || !install
            .bytes
            .is_some_and(|n| n > 0 && n <= MAX_RUNTIME_DOWNLOAD_BYTES)
    {
        return Err(fail("Invalid separator runtime descriptor"));
    }
    runtime_path(&entry.id, os).map_err(|message| fail(&message))?;
    runtime.validate_layout(os).map_err(|mut error| {
        error.path = format!("{path}.runtime");
        error
    })
}
/// Validate companion archive metadata when present.
pub fn validate_companion(install: &Install, path: &str) -> Result<(), CatalogError> {
    let Some(content) = &install.companion_content else {
        return Ok(());
    };
    let fail = |message: &str| {
        CatalogError::new(
            CatalogErrorCode::Companion,
            format!("{path}.companion_content"),
            message,
        )
    };
    if content.target != "surge-xt-factory"
        || content.archive != Archive::TarGz
        || !matches!(
            install.platform,
            Platform::MacosAarch64 | Platform::MacosX86_64
        )
        || install.archive != Some(Archive::Zip)
        || !matches!(install.format, Format::Vst3 | Format::Clap)
        || install.runtime.is_some()
        || install.kind == InstallKind::VendorPage
        || !is_lowercase_sha256(&content.sha256)
        || content.bytes == 0
        || content.bytes > MAX_DOWNLOAD_BYTES
        || !crate::is_install_url(&content.url)
        || !install.sha256.as_deref().is_some_and(is_lowercase_sha256)
        || !install
            .bytes
            .is_some_and(|n| n > 0 && n <= MAX_DOWNLOAD_BYTES)
    {
        return Err(fail("Invalid companion factory content descriptor"));
    }
    if !content.strip_prefix.is_empty()
        && crate::sanitize_zip_path(&content.strip_prefix)
            .map_err(|message| fail(&message))?
            .to_str()
            != Some(content.strip_prefix.as_str())
    {
        return Err(fail("Invalid companion archive prefix"));
    }
    Ok(())
}
/// Whether the declared plugin id matches its format.
pub fn valid_plugin_id(install: &Install) -> bool {
    let Some(id) = &install.plugin_id else {
        return true;
    };
    if id.is_empty() || id.len() > 256 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    match install.format {
        Format::Vst2 | Format::Vst3 => id
            .strip_prefix(&format!("{}:", install.format.as_str()))
            .is_some_and(|slug| {
                !slug.is_empty()
                    && slug
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            }),
        Format::Lv2 => {
            let Some((scheme, rest)) = id.split_once(':') else {
                return false;
            };
            !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
                && match rest.strip_prefix("//") {
                    Some(authority) => !authority.split('/').next().unwrap_or_default().is_empty(),
                    None => scheme == "urn" && !rest.is_empty(),
                }
        }
        Format::Clap | Format::Au | Format::Standalone => true,
    }
}
/// Validate an instrument pack entry.
pub fn validate_pack(pack: &PackEntry, path: &str) -> Result<(), CatalogError> {
    let fail = |field: &str, message: &str| {
        CatalogError::new(CatalogErrorCode::Pack, format!("{path}.{field}"), message)
    };
    if pack.kind != SOUNDFONT_PACK_KIND || !is_valid_id(&pack.id) {
        return Err(fail("id", "Invalid instrument pack id or kind"));
    }
    for (field, value, limit) in [
        ("name", pack.name.as_str(), 128),
        ("version", pack.version.as_str(), 64),
        ("description", pack.description.as_str(), 280),
        ("license.name", pack.license.name.as_str(), 64),
        ("license.text", pack.license.text.as_str(), 16 * 1024),
    ] {
        if value.trim().is_empty() || value.len() > limit {
            return Err(fail(
                field,
                "Instrument pack text is empty or exceeds its limit",
            ));
        }
    }
    for (field, url) in [("url", &pack.url), ("license.url", &pack.license.url)] {
        if !is_absolute_http_url(url) {
            return Err(fail(field, "Instrument pack has an unusable URL"));
        }
    }
    if pack.bytes == 0 {
        return Err(fail("bytes", "Instrument pack declares no size"));
    }
    if !is_lowercase_sha256(&pack.sha256) {
        return Err(fail("sha256", "Instrument pack has an invalid sha256"));
    }
    Ok(())
}

/// Validate a whole catalog and report the first error.
pub fn validate(catalog: &Catalog) -> Result<(), CatalogError> {
    use CatalogErrorCode as Code;
    if !(crate::index::MIN_CATALOG_SCHEMA..=crate::index::CATALOG_SCHEMA).contains(&catalog.schema)
    {
        return Err(CatalogError::new(
            Code::Schema,
            "schema",
            "Unsupported catalog schema",
        ));
    }
    crate::index::generated_at(&catalog.generated_at)
        .map_err(|error| CatalogError::new(Code::Timestamp, "generated_at", error.message))?;
    if catalog.entries.len() > crate::index::MAX_CATALOG_ENTRIES || catalog.packs.len() > MAX_PACKS
    {
        return Err(CatalogError::new(
            Code::SizeLimit,
            "$",
            "Catalog item count exceeds its limit",
        ));
    }
    let mut ids = HashSet::new();
    for (index, entry) in catalog.entries.iter().enumerate() {
        let path = format!("entries[{index}]");
        let fail = |code, field: &str, message: &str| {
            CatalogError::new(code, format!("{path}.{field}"), message)
        };
        if !is_valid_id(&entry.id) {
            return Err(fail(Code::InvalidId, "id", "Invalid plugin id"));
        }
        if !ids.insert(&entry.id) {
            return Err(fail(Code::DuplicateId, "id", "Duplicate plugin id"));
        }
        for (field, value) in [("name", &entry.name), ("vendor", &entry.vendor)] {
            if value.trim().is_empty() {
                return Err(fail(
                    Code::RequiredField,
                    field,
                    "Plugin must have a name and vendor",
                ));
            }
        }
        if entry
            .homepage
            .as_deref()
            .is_some_and(|url| !is_https_url(url))
        {
            return Err(fail(
                Code::InvalidUrl,
                "homepage",
                "Homepage must use https://",
            ));
        }
        if entry.description.as_deref().is_some_and(|value| {
            !value
                .chars()
                .last()
                .is_some_and(|c| matches!(c, '.' | '!' | '?'))
                || value.contains(". ")
        }) {
            return Err(fail(
                Code::Description,
                "description",
                "Plugin description must be one sentence",
            ));
        }
        if entry.license.as_deref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > 64
                || !value.chars().all(|c| c.is_ascii_graphic() || c == ' ')
        }) {
            return Err(fail(
                Code::InvalidLicense,
                "license",
                "Invalid plugin license",
            ));
        }
        if entry.tags.is_empty()
            || entry.tags.len() > 128
            || entry
                .tags
                .iter()
                .any(|tag| tag.trim().is_empty() || tag.len() > 64)
        {
            return Err(fail(
                Code::InvalidTag,
                "tags",
                "Plugin must declare non-empty tags of at most 64 bytes",
            ));
        }
        if entry.aliases.len() > 128 || entry.known_issues.len() > 128 {
            return Err(fail(
                Code::SizeLimit,
                "aliases",
                "Catalog metadata list exceeds its limit",
            ));
        }
        if entry.installs.is_empty() || entry.installs.len() > MAX_INSTALLS {
            return Err(fail(
                Code::RequiredField,
                "installs",
                "Plugin must declare at least one install within the count limit",
            ));
        }
        let mut seen = HashSet::new();
        for (number, install) in entry.installs.iter().enumerate() {
            let path = format!("{path}.installs[{number}]");
            let fail = |code, field: &str, message: &str| {
                CatalogError::new(code, format!("{path}.{field}"), message)
            };
            if !crate::is_install_url(&install.url) {
                return Err(fail(
                    Code::InvalidUrl,
                    "url",
                    "Install URL must use https:// (or loopback http)",
                ));
            }
            if install.kind == InstallKind::GithubRelease
                && install
                    .url
                    .strip_prefix("https://")
                    .and_then(|url| url.split('/').next())
                    != Some("github.com")
            {
                return Err(fail(
                    Code::InvalidUrl,
                    "url",
                    "Github release URL must use github.com",
                ));
            }
            if install
                .sha256
                .as_deref()
                .is_some_and(|value| !is_lowercase_sha256(value))
            {
                return Err(fail(
                    Code::Checksum,
                    "sha256",
                    "Install sha256 must be 64 lowercase hex characters",
                ));
            }
            if install.bytes == Some(0) {
                return Err(fail(
                    Code::InvalidSize,
                    "bytes",
                    "Install bytes must be greater than zero",
                ));
            }
            if !valid_plugin_id(install) {
                return Err(fail(
                    Code::PluginId,
                    "plugin_id",
                    "Invalid scanner plugin_id",
                ));
            }
            if !seen.insert((install.kind, install.platform, install.format)) {
                return Err(fail(
                    Code::DuplicateInstall,
                    "platform",
                    "Duplicate install for this source, platform and format",
                ));
            }
            validate_runtime(entry, install, &path)?;
            validate_companion(install, &path)?;
        }
    }
    let mut pack_ids = HashSet::new();
    for (index, pack) in catalog.packs.iter().enumerate() {
        let path = format!("packs[{index}]");
        if !is_valid_id(pack.id()) || pack.kind().trim().is_empty() || pack.kind().len() > 128 {
            return Err(CatalogError::new(
                Code::Pack,
                format!("{path}.id"),
                "Instrument pack must have a valid id and kind",
            ));
        }
        if !pack_ids.insert(pack.id()) {
            return Err(CatalogError::new(
                Code::DuplicateId,
                format!("{path}.id"),
                "Duplicate pack id",
            ));
        }
        if let Some(pack) = pack.soundfont() {
            validate_pack(pack, &path)?;
        }
    }
    Ok(())
}
/// Parse and validate catalog JSON, bounded by the size limit.
pub fn parse(bytes: &[u8]) -> Result<Catalog, CatalogError> {
    if bytes.len() > crate::index::MAX_CATALOG_BYTES {
        return Err(CatalogError::new(
            CatalogErrorCode::SizeLimit,
            "$",
            "Catalog exceeds its byte limit",
        ));
    }
    #[derive(Deserialize)]
    struct Parts {
        schema: u32,
        generated_at: String,
        #[serde(deserialize_with = "raw_entries")]
        entries: Vec<Box<RawValue>>,
        #[serde(default, deserialize_with = "raw_packs")]
        packs: Vec<Box<RawValue>>,
    }
    fn raw_entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Box<RawValue>>, D::Error> {
        bounded::<D, Box<RawValue>, { crate::index::MAX_CATALOG_ENTRIES }>(d)
    }
    fn raw_packs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Box<RawValue>>, D::Error> {
        bounded::<D, Box<RawValue>, MAX_PACKS>(d)
    }
    let parts: Parts = serde_json::from_slice(bytes)
        .map_err(|error| CatalogError::new(CatalogErrorCode::Parse, "$", error.to_string()))?;
    let entries = parts
        .entries
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            serde_json::from_str(raw.get()).map_err(|error| {
                CatalogError::new(
                    CatalogErrorCode::Parse,
                    format!("entries[{index}]"),
                    error.to_string(),
                )
            })
        })
        .collect::<Result<Vec<Entry>, _>>()?;
    let packs = parts
        .packs
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            serde_json::from_str(raw.get()).map_err(|error| {
                CatalogError::new(
                    CatalogErrorCode::Parse,
                    format!("packs[{index}]"),
                    error.to_string(),
                )
            })
        })
        .collect::<Result<Vec<Pack>, _>>()?;
    let catalog = Catalog {
        schema: parts.schema,
        generated_at: parts.generated_at,
        entries,
        packs,
    };
    validate(&catalog)?;
    Ok(catalog)
}
