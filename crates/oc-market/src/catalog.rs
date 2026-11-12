//! One resolution policy for catalog entries.
//!
//! Both the GUI installer and the `ocp plugin install` wrapper call
//! [`plan`], so a build that one of them installs is exactly the build the
//! other one installs, and both refuse in the same words.
//!
//! The rule this module exists to enforce: **an artifact installs only when the
//! signed catalog pins its sha256**. A URL alone is a suggestion, not a
//! verified artifact, so an unpinned build is refused instead of downloaded
//! and trusted.

use std::fmt;

use crate::install::{InstallArchive, InstallFormat, InstallKind, InstallSource};

/// What an accepted plan does after the bytes are verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Unpack into the user's plugin directory. Runs headlessly.
    Extract,
    /// Write the vendor installer and open it. A person finishes the install.
    HandOff,
}

impl Mode {
    /// The wire spelling used by `plugin_search` and `plugin_pull`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::HandOff => "installer",
        }
    }
}

/// One catalog install, normalised for the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub source: InstallSource,
    pub platform: String,
    pub format: InstallFormat,
    pub archive: Option<InstallArchive>,
    pub url: String,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    /// The catalog marks this build as a managed separator runtime.
    pub runtime: bool,
    /// The catalog marks this archive as wrapping a vendor installer.
    pub wrapped: bool,
}

/// A verified, installable build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub source: InstallSource,
    pub format: InstallFormat,
    pub archive: Option<InstallArchive>,
    pub url: String,
    /// Pinned by the signed catalog; never optional in a plan.
    pub sha256: String,
    pub bytes: Option<u64>,
    pub kind: InstallKind,
    pub mode: Mode,
}

/// Why a catalog entry cannot be installed. The code is stable for machines;
/// the message is what `plugin_search`, `plugin_pull` and the CLI print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    UnknownId {
        id: String,
    },
    NoBuild {
        name: String,
        platform: String,
    },
    /// The entry links a product page and nothing else.
    PageOnly {
        name: String,
        url: String,
    },
    /// The entry names a file the installer cannot unpack.
    NoArchive {
        name: String,
        format: InstallFormat,
    },
    /// A direct artifact without a pinned sha256.
    Unpinned {
        name: String,
        url: String,
        bytes: Option<u64>,
    },
    PlatformMismatch {
        platform: String,
    },
}

impl Refusal {
    /// Stable machine code; the CLI prints it, the agent tools return it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownId { .. } => "unknown_id",
            Self::NoBuild { .. } => "no_build",
            Self::PageOnly { .. } => "no_direct_archive",
            Self::NoArchive { .. } => "no_archive",
            Self::Unpinned { .. } => "unpinned",
            Self::PlatformMismatch { .. } => "platform_mismatch",
        }
    }

    /// The URL a person can open by hand, when the refusal has one.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::PageOnly { url, .. } | Self::Unpinned { url, .. } => Some(url),
            _ => None,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownId { id } => write!(formatter, "Unknown plugin catalog id '{id}'"),
            Self::NoBuild { name, platform } => {
                write!(formatter, "{name} has no build for {platform}")
            }
            Self::PageOnly { name, url } => {
                write!(
                    formatter,
                    "{name} has no direct archive; open {url} manually"
                )
            }
            Self::NoArchive { name, format } => write!(
                formatter,
                "{name} has no supported install archive for {}",
                format.as_str()
            ),
            Self::Unpinned { name, url, bytes } => {
                write!(
                    formatter,
                    "{name} has no pinned sha256 in the signed index; refusing to install {url}"
                )?;
                match bytes {
                    Some(bytes) => write!(
                        formatter,
                        " (a size of {bytes} bytes is not an integrity check)"
                    ),
                    None => Ok(()),
                }
            }
            Self::PlatformMismatch { platform } => write!(
                formatter,
                "Can only install for the current platform, not {platform}"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// A URL the installer may download from: https anywhere, or plain http on
/// loopback so the install path can be rehearsed against a fixture server.
/// Loopback is the user's own machine, so it grants an attacker nothing that
/// running the installer already implies.
pub fn is_install_url(url: &str) -> bool {
    if url
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return false;
    }
    if let Some(rest) = url.strip_prefix("https://") {
        let host = host_of(rest);
        return !host.is_empty() && host.contains('.');
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    // Loopback HTTP is a rehearsal seam, not a shipping transport: a release
    // build only ever downloads over TLS.
    cfg!(debug_assertions) && matches!(host_of(rest), "127.0.0.1" | "localhost" | "[::1]")
}

/// The host of an authority component, without its port. Bracketed IPv6
/// addresses keep their brackets so `[::1]` stays distinguishable.
fn host_of(rest: &str) -> &str {
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return "";
    }
    if authority.starts_with('[') {
        return match authority.find(']') {
            Some(close) => &authority[..=close],
            None => authority,
        };
    }
    authority.split(':').next().unwrap_or_default()
}

/// Where a build sits in the preference order: a real artifact beats a page,
/// and VST3 beats every other format.
fn rank(candidate: &Candidate) -> (u8, u8) {
    let source = if candidate.source == InstallSource::VendorPage {
        1
    } else {
        0
    };
    let format = match candidate.format {
        InstallFormat::Vst3 => 0,
        InstallFormat::Clap => 1,
        InstallFormat::Vst2 => 2,
        InstallFormat::Lv2 => 3,
        InstallFormat::Au => 4,
        InstallFormat::Standalone => 5,
    };
    (source, format)
}

/// Position of the build the installer would use for `platform`.
pub fn choose_index(
    candidates: &[Candidate],
    platform: &str,
    format: Option<InstallFormat>,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.platform == platform && format.is_none_or(|value| candidate.format == value)
        })
        .min_by_key(|(_, candidate)| rank(candidate))
        .map(|(index, _)| index)
}

/// The build the installer would use for `platform`.
pub fn choose<'a>(
    candidates: &'a [Candidate],
    platform: &str,
    format: Option<InstallFormat>,
) -> Option<&'a Candidate> {
    choose_index(candidates, platform, format).map(|index| &candidates[index])
}

/// Infer the archive from the file name when the catalog leaves it out.
pub fn infer_archive(url: &str) -> Option<InstallArchive> {
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Some(InstallArchive::TarGz)
    } else if path.ends_with(".zip") {
        Some(InstallArchive::Zip)
    } else if path.ends_with(".dmg") {
        Some(InstallArchive::Dmg)
    } else if path.ends_with(".pkg") {
        Some(InstallArchive::Pkg)
    } else if path.ends_with(".exe")
        || path.ends_with(".msi")
        || path.ends_with(".appimage")
        || path.ends_with(".deb")
        || path.ends_with(".rpm")
        || path.ends_with(".7z")
        || path.ends_with(".tar.xz")
    {
        Some(InstallArchive::Installer)
    } else {
        None
    }
}

/// How a verified download is applied, or `None` when the format has no
/// installable bundle (AU and standalone builds have no directory to unpack).
pub fn install_kind(
    source: InstallSource,
    archive: Option<InstallArchive>,
    url: &str,
    format: InstallFormat,
    runtime: bool,
    wrapped: bool,
) -> Option<InstallKind> {
    if runtime {
        return Some(InstallKind::SeparatorRuntime);
    }
    if wrapped {
        return Some(InstallKind::InstallerArchive);
    }
    if source == InstallSource::VendorPage {
        return Some(InstallKind::Page);
    }
    match archive.or_else(|| infer_archive(url))? {
        InstallArchive::Zip | InstallArchive::TarGz => match format {
            InstallFormat::Vst3 => Some(InstallKind::ZipVst3),
            InstallFormat::Vst2 => Some(InstallKind::ZipVst2),
            InstallFormat::Clap => Some(InstallKind::ZipClap),
            InstallFormat::Lv2 => Some(InstallKind::ZipLv2),
            InstallFormat::Au | InstallFormat::Standalone => Some(InstallKind::InstallerArchive),
        },
        InstallArchive::Dmg => Some(InstallKind::Dmg),
        InstallArchive::Pkg => Some(InstallKind::Pkg),
        InstallArchive::Installer => {
            let lower = url.to_ascii_lowercase();
            if lower.ends_with(".msi") {
                Some(InstallKind::Msi)
            } else if lower.ends_with(".appimage") {
                Some(InstallKind::Appimage)
            } else {
                Some(InstallKind::Exe)
            }
        }
    }
}

/// The mode a plan runs in. AppImages are copied, not unpacked, so they are
/// still headless.
fn mode_of(kind: InstallKind) -> Mode {
    match kind {
        InstallKind::ZipVst3
        | InstallKind::ZipVst2
        | InstallKind::ZipClap
        | InstallKind::ZipLv2
        | InstallKind::Appimage
        | InstallKind::SeparatorRuntime => Mode::Extract,
        InstallKind::Dmg
        | InstallKind::Pkg
        | InstallKind::Msi
        | InstallKind::Exe
        | InstallKind::InstallerArchive
        | InstallKind::Page => Mode::HandOff,
    }
}

/// Resolve the entry's build for one platform, or say why it is impossible.
///
/// Every surface goes through this function: a page-only entry, an unpinned
/// artifact and a missing platform all come back as a [`Refusal`], never as a
/// plan that would download something unverified.
pub fn plan(
    id: &str,
    name: &str,
    candidates: &[Candidate],
    platform: &str,
    format: Option<InstallFormat>,
) -> Result<Plan, Refusal> {
    let Some(candidate) = choose(candidates, platform, format) else {
        if candidates.is_empty() {
            return Err(Refusal::UnknownId { id: id.to_string() });
        }
        return Err(Refusal::NoBuild {
            name: name.to_string(),
            platform: platform.to_string(),
        });
    };
    if candidate.source == InstallSource::VendorPage {
        return Err(Refusal::PageOnly {
            name: name.to_string(),
            url: candidate.url.clone(),
        });
    }
    let kind = install_kind(
        candidate.source,
        candidate.archive,
        &candidate.url,
        candidate.format,
        candidate.runtime,
        candidate.wrapped,
    )
    .ok_or_else(|| Refusal::NoArchive {
        name: name.to_string(),
        format: candidate.format,
    })?;
    let Some(sha256) = candidate.sha256.clone() else {
        return Err(Refusal::Unpinned {
            name: name.to_string(),
            url: candidate.url.clone(),
            bytes: candidate.bytes,
        });
    };
    Ok(Plan {
        source: candidate.source,
        format: candidate.format,
        archive: candidate.archive,
        url: candidate.url.clone(),
        sha256,
        bytes: candidate.bytes,
        kind,
        mode: mode_of(kind),
    })
}

/// Whether an entry can be installed here, and what the caller should expect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStatus {
    pub installable: bool,
    pub mode: Option<Mode>,
    pub refusal: Option<Refusal>,
}

impl InstallStatus {
    fn refused(refusal: Refusal) -> Self {
        Self {
            installable: false,
            mode: None,
            refusal: Some(refusal),
        }
    }
}

/// [`plan`] without the error: the shape `plugin_search` reports per entry.
pub fn status(
    id: &str,
    name: &str,
    candidates: &[Candidate],
    platform: &str,
    format: Option<InstallFormat>,
) -> InstallStatus {
    match plan(id, name, candidates, platform, format) {
        Ok(plan) => InstallStatus {
            installable: true,
            mode: Some(plan.mode),
            refusal: None,
        },
        Err(refusal) => InstallStatus::refused(refusal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source: InstallSource, format: InstallFormat, url: &str) -> Candidate {
        Candidate {
            source,
            platform: "macos-aarch64".into(),
            format,
            archive: None,
            url: url.into(),
            sha256: Some("a".repeat(64)),
            bytes: Some(1024),
            runtime: false,
            wrapped: false,
        }
    }

    #[test]
    fn a_direct_artifact_beats_a_vendor_page() {
        let candidates = vec![
            candidate(
                InstallSource::VendorPage,
                InstallFormat::Vst3,
                "https://x.test/",
            ),
            candidate(
                InstallSource::GithubRelease,
                InstallFormat::Clap,
                "https://x.test/a.zip",
            ),
        ];
        let chosen = choose(&candidates, "macos-aarch64", None).unwrap();
        assert_eq!(chosen.source, InstallSource::GithubRelease);
    }

    #[test]
    fn vst3_is_preferred_over_other_formats() {
        let candidates = vec![
            candidate(
                InstallSource::Direct,
                InstallFormat::Au,
                "https://x.test/a.zip",
            ),
            candidate(
                InstallSource::Direct,
                InstallFormat::Vst3,
                "https://x.test/b.zip",
            ),
        ];
        let chosen = choose(&candidates, "macos-aarch64", None).unwrap();
        assert_eq!(chosen.format, InstallFormat::Vst3);
    }

    #[test]
    fn a_pinned_zip_is_an_extract_plan() {
        let candidates = vec![candidate(
            InstallSource::GithubRelease,
            InstallFormat::Vst3,
            "https://x.test/dexed.zip",
        )];
        let plan = plan("dexed", "Dexed", &candidates, "macos-aarch64", None).unwrap();
        assert_eq!(plan.kind, InstallKind::ZipVst3);
        assert_eq!(plan.mode, Mode::Extract);
        assert_eq!(plan.sha256.len(), 64);
    }

    #[test]
    fn a_runtime_descriptor_plans_a_managed_extract() {
        let mut candidate = candidate(
            InstallSource::GithubRelease,
            InstallFormat::Standalone,
            "https://x.test/spleeter.tar.gz",
        );
        candidate.archive = Some(InstallArchive::TarGz);
        candidate.runtime = true;
        let plan = plan("spleeter", "Spleeter", &[candidate], "macos-aarch64", None).unwrap();
        assert_eq!(plan.kind, InstallKind::SeparatorRuntime);
        assert_eq!(plan.mode, Mode::Extract);
    }

    #[test]
    fn a_disk_image_is_a_hand_off_plan() {
        let candidates = vec![candidate(
            InstallSource::Direct,
            InstallFormat::Vst3,
            "https://x.test/a.dmg",
        )];
        let plan = plan("a", "A", &candidates, "macos-aarch64", None).unwrap();
        assert_eq!(plan.kind, InstallKind::Dmg);
        assert_eq!(plan.mode, Mode::HandOff);
    }

    #[test]
    fn an_unpinned_artifact_is_refused_by_name() {
        let mut candidate = candidate(
            InstallSource::Direct,
            InstallFormat::Vst3,
            "https://x.test/a.zip",
        );
        candidate.sha256 = None;
        candidate.bytes = Some(4096);
        let refusal = plan("a", "Alpha", &[candidate], "macos-aarch64", None).unwrap_err();
        assert_eq!(refusal.code(), "unpinned");
        let message = refusal.to_string();
        assert!(
            message.starts_with("Alpha has no pinned sha256"),
            "{message}"
        );
        assert!(message.contains("https://x.test/a.zip"), "{message}");
        assert!(message.contains("4096 bytes"), "{message}");
    }

    #[test]
    fn a_page_only_entry_names_the_page() {
        let candidates = vec![candidate(
            InstallSource::VendorPage,
            InstallFormat::Vst3,
            "https://vital.audio/",
        )];
        let refusal = plan("vital", "Vital", &candidates, "macos-aarch64", None).unwrap_err();
        assert_eq!(refusal.code(), "no_direct_archive");
        assert_eq!(
            refusal.to_string(),
            "Vital has no direct archive; open https://vital.audio/ manually"
        );
    }

    #[test]
    fn a_missing_platform_is_not_an_unknown_id() {
        let candidates = vec![candidate(
            InstallSource::Direct,
            InstallFormat::Vst3,
            "https://x.test/a.zip",
        )];
        let refusal = plan("a", "A", &candidates, "linux-x86_64", None).unwrap_err();
        assert_eq!(refusal.code(), "no_build");
        assert!(refusal.to_string().contains("linux-x86_64"));
        let unknown = plan("a", "A", &[], "macos-aarch64", None).unwrap_err();
        assert_eq!(unknown.code(), "unknown_id");
    }

    #[test]
    fn a_url_without_a_known_archive_is_refused() {
        let candidates = vec![candidate(
            InstallSource::Direct,
            InstallFormat::Vst3,
            "https://x.test/download",
        )];
        let refusal = plan("a", "A", &candidates, "macos-aarch64", None).unwrap_err();
        assert_eq!(refusal.code(), "no_archive");
    }

    #[test]
    fn an_au_archive_is_an_installer_hand_off() {
        // An AU build has no bundle directory to place, so a zip that carries it
        // can only wrap the vendor installer.
        let candidates = vec![candidate(
            InstallSource::Direct,
            InstallFormat::Au,
            "https://x.test/Podolski_Mac.zip",
        )];
        let plan = plan("podolski", "Podolski", &candidates, "macos-aarch64", None).unwrap();
        assert_eq!(plan.kind, InstallKind::InstallerArchive);
        assert_eq!(plan.mode, Mode::HandOff);
    }

    #[test]
    fn status_reports_the_refusal_for_search() {
        let mut unpinned = candidate(
            InstallSource::Direct,
            InstallFormat::Vst3,
            "https://x.test/a.zip",
        );
        unpinned.sha256 = None;
        let status = status("a", "A", &[unpinned], "macos-aarch64", None);
        assert!(!status.installable);
        assert_eq!(status.refusal.unwrap().code(), "unpinned");
    }

    #[test]
    fn install_urls_allow_https_and_loopback_http_only() {
        assert!(is_install_url("https://github.com/x/y.zip"));
        assert!(is_install_url("http://127.0.0.1:8080/a.zip"));
        assert!(is_install_url("http://localhost:8080/a.zip"));
        assert!(is_install_url("http://[::1]:8080/a.zip"));
        assert!(!is_install_url("http://[::2]:8080/a.zip"));
        assert!(!is_install_url("http://example.com/a.zip"));
        assert!(!is_install_url("ftp://example.com/a.zip"));
        assert!(!is_install_url("https://"));
    }

    #[test]
    fn archive_inference_reads_the_path_not_the_query() {
        assert_eq!(
            infer_archive("https://x.test/a.zip?v=2"),
            Some(InstallArchive::Zip)
        );
        assert_eq!(
            infer_archive("https://x.test/a.tar.gz"),
            Some(InstallArchive::TarGz)
        );
        assert_eq!(infer_archive("https://x.test/download"), None);
    }
}
