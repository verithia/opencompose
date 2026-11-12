//! Signed catalog wire types and validation shared by publishers and clients.

use crate::install::{
    HostOs, InstallArchive as Archive, InstallFormat as Format, InstallSource as InstallKind,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::fmt;

/// Most packs a catalog may declare.
pub const MAX_PACKS: usize = 1024;
/// Largest single pack payload.
pub const MAX_PACK_BYTES: usize = 64 * 1024;
/// Most install entries per catalog.
pub const MAX_INSTALLS: usize = 64;
/// Most runtime files per descriptor.
pub const MAX_RUNTIME_FILES: usize = 256;
/// Largest install download.
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Largest runtime download.
pub const MAX_RUNTIME_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Pack kind for a SoundFont instrument.
pub const SOUNDFONT_PACK_KIND: &str = "soundfont";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Stable machine-readable reason a catalog failed validation.
pub enum CatalogErrorCode {
    Parse,
    SizeLimit,
    Schema,
    Timestamp,
    InvalidId,
    DuplicateId,
    RequiredField,
    InvalidUrl,
    InvalidLicense,
    InvalidTag,
    Description,
    DuplicateInstall,
    Checksum,
    InvalidSize,
    PluginId,
    Runtime,
    Companion,
    Pack,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// One validation failure with its JSON path and message.
pub struct CatalogError {
    pub code: CatalogErrorCode,
    pub path: String,
    pub message: String,
}
impl CatalogError {
    /// An error at `path` with a stable code.
    pub fn new(
        code: CatalogErrorCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}
impl fmt::Display for CatalogError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{:?} at {}: {}", self.code, self.path, self.message)
    }
}
impl std::error::Error for CatalogError {}

fn bounded<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>, const LIMIT: usize>(
    deserializer: D,
) -> Result<Vec<T>, D::Error> {
    struct Items<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> serde::de::Visitor<'de> for Items<T, LIMIT> {
        type Value = Vec<T>;
        fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(out, "an array of at most {LIMIT} items")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Vec<T>, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = sequence.next_element()? {
                if items.len() >= LIMIT {
                    return Err(serde::de::Error::custom(
                        "Catalog item count exceeds its limit",
                    ));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    deserializer.deserialize_seq(Items::<T, LIMIT>(std::marker::PhantomData))
}
fn entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Entry>, D::Error> {
    bounded::<D, Entry, { crate::index::MAX_CATALOG_ENTRIES }>(d)
}
fn packs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Pack>, D::Error> {
    bounded::<D, Pack, MAX_PACKS>(d)
}
fn installs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Install>, D::Error> {
    bounded::<D, Install, MAX_INSTALLS>(d)
}
fn runtime_files<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<RuntimeFile>, D::Error> {
    bounded::<D, RuntimeFile, MAX_RUNTIME_FILES>(d)
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A catalog build target.
pub enum Platform {
    #[serde(rename = "macos-aarch64")]
    MacosAarch64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
}

impl Platform {
    /// Wire spelling used by the catalog and by `ocp plugin install`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacosAarch64 => "macos-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
            Self::WindowsX86_64 => "windows-x86_64",
            Self::LinuxX86_64 => "linux-x86_64",
        }
    }

    /// Parse a wire spelling; `None` for an unknown platform.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "macos-aarch64" => Some(Self::MacosAarch64),
            "macos-x86_64" => Some(Self::MacosX86_64),
            "windows-x86_64" => Some(Self::WindowsX86_64),
            "linux-x86_64" => Some(Self::LinuxX86_64),
            _ => None,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}
impl std::ops::Deref for Platform {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl PartialEq<str> for Platform {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<&str> for Platform {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl PartialEq<String> for Platform {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other
    }
}
impl PartialEq<Platform> for String {
    fn eq(&self, other: &Platform) -> bool {
        self.as_str() == other.as_str()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// The signed catalog document and its plugin entries.
pub struct Catalog {
    pub schema: u32,
    pub generated_at: String,
    #[serde(deserialize_with = "entries")]
    pub entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(deserialize_with = "packs")]
    pub packs: Vec<Pack>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// One plugin offered by the catalog.
pub struct Entry {
    pub id: String,
    pub name: String,
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// SPDX-style identifier or `Freeware` for a proprietary free download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Other names the plugin is installed under; the client matches them when
    /// it reports what is already on disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Non-blocking notes shown beside the entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_issues: Vec<String>,
    #[serde(deserialize_with = "installs")]
    pub installs: Vec<Install>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// One downloadable build of a plugin entry.
pub struct Install {
    pub kind: InstallKind,
    pub platform: Platform,
    pub format: Format,
    pub url: String,
    /// The id the scanner emits for this build, e.g. `vst2:surge_xt` or the
    /// plugin URI for LV2. Required for the VST2 and LV2 formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// The archive wraps a vendor installer instead of bundle directories.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrapped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeDescriptor>,
    /// Signed companion content installed beside the plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_content: Option<CompanionContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<Archive>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// One file a managed runtime needs with its digest.
pub struct RuntimeFile {
    pub path: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// A managed runtime launcher and its required files.
pub struct RuntimeDescriptor {
    pub provider: String,
    pub layout_version: u32,
    pub launcher: String,
    #[serde(deserialize_with = "runtime_files")]
    pub required_files: Vec<RuntimeFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A companion archive installed beside a plugin.
pub struct CompanionContent {
    pub target: String,
    pub archive: Archive,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    #[serde(default)]
    pub strip_prefix: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// The licence text and URL for an instrument pack.
pub struct PackLicense {
    pub name: String,
    pub url: String,
    pub text: String,
}

/// A downloadable instrument pack from the catalog's `packs` array.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackEntry {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    pub license: PackLicense,
}

/// Unknown kinds stay opaque and bounded so a publisher can forward newer pack formats.
#[derive(Debug, Clone)]
pub enum Pack {
    Soundfont(PackEntry),
    Unknown(UnknownPack),
}
#[derive(Debug, Clone)]
/// A pack whose kind this build does not understand but retains.
pub struct UnknownPack {
    kind: String,
    id: String,
    raw: Box<RawValue>,
}
impl Pack {
    /// Pack id, whatever the kind.
    pub fn id(&self) -> &str {
        match self {
            Self::Soundfont(pack) => &pack.id,
            Self::Unknown(pack) => &pack.id,
        }
    }
    /// Pack kind string.
    pub fn kind(&self) -> &str {
        match self {
            Self::Soundfont(pack) => &pack.kind,
            Self::Unknown(pack) => &pack.kind,
        }
    }
    /// The SoundFont pack when this is one.
    pub fn soundfont(&self) -> Option<&PackEntry> {
        match self {
            Self::Soundfont(pack) => Some(pack),
            _ => None,
        }
    }
}
impl Serialize for Pack {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Soundfont(pack) => pack.serialize(serializer),
            Self::Unknown(pack) => pack.raw.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for Pack {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        if raw.get().len() > MAX_PACK_BYTES {
            return Err(serde::de::Error::custom(
                "Instrument pack exceeds its byte limit",
            ));
        }
        #[derive(Deserialize)]
        struct Header {
            kind: String,
            id: String,
        }
        let header: Header = serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
        if header.kind == SOUNDFONT_PACK_KIND {
            serde_json::from_str(raw.get())
                .map(Self::Soundfont)
                .map_err(serde::de::Error::custom)
        } else {
            let _: serde_json::Value =
                serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
            Ok(Self::Unknown(UnknownPack {
                kind: header.kind,
                id: header.id,
                raw,
            }))
        }
    }
}

mod validation;
pub use validation::*;
#[cfg(test)]
mod tests;
