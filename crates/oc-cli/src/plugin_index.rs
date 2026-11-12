//! Remote plugin index: canonical manifest, zstd catalog, detached Ed25519 signature.

use anyhow::{Context, Result, bail};
use oc_diag::time::format_unix_seconds as format_rfc3339;
use oc_market::{index::hex_decode, key_id, sha256_hex, verify::hex as hex_encode};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::Serialize;
#[cfg(test)]
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use oc_market::index::{
    CATALOG_SCHEMA, MANIFEST_FILE, MANIFEST_SCHEMA, MIN_CATALOG_SCHEMA, SIGNATURE_FILE,
};
pub const DEFAULT_ZSTD_LEVEL: i32 = 9;
pub const PRIVATE_KEY_FILE: &str = "plugin-index.key";
pub const PUBLIC_KEY_FILE: &str = "plugin-index.pub";

const CATALOG_PREFIX: &str = "catalog-";
const CATALOG_SUFFIX: &str = ".json.zst";
const MAX_CATALOG_BYTES: u64 = oc_market::index::MAX_CATALOG_BYTES as u64;

pub use oc_market::catalog_wire::{Catalog, Entry, Install, Platform};
pub use oc_market::install::{
    InstallArchive as Archive, InstallFormat as Format, InstallSource as InstallKind,
};

pub use oc_market::index::{CatalogRef, Manifest, Mirror, SignatureInfo};

pub struct BuildOptions {
    pub catalog_path: PathBuf,
    pub out_dir: PathBuf,
    pub key_path: Option<PathBuf>,
    pub catalog_version: Option<String>,
    pub min_app_version: String,
    pub generated_at: Option<String>,
    pub level: i32,
    pub mirrors: Vec<Mirror>,
}

#[derive(Debug)]
pub struct BuildReport {
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    pub manifest_bytes: u64,
    pub signature_bytes: u64,
    pub sha256: String,
    pub catalog_path: String,
    pub catalog_version: String,
    pub generated_at: String,
    pub key_id: String,
    pub uploads: Vec<String>,
}

#[derive(Debug)]
pub struct VerifyReport {
    pub catalog_version: String,
    pub generated_at: String,
    pub entries: usize,
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    pub key_id: String,
}

pub struct KeygenReport {
    pub key_path: PathBuf,
    pub public_key_path: PathBuf,
    pub public_key: String,
    pub key_id: String,
}

pub fn build(options: &BuildOptions) -> Result<BuildReport> {
    if !(1..=22).contains(&options.level) {
        bail!("zstd level must be 1..=22, got {}", options.level);
    }
    validate_app_version(&options.min_app_version)?;
    for mirror in &options.mirrors {
        validate_mirror(mirror)?;
    }

    let generated_at = match &options.generated_at {
        Some(value) => {
            validate_timestamp(value)?;
            value.clone()
        }
        None => now_rfc3339()?,
    };
    validate_timestamp(&generated_at)?;
    let catalog_version = options
        .catalog_version
        .clone()
        .unwrap_or_else(|| default_catalog_version(&generated_at));
    if catalog_version.trim().is_empty() {
        bail!("catalog version must not be empty");
    }

    let input = fs::read(&options.catalog_path)
        .with_context(|| format!("read catalog {}", options.catalog_path.display()))?;
    let catalog = oc_market::catalog_wire::parse(&input)?;
    let raw = canonical_json(&catalog, true)?;
    if raw.len() as u64 > MAX_CATALOG_BYTES {
        bail!(
            "catalog is {} bytes, above the {} byte limit",
            raw.len(),
            MAX_CATALOG_BYTES
        );
    }

    let compressed = zstd::bulk::compress(&raw, options.level).context("compress catalog")?;
    let sha256 = sha256_hex(&compressed);
    let catalog_path = format!("{CATALOG_PREFIX}{sha256}{CATALOG_SUFFIX}");

    let key = load_signing_key(options.key_path.as_deref())?;
    let public_key = key.public_key().as_ref().to_vec();
    let key_id = key_id(&public_key);
    let manifest = Manifest {
        schema: MANIFEST_SCHEMA,
        generated_at: generated_at.clone(),
        catalog_version: catalog_version.clone(),
        min_app_version: options.min_app_version.clone(),
        catalog: CatalogRef {
            path: catalog_path.clone(),
            sha256: sha256.clone(),
            bytes: compressed.len() as u64,
            uncompressed_bytes: raw.len() as u64,
            encoding: "zstd".to_string(),
            compression_level: options.level,
        },
        signature: Some(SignatureInfo {
            algorithm: "ed25519".to_string(),
            path: SIGNATURE_FILE.to_string(),
            key_id: key_id.clone(),
            encoding: "raw".to_string(),
        }),
        mirrors: options.mirrors.clone(),
    };
    let manifest_bytes = canonical_json(&manifest, true)?;
    let signature = key.sign(&manifest_bytes);

    fs::create_dir_all(&options.out_dir)
        .with_context(|| format!("create output dir {}", options.out_dir.display()))?;
    write_file(&options.out_dir.join(&catalog_path), &compressed)?;
    write_file(&options.out_dir.join(SIGNATURE_FILE), signature.as_ref())?;
    write_file(&options.out_dir.join(MANIFEST_FILE), &manifest_bytes)?;

    let uploads = vec![
        format!("{catalog_path} ({} bytes, immutable)", compressed.len()),
        format!(
            "{SIGNATURE_FILE} ({} bytes, sign the new manifest)",
            signature.as_ref().len()
        ),
        format!(
            "{MANIFEST_FILE} ({} bytes, publish last: commit point)",
            manifest_bytes.len()
        ),
    ];
    Ok(BuildReport {
        raw_bytes: raw.len() as u64,
        compressed_bytes: compressed.len() as u64,
        manifest_bytes: manifest_bytes.len() as u64,
        signature_bytes: signature.as_ref().len() as u64,
        sha256,
        catalog_path,
        catalog_version,
        generated_at,
        key_id,
        uploads,
    })
}

pub fn verify(dir: &Path, public_key_path: &Path) -> Result<VerifyReport> {
    let manifest_bytes = oc_market::index::read_bounded_file(
        &dir.join(MANIFEST_FILE),
        oc_market::index::MAX_MANIFEST_BYTES,
    )?;
    let signature = oc_market::index::read_bounded_file(
        &dir.join(SIGNATURE_FILE),
        oc_market::index::MAX_SIGNATURE_BYTES,
    )?;
    let public_key = load_public_key(public_key_path)?;
    let manifest = oc_market::index::authenticate(&manifest_bytes, &signature, &public_key)?;
    let compressed = oc_market::index::read_bounded_file(
        &dir.join(&manifest.catalog.path),
        oc_market::index::MAX_CATALOG_DOWNLOAD_BYTES,
    )?;
    let index = oc_market::index::verify_parts(
        &oc_market::index::Policy {
            public_key,
            origins: Vec::new(),
            app_version: None,
        },
        manifest_bytes,
        signature,
        compressed,
        String::new(),
        None,
    )?;
    let raw = &index.catalog;
    let compressed = &index.compressed;
    let derived_key_id = index.manifest.signature.as_ref().unwrap().key_id.clone();
    let catalog = &index.document;
    Ok(VerifyReport {
        catalog_version: manifest.catalog_version,
        generated_at: manifest.generated_at,
        entries: catalog.entries.len(),
        raw_bytes: raw.len() as u64,
        compressed_bytes: compressed.len() as u64,
        key_id: derived_key_id,
    })
}

pub fn keygen(out_dir: &Path, force: bool) -> Result<KeygenReport> {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|error| anyhow::anyhow!("generate ed25519 key: {error:?}"))?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|error| anyhow::anyhow!("load generated key: {error:?}"))?;
    let public_key = pair.public_key().as_ref().to_vec();
    fs::create_dir_all(out_dir).with_context(|| format!("create key dir {}", out_dir.display()))?;
    let key_path = out_dir.join(PRIVATE_KEY_FILE);
    let public_key_path = out_dir.join(PUBLIC_KEY_FILE);
    if !force && (key_path.exists() || public_key_path.exists()) {
        bail!(
            "{} or {} already exists; pass --force to overwrite",
            key_path.display(),
            public_key_path.display()
        );
    }
    write_private(&key_path, pkcs8.as_ref())?;
    let public_hex = hex_encode(&public_key);
    write_file(&public_key_path, format!("{public_hex}\n").as_bytes())?;
    Ok(KeygenReport {
        key_path,
        public_key_path,
        key_id: key_id(&public_key),
        public_key: public_hex,
    })
}

pub fn parse_mirror_arg(value: &str) -> Result<Mirror> {
    let (name, base_url) = value
        .split_once('=')
        .with_context(|| format!("mirror must be NAME=URL, got {value}"))?;
    if name.trim().is_empty() {
        bail!("mirror name must not be empty");
    }
    let mut base_url = base_url.trim().to_string();
    if !base_url.ends_with('/') {
        base_url.push('/');
    }
    let mirror = Mirror {
        name: name.trim().to_string(),
        base_url,
    };
    validate_mirror(&mirror)?;
    Ok(mirror)
}

pub fn validate_catalog(catalog: &Catalog) -> Result<()> {
    oc_market::catalog_wire::validate(catalog).map_err(anyhow::Error::new)
}

fn validate_app_version(version: &str) -> Result<()> {
    semver::Version::parse(version)
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

fn validate_timestamp(value: &str) -> Result<()> {
    oc_market::index::generated_at(value)
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

fn validate_https_url(field: &str, url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("{field} must use https://, got {url}");
    }
    let rest = &url["https://".len()..];
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty()
        || host.starts_with(':')
        || rest.chars().any(char::is_whitespace)
        || !host.contains('.')
    {
        bail!("{field} has an invalid host: {url}");
    }
    Ok(())
}

fn validate_mirror(mirror: &Mirror) -> Result<()> {
    if mirror.name.trim().is_empty() {
        bail!("mirror name must not be empty");
    }
    validate_https_url("mirror base_url", &mirror.base_url)?;
    if !mirror.base_url.ends_with('/') {
        bail!("mirror base_url must end with '/', got {}", mirror.base_url);
    }
    Ok(())
}

fn canonical_json<T: Serialize>(value: &T, pretty: bool) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).context("canonicalize JSON")?;
    let mut bytes = if pretty {
        serde_json::to_vec_pretty(&value)?
    } else {
        serde_json::to_vec(&value)?
    };
    bytes.push(b'\n');
    Ok(bytes)
}

fn load_signing_key(explicit: Option<&Path>) -> Result<Ed25519KeyPair> {
    let bytes = if let Some(path) = explicit {
        fs::read(path).with_context(|| format!("read signing key {}", path.display()))?
    } else if let Ok(path) = env::var("OCP_PLUGIN_INDEX_KEY") {
        let path = PathBuf::from(path);
        fs::read(&path).with_context(|| format!("read signing key {}", path.display()))?
    } else if let Ok(encoded) = env::var("OCP_PLUGIN_INDEX_KEY_PKCS8_HEX") {
        hex_decode(encoded.trim())
            .map_err(anyhow::Error::msg)
            .context("OCP_PLUGIN_INDEX_KEY_PKCS8_HEX is not valid hex")?
    } else {
        bail!(
            "no signing key: pass --key FILE, or set OCP_PLUGIN_INDEX_KEY, \
             or OCP_PLUGIN_INDEX_KEY_PKCS8_HEX"
        );
    };
    Ed25519KeyPair::from_pkcs8(&bytes)
        .map_err(|error| anyhow::anyhow!("parse PKCS#8 ed25519 signing key: {error:?}"))
}

pub fn load_public_key(path: &Path) -> Result<Vec<u8>> {
    let bytes = oc_market::index::read_bounded_file(path, 256)
        .with_context(|| format!("read public key {}", path.display()))?;
    if bytes.len() == 32 {
        return Ok(bytes);
    }
    let text = std::str::from_utf8(&bytes).context("public key is not raw bytes or ASCII hex")?;
    let key = hex_decode(text.trim())
        .map_err(anyhow::Error::msg)
        .context("public key is not 32 bytes or 64 hex characters")?;
    if key.len() != 32 {
        bail!("public key must be 32 bytes, got {}", key.len());
    }
    Ok(key)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create private key {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write private key {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod private key {}", path.display()))?;
    }
    Ok(())
}

fn now_rfc3339() -> Result<String> {
    let seconds = match env::var("SOURCE_DATE_EPOCH") {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .context("SOURCE_DATE_EPOCH must be Unix seconds")?,
        Err(_) => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs(),
    };
    Ok(format_rfc3339(seconds))
}

fn default_catalog_version(generated_at: &str) -> String {
    let digits = generated_at
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect::<String>();
    if digits.len() < 14 {
        return generated_at.to_string();
    }
    format!(
        "{}.{}.{}.{}",
        &digits[0..4],
        &digits[4..6],
        &digits[6..8],
        &digits[8..14]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "ocp-plugin-index-{name}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_catalog() -> Catalog {
        let vendors = [
            "Northwind Audio",
            "Kestrel DSP",
            "Marlowe Labs",
            "Fjord Instruments",
            "Cinder Works",
            "Halcyon Audio",
        ];
        let tags: [&[&str]; 6] = [
            &["reverb", "ambience"],
            &["compressor", "dynamics", "mixing"],
            &["synthesizer", "modulation", "wavetable"],
            &["equalizer", "mastering"],
            &["delay", "creative"],
            &["sampler", "drums"],
        ];
        let descriptions = [
            "A lush algorithmic reverb with three decay models, damping, and a modulation stage tuned for long ambient tails.",
            "A transparent bus compressor with program-dependent release, sidechain filtering, and parallel mix for glue without pumping.",
            "A hybrid wavetable synthesizer with two oscillators, per-voice modulation, and a built-in arpeggiator with probability.",
            "A mastering equalizer with dynamic bands, mid-side processing, and linear-phase mode for delicate high-frequency work.",
            "A stereo delay with pitch-shifted taps, diffusion, and tempo-synced modulation for spacious rhythmic echoes.",
            "A drum sampler with layered velocity, per-pad envelopes, and drag-and-drop import for building custom kits quickly.",
        ];
        let entries = (0..60)
            .map(|index| {
                let vendor = vendors[index % vendors.len()];
                let slug = format!("{}-{}", vendor.to_lowercase().replace(' ', "-"), index);
                let tags = tags[index % tags.len()]
                    .iter()
                    .map(|tag| tag.to_string())
                    .collect::<Vec<_>>();
                Entry {
                    id: format!("{slug}.plugin"),
                    name: format!("{} {}", vendor.split(' ').next().unwrap(), index),
                    vendor: vendor.to_string(),
                    version: Some(format!("1.{}.{}", index % 10, index % 7)),
                    description: Some(descriptions[index % descriptions.len()].to_string()),
                    homepage: Some(format!("https://plugins.example.com/{slug}")),
                    license: Some("MIT".to_string()),
                    tags,
                    aliases: Vec::new(),
                    known_issues: Vec::new(),
                    installs: vec![
                        Install {
                            kind: InstallKind::GithubRelease,
                            platform: Platform::MacosAarch64,
                            format: Format::Vst3,
                            url: format!(
                                "https://github.com/example/{slug}/releases/download/v1.0.0/{slug}.zip"
                            ),
                            plugin_id: Some(format!("vst3:plugin_{index}")),
                            wrapped: false,
                            runtime: None,
                            companion_content: None,
                            sha256: Some(format!("{:064x}", index + 1)),
                            bytes: Some(12_000_000 + index as u64 * 1_000),
                            archive: Some(Archive::Zip),
                        },
                        Install {
                            kind: InstallKind::Direct,
                            platform: Platform::WindowsX86_64,
                            format: Format::Vst3,
                            url: format!("https://downloads.example.net/{slug}/setup.exe"),
                            plugin_id: None,
                            wrapped: false,
                            runtime: None,
                            companion_content: None,
                            sha256: None,
                            bytes: None,
                            archive: Some(Archive::Installer),
                        },
                    ],
                }
            })
            .collect();
        Catalog {
            schema: CATALOG_SCHEMA,
            generated_at: "2026-10-22T08:00:00Z".to_string(),
            entries,
            packs: Vec::new(),
        }
    }

    fn write_catalog(dir: &Path, catalog: &Catalog) -> PathBuf {
        let path = dir.join("catalog-source.json");
        let bytes = canonical_json(catalog, true).expect("serialize catalog");
        fs::write(&path, bytes).expect("write catalog");
        path
    }

    fn keypair(dir: &Path) -> PathBuf {
        keygen(dir, false).expect("keygen").key_path
    }

    fn build_fixture(dir: &Path, catalog: &Catalog, key: &Path) -> BuildReport {
        let catalog_path = write_catalog(dir, catalog);
        build(&BuildOptions {
            catalog_path,
            out_dir: dir.join("out"),
            key_path: Some(key.to_path_buf()),
            catalog_version: Some("2026.10.22.1".to_string()),
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: vec![parse_mirror_arg("raw=https://mirror.invalid/marketplace/main").unwrap()],
        })
        .unwrap_or_else(|error| panic!("build failed: {error:#}"))
    }

    #[test]
    fn keygen_writes_usable_keypair() {
        let dir = TestDir::new("keygen");
        let report = keygen(dir.path(), false).expect("keygen");
        assert_eq!(report.public_key.len(), 64);
        assert!(report.key_id.starts_with("ed25519:"));
        assert!(report.key_path.exists());
        assert!(report.public_key_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&report.key_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(keygen(dir.path(), false).is_err());
    }

    #[test]
    fn epoch_timestamp_conversion() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert!(validate_timestamp("2026-13-22T08:00:00Z").is_err());
        assert!(validate_timestamp("2026-10-22T24:00:00Z").is_err());
    }

    #[test]
    fn build_verify_round_trip() {
        let dir = TestDir::new("round-trip");
        let key = keypair(dir.path());
        let catalog = sample_catalog();
        let report = build_fixture(dir.path(), &catalog, &key);
        assert!(report.raw_bytes > report.compressed_bytes);
        let verified = verify(&dir.path().join("out"), &dir.path().join(PUBLIC_KEY_FILE))
            .expect("verify built index");
        assert_eq!(verified.entries, 60);
        assert_eq!(verified.catalog_version, "2026.10.22.1");
        assert_eq!(verified.key_id, report.key_id);
        assert_eq!(verified.compressed_bytes, report.compressed_bytes);
    }

    #[test]
    fn a_published_catalog_keeps_aliases_runtimes_and_companion_content() {
        let dir = TestDir::new("entry-extras");
        let key = keypair(dir.path());
        let mut catalog = sample_catalog();
        let entry = &mut catalog.entries[0];
        entry.aliases = vec!["Northwind Reverb".to_string(), "NW Reverb".to_string()];
        entry.known_issues = vec!["The first launch scans its content folder.".to_string()];
        let shared: serde_json::Value = serde_json::from_str(include_str!(
            "../../oc-market/tests/fixtures/catalog-validation.json"
        ))
        .unwrap();
        let complete =
            oc_market::catalog_wire::parse(&serde_json::to_vec(&shared["complete"]).unwrap())
                .unwrap();
        entry.installs = complete.entries[0].installs.clone();
        entry.installs[0].wrapped = true;
        let report = build_fixture(dir.path(), &catalog, &key);
        let bytes = fs::read(dir.path().join("out").join(&report.catalog_path)).unwrap();
        let raw = zstd::bulk::decompress(&bytes, 64 * 1024 * 1024).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let entry = &value["entries"][0];
        assert_eq!(entry["aliases"][0], "Northwind Reverb");
        assert_eq!(entry["aliases"][1], "NW Reverb");
        assert_eq!(
            entry["known_issues"][0],
            "The first launch scans its content folder."
        );
        assert_eq!(entry["installs"][1]["runtime"]["provider"], "spleeter");
        assert_eq!(
            entry["installs"][1]["runtime"]["required_files"][0]["path"],
            "bin/spleeter"
        );
        assert_eq!(
            entry["installs"][0]["companion_content"]["target"],
            "surge-xt-factory"
        );
        assert_eq!(entry["installs"][0]["companion_content"]["bytes"], 1024u64);
        assert_eq!(entry["installs"][0]["wrapped"], true);
        // The client parses the descriptor with deny_unknown_fields, so the
        // round trip has to keep every key it was given.
        assert_eq!(
            entry["installs"][1]["runtime"].as_object().unwrap().len(),
            4
        );
    }

    #[test]
    fn publisher_rejects_shared_invalid_documents_with_typed_codes_and_paths() {
        let dir = TestDir::new("shared-catalog-boundaries");
        let key = keypair(dir.path());
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../oc-market/tests/fixtures/catalog-validation.json"
        ))
        .unwrap();
        for case in fixture["invalid"].as_array().unwrap() {
            let error = build_json_error(dir.path(), &case["document"], &key);
            let actual = error
                .downcast_ref::<oc_market::catalog_wire::CatalogError>()
                .expect("shared typed validation error");
            assert_eq!(
                serde_json::to_value(actual.code).unwrap(),
                case["code"],
                "{}: {error}",
                case["name"]
            );
            assert_eq!(actual.path, case["path"], "{}: {error}", case["name"]);
            assert!(
                !dir.path().join("out").exists(),
                "rejected descriptors must not publish an index"
            );
        }
        let complete =
            oc_market::catalog_wire::parse(&serde_json::to_vec(&fixture["complete"]).unwrap())
                .unwrap();
        let report = build_fixture(dir.path(), &complete, &key);
        let verified = verify(&dir.path().join("out"), &dir.path().join(PUBLIC_KEY_FILE)).unwrap();
        assert_eq!(verified.entries, 1);
        let raw = zstd::bulk::decompress(
            &fs::read(dir.path().join("out").join(report.catalog_path)).unwrap(),
            MAX_CATALOG_BYTES as usize,
        )
        .unwrap();
        let encoded: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(
            encoded["packs"], fixture["complete"]["packs"],
            "unknown pack payloads stay intact"
        );
    }

    #[test]
    fn a_published_catalog_keeps_its_instrument_packs() {
        let dir = TestDir::new("packs");
        let key = keypair(dir.path());
        let mut catalog = sample_catalog();
        catalog.packs = vec![
            serde_json::from_value(serde_json::json!({
                "kind": "soundfont",
                "id": "ms-basic",
                "name": "MS Basic",
                "version": "1.0.0",
                "description": "General MIDI sound set for the built-in SoundFont engine.",
                "url": "https://plugins.example.com/packs/ms-basic-1.0.0.sf3",
                "bytes": 49_999_999u64,
                "sha256": "a".repeat(64),
                "license": {
                    "name": "MIT",
                    "url": "https://plugins.example.com/packs/MS%20Basic_License.md",
                    "text": "MuseScore_General, MIT, derived from FluidR3Mono and FluidR3."
                }
            }))
            .unwrap(),
        ];
        let report = build_fixture(dir.path(), &catalog, &key);
        let bytes = fs::read(dir.path().join("out").join(&report.catalog_path)).unwrap();
        let raw = zstd::bulk::decompress(&bytes, 64 * 1024 * 1024).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(value["packs"][0]["id"], "ms-basic");
        assert_eq!(value["packs"][0]["bytes"], 49_999_999u64);
        assert_eq!(value["packs"][0]["license"]["name"], "MIT");
        assert_eq!(value["entries"].as_array().unwrap().len(), 60);

        // The publisher refuses a pack the client would reject.
        let mut broken = sample_catalog();
        let mut pack = catalog.packs[0].clone();
        let oc_market::catalog_wire::Pack::Soundfont(soundfont) = &mut pack else {
            panic!("expected soundfont pack");
        };
        soundfont.sha256 = "abc".into();
        broken.packs = vec![pack];
        let error = validate_catalog(&broken).unwrap_err();
        assert!(format!("{error:#}").contains("sha256"), "{error:#}");
    }

    #[test]
    fn tampered_catalog_rejected() {
        let dir = TestDir::new("tampered-catalog");
        let key = keypair(dir.path());
        build_fixture(dir.path(), &sample_catalog(), &key);
        let out = dir.path().join("out");
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(out.join(MANIFEST_FILE)).unwrap()).unwrap();
        let catalog_path = out.join(&manifest.catalog.path);
        let mut bytes = fs::read(&catalog_path).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&catalog_path, bytes).unwrap();
        let error = verify(&out, &dir.path().join(PUBLIC_KEY_FILE)).expect_err("tampered catalog");
        assert_eq!(
            error
                .downcast_ref::<oc_market::index::IndexError>()
                .expect("shared catalog verification error")
                .stage,
            "catalog_hash"
        );
    }

    #[test]
    fn tampered_manifest_rejected() {
        let dir = TestDir::new("tampered-manifest");
        let key = keypair(dir.path());
        build_fixture(dir.path(), &sample_catalog(), &key);
        let out = dir.path().join("out");
        let path = out.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("2026.10.22.1", "2026.10.22.2");
        fs::write(&path, text).unwrap();
        let error = verify(&out, &dir.path().join(PUBLIC_KEY_FILE)).expect_err("tampered manifest");
        assert_eq!(
            error
                .downcast_ref::<oc_market::index::IndexError>()
                .expect("shared manifest authentication error")
                .stage,
            "signature"
        );
    }

    #[test]
    fn tampered_signature_rejected() {
        let dir = TestDir::new("tampered-signature");
        let key = keypair(dir.path());
        build_fixture(dir.path(), &sample_catalog(), &key);
        let out = dir.path().join("out");
        let path = out.join(SIGNATURE_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0x01;
        fs::write(&path, bytes).unwrap();
        let error = verify(&out, &dir.path().join(PUBLIC_KEY_FILE)).expect_err("tampered sig");
        assert_eq!(
            error
                .downcast_ref::<oc_market::index::IndexError>()
                .expect("shared signature authentication error")
                .stage,
            "signature"
        );
    }

    #[test]
    fn unknown_fields_tolerated() {
        let dir = TestDir::new("unknown-fields");
        let key = keypair(dir.path());
        let mut value = serde_json::to_value(sample_catalog()).unwrap();
        value["future_top_level"] = serde_json::json!({"a": 1});
        value["entries"][0]["future_entry_field"] = serde_json::json!(["x"]);
        value["entries"][0]["installs"][0]["future_install_field"] = serde_json::json!(true);
        let catalog_path = dir.path().join("catalog-source.json");
        fs::write(&catalog_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        build(&BuildOptions {
            catalog_path,
            out_dir: dir.path().join("out"),
            key_path: Some(key),
            catalog_version: None,
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect("unknown fields should be ignored");
        verify(&dir.path().join("out"), &dir.path().join(PUBLIC_KEY_FILE))
            .expect("verify catalog with unknown fields");
    }

    #[test]
    fn non_https_url_rejected() {
        let dir = TestDir::new("non-https");
        let key = keypair(dir.path());
        let mut catalog = sample_catalog();
        catalog.entries[0].installs[0].url = "http://downloads.example.net/plugin.zip".to_string();
        let error = build_fixture_error(dir.path(), &catalog, &key);
        assert!(format!("{error:#}").contains("must use https://"));
    }

    #[test]
    fn duplicate_id_rejected() {
        let dir = TestDir::new("duplicate-id");
        let key = keypair(dir.path());
        let mut catalog = sample_catalog();
        catalog.entries[1].id = catalog.entries[0].id.clone();
        let error = build_fixture_error(dir.path(), &catalog, &key);
        assert_eq!(
            error
                .downcast_ref::<oc_market::catalog_wire::CatalogError>()
                .unwrap()
                .code,
            oc_market::catalog_wire::CatalogErrorCode::DuplicateId
        );
    }

    #[test]
    fn sixty_entry_catalog_compresses_under_12kb() {
        let dir = TestDir::new("size");
        let key = keypair(dir.path());
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-catalog-60.json");
        let input = fs::read(&fixture).expect("read fixture");
        let catalog: Catalog = serde_json::from_slice(&input).expect("parse fixture");
        assert_eq!(
            canonical_json(&catalog, true).expect("canonicalize fixture"),
            input,
            "fixture must stay canonical so documented sizes match the builder"
        );
        let report = build(&BuildOptions {
            catalog_path: fixture,
            out_dir: dir.path().join("out"),
            key_path: Some(key),
            catalog_version: Some("2026.10.22.1".to_string()),
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect("build fixture");
        println!(
            "fixture raw={} zstd={} manifest={}",
            report.raw_bytes, report.compressed_bytes, report.manifest_bytes
        );
        assert!(
            report.compressed_bytes < 12 * 1024,
            "60-entry catalog compressed to {} bytes",
            report.compressed_bytes
        );
    }

    #[test]
    fn build_is_deterministic() {
        let dir = TestDir::new("deterministic");
        let key = keypair(dir.path());
        let first = build_fixture(dir.path(), &sample_catalog(), &key);
        let out = dir.path().join("out");
        let manifest_first = fs::read(out.join(MANIFEST_FILE)).unwrap();
        let sig_first = fs::read(out.join(SIGNATURE_FILE)).unwrap();
        fs::remove_dir_all(&out).unwrap();
        let second = build_fixture(dir.path(), &sample_catalog(), &key);
        let manifest_second = fs::read(out.join(MANIFEST_FILE)).unwrap();
        let sig_second = fs::read(out.join(SIGNATURE_FILE)).unwrap();
        assert_eq!(first.catalog_path, second.catalog_path);
        assert_eq!(manifest_first, manifest_second);
        assert_eq!(sig_first, sig_second);
        let text = String::from_utf8(manifest_second).unwrap();
        assert!(text.find("\"catalog\"").unwrap() < text.find("\"schema\"").unwrap());
    }

    fn build_fixture_error(dir: &Path, catalog: &Catalog, key: &Path) -> anyhow::Error {
        let catalog_path = write_catalog(dir, catalog);
        build(&BuildOptions {
            catalog_path,
            out_dir: dir.join("out"),
            key_path: Some(key.to_path_buf()),
            catalog_version: None,
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect_err("build should reject invalid catalog")
    }

    fn catalog_json() -> serde_json::Value {
        serde_json::to_value(sample_catalog()).expect("serialize sample catalog")
    }

    fn build_json_error(dir: &Path, value: &serde_json::Value, key: &Path) -> anyhow::Error {
        let path = dir.join("catalog-source.json");
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        build(&BuildOptions {
            catalog_path: path,
            out_dir: dir.join("out"),
            key_path: Some(key.to_path_buf()),
            catalog_version: None,
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect_err("build should reject the catalog")
    }

    #[test]
    fn unknown_format_rejected() {
        let dir = TestDir::new("unknown-format");
        let key = keypair(dir.path());
        let mut value = catalog_json();
        value["entries"][0]["installs"][0]["format"] = serde_json::json!("aax");
        let error = build_json_error(dir.path(), &value, &key);
        assert!(
            format!("{error:#}").contains("unknown variant `aax`"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn missing_required_field_rejected() {
        let dir = TestDir::new("missing-field");
        let key = keypair(dir.path());
        let mut value = catalog_json();
        value["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("vendor");
        let error = build_json_error(dir.path(), &value, &key);
        assert!(
            format!("{error:#}").contains("missing field `vendor`"),
            "unexpected error: {error:#}"
        );

        let mut value = catalog_json();
        value["entries"][0]["installs"][0]
            .as_object_mut()
            .unwrap()
            .remove("url");
        let error = build_json_error(dir.path(), &value, &key);
        assert!(
            format!("{error:#}").contains("missing field `url`"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn bad_id_rejected() {
        let dir = TestDir::new("bad-id");
        let key = keypair(dir.path());
        let long = "x".repeat(129);
        for bad in ["bad id", "", long.as_str()] {
            let mut value = catalog_json();
            value["entries"][0]["id"] = serde_json::json!(bad);
            let error = build_json_error(dir.path(), &value, &key);
            let source = error
                .downcast_ref::<oc_market::catalog_wire::CatalogError>()
                .expect("shared catalog validation error");
            assert_eq!(
                source.code,
                oc_market::catalog_wire::CatalogErrorCode::InvalidId,
                "invalid id {bad:?}"
            );
            assert_eq!(source.path, "entries[0].id", "invalid id {bad:?}");
        }
    }

    #[test]
    fn non_http_url_rejected() {
        let dir = TestDir::new("non-http");
        let key = keypair(dir.path());
        for bad in [
            "ftp://downloads.example.net/plugin.zip",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://downloads.example.net/plugin.zip",
        ] {
            let mut value = catalog_json();
            value["entries"][0]["installs"][0]["url"] = serde_json::json!(bad);
            let error = build_json_error(dir.path(), &value, &key);
            assert!(
                format!("{error:#}").contains("must use https://"),
                "unexpected error for {bad}: {error:#}"
            );
        }
    }

    #[test]
    fn scanner_plugin_id_is_validated_when_present() {
        let dir = TestDir::new("plugin-id");
        let key = keypair(dir.path());
        for (format, plugin_ids) in [
            (
                "vst2",
                [
                    "vst2:surge_xt",
                    "vst2:surge_xt_2",
                    "vst2:Surge_XT",
                    "surge_xt",
                    "vst2:",
                ],
            ),
            (
                "lv2",
                [
                    "http://calf.sourceforge.net/plugins/Reverb",
                    "urn:example:plugin",
                    "not-a-uri",
                    "http://",
                    "urn:",
                ],
            ),
        ] {
            for (index, plugin_id) in plugin_ids.iter().enumerate() {
                let mut value = catalog_json();
                value["entries"][0]["installs"][0]["format"] = serde_json::json!(format);
                value["entries"][0]["installs"][0]["plugin_id"] = serde_json::json!(plugin_id);
                let path = dir.path().join(format!("catalog-{format}-{index}.json"));
                fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
                let result = build(&BuildOptions {
                    catalog_path: path,
                    out_dir: dir.path().join(format!("out-{format}-{index}")),
                    key_path: Some(key.clone()),
                    catalog_version: None,
                    min_app_version: "0.1.0".to_string(),
                    generated_at: Some("2026-10-22T08:00:00Z".to_string()),
                    level: DEFAULT_ZSTD_LEVEL,
                    mirrors: Vec::new(),
                });
                if index < 2 {
                    result.unwrap_or_else(|error| {
                        panic!("valid {format} plugin_id {plugin_id} rejected: {error:#}")
                    });
                } else {
                    let error = result.expect_err("invalid plugin_id accepted");
                    assert!(
                        format!("{error:#}").contains("plugin_id"),
                        "unexpected error for {plugin_id}: {error:#}"
                    );
                }
            }

            // Multi-plugin archives cannot name one id, so the field is optional.
            let mut value = catalog_json();
            value["entries"][0]["installs"][0]["format"] = serde_json::json!(format);
            value["entries"][0]["installs"][0]
                .as_object_mut()
                .unwrap()
                .remove("plugin_id");
            let path = dir.path().join(format!("optional-{format}.json"));
            fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            build(&BuildOptions {
                catalog_path: path,
                out_dir: dir.path().join(format!("out-optional-{format}")),
                key_path: Some(key.clone()),
                catalog_version: None,
                min_app_version: "0.1.0".to_string(),
                generated_at: Some("2026-10-22T08:00:00Z".to_string()),
                level: DEFAULT_ZSTD_LEVEL,
                mirrors: Vec::new(),
            })
            .unwrap_or_else(|error| panic!("{format} without plugin_id rejected: {error:#}"));
        }
    }

    #[test]
    fn catalog_schema_accepts_older_and_rejects_newer() {
        let dir = TestDir::new("schema");
        let key = keypair(dir.path());
        let mut value = catalog_json();
        value["schema"] = serde_json::json!(MIN_CATALOG_SCHEMA);
        let path = dir.path().join("old.json");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        build(&BuildOptions {
            catalog_path: path,
            out_dir: dir.path().join("out-old"),
            key_path: Some(key.clone()),
            catalog_version: None,
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-22T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect("schema 1 catalogs stay readable");

        let mut value = catalog_json();
        value["schema"] = serde_json::json!(CATALOG_SCHEMA + 1);
        let error = build_json_error(dir.path(), &value, &key);
        assert_eq!(
            error
                .downcast_ref::<oc_market::catalog_wire::CatalogError>()
                .unwrap()
                .code,
            oc_market::catalog_wire::CatalogErrorCode::Schema
        );
    }

    #[test]
    fn bundled_catalog_builds_and_verifies() {
        let dir = TestDir::new("bundled");
        let key = keypair(dir.path());
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../oc-gui/frontend/src/data/plugin-catalog.json");
        let catalog: Catalog =
            serde_json::from_slice(&fs::read(&fixture).expect("read bundled catalog"))
                .expect("parse bundled catalog");
        assert_eq!(catalog.schema, CATALOG_SCHEMA);
        let mut formats = HashSet::new();
        for entry in &catalog.entries {
            assert!(
                entry.license.is_some(),
                "bundled entry {} has no license",
                entry.id
            );
            for install in &entry.installs {
                formats.insert(install.format);
                if install.plugin_id.is_some() {
                    assert!(
                        matches!(install.format, Format::Vst2 | Format::Vst3 | Format::Lv2),
                        "{} records a plugin_id for {}",
                        entry.id,
                        install.format.as_str()
                    );
                }
            }
        }
        for format in [
            Format::Vst3,
            Format::Vst2,
            Format::Clap,
            Format::Au,
            Format::Lv2,
        ] {
            assert!(formats.contains(&format), "no {} installs", format.as_str());
        }
        let expected = catalog.entries.len();
        let report = build(&BuildOptions {
            catalog_path: fixture,
            out_dir: dir.path().join("out"),
            key_path: Some(key),
            catalog_version: Some("2026.10.26.1".to_string()),
            min_app_version: "0.1.0".to_string(),
            generated_at: Some("2026-10-26T08:00:00Z".to_string()),
            level: DEFAULT_ZSTD_LEVEL,
            mirrors: Vec::new(),
        })
        .expect("build the bundled catalog");
        let verified = verify(&dir.path().join("out"), &dir.path().join(PUBLIC_KEY_FILE))
            .expect("verify the bundled catalog");
        assert_eq!(verified.entries, expected);
        assert!(report.compressed_bytes > 0);
    }
}
