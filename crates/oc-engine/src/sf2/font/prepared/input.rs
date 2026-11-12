//! One bounded immutable bank snapshot shared by subset and full fallback.
use std::io::{self, Read};

use super::*;

pub(super) const MAX_INPUT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum InputError {
    #[error("SoundFont input exceeds the {limit}-byte streaming preparation limit")]
    TooLarge { limit: usize },
    #[error("cannot read SoundFont input: {0}")]
    Io(#[from] io::Error),
    #[error("cannot allocate bounded SoundFont input: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
    #[error("SoundFont input changed while its streaming snapshot was read")]
    SourceChanged,
}

pub(super) struct BankSnapshot {
    pub bytes: Vec<u8>,
    pub(super) key: PathBuf,
    pub(super) identity: BankIdentity,
}

impl BankSnapshot {
    /// Read a bounded prepared bank snapshot.
    pub fn read(path: &Path) -> Result<Self, InputError> {
        let key = cache_key(path);
        let mut file = std::fs::File::open(path)?;
        let before = BankIdentity::from_metadata(&file.metadata()?);
        let bytes = read_bounded(&mut file, before.length, MAX_INPUT_BYTES)?;
        let after = BankIdentity::from_metadata(&file.metadata()?);
        if before != after || before.length != bytes.len() as u64 {
            return Err(InputError::SourceChanged);
        }
        Ok(Self {
            bytes,
            key,
            identity: before,
        })
    }

    /// Reuse completed full-bank entries and preserve their existing LRU policy.
    /// A miss parses this exact admitted snapshot outside the global cache lock;
    /// it never reopens the path or waits behind another bank's decoder.
    pub fn full_font(&self, output_limit: usize) -> Result<Arc<SoundFont>, String> {
        if let Some(font) = SoundFont::ready_shared_identity(&self.key, &self.identity) {
            font.check_preparation_limit(output_limit)
                .map_err(|error| error.to_string())?;
            return Ok(font);
        }
        let font = Arc::new(
            SoundFont::parse_prepared_fallback(&self.bytes, output_limit)
                .map_err(|error| error.to_string())?,
        );
        if file_identity(&self.key).as_ref() == Some(&self.identity)
            && let Ok(mut cache) = shared_banks().try_lock()
        {
            // Another loader may have completed the same snapshot while this
            // one parsed. Reuse its owner; no I/O or decoding occurs under lock.
            if let Some(existing) = cache.get(&self.key, &self.identity) {
                existing
                    .check_preparation_limit(output_limit)
                    .map_err(|error| error.to_string())?;
                cache.touch(&self.key);
                return Ok(existing);
            }
            cache.insert(self.key.clone(), self.identity.clone(), Arc::clone(&font));
        }
        Ok(font)
    }
}

fn read_bounded(
    reader: &mut impl Read,
    announced_bytes: u64,
    limit: usize,
) -> Result<Vec<u8>, InputError> {
    if announced_bytes > limit as u64 {
        return Err(InputError::TooLarge { limit });
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(announced_bytes as usize)?;
    let mut block = [0_u8; 8192];
    loop {
        let remaining = limit - bytes.len();
        if remaining == 0 {
            // The overflow byte stays on the stack. The owned input vector
            // never stores or reserves limit+1 bytes, even if metadata lied.
            let mut overflow = [0_u8; 1];
            match reader.read(&mut overflow) {
                Ok(0) => return Ok(bytes),
                Ok(_) => return Err(InputError::TooLarge { limit }),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let take = remaining.min(block.len());
        match reader.read(&mut block[..take]) {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                bytes.try_reserve_exact(count)?;
                bytes.extend_from_slice(&block[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct ObservedReader {
        bytes: Cursor<Vec<u8>>,
        calls: usize,
        interrupt_once: bool,
    }

    impl Read for ObservedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if std::mem::take(&mut self.interrupt_once) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.bytes.read(buffer)
        }
    }

    fn reader(bytes: &[u8]) -> ObservedReader {
        ObservedReader {
            bytes: Cursor::new(bytes.to_vec()),
            calls: 0,
            interrupt_once: false,
        }
    }

    #[test]
    fn bounded_bank_input_rejects_oversized_metadata_before_any_read() {
        let mut reader = reader(b"abc");
        assert!(matches!(
            read_bounded(&mut reader, 5, 4),
            Err(InputError::TooLarge { limit: 4 })
        ));
        assert_eq!(reader.calls, 0);
    }

    #[test]
    fn bounded_bank_input_accepts_exact_cap_and_retries_interrupted_read() {
        let mut reader = reader(b"abcd");
        reader.interrupt_once = true;
        let bytes = read_bounded(&mut reader, 1, 4).unwrap();
        assert_eq!(bytes, b"abcd");
        assert_eq!(reader.bytes.position(), 4);
        assert!(bytes.capacity() <= 4);
    }

    #[test]
    fn bounded_bank_input_growth_probe_consumes_only_one_overflow_byte() {
        let mut reader = reader(b"abcdefghi");
        assert!(matches!(
            read_bounded(&mut reader, 1, 4),
            Err(InputError::TooLarge { limit: 4 })
        ));
        assert_eq!(reader.bytes.position(), 5);
    }

    #[test]
    fn bounded_bank_input_zero_cap_handles_empty_input_and_refuses_one_byte() {
        assert!(read_bounded(&mut Cursor::new([]), 0, 0).unwrap().is_empty());
        let mut reader = reader(b"a");
        assert!(matches!(
            read_bounded(&mut reader, 0, 0),
            Err(InputError::TooLarge { limit: 0 })
        ));
        assert_eq!(reader.bytes.position(), 1);
    }

    #[test]
    fn full_fallback_keeps_its_admitted_bytes_when_the_source_is_replaced() {
        let _cache = super::super::super::tests::take_cache_for_test();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.sf2");
        let original = include_bytes!("../../../../tests/fixtures/minimal-test.sf2");
        std::fs::write(&path, original).unwrap();
        let snapshot = BankSnapshot::read(&path).unwrap();
        std::fs::write(&path, b"replacement is not a SoundFont").unwrap();
        let font = snapshot.full_font(MAX_LOGICAL_BYTES).unwrap();
        let expected = SoundFont::parse(original).unwrap();
        assert_eq!(font.sample_bank.frames(), expected.sample_bank.frames());
        assert_eq!(font.presets.len(), expected.presets.len());
        assert!(SoundFont::ready_shared(&path).is_none());
    }

    #[test]
    fn full_fallback_reuses_the_completed_shared_bank_owner() {
        let _cache = super::super::super::tests::take_cache_for_test();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.sf2");
        std::fs::write(
            &path,
            include_bytes!("../../../../tests/fixtures/minimal-test.sf2"),
        )
        .unwrap();
        let first = BankSnapshot::read(&path)
            .unwrap()
            .full_font(MAX_LOGICAL_BYTES)
            .unwrap();
        let second = BankSnapshot::read(&path)
            .unwrap()
            .full_font(MAX_LOGICAL_BYTES)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    fn ogg_bank() -> Vec<u8> {
        let mut bank = include_bytes!("../../../../tests/fixtures/minimal-test.sf2").to_vec();
        let ogg = include_bytes!("../../../../tests/fixtures/sf3-tone-22050.ogg");
        let smpl = bank.windows(4).position(|chunk| chunk == b"smpl").unwrap();
        let old_size = u32::from_le_bytes(bank[smpl + 4..smpl + 8].try_into().unwrap()) as usize;
        let sdta = bank.windows(4).position(|chunk| chunk == b"sdta").unwrap();
        let old_list_size = u32::from_le_bytes(bank[sdta - 4..sdta].try_into().unwrap());
        let mut payload = ogg.to_vec();
        if !payload.len().is_multiple_of(2) {
            payload.push(0);
        }
        bank.splice(smpl + 8..smpl + 8 + old_size, payload.iter().copied());
        bank[smpl + 4..smpl + 8].copy_from_slice(&(ogg.len() as u32).to_le_bytes());
        bank[sdta - 4..sdta].copy_from_slice(
            &(old_list_size - old_size as u32 + payload.len() as u32).to_le_bytes(),
        );
        let riff_size = (bank.len() - 8) as u32;
        bank[4..8].copy_from_slice(&riff_size.to_le_bytes());
        let record = bank.windows(4).rposition(|chunk| chunk == b"shdr").unwrap() + 8;
        bank[record + 20..record + 24].copy_from_slice(&0_u32.to_le_bytes());
        bank[record + 24..record + 28].copy_from_slice(&(ogg.len() as u32).to_le_bytes());
        bank[record + 28..record + 36].fill(0);
        bank
    }

    #[test]
    fn bounded_full_fallback_refuses_raw_expansion_and_accepts_exact_capacity() {
        let bytes = include_bytes!("../../../../tests/fixtures/minimal-test.sf2");
        let full = SoundFont::parse(bytes).unwrap();
        let size = full.sample_bank.frames() * size_of::<i16>();
        assert!(matches!(SoundFont::parse_prepared_fallback(bytes, size-1),
            Err(Error::PreparationLimit { limit }) if limit == size-1));
        let bounded = SoundFont::parse_prepared_fallback(bytes, size).unwrap();
        for frame in 0..full.sample_bank.frames() {
            assert_eq!(
                bounded.sample_bank.point(frame),
                full.sample_bank.point(frame)
            );
        }
    }

    #[test]
    fn bounded_full_fallback_refuses_actual_compressed_pcm_expansion() {
        let bytes = ogg_bank();
        assert!(SoundFont::parse(&bytes).unwrap().sample_bank.frames() > 64);
        assert!(matches!(
            SoundFont::parse_prepared_fallback(&bytes, 128),
            Err(Error::PreparationLimit { limit: 128 })
        ));
    }

    #[test]
    fn indexed_logical_limit_refusal_is_typed_before_waveform_allocation() {
        let bytes = ogg_bank();
        let raw = RawBank::parse(&bytes).unwrap();
        let result = super::super::index(
            raw,
            crate::render_parallel::RenderWorkerOptions::default(),
            2,
        );
        assert!(matches!(
            result,
            Err(super::super::IndexFailure::LogicalLimit { limit: 2 })
        ));
    }
}
