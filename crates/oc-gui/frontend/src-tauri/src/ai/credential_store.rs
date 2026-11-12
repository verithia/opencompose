//! Credential payloads for the OS store. Small values stay as raw UTF-8 bytes
//! (readable by older builds); large values are zstd-compressed or sharded.
use super::secret::Secret;
use zeroize::Zeroizing;

/// Windows `CRED_MAX_CREDENTIAL_BLOB_SIZE`; the blob, not characters, is capped.
pub const MAX_CREDENTIAL_BYTES: usize = 2560;
const MAGIC: &[u8] = b"OCPSCR1\0";
const KIND_RAW: u8 = b'R';
const KIND_ZSTD: u8 = b'Z';
const KIND_SHARD_MANIFEST: u8 = b'S';
const KIND_SHARD_PART: u8 = b'P';
const SHARD_ACCOUNT: &str = "~ocp1~";
/// Magic + marker + kind + count + generation.
const MANIFEST_LEN: usize = MAGIC.len() + 7;
/// Magic + marker + generation + index.
const PART_HEADER_LEN: usize = MAGIC.len() + 6;
const MAX_SHARDS: usize = 255;
const MAX_DECODED_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a secret-store operation failed.
pub enum StoreError {
    Unavailable,
    /// The store exists but cannot be opened: on Linux a locked collection,
    /// a missing default collection or a failed unlock prompt.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Locked,
    Unreadable,
    Write,
    Remove,
}

/// Byte-oriented credential store; sharding needs more than one account.
pub trait SecretStore {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn set(&self, account: &str, value: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

/// Secret store backed by the OS keyring.
pub struct KeyringSecrets {
    service: &'static str,
}

impl KeyringSecrets {
    /// Bind the store to a service name.
    pub const fn new(service: &'static str) -> Self {
        Self { service }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(self.service, account).map_err(|_| StoreError::Unavailable)
    }
}

/// Keeps "no store" and "locked store" apart from a plain read or write
/// failure; only Linux reports them as `NoStorageAccess`/`PlatformFailure`.
fn store_error(error: keyring::Error, fallback: StoreError) -> StoreError {
    match error {
        #[cfg(target_os = "linux")]
        keyring::Error::NoStorageAccess(_) => StoreError::Locked,
        #[cfg(target_os = "linux")]
        keyring::Error::PlatformFailure(_) => StoreError::Unavailable,
        _ => fallback,
    }
}

impl SecretStore for KeyringSecrets {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match self.entry(account)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error, StoreError::Unreadable)),
        }
    }

    fn set(&self, account: &str, value: &[u8]) -> Result<(), StoreError> {
        self.entry(account)?
            .set_secret(value)
            .map_err(|error| store_error(error, StoreError::Write))
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error, StoreError::Remove)),
        }
    }
}

/// Load and decode a stored secret.
pub fn load_text(store: &dyn SecretStore, account: &str) -> Result<Option<Secret>, StoreError> {
    let Some(value) = store.get(account)? else {
        return Ok(None);
    };
    Ok(Some(Secret::new(decode(store, account, &value)?)))
}

/// Encode and store a secret within its size limit.
pub fn store_text(store: &dyn SecretStore, account: &str, value: &str) -> Result<(), StoreError> {
    store_with_limit(store, account, value, MAX_CREDENTIAL_BYTES)
}

/// Delete a stored secret and its shards.
pub fn delete_entry(store: &dyn SecretStore, account: &str) -> Result<(), StoreError> {
    let (generation, shards) = existing_shards(store, account)?;
    delete_shards(store, account, generation, shards)?;
    store.delete(account)
}

fn store_with_limit(
    store: &dyn SecretStore,
    account: &str,
    value: &str,
    limit: usize,
) -> Result<(), StoreError> {
    let inline_capacity = limit
        .checked_sub(MAGIC.len() + 1)
        .ok_or(StoreError::Write)?;
    let chunk_capacity = limit
        .checked_sub(PART_HEADER_LEN)
        .ok_or(StoreError::Write)?;
    if inline_capacity == 0 || chunk_capacity == 0 {
        return Err(StoreError::Write);
    }
    let (kind, payload) = encode(value.as_bytes(), inline_capacity)?;
    let (old_generation, old_shards) = existing_shards(store, account)?;

    if payload.len() <= inline_capacity {
        if kind == KIND_RAW {
            store.set(account, &payload)?;
        } else {
            let mut encoded = Vec::with_capacity(MAGIC.len() + 1 + payload.len());
            encoded.extend_from_slice(MAGIC);
            encoded.push(kind);
            encoded.extend_from_slice(&payload);
            store.set(account, &encoded)?;
        }
        delete_shards(store, account, old_generation, old_shards)?;
        return Ok(());
    }

    let chunks: Vec<&[u8]> = payload.chunks(chunk_capacity).collect();
    if chunks.len() > MAX_SHARDS {
        return Err(StoreError::Write);
    }
    // A generation that matches the one being replaced would make the cleanup
    // below delete the shards this write just stored.
    let mut generation = next_generation();
    while old_shards > 0 && generation == old_generation {
        generation = next_generation();
    }
    for (index, chunk) in chunks.iter().enumerate() {
        let mut part = Vec::with_capacity(PART_HEADER_LEN + chunk.len());
        part.extend_from_slice(MAGIC);
        part.push(KIND_SHARD_PART);
        part.extend_from_slice(&generation.to_le_bytes());
        part.push(index as u8);
        part.extend_from_slice(chunk);
        store.set(&shard_account(account, generation, index), &part)?;
    }
    let mut manifest = Vec::with_capacity(MANIFEST_LEN);
    manifest.extend_from_slice(MAGIC);
    manifest.push(KIND_SHARD_MANIFEST);
    manifest.push(kind);
    manifest.push(chunks.len() as u8);
    manifest.extend_from_slice(&generation.to_le_bytes());
    store.set(account, &manifest)?;
    // Old shards stay intact until the manifest switches, so a crash keeps the
    // previous token readable.
    delete_shards(store, account, old_generation, old_shards)?;
    Ok(())
}

/// Picks the smallest representation that can be split across entries.
fn encode(value: &[u8], inline_capacity: usize) -> Result<(u8, Zeroizing<Vec<u8>>), StoreError> {
    if value.len() <= inline_capacity {
        return Ok((KIND_RAW, Zeroizing::new(value.to_vec())));
    }
    let compressed = Zeroizing::new(zstd::bulk::compress(value, 3).map_err(|_| StoreError::Write)?);
    if compressed.len() <= inline_capacity || compressed.len() < value.len() {
        Ok((KIND_ZSTD, compressed))
    } else {
        Ok((KIND_RAW, Zeroizing::new(value.to_vec())))
    }
}

fn decode(store: &dyn SecretStore, account: &str, value: &[u8]) -> Result<String, StoreError> {
    let Some(rest) = value.strip_prefix(MAGIC) else {
        return legacy_text(value);
    };
    let (kind, payload) = rest.split_first().ok_or(StoreError::Unreadable)?;
    if *kind != KIND_SHARD_MANIFEST {
        return decode_payload(*kind, payload);
    }
    let kind = *payload.first().ok_or(StoreError::Unreadable)?;
    let count = *payload.get(1).ok_or(StoreError::Unreadable)? as usize;
    let generation = payload
        .get(2..6)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or(StoreError::Unreadable)?;
    if count == 0 || count > MAX_SHARDS {
        return Err(StoreError::Unreadable);
    }
    let mut joined = Zeroizing::new(Vec::new());
    for index in 0..count {
        let part = store
            .get(&shard_account(account, generation, index))?
            .ok_or(StoreError::Unreadable)?;
        let body = part.strip_prefix(MAGIC).ok_or(StoreError::Unreadable)?;
        let index_ok = body.len() >= 6
            && body[0] == KIND_SHARD_PART
            && body[1..5] == generation.to_le_bytes()
            && body[5] as usize == index;
        if !index_ok {
            return Err(StoreError::Unreadable);
        }
        joined.extend_from_slice(&body[6..]);
    }
    decode_payload(kind, &joined)
}

fn decode_payload(kind: u8, payload: &[u8]) -> Result<String, StoreError> {
    let bytes = match kind {
        KIND_RAW => payload.to_vec(),
        KIND_ZSTD => zstd::bulk::decompress(payload, MAX_DECODED_BYTES)
            .map_err(|_| StoreError::Unreadable)?,
        _ => return Err(StoreError::Unreadable),
    };
    String::from_utf8(bytes).map_err(|_| StoreError::Unreadable)
}

/// Old builds wrote a UTF-8 password on macOS/Linux and UTF-16LE on Windows.
fn legacy_text(value: &[u8]) -> Result<String, StoreError> {
    if value.contains(&0) {
        utf16_text(value).or_else(|_| utf8_text(value))
    } else {
        utf8_text(value).or_else(|_| utf16_text(value))
    }
}

fn utf8_text(value: &[u8]) -> Result<String, StoreError> {
    String::from_utf8(value.to_vec()).map_err(|_| StoreError::Unreadable)
}

fn utf16_text(value: &[u8]) -> Result<String, StoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(StoreError::Unreadable);
    }
    let units = Zeroizing::new(
        value
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    );
    String::from_utf16(&units).map_err(|_| StoreError::Unreadable)
}

fn shard_account(account: &str, generation: u32, index: usize) -> String {
    format!("{account}{SHARD_ACCOUNT}{generation:08x}~{index}")
}

/// Generation and shard count of the manifest currently stored at `account`.
fn existing_shards(store: &dyn SecretStore, account: &str) -> Result<(u32, usize), StoreError> {
    let Some(value) = store.get(account)? else {
        return Ok((0, 0));
    };
    let Some(rest) = value.strip_prefix(MAGIC) else {
        return Ok((0, 0));
    };
    if rest.len() < 7 || rest[0] != KIND_SHARD_MANIFEST {
        return Ok((0, 0));
    }
    let count = rest[2] as usize;
    let generation = u32::from_le_bytes([rest[3], rest[4], rest[5], rest[6]]);
    Ok((generation, count))
}

fn delete_shards(
    store: &dyn SecretStore,
    account: &str,
    generation: u32,
    count: usize,
) -> Result<(), StoreError> {
    for index in 0..count {
        store.delete(&shard_account(account, generation, index))?;
    }
    Ok(())
}

/// Monotone per call, so consecutive writes never collide on a generation.
fn next_generation() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static GENERATION: AtomicU32 = AtomicU32::new(0);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u32)
        .unwrap_or(1);
    GENERATION
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(seed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    struct FakeStore {
        limit: usize,
        items: RefCell<BTreeMap<String, Vec<u8>>>,
    }

    impl FakeStore {
        fn new(limit: usize) -> Self {
            Self {
                limit,
                items: RefCell::new(BTreeMap::new()),
            }
        }
    }

    impl SecretStore for FakeStore {
        fn get(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.items.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, value: &[u8]) -> Result<(), StoreError> {
            if value.len() > self.limit {
                return Err(StoreError::Write);
            }
            self.items
                .borrow_mut()
                .insert(account.to_owned(), value.to_vec());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), StoreError> {
            self.items.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn payload(length: usize) -> String {
        let mut state = 0x5eed_1234_u64;
        (0..length)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (b'0' + (state >> 33) as u8 % 62) as char
            })
            .collect()
    }

    /// A store holding an entry it cannot open, as a locked collection does:
    /// searching for a missing entry looks like absence.
    struct LockedStore {
        present: bool,
    }

    impl SecretStore for LockedStore {
        fn get(&self, _account: &str) -> Result<Option<Vec<u8>>, StoreError> {
            if self.present {
                Err(StoreError::Locked)
            } else {
                Ok(None)
            }
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), StoreError> {
            Err(StoreError::Locked)
        }

        fn delete(&self, _account: &str) -> Result<(), StoreError> {
            Err(StoreError::Locked)
        }
    }

    /// A store that reports absence on reads but refuses the write, the shape
    /// keyring 3.6.3 produces when `CreateItem` meets a locked collection.
    struct RefusingStore;

    impl SecretStore for RefusingStore {
        fn get(&self, _account: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }

        fn set(&self, _account: &str, _value: &[u8]) -> Result<(), StoreError> {
            Err(StoreError::Unavailable)
        }

        fn delete(&self, _account: &str) -> Result<(), StoreError> {
            Err(StoreError::Unavailable)
        }
    }

    #[test]
    fn backend_failures_keep_their_store_kind() {
        let no_access =
            || keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked")));
        let platform = || keyring::Error::PlatformFailure(Box::new(std::io::Error::other("bus")));
        // macOS and Windows keep the previous fallback: their `NoStorageAccess`
        // means "keychain not available", which the old messages already said.
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                store_error(no_access(), StoreError::Write),
                StoreError::Locked
            );
            assert_eq!(
                store_error(platform(), StoreError::Write),
                StoreError::Unavailable
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                store_error(no_access(), StoreError::Write),
                StoreError::Write
            );
            assert_eq!(
                store_error(platform(), StoreError::Write),
                StoreError::Write
            );
        }
        assert_eq!(
            store_error(
                keyring::Error::TooLong("secret".into(), 2560),
                StoreError::Write
            ),
            StoreError::Write
        );
    }

    #[test]
    fn a_failed_manifest_probe_keeps_its_kind() {
        // An existing account is unreadable, so the probe fails before the write
        // and its kind must survive the `?`.
        let stored = LockedStore { present: true };
        assert_eq!(
            load_text(&stored, "fixture").unwrap_err(),
            StoreError::Locked
        );
        assert_eq!(
            store_text(&stored, "fixture", "sk-fixture"),
            Err(StoreError::Locked)
        );
        assert_eq!(delete_entry(&stored, "fixture"), Err(StoreError::Locked));
    }

    #[test]
    fn a_refused_write_keeps_its_kind() {
        assert_eq!(
            store_text(&RefusingStore, "fixture", "sk-fixture"),
            Err(StoreError::Unavailable)
        );
    }

    fn round_trip(length: usize, limit: usize) {
        let store = FakeStore::new(limit);
        let value = payload(length);
        store_with_limit(&store, "fixture", &value, limit).unwrap();
        assert_eq!(
            load_text(&store, "fixture").unwrap().unwrap().as_str(),
            value,
            "length {length}"
        );
    }

    #[test]
    fn inline_payloads_keep_the_legacy_unprefixed_bytes() {
        let store = FakeStore::new(MAX_CREDENTIAL_BYTES);
        store_text(&store, "fixture", "sk-fixture-key").unwrap();
        assert_eq!(
            store.items.borrow().get("fixture").unwrap(),
            b"sk-fixture-key"
        );
    }

    #[test]
    fn payload_boundaries_round_trip_under_a_1280_byte_backend() {
        for length in [1279, 1280, 1281, 4096] {
            round_trip(length, 1280);
        }
    }

    #[test]
    fn payload_boundaries_round_trip_under_the_windows_blob_limit() {
        for length in [
            MAX_CREDENTIAL_BYTES - 1,
            MAX_CREDENTIAL_BYTES,
            MAX_CREDENTIAL_BYTES + 1,
            16 * 1024,
        ] {
            round_trip(length, MAX_CREDENTIAL_BYTES);
        }
    }

    #[test]
    fn large_payloads_use_shards_and_delete_removes_them() {
        let store = FakeStore::new(1280);
        let value = payload(4096);
        store_with_limit(&store, "fixture", &value, 1280).unwrap();
        assert!(
            store
                .items
                .borrow()
                .keys()
                .any(|account| account.starts_with("fixture~ocp1~"))
        );
        delete_entry(&store, "fixture").unwrap();
        assert!(store.items.borrow().is_empty());
    }

    #[test]
    fn shard_generations_never_repeat() {
        // Equal generations make the old-shard cleanup delete the new shards.
        let mut previous = next_generation();
        for _ in 0..64 {
            let next = next_generation();
            assert_ne!(next, previous, "generation repeated");
            previous = next;
        }
    }

    #[test]
    fn a_second_sharded_write_deletes_the_previous_generation() {
        let store = FakeStore::new(1280);
        store_with_limit(&store, "fixture", &payload(4096), 1280).unwrap();
        store_with_limit(&store, "fixture", &payload(5000), 1280).unwrap();
        let base = store.items.borrow().get("fixture").cloned().unwrap();
        assert_eq!(base[0..MAGIC.len()], *MAGIC);
        let shards = base[MAGIC.len() + 2] as usize;
        assert!(shards > 1);
        assert_eq!(store.items.borrow().len(), shards + 1);
        assert_eq!(
            load_text(&store, "fixture").unwrap().unwrap().as_str(),
            payload(5000)
        );
    }

    #[test]
    fn an_old_utf8_entry_still_loads() {
        let store = FakeStore::new(1280);
        let legacy = r#"{"v":1,"access":"legacy-access","refresh":"legacy-refresh"}"#;
        store
            .items
            .borrow_mut()
            .insert("fixture".into(), legacy.as_bytes().to_vec());
        assert_eq!(
            load_text(&store, "fixture").unwrap().unwrap().as_str(),
            legacy
        );
    }

    #[test]
    fn an_old_windows_utf16_entry_still_loads() {
        let store = FakeStore::new(1280);
        let legacy = r#"{"v":1,"access":"legacy-access"}"#;
        let mut encoded = Vec::new();
        for unit in legacy.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        store.items.borrow_mut().insert("fixture".into(), encoded);
        assert_eq!(
            load_text(&store, "fixture").unwrap().unwrap().as_str(),
            legacy
        );
    }

    #[test]
    fn a_payload_over_the_shard_limit_fails_closed() {
        let store = FakeStore::new(64);
        let value = payload(64 * 300);
        assert_eq!(
            store_with_limit(&store, "fixture", &value, 64),
            Err(StoreError::Write)
        );
        assert!(load_text(&store, "fixture").unwrap().is_none());
        assert_eq!(
            store_with_limit(&store, "fixture", "fixture", 14),
            Err(StoreError::Write)
        );
    }
}
