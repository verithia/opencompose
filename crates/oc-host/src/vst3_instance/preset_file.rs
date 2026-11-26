//! Bounded decoding of a VST3 preset file. Native state stays opaque.

use crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES;

const HEADER_SIZE: usize = 48;
const CLASS_ID_RANGE: std::ops::Range<usize> = 8..40;
const ENTRY_SIZE: usize = 20;
const MAX_ENTRIES: usize = 128;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PresetFile<'a> {
    pub component: &'a [u8],
    pub controller: Option<&'a [u8]>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum PresetFileError {
    #[error("VST3 preset file exceeds its state size limit")]
    TooLarge,
    #[error("VST3 preset file has an invalid header")]
    Header,
    #[error("VST3 preset file has unsupported version {0}")]
    Version(u32),
    #[error("VST3 preset file belongs to a different plugin class")]
    Class,
    #[error("VST3 preset file has an invalid chunk list")]
    ChunkList,
    #[error("VST3 preset file has duplicate or overlapping chunks")]
    DuplicateOrOverlap,
    #[error("VST3 preset file has no component state")]
    MissingComponent,
}

impl<'a> PresetFile<'a> {
    /// A full signature distinguishes the container from a legacy opaque state.
    pub(super) fn recognize(bytes: &[u8]) -> bool {
        bytes.starts_with(b"VST3")
            && bytes
                .get(CLASS_ID_RANGE)
                .is_some_and(|class| class.iter().all(u8::is_ascii_hexdigit))
    }

    pub(super) fn decode(bytes: &'a [u8], factory_id: [i8; 16]) -> Result<Self, PresetFileError> {
        if bytes.len() > MAX_PLUGIN_STATE_BYTES {
            return Err(PresetFileError::TooLarge);
        }
        if bytes.len() < HEADER_SIZE || !Self::recognize(bytes) {
            return Err(PresetFileError::Header);
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != 1 {
            return Err(PresetFileError::Version(version));
        }
        let expected = class_id_text(factory_id);
        if !bytes[CLASS_ID_RANGE].eq_ignore_ascii_case(&expected) {
            return Err(PresetFileError::Class);
        }
        let invalid = PresetFileError::ChunkList;
        let table_start = index(&bytes[40..48]).ok_or(PresetFileError::ChunkList)?;
        let table = bytes.get(table_start..).ok_or(PresetFileError::ChunkList)?;
        if table_start < HEADER_SIZE || table.len() < 8 || !table.starts_with(b"List") {
            return Err(invalid);
        }
        let count = u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize;
        let table = &table[8..];
        if count == 0 || count > MAX_ENTRIES || table.len() != count * ENTRY_SIZE {
            return Err(invalid);
        }
        let mut component = None;
        let mut controller = None;
        let mut ranges = Vec::with_capacity(count);
        for entry in table.chunks_exact(ENTRY_SIZE) {
            let start = index(&entry[4..12]).ok_or(PresetFileError::ChunkList)?;
            let size = index(&entry[12..20]).ok_or(PresetFileError::ChunkList)?;
            let end = start.checked_add(size).ok_or(PresetFileError::ChunkList)?;
            if start < HEADER_SIZE || end > table_start {
                return Err(invalid);
            }
            if size != 0 {
                ranges.push((start, end));
            }
            let destination = match &entry[..4] {
                b"Comp" => &mut component,
                b"Cont" => &mut controller,
                _ => continue,
            };
            if destination.replace(&bytes[start..end]).is_some() {
                return Err(PresetFileError::DuplicateOrOverlap);
            }
        }
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(PresetFileError::DuplicateOrOverlap);
        }
        Ok(Self {
            component: component.ok_or(PresetFileError::MissingComponent)?,
            controller: controller.filter(|bytes| !bytes.is_empty()),
        })
    }

    pub(super) fn encode(&self, factory_id: [i8; 16]) -> Result<Vec<u8>, PresetFileError> {
        let entries = [
            Some((b"Comp", self.component)),
            self.controller.map(|bytes| (b"Cont", bytes)),
        ];
        let entries = entries.into_iter().flatten();
        let table_start = HEADER_SIZE
            .checked_add(self.component.len())
            .and_then(|start| start.checked_add(self.controller.map_or(0, <[u8]>::len)))
            .ok_or(PresetFileError::TooLarge)?;
        let entry_count = entries.clone().count();
        let total = table_start
            .checked_add(8 + entry_count * ENTRY_SIZE)
            .filter(|total| *total <= MAX_PLUGIN_STATE_BYTES)
            .ok_or(PresetFileError::TooLarge)?;
        let mut file = Vec::with_capacity(total);
        file.extend_from_slice(b"VST3\x01\0\0\0");
        file.extend_from_slice(&class_id_text(factory_id));
        file.extend_from_slice(&(table_start as i64).to_le_bytes());
        for (_, bytes) in entries.clone() {
            file.extend_from_slice(bytes);
        }
        file.extend_from_slice(b"List");
        file.extend_from_slice(&(entry_count as u32).to_le_bytes());
        let mut start = HEADER_SIZE;
        for (tag, bytes) in entries {
            file.extend_from_slice(tag);
            file.extend_from_slice(&(start as i64).to_le_bytes());
            file.extend_from_slice(&(bytes.len() as i64).to_le_bytes());
            start += bytes.len();
        }
        Ok(file)
    }
}

fn index(bytes: &[u8]) -> Option<usize> {
    usize::try_from(i64::from_le_bytes(bytes.try_into().unwrap())).ok()
}

pub(super) fn class_id_bytes(factory_id: [i8; 16]) -> [u8; 16] {
    let mut bytes = factory_id.map(|byte| byte as u8);
    if cfg!(windows) {
        // The factory uses native COM GUID layout on Windows. The file uses text order.
        bytes[..4].reverse();
        bytes[4..6].reverse();
        bytes[6..8].reverse();
    }
    bytes
}

fn class_id_text(factory_id: [i8; 16]) -> [u8; 32] {
    let bytes = class_id_bytes(factory_id);
    let hex = b"0123456789ABCDEF";
    let mut out = [0; 32];
    for (byte, digits) in bytes.into_iter().zip(out.chunks_exact_mut(2)) {
        digits[0] = hex[(byte >> 4) as usize];
        digits[1] = hex[(byte & 15) as usize];
    }
    out
}

#[cfg(test)]
pub(super) fn fixture(class: [i8; 16], component: &[u8], controller: &[u8]) -> Vec<u8> {
    PresetFile {
        component,
        controller: Some(controller),
    }
    .encode(class)
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vst3_preset_file_validates_chunks_and_keeps_legacy_state_distinct() {
        let class = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let bytes = fixture(class, b"processor", b"editor");
        assert!(PresetFile::recognize(&bytes));
        assert!(!PresetFile::recognize(b"VST3\x01\0\0\0raw component"));
        assert_eq!(
            PresetFile::decode(&bytes, class).unwrap().component,
            b"processor"
        );
        assert_eq!(
            PresetFile::decode(&bytes, class).unwrap().controller,
            Some(b"editor".as_slice())
        );
        assert_eq!(
            PresetFile::decode(&bytes, [0; 16]),
            Err(PresetFileError::Class)
        );
        assert_eq!(
            PresetFile::decode(&bytes[..bytes.len() - 1], class),
            Err(PresetFileError::ChunkList)
        );
        let mut overlapping = bytes.clone();
        let second_entry = bytes.len() - ENTRY_SIZE;
        overlapping[second_entry + 4..second_entry + 12]
            .copy_from_slice(&(HEADER_SIZE as i64).to_le_bytes());
        assert_eq!(
            PresetFile::decode(&overlapping, class),
            Err(PresetFileError::DuplicateOrOverlap)
        );
        let mut duplicate = bytes;
        duplicate[second_entry..second_entry + 4].copy_from_slice(b"Comp");
        assert_eq!(
            PresetFile::decode(&duplicate, class),
            Err(PresetFileError::DuplicateOrOverlap)
        );
        let empty = fixture(class, b"", b"");
        assert_eq!(PresetFile::decode(&empty, class).unwrap().controller, None);
    }
}
