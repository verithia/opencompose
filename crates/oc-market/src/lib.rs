//! Shared marketplace policy for the free-plugin catalog.
//!
//! The GUI installer and the `ocp plugin install` wrapper both use this crate,
//! so one resolver decides which build is installed, one rule verifies the
//! bytes, and both refuse in the same vocabulary:
//!
//! * [`catalog::plan`] resolves an entry to a pinned artifact or a
//!   [`catalog::Refusal`];
//! * [`verify`] checks the downloaded bytes against that pin;
//! * [`install`] unpacks a verified archive into the user's plugin directory;
//! * [`index`] holds the pinned index key and the signature check.

pub mod catalog;
pub mod catalog_wire;
/// Verified companion data and plugin bundles.
pub mod companion;
pub mod index;
pub mod install;
#[cfg(feature = "loopback")]
pub mod loopback;
pub mod tar;
pub mod temporary;
pub mod verify;

pub use catalog::{
    Candidate, InstallStatus, Mode, Plan, Refusal, choose, choose_index, install_kind,
    is_install_url, plan, status,
};
pub use index::{configured_public_key, key_id, trust_scope, verify_manifest_signature};
pub use install::{
    BundleFormat, HostDirs, HostOs, InstallArchive, InstallFormat, InstallKind, InstallOutcome,
    InstallSource, InstallTarget, MAX_ZIP_ENTRIES, MAX_ZIP_ENTRY_BYTES, MAX_ZIP_TOTAL_BYTES,
    StagingCleanup, bundle_format, bundle_kind, extract_bundles, extract_tar_bundles, host_os,
    host_platform, path_is_writable, platform_os, sanitize_zip_path, select_install_target,
    tar_octal, tar_text,
};
pub use verify::{Mismatch, sha256_hex, verify_artifact, verify_download};
