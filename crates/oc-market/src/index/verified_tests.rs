use super::*;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct Fixture {
    key: Arc<Ed25519KeyPair>,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    compressed: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let key = Arc::new(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap());
        let catalog = serde_json::to_vec(
            &json!({"schema": 2, "generated_at": "2026-11-01T00:00:00Z", "entries": []}),
        )
        .unwrap();
        let compressed = zstd::bulk::compress(&catalog, 3).unwrap();
        let hash = crate::sha256_hex(&compressed);
        let mut fixture = Self {
            key,
            manifest: Vec::new(),
            signature: Vec::new(),
            compressed,
        };
        fixture.set(json!({
            "schema": 1, "generated_at": "2026-11-01T00:00:00Z", "catalog_version": "first", "min_app_version": "0.1.0",
            "catalog": {"path": format!("catalog-{hash}.json.zst"), "sha256": hash, "bytes": fixture.compressed.len(), "uncompressed_bytes": catalog.len(), "encoding": "zstd", "compression_level": 3},
            "signature": {"algorithm": "ed25519", "encoding": "raw", "path": SIGNATURE_FILE, "key_id": key_id(fixture.key.public_key().as_ref())}
        }));
        fixture
    }
    fn set(&mut self, value: Value) {
        self.manifest = serde_json::to_vec(&value).unwrap();
        self.signature = self.key.sign(&self.manifest).as_ref().to_vec();
    }
    fn changed(&self, mutate: impl FnOnce(&mut Value)) -> Self {
        let mut changed = self.clone();
        let mut value = serde_json::from_slice(&self.manifest).unwrap();
        mutate(&mut value);
        changed.set(value);
        changed
    }
    fn dated(&self, date: &str, version: &str) -> Self {
        self.changed(|value| {
            value["generated_at"] = date.into();
            value["catalog_version"] = version.into();
        })
    }
    fn policy(&self) -> Policy {
        Policy {
            public_key: self.key.public_key().as_ref().to_vec(),
            origins: vec!["https://index.example.test/".into()],
            app_version: Some("0.1.0".into()),
        }
    }
    fn verified(&self) -> Result<VerifiedIndex, IndexError> {
        verify_parts(
            &self.policy(),
            self.manifest.clone(),
            self.signature.clone(),
            self.compressed.clone(),
            self.policy().origins[0].clone(),
            Some("fixture".into()),
        )
    }
    fn responses(&self) -> Vec<Response> {
        vec![
            Response {
                status: 200,
                body: self.manifest.clone(),
                etag: Some("fixture".into()),
            },
            Response {
                status: 200,
                body: self.signature.clone(),
                etag: None,
            },
            Response {
                status: 200,
                body: self.compressed.clone(),
                etag: None,
            },
        ]
    }
}

fn drive(
    mut session: FetchSession,
    responses: Vec<Response>,
) -> Result<(VerifiedIndex, Vec<Request>), IndexError> {
    let mut responses = responses.into_iter();
    let mut requests = Vec::new();
    while let Some(request) = session.request()? {
        requests.push(request);
        session.accept(Ok(responses.next().expect("unexpected request")))?;
    }
    Ok((session.into_verified()?, requests))
}
struct Temp {
    path: PathBuf,
    _cleanup: crate::StagingCleanup,
}
impl Temp {
    fn new() -> Self {
        use ring::rand::SecureRandom;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Clock ticks are not unique across parallel tests. The counter separates
        // this process's calls; entropy also separates reused process identities.
        for _ in 0..8 {
            let mut nonce = [0u8; 16];
            SystemRandom::new().fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "oc-index-policy-{}-{}-{:032x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                u128::from_le_bytes(nonce),
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Self {
                        _cleanup: crate::StagingCleanup(path.clone()),
                        path,
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Could not create isolated catalog fixture: {error}"),
            }
        }
        panic!("Could not allocate a unique catalog fixture directory");
    }
}

#[test]
fn verified_fetch_uses_one_bounded_request_sequence() {
    let fixture = Fixture::new();
    let (index, requests) = drive(
        FetchSession::new(fixture.policy(), None, None).unwrap(),
        fixture.responses(),
    )
    .unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].max_bytes, MAX_MANIFEST_BYTES);
    assert_eq!(requests[1].max_bytes, SIGNATURE_BYTES);
    assert_eq!(requests[2].max_bytes, MAX_CATALOG_DOWNLOAD_BYTES);
    assert!(requests[1].url.ends_with(&format!(
        "{SIGNATURE_FILE}?v={}",
        crate::sha256_hex(&fixture.manifest)
    )));
    assert_eq!(index.manifest.catalog_version, "first");
}

#[test]
fn utc_calendar_and_semver_rules_reject_malformed_inputs() {
    for date in [
        "2026-02-30T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-11-01T00:00:60Z",
        "2026-11-01T00:00:00+00:00",
        "2026-11-01T00:00:00.1Z",
        "invalid",
    ] {
        assert!(generated_at(date).is_err(), "{date}");
    }
    assert!(generated_at("2024-02-29T00:00:00Z").is_ok());
    assert!(version_newer("0.1.0", "0.1.0-beta.1"));
    assert!(!version_newer("0.1.0-beta.1", "0.1.0"));
    assert!(!version_newer("0.1.0+new", "0.1.0+old"));
    assert!(version_newer("garbage", "0.1.0"));
}

#[test]
fn authenticated_metadata_and_payload_lengths_are_checked_by_all_readers() {
    let fixture = Fixture::new();
    let cases: [fn(&mut Value); 9] = [
        |v: &mut Value| {
            v["schema"] = 0.into();
        },
        |v: &mut Value| {
            v["schema"] = 2.into();
        },
        |v: &mut Value| {
            v["generated_at"] = "2026-02-30T00:00:00Z".into();
        },
        |v: &mut Value| {
            v["min_app_version"] = "garbage".into();
        },
        |v: &mut Value| {
            v["catalog"]["path"] = "../escape".into();
        },
        |v: &mut Value| {
            v["catalog"]["encoding"] = "zip".into();
        },
        |v: &mut Value| {
            v["catalog"]["bytes"] = (MAX_CATALOG_DOWNLOAD_BYTES as u64 + 1).into();
        },
        |v: &mut Value| {
            v["catalog"]["uncompressed_bytes"] = (MAX_CATALOG_BYTES as u64 + 1).into();
        },
        |v: &mut Value| {
            v["signature"]["path"] = "elsewhere.sig".into();
        },
    ];
    for mutate in cases {
        assert!(fixture.changed(mutate).verified().is_err());
    }
    for delta in [-1_i64, 1] {
        let changed = fixture.changed(|value| {
            let count = value["catalog"]["uncompressed_bytes"].as_i64().unwrap();
            value["catalog"]["uncompressed_bytes"] = (count + delta).into();
        });
        assert_eq!(changed.verified().unwrap_err().stage, "decompression");
    }
    let newer = fixture.changed(|value| value["min_app_version"] = "0.2.0".into());
    assert_eq!(newer.verified().unwrap_err().stage, "min_app_version");
    let mut publisher = newer.policy();
    publisher.app_version = None;
    publisher.origins.clear();
    assert!(
        verify_parts(
            &publisher,
            newer.manifest,
            newer.signature,
            newer.compressed,
            String::new(),
            None
        )
        .is_ok()
    );
}

#[test]
fn catalog_entry_count_is_bounded_during_deserialization() {
    let item = r#"{"id":"x","name":"x","vendor":"x","tags":["x"],"installs":[{"kind":"direct","platform":"linux-x86_64","format":"vst3","url":"https://x.test"}]}"#;
    let raw = format!(
        "{{\"schema\":2,\"generated_at\":\"2026-11-01T00:00:00Z\",\"entries\":[{}{}]}}",
        format!("{item},").repeat(MAX_CATALOG_ENTRIES),
        item
    );
    let compressed = zstd::bulk::compress(raw.as_bytes(), 3).unwrap();
    let error = decompress_catalog(&compressed, raw.len() as u64).unwrap_err();
    assert_eq!(error.stage, "catalog_validation");
    assert!(error.message.contains("item count"));
}

#[test]
fn unsigned_and_oversized_objects_stop_before_catalog_requests() {
    let fixture = Fixture::new();
    let unsigned = fixture.changed(|value| {
        value.as_object_mut().unwrap().remove("signature");
    });
    let mut session = FetchSession::new(fixture.policy(), None, None).unwrap();
    assert!(
        session
            .request()
            .unwrap()
            .unwrap()
            .url
            .ends_with(MANIFEST_FILE)
    );
    assert_eq!(
        session
            .accept(Ok(unsigned.responses().remove(0)))
            .unwrap_err()
            .stage,
        "signature"
    );
    let mut session = FetchSession::new(fixture.policy(), None, None).unwrap();
    assert!(
        session
            .accept(Ok(Response {
                status: 200,
                body: vec![0; MAX_MANIFEST_BYTES + 1],
                etag: None
            }))
            .is_err()
    );
    assert!(authenticate(&fixture.manifest, &[0; 65], &fixture.policy().public_key).is_err());
}

#[test]
fn not_modified_and_same_bytes_still_check_current_policy_and_watermark() {
    let fixture = Fixture::new();
    let index = fixture.verified().unwrap();
    let highest = fixture
        .dated("2026-11-02T00:00:00Z", "new")
        .verified()
        .unwrap()
        .stamp()
        .unwrap();
    let mut session =
        FetchSession::new(fixture.policy(), Some(index.clone()), Some(highest)).unwrap();
    assert_eq!(
        session.request().unwrap().unwrap().if_none_match.as_deref(),
        Some("fixture")
    );
    assert_eq!(
        session
            .accept(Ok(Response {
                status: 304,
                body: Vec::new(),
                etag: None
            }))
            .unwrap_err()
            .stage,
        "rollback"
    );
    let mut policy = fixture.policy();
    policy.app_version = Some("0.0.1".into());
    let mut session = FetchSession::new(policy, Some(index), None).unwrap();
    assert_eq!(
        session
            .accept(Ok(fixture.responses().remove(0)))
            .unwrap_err()
            .stage,
        "min_app_version"
    );
}

#[test]
fn conditional_cache_reuse_rechecks_the_payload() {
    let fixture = Fixture::new();
    let mut index = fixture.verified().unwrap();
    let mut compressed = index.compressed.to_vec();
    compressed[0] ^= 1;
    index.compressed = compressed.into();
    let mut session = FetchSession::new(fixture.policy(), Some(index), None).unwrap();
    assert_eq!(
        session
            .accept(Ok(Response {
                status: 304,
                body: Vec::new(),
                etag: None
            }))
            .unwrap_err()
            .stage,
        "catalog_hash"
    );
}

#[test]
fn signed_watermarks_refuse_rollback_equivocation_and_allow_exact_replay() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let index = fixture.verified().unwrap();
    let store = IndexStore::new(temp.path.clone(), fixture.policy());
    store.commit(&index, 10).unwrap();
    store.commit(&index, 11).unwrap();
    assert_eq!(store.load().unwrap().unwrap().checked_at, 11);
    assert_eq!(store.load().unwrap().unwrap().fetched_at, 10);
    let conflict = fixture
        .changed(|value| value["catalog_version"] = "conflicting".into())
        .verified()
        .unwrap();
    assert_eq!(
        store.commit(&conflict, 12).unwrap_err().stage,
        "equivocation"
    );
    let newer = fixture
        .dated("2026-11-02T00:00:00Z", "new")
        .verified()
        .unwrap();
    store.commit(&newer, 13).unwrap();
    assert_eq!(store.commit(&index, 14).unwrap_err().stage, "rollback");
    fs::remove_file(store.scope_dir().join("highest")).unwrap();
    assert_eq!(store.commit(&index, 15).unwrap_err().stage, "rollback");
}

#[test]
fn interrupted_publication_keeps_watermark_and_allows_exact_retry() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let store = IndexStore::new(temp.path.clone(), fixture.policy());
    let old = fixture.verified().unwrap();
    store.commit(&old, 10).unwrap();
    let newer = fixture
        .dated("2026-11-02T00:00:00Z", "new")
        .verified()
        .unwrap();
    let blocked = store.scope_dir().join("catalogs").join(newer.digest());
    fs::write(&blocked, b"publication blocked").unwrap();
    assert!(store.commit(&newer, 11).is_err());
    assert_eq!(store.watermark().unwrap().unwrap().digest, newer.digest());
    assert_eq!(store.load().unwrap().unwrap().index.digest(), old.digest());
    assert_eq!(store.commit(&old, 12).unwrap_err().stage, "rollback");
    fs::remove_file(blocked).unwrap();
    store.commit(&newer, 13).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().index.digest(),
        newer.digest()
    );
}

#[test]
fn cache_load_revalidates_app_compatibility_and_signed_watermark() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let store = IndexStore::new(temp.path.clone(), fixture.policy());
    store.commit(&fixture.verified().unwrap(), 10).unwrap();
    let mut older_app = fixture.policy();
    older_app.app_version = Some("0.0.1".into());
    assert_eq!(
        IndexStore::new(temp.path.clone(), older_app)
            .load()
            .unwrap_err()
            .stage,
        "min_app_version"
    );
    let watermark = store
        .scope_dir()
        .join("watermarks")
        .join(fixture.verified().unwrap().digest())
        .join(SIGNATURE_FILE);
    fs::write(watermark, [0; 64]).unwrap();
    assert!(store.load().is_err());
    assert!(
        store
            .commit(
                &fixture
                    .dated("2026-11-03T00:00:00Z", "new")
                    .verified()
                    .unwrap(),
                11
            )
            .is_err()
    );
}

#[test]
fn namespaces_keep_each_authority_and_origin_history() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let first = IndexStore::new(temp.path.clone(), fixture.policy());
    first.commit(&fixture.verified().unwrap(), 10).unwrap();
    let mut other_policy = fixture.policy();
    other_policy.origins = vec!["https://other.example.test/".into()];
    let other = IndexStore::new(temp.path.clone(), other_policy.clone());
    assert!(other.load().unwrap().is_none());
    let mut index = fixture
        .dated("2026-11-02T00:00:00Z", "other")
        .verified()
        .unwrap();
    index.source_base = other_policy.origins[0].clone();
    other.commit(&index, 11).unwrap();
    assert_eq!(
        first
            .load()
            .unwrap()
            .unwrap()
            .index
            .manifest
            .catalog_version,
        "first"
    );
    assert_eq!(
        other
            .load()
            .unwrap()
            .unwrap()
            .index
            .manifest
            .catalog_version,
        "other"
    );
    let third = IndexStore::new(temp.path.clone(), Fixture::new().policy());
    assert!(third.load().unwrap().is_none());
}

#[test]
fn scoped_migration_reads_legacy_without_modifying_it() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let index = fixture.verified().unwrap();
    let policy = fixture.policy();
    let legacy = temp.path.join("last-good");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join(MANIFEST_FILE), &index.manifest_bytes).unwrap();
    fs::write(legacy.join(SIGNATURE_FILE), &index.signature).unwrap();
    fs::write(legacy.join(&index.manifest.catalog.path), &index.compressed).unwrap();
    let state = CacheState {
        trust_scope: policy.scope(),
        fetched_at: 10,
        checked_at: 11,
        etag: index.etag.clone(),
        generated_at: index.manifest.generated_at.clone(),
        catalog_version: index.manifest.catalog_version.clone(),
        manifest_sha256: index.digest(),
        source_base: index.source_base.clone(),
    };
    let state_bytes = serde_json::to_vec(&state).unwrap();
    fs::write(legacy.join("state.json"), &state_bytes).unwrap();
    let store = IndexStore::new(temp.path.clone(), policy);
    let loaded = store.load().unwrap().unwrap();
    assert!(!temp.path.join("scopes").exists());
    store.commit(&loaded.index, loaded.checked_at).unwrap();
    assert!(store.cache_dir().unwrap().unwrap().is_dir());
    assert_eq!(fs::read(legacy.join("state.json")).unwrap(), state_bytes);
    assert_eq!(
        store.load().unwrap().unwrap().index.digest(),
        index.digest()
    );
}

fn save_child_fixture(fixture: &Fixture, path: &Path) {
    fs::create_dir(path).unwrap();
    fs::write(path.join(MANIFEST_FILE), &fixture.manifest).unwrap();
    fs::write(path.join(SIGNATURE_FILE), &fixture.signature).unwrap();
    fs::write(path.join("catalog"), &fixture.compressed).unwrap();
    fs::write(path.join("public-key"), fixture.key.public_key().as_ref()).unwrap();
}

#[test]
fn index_store_writer_child() {
    let Some(fixture) = std::env::var_os("OCP_INDEX_TEST_CHILD_FIXTURE") else {
        return;
    };
    let fixture = PathBuf::from(fixture);
    let root =
        PathBuf::from(std::env::var_os("OCP_INDEX_TEST_CHILD_CACHE").expect("child cache root"));
    let policy = Policy {
        public_key: fs::read(fixture.join("public-key")).unwrap(),
        origins: vec!["https://index.example.test/".into()],
        app_version: Some("0.1.0".into()),
    };
    let index = verify_parts(
        &policy,
        fs::read(fixture.join(MANIFEST_FILE)).unwrap(),
        fs::read(fixture.join(SIGNATURE_FILE)).unwrap(),
        fs::read(fixture.join("catalog")).unwrap(),
        policy.origins[0].clone(),
        None,
    )
    .unwrap();
    let outcome = IndexStore::new(root, policy).commit(&index, 10);
    assert!(
        outcome.as_ref().map_or_else(
            |error| matches!(error.stage, "rollback" | "equivocation"),
            |_| true
        ),
        "{outcome:?}"
    );
    fs::write(fixture.join("attempted"), b"yes").unwrap();
}

#[test]
fn separate_process_writers_keep_the_highest_authenticated_catalog() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let newer = fixture.dated("2026-11-03T00:00:00Z", "newer");
    let old_path = temp.path.join("old");
    let new_path = temp.path.join("new");
    save_child_fixture(&fixture, &old_path);
    save_child_fixture(&newer, &new_path);
    let cache = temp.path.join("cache");
    let mut children = Vec::new();
    for path in [&old_path, &new_path] {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "index::verified_tests::index_store_writer_child",
                    "--nocapture",
                ])
                .env("OCP_INDEX_TEST_CHILD_FIXTURE", path)
                .env("OCP_INDEX_TEST_CHILD_CACHE", &cache)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(old_path.join("attempted").exists() && new_path.join("attempted").exists());
    let store = IndexStore::new(cache, fixture.policy());
    assert_eq!(
        store
            .load()
            .unwrap()
            .unwrap()
            .index
            .manifest
            .catalog_version,
        "newer"
    );
}

#[test]
fn repeated_publications_retain_current_previous_and_highest_only() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let store = IndexStore::new(temp.path.clone(), fixture.policy());
    let current = fixture
        .dated("2026-11-10T00:00:00Z", "version-10")
        .verified()
        .unwrap();
    for day in 1..=10 {
        let index = fixture
            .dated(
                &format!("2026-11-{day:02}T00:00:00Z"),
                &format!("version-{day}"),
            )
            .verified()
            .unwrap();
        store.commit(&index, day).unwrap();
        assert_eq!(
            fs::read_dir(store.scope_dir().join("catalogs"))
                .unwrap()
                .count(),
            usize::from(day > 1) + 1
        );
        assert_eq!(
            fs::read_dir(store.scope_dir().join("watermarks"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(store.watermark().unwrap().unwrap().digest, index.digest());
    }
    let previous = fs::read_to_string(store.scope_dir().join("previous")).unwrap();
    let orphan = store.scope_dir().join("catalogs/.snapshot-orphan");
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("payload"), b"old").unwrap();
    fs::write(store.scope_dir().join(".index-orphan"), b"old").unwrap();
    store.commit(&current, 11).unwrap();
    assert_eq!(
        fs::read_to_string(store.scope_dir().join("previous")).unwrap(),
        previous
    );
    assert_eq!(
        fs::read_dir(store.scope_dir().join("catalogs"))
            .unwrap()
            .count(),
        2
    );
    assert!(!orphan.exists());
    assert!(!store.scope_dir().join(".index-orphan").exists());
}

#[test]
fn shared_readers_observe_complete_snapshots_while_publishers_prune() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let store = Arc::new(IndexStore::new(temp.path.clone(), fixture.policy()));
    store.commit(&fixture.verified().unwrap(), 1).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let reader = {
        let store = store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..100 {
                let loaded = store.load().unwrap().unwrap();
                assert_eq!(
                    loaded.index.digest(),
                    crate::sha256_hex(&loaded.index.manifest_bytes)
                );
                assert_eq!(
                    loaded.index.manifest.catalog.sha256,
                    crate::sha256_hex(&loaded.index.compressed)
                );
            }
        })
    };
    barrier.wait();
    for day in 2..=8 {
        let index = fixture
            .dated(
                &format!("2026-11-{day:02}T00:00:00Z"),
                &format!("version-{day}"),
            )
            .verified()
            .unwrap();
        store.commit(&index, day).unwrap();
    }
    reader.join().unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .unwrap()
            .index
            .manifest
            .catalog_version,
        "version-8"
    );
    assert_eq!(
        fs::read_dir(store.scope_dir().join("catalogs"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn installation_authority_rechecks_after_network_and_holds_publication_stable() {
    let temp = Temp::new();
    let fixture = Fixture::new();
    let first = fixture.verified().unwrap();
    let store = IndexStore::new(temp.path.clone(), fixture.policy());
    store.commit(&first, 10).unwrap();
    drop(store.authorize(&first).unwrap());
    // A different client publishes while an artifact is being downloaded.
    let other = IndexStore::new(temp.path.clone(), fixture.policy());
    let newer = fixture
        .dated("2026-11-02T00:00:00Z", "new")
        .verified()
        .unwrap();
    other.commit(&newer, 11).unwrap();
    assert_eq!(store.authorize(&first).unwrap_err().stage, "rollback");
    let publication = store.authorize(&newer).unwrap();
    let latest = fixture
        .dated("2026-11-03T00:00:00Z", "latest")
        .verified()
        .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        other.commit(&latest, 12).unwrap();
        finished_tx.send(()).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(std::time::Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    drop(publication);
    finished_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    writer.join().unwrap();
    assert_eq!(store.authorize(&newer).unwrap_err().stage, "rollback");
}

#[test]
fn parallel_catalog_fixtures_have_distinct_guarded_roots() {
    let fixtures = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| (0..8).map(|_| Temp::new()).collect::<Vec<_>>()))
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let paths: std::collections::HashSet<_> =
        fixtures.iter().map(|temp| temp.path.clone()).collect();
    assert_eq!(paths.len(), 128);
    assert!(paths.iter().all(|path| path.is_dir()));
    drop(fixtures);
    assert!(paths.iter().all(|path| !path.exists()));
}
