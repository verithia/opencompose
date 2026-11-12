//! Bounded container identification for downloaded sample files. The importer
//! still validates and decodes the audio codec when a sample is opened.

/// Recognize audio containers from their contents. URL suffixes and names are
/// hints only, since authenticated endpoints commonly name a JSON resource.
pub(crate) fn audio_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 {
        if &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
            return riff_audio(bytes, false).then_some("wav");
        }
        if &bytes[..4] == b"FORM" && matches!(&bytes[8..12], b"AIFF" | b"AIFC") {
            return riff_audio(bytes, true).then_some("aiff");
        }
    }
    // STREAMINFO must be the first FLAC metadata block and is 34 bytes long.
    if bytes.len() > 42
        && bytes.starts_with(b"fLaC")
        && bytes[4] & 0x7f == 0
        && bytes[5..8] == [0, 0, 34]
    {
        return Some("flac");
    }
    if bytes.len() >= 27 && bytes.starts_with(b"OggS") && bytes[4] == 0 {
        let segments = usize::from(bytes[26]);
        let header_end = 27 + segments;
        let lacing = bytes.get(27..header_end)?;
        let payload_len: usize = lacing.iter().map(|value| usize::from(*value)).sum();
        let payload = bytes.get(header_end..header_end + payload_len)?;
        if payload.starts_with(b"\x01vorbis")
            || payload.starts_with(b"OpusHead")
            || payload.starts_with(b"\x7fFLAC")
        {
            return Some("ogg");
        }
    }
    if mp3_frame(bytes) {
        return Some("mp3");
    }
    // ISO base media files need an audio-specific brand or a sound handler.
    if bytes.len() >= 16 && &bytes[4..8] == b"ftyp" {
        let end = u32::from_be_bytes(bytes[..4].try_into().ok()?) as usize;
        let brands = bytes.get(8..end)?;
        if end >= 16
            && (brands.starts_with(b"M4A ")
                || brands.starts_with(b"M4B ")
                || bytes[..bytes.len().min(1024 * 1024)]
                    .windows(16)
                    .any(|window| &window[..4] == b"hdlr" && &window[12..] == b"soun"))
        {
            return Some("m4a");
        }
    }
    None
}

fn riff_audio(bytes: &[u8], big_endian: bool) -> bool {
    let size = |value: &[u8]| -> usize {
        let value: [u8; 4] = value.try_into().unwrap();
        if big_endian {
            u32::from_be_bytes(value) as usize
        } else {
            u32::from_le_bytes(value) as usize
        }
    };
    let Some(end) = size(&bytes[4..8]).checked_add(8) else {
        return false;
    };
    if end < 12 || end > bytes.len() {
        return false;
    }
    let mut offset = 12;
    let mut format = false;
    let mut data = false;
    while offset + 8 <= end {
        let chunk = &bytes[offset..offset + 4];
        let length = size(&bytes[offset + 4..offset + 8]);
        let Some(next) = (offset + 8).checked_add(length) else {
            return false;
        };
        if next > end {
            return false;
        }
        format |= (!big_endian && chunk == b"fmt " && length >= 16)
            || (big_endian && chunk == b"COMM" && length >= 18);
        data |= (!big_endian && chunk == b"data" && length > 0)
            || (big_endian && chunk == b"SSND" && length > 8);
        offset = next + length % 2;
    }
    format && data
}

fn mp3_frame(bytes: &[u8]) -> bool {
    let offset = if bytes.starts_with(b"ID3") {
        let Some(header) = bytes.get(..10) else {
            return false;
        };
        if header[6..10].iter().any(|value| value & 0x80 != 0) {
            return false;
        }
        let size = header[6..10]
            .iter()
            .fold(0usize, |size, value| (size << 7) | usize::from(*value));
        10 + size + if header[5] & 0x10 != 0 { 10 } else { 0 }
    } else {
        0
    };
    let Some(header) = bytes.get(offset..offset + 5) else {
        return false;
    };
    header[0] == 0xff
        && header[1] & 0xe0 == 0xe0
        && header[1] & 0x18 != 0x08
        && header[1] & 0x06 != 0
        && header[2] & 0xf0 != 0
        && header[2] & 0xf0 != 0xf0
        && header[2] & 0x0c != 0x0c
}
