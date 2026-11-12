//! Allocation and traversal limits for untrusted preset containers.
use std::io::{Read, Write};

pub(super) const MAX_INPUT: usize = 64 * 1024 * 1024;
pub(super) const MAX_METADATA: usize = 1024 * 1024;
pub(super) const MAX_EXPANDED: usize = 32 * 1024 * 1024;
pub(super) const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 1_000_000;
const MAX_ITEMS: usize = 262_144;

pub(super) fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    reader
        .take((limit as u64) + 1)
        .read_to_end(&mut out)
        .map_err(|error| format!("could not read bounded preset data: {error}"))?;
    if out.len() > limit {
        return Err(format!("preset data exceeds {limit} bytes"));
    }
    Ok(out)
}

pub(super) struct BoundedWriter {
    pub bytes: Vec<u8>,
    pub limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("preset output exceeds byte budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Inspect lengths before the generic decoder can reserve collection storage.
/// Node work includes byte-string expansion into JSON numeric elements.
pub(super) fn validate_cbor(bytes: &[u8]) -> Result<(), String> {
    struct Scan<'a> {
        bytes: &'a [u8],
        at: usize,
        nodes: usize,
    }
    impl Scan<'_> {
        fn take(&mut self, count: usize) -> Result<&[u8], String> {
            let end = self.at.checked_add(count).ok_or("CBOR length overflow")?;
            let value = self
                .bytes
                .get(self.at..end)
                .ok_or("truncated preset CBOR")?;
            self.at = end;
            Ok(value)
        }
        fn charge(&mut self, count: usize) -> Result<(), String> {
            self.nodes = self.nodes.checked_add(count).ok_or("CBOR work overflow")?;
            if self.nodes > MAX_NODES {
                return Err("preset CBOR exceeds node/work budget".into());
            }
            Ok(())
        }
        fn item(&mut self, depth: usize) -> Result<(), String> {
            if depth > MAX_DEPTH {
                return Err("preset CBOR exceeds depth budget".into());
            }
            self.charge(1)?;
            let initial = self.take(1)?[0];
            let major = initial >> 5;
            let info = initial & 31;
            let raw = match info {
                0..=23 => u64::from(info),
                24..=27 => {
                    let count = 1usize << (info - 24);
                    self.take(count)?
                        .iter()
                        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
                }
                // Indefinite containers need a separate item-count budget too.
                31 if (2..=5).contains(&major) => {
                    let mut count = 0;
                    while self.bytes.get(self.at) != Some(&255) {
                        count += 1;
                        if count > MAX_ITEMS {
                            return Err("preset CBOR exceeds item budget".into());
                        }
                        self.item(depth + 1)?;
                    }
                    self.take(1)?;
                    return Ok(());
                }
                _ => return Err("invalid preset CBOR header".into()),
            };
            match major {
                0 | 1 | 7 => {}
                2 | 3 => {
                    let size = usize::try_from(raw).map_err(|_| "CBOR string length overflow")?;
                    if major == 2 {
                        self.charge(size)?;
                    }
                    self.take(size)?;
                }
                4 | 5 => {
                    let count = usize::try_from(raw).map_err(|_| "CBOR item count overflow")?;
                    if count > MAX_ITEMS {
                        return Err("preset CBOR exceeds item budget".into());
                    }
                    let children = count
                        .checked_mul(if major == 5 { 2 } else { 1 })
                        .ok_or("CBOR item count overflow")?;
                    if children > MAX_NODES.saturating_sub(self.nodes) {
                        return Err("preset CBOR exceeds node/work budget".into());
                    }
                    for _ in 0..children {
                        self.item(depth + 1)?;
                    }
                }
                6 => self.item(depth + 1)?,
                _ => return Err("invalid preset CBOR type".into()),
            }
            Ok(())
        }
    }
    let mut scan = Scan {
        bytes,
        at: 0,
        nodes: 0,
    };
    scan.item(0)?;
    if scan.at != bytes.len() {
        return Err("trailing preset CBOR data".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cbor_preflight_rejects_depth_counts_and_byte_expansion_before_allocation() {
        let mut nested = vec![0x81; MAX_DEPTH + 2];
        nested.push(0xf6);
        assert!(validate_cbor(&nested).unwrap_err().contains("depth"));
        assert!(validate_cbor(&[0x9b, 255, 255, 255, 255, 255, 255, 255, 255]).is_err());
        assert!(
            validate_cbor(&[0x5a, 0, 0x10, 0, 0])
                .unwrap_err()
                .contains("work")
        );
        assert!(
            validate_cbor(&[0x80, 0xf6])
                .unwrap_err()
                .contains("trailing")
        );
        assert!(validate_cbor(&[0x7f, 0x61, b'a', 0xff]).is_ok());
        assert!(validate_cbor(&[0xbf, 0x61, b'a', 0x01, 0xff]).is_ok());
    }
    #[test]
    fn bounded_io_stops_at_limit_and_checks_output_before_append() {
        assert_eq!(read_limited(&b"abc"[..], 3).unwrap(), b"abc");
        assert!(read_limited(&b"abcd"[..], 3).is_err());
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            limit: 3,
        };
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert_eq!(writer.bytes, b"abc");
    }
}
