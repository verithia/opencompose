//! RIFF chunk reader for the SoundFont 2 container (`RIFF` / `sfbk`).
//!
//! Only the chunk layer lives here: the bank semantics are in [`super::font`].
//! The reader is deliberately tolerant of the two defects real banks ship with:
//! a chunk size that runs past its parent payload, and a record chunk whose
//! length is not a whole multiple of the record size (the 38-byte trap).

/// One chunk: a four-byte identifier and the payload it declares.
#[derive(Debug, Clone, Copy)]
pub struct Chunk<'a> {
    pub id: [u8; 4],
    pub data: &'a [u8],
}

impl Chunk<'_> {
    /// Identifier as text; non-ASCII bytes are replaced by `?`.
    pub fn id_str(&self) -> String {
        String::from_utf8_lossy(&self.id).into_owned()
    }
}

/// Why a bank could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a RIFF file: expected 'RIFF', found {found:?}")]
    NotRiff { found: [u8; 4] },
    #[error("RIFF form type is {found:?}, expected 'sfbk'")]
    NotSoundFont { found: [u8; 4] },
    #[error("LIST chunk declares form {found:?}, expected {expected:?}")]
    WrongListForm { expected: [u8; 4], found: [u8; 4] },
    #[error("chunk '{id}' header is cut short at byte {offset}")]
    TruncatedHeader { id: String, offset: usize },
    #[error("chunk '{id}' needs {size} bytes but only {available} remain in its parent")]
    TruncatedPayload {
        id: String,
        size: usize,
        available: usize,
    },
    #[error("missing required chunk '{0}'")]
    MissingChunk(&'static str),
    #[error("chunk '{id}' is {size} bytes, which is not enough for one record")]
    ShortRecords { id: String, size: usize },
    #[error("SoundFont file has no {0}")]
    Empty(&'static str),
}

/// A cursor over one RIFF payload level (the file body or a `LIST` body).
#[derive(Debug)]
pub struct ChunkReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    end: usize,
}

impl<'a> ChunkReader<'a> {
    /// Split a whole `.sf2` file into its top-level chunks.
    pub fn open(bytes: &'a [u8]) -> Result<Self, Error> {
        let header = bytes.get(..12).ok_or(Error::NotRiff {
            found: first_four(bytes),
        })?;
        let id: [u8; 4] = header[..4].try_into().unwrap_or(*b"????");
        if &id != b"RIFF" {
            return Err(Error::NotRiff { found: id });
        }
        let form: [u8; 4] = header[8..12].try_into().unwrap_or(*b"????");
        if &form != b"sfbk" {
            return Err(Error::NotSoundFont { found: form });
        }
        let declared = u32_at(header, 4) as usize;
        // A bank with a wrong RIFF size is still readable; clamp to the file.
        let end = declared.saturating_add(8).min(bytes.len());
        Ok(Self {
            bytes,
            offset: 12,
            end,
        })
    }

    /// Cursor over the payload of a `LIST` chunk whose form type must match.
    pub fn list(payload: &'a [u8], expected: &[u8; 4]) -> Result<Self, Error> {
        let found: [u8; 4] = payload
            .get(..4)
            .and_then(|form| form.try_into().ok())
            .unwrap_or(*b"????");
        if &found != expected {
            return Err(Error::WrongListForm {
                expected: *expected,
                found,
            });
        }
        Ok(Self {
            bytes: payload,
            offset: 4,
            end: payload.len(),
        })
    }

    /// Next complete chunk, or `None` at the end of this level.
    pub fn next_chunk(&mut self) -> Result<Option<Chunk<'a>>, Error> {
        if self.end.saturating_sub(self.offset) < 8 {
            return Ok(None);
        }
        let header = &self.bytes[self.offset..self.offset + 8];
        let id: [u8; 4] = header[..4].try_into().unwrap_or(*b"????");
        let size = u32_at(header, 4) as usize;
        let payload_start = self.offset + 8;
        let available = self.end.saturating_sub(payload_start);
        if size > available {
            return Err(Error::TruncatedPayload {
                id: String::from_utf8_lossy(&id).into_owned(),
                size,
                available,
            });
        }
        let data = &self.bytes[payload_start..payload_start + size];
        // RIFF pads every chunk to an even length; the pad byte is not payload.
        self.offset = payload_start + size + (size & 1);
        Ok(Some(Chunk { id, data }))
    }
}

/// Iterate the chunks of one level, surfacing the first structural error.
pub struct Chunks<'a>(ChunkReader<'a>);

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0.next_chunk() {
            Ok(Some(chunk)) => Some(Ok(chunk)),
            Ok(None) => None,
            Err(error) => {
                // Stop after a structural error: the cursor cannot resynchronise.
                self.0.offset = self.0.end;
                Some(Err(error))
            }
        }
    }
}

impl<'a> IntoIterator for ChunkReader<'a> {
    type Item = Result<Chunk<'a>, Error>;
    type IntoIter = Chunks<'a>;

    fn into_iter(self) -> Self::IntoIter {
        Chunks(self)
    }
}

/// Number of whole records in a chunk, ignoring trailing padding.
pub fn record_count(chunk: &Chunk<'_>, record_size: usize) -> Result<usize, Error> {
    if record_size == 0 {
        return Ok(0);
    }
    if chunk.data.len() < record_size {
        return Err(Error::ShortRecords {
            id: chunk.id_str(),
            size: chunk.data.len(),
        });
    }
    Ok(chunk.data.len() / record_size)
}

/// Borrow record `index` of `record_size` bytes from a chunk payload.
pub fn record(payload: &[u8], index: usize, record_size: usize) -> Option<&[u8]> {
    let start = index.checked_mul(record_size)?;
    field(payload, start, record_size)
}

/// Little-endian readers that never panic on a short slice.
pub fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    field(bytes, offset, 2)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .unwrap_or(0)
}

/// Little-endian i16 at an offset, zero when out of range.
pub fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    u16_at(bytes, offset) as i16
}

/// Little-endian u32 at an offset, zero when out of range.
pub fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    field(bytes, offset, 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}

/// Fixed-width, NUL-padded ASCII name field.
pub fn name_at(bytes: &[u8], offset: usize) -> String {
    let field = field(bytes, offset, 20).unwrap_or_default();
    let end = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_string()
}

fn field(bytes: &[u8], offset: usize, width: usize) -> Option<&[u8]> {
    bytes.get(offset..)?.get(..width)
}

fn first_four(bytes: &[u8]) -> [u8; 4] {
    bytes
        .get(..4)
        .and_then(|head| head.try_into().ok())
        .unwrap_or(*b"\0\0\0\0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn riff(body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        bytes.extend_from_slice(b"sfbk");
        bytes.extend_from_slice(body);
        bytes
    }

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() & 1 == 1 {
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn record_and_field_readers_reject_overflowing_ranges() {
        let bytes = [0x34, 0x12, 0x56, 0x78];
        for offset in [bytes.len(), bytes.len() + 1, usize::MAX - 1, usize::MAX] {
            assert_eq!(u16_at(&bytes, offset), 0);
            assert_eq!(i16_at(&bytes, offset), 0);
            assert_eq!(u32_at(&bytes, offset), 0);
            assert_eq!(name_at(&bytes, offset), "");
        }
        for (index, width) in [(1, usize::MAX), (usize::MAX, 2), (2, usize::MAX)] {
            assert!(record(&bytes, index, width).is_none());
        }
        assert_eq!(record(&bytes, usize::MAX, 0), Some(&[][..]));
        assert_eq!(record(&bytes, 1, 2), Some(&bytes[2..]));
        assert_eq!(u16_at(&bytes, 0), 0x1234);
        assert_eq!(u32_at(&bytes, 0), 0x7856_1234);
    }

    #[test]
    fn reads_top_level_chunks() {
        let mut info_body = b"INFO".to_vec();
        info_body.extend_from_slice(&chunk(b"ifil", &[2, 0, 1, 0]));
        let mut body = chunk(b"LIST", &info_body);
        body.extend_from_slice(&chunk(b"LIST", b"sdta"));
        let bytes = riff(&body);
        let mut reader = ChunkReader::open(&bytes).unwrap();
        let first = reader.next_chunk().unwrap().unwrap();
        assert_eq!(&first.id, b"LIST");
        assert_eq!(first.data, &info_body[..]);
        let mut info = ChunkReader::list(first.data, b"INFO").unwrap();
        assert_eq!(info.next_chunk().unwrap().unwrap().id_str(), "ifil");
        let second = reader.next_chunk().unwrap().unwrap();
        assert_eq!(second.id_str(), "LIST");
        assert!(reader.next_chunk().unwrap().is_none());
    }

    #[test]
    fn skips_the_pad_byte_of_odd_chunks() {
        let mut body = chunk(b"pdta", b"abc");
        body.extend_from_slice(&chunk(b"INFO", b"xy"));
        let bytes = riff(&body);
        let mut reader = ChunkReader::open(&bytes).unwrap();
        assert_eq!(reader.next_chunk().unwrap().unwrap().data, b"abc");
        assert_eq!(reader.next_chunk().unwrap().unwrap().data, b"xy");
        assert!(reader.next_chunk().unwrap().is_none());
    }

    #[test]
    fn rejects_foreign_containers() {
        let error = ChunkReader::open(b"RIFX\0\0\0\0WAVE").expect_err("not RIFF");
        assert!(matches!(error, Error::NotRiff { .. }), "{error}");
        let mut bytes = riff(b"");
        bytes[8..12].copy_from_slice(b"WAVE");
        let error = ChunkReader::open(&bytes).expect_err("not a bank");
        assert!(matches!(error, Error::NotSoundFont { .. }), "{error}");
    }

    #[test]
    fn clamps_a_riff_size_that_runs_past_the_file() {
        let mut bytes = riff(&chunk(b"LIST", b"INFO"));
        let declared = (bytes.len() as u32 + 64).to_le_bytes();
        bytes[4..8].copy_from_slice(&declared);
        let mut reader = ChunkReader::open(&bytes).unwrap();
        assert!(reader.next_chunk().unwrap().is_some());
    }

    #[test]
    fn ignores_record_padding() {
        let mut payload = vec![0u8; 38 * 2];
        payload.extend_from_slice(&[0u8; 4]);
        let chunk = Chunk {
            id: *b"phdr",
            data: &payload,
        };
        assert_eq!(record_count(&chunk, 38).unwrap(), 2);
        assert!(record(chunk.data, 1, 38).is_some());
        assert!(record(chunk.data, 2, 38).is_none());
    }

    #[test]
    fn short_record_chunks_are_an_error() {
        let payload = [0u8; 4];
        let chunk = Chunk {
            id: *b"shdr",
            data: &payload,
        };
        let error = record_count(&chunk, 46).unwrap_err();
        assert!(matches!(error, Error::ShortRecords { .. }), "{error}");
    }

    #[test]
    fn truncated_payload_is_reported_with_its_chunk() {
        let mut body = b"LIST".to_vec();
        body.extend_from_slice(&99u32.to_le_bytes());
        body.extend_from_slice(b"INFO");
        let bytes = riff(&body);
        let mut reader = ChunkReader::open(&bytes).unwrap();
        let error = reader.next_chunk().expect_err("payload overruns");
        let text = error.to_string();
        assert!(text.contains("LIST") && text.contains("99"), "{text}");
    }
}
