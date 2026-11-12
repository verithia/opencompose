//! Ogg Vorbis decoding for SF3 banks.
//!
//! An SF3 bank is a SoundFont 2 whose `smpl` chunk holds one Ogg Vorbis stream
//! per sample. The bank's semantics are unchanged: [`super::font`] builds the
//! same presets, zones and sample headers, and the player reads the same flat
//! waveform. This module only turns one stream into 16-bit points at the
//! stream's own rate.

use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// True when the bytes begin with an Ogg page header.
pub(crate) fn is_ogg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
}

/// True when the page chain is intact and some page carries the end-of-stream
/// flag. A stream cut anywhere keeps its `OggS` prefix but loses that page, so
/// this is what refuses a truncated sample instead of decoding a prefix of it.
/// Bytes after the end-of-stream page are ignored; some banks pad a sample.
pub(crate) fn is_complete(bytes: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset + 27 <= bytes.len() {
        let page = &bytes[offset..];
        if !page.starts_with(b"OggS") {
            return false;
        }
        let segments = usize::from(page[26]);
        let table_end = offset + 27 + segments;
        if table_end > bytes.len() {
            return false;
        }
        let body_len: usize = bytes[offset + 27..table_end]
            .iter()
            .map(|count| usize::from(*count))
            .sum();
        if page[5] & 0x04 != 0 {
            return body_len <= bytes.len() - table_end;
        }
        offset = table_end + body_len;
    }
    false
}

/// Decode one sample's Ogg Vorbis stream to mono 16-bit points, with the rate
/// the stream declares. The caller names the sample in any error it reports.
pub(crate) fn decode(bytes: &[u8]) -> Result<(u32, Vec<i16>), String> {
    decode_with_limit(bytes, None).map_err(|error| error.to_string())
}

/// A prepared sample cannot grow beyond its proved logical span. The limit is
/// checked before each PCM allocation/append, never after collecting the stream.
#[cfg(test)]
pub(crate) fn decode_bounded(bytes: &[u8], frames: usize) -> Result<(u32, Vec<i16>), String> {
    decode_with_limit(bytes, Some(frames)).map_err(|error| error.to_string())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("Vorbis sample exceeds its indexed preparation frame limit")]
    FrameLimit,
    #[error("{0}")]
    Invalid(String),
}
impl From<String> for DecodeError {
    fn from(reason: String) -> Self {
        Self::Invalid(reason)
    }
}
impl From<&str> for DecodeError {
    fn from(reason: &str) -> Self {
        Self::Invalid(reason.into())
    }
}

pub(crate) fn decode_capped(bytes: &[u8], frames: usize) -> Result<(u32, Vec<i16>), DecodeError> {
    decode_with_limit(bytes, Some(frames))
}

fn decode_with_limit(
    bytes: &[u8],
    frame_limit: Option<usize>,
) -> Result<(u32, Vec<i16>), DecodeError> {
    if !is_complete(bytes) {
        return Err("the stream is cut short: no end-of-stream page".into());
    }
    let mut format = open_stream(bytes)?;
    let track = format
        .default_track()
        .ok_or_else(|| "stream has no audio track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format!("unsupported codec: {error}"))?;

    let mut rate = 0u32;
    let mut points = Vec::new();
    if let Some(limit) = frame_limit {
        points
            .try_reserve_exact(limit.min(65_536))
            .map_err(|error| format!("cannot allocate prepared Vorbis sample: {error}"))?;
    }
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(format!("read error: {error}").into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|error| format!("decode error: {error}"))?;
        if frame_limit.is_some_and(|limit| {
            points
                .len()
                .checked_add(decoded.frames())
                .is_none_or(|total| total > limit)
        }) {
            return Err(DecodeError::FrameLimit);
        }
        if frame_limit.is_some() {
            points
                .try_reserve_exact(decoded.frames())
                .map_err(|error| format!("cannot allocate prepared Vorbis sample: {error}"))?;
        }
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        if spec.rate == 0 {
            return Err("stream declares no sample rate".into());
        }
        rate = spec.rate;
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        points.extend(buffer.samples().chunks(channels).map(|frame| {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            (mono.clamp(-1.0, 1.0) * 32768.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16
        }));
    }
    if rate == 0 {
        return Err("stream declares no sample rate".into());
    }
    Ok((rate, points))
}

/// The decoder's demuxer owns packet durations, including its gapless trims.
/// A single audio page can produce more frames than its end granule declares,
/// so logical sample addresses follow emitted packet extents, not that granule.
/// Indexing scans compressed packets without synthesizing unrelated PCM.
pub(crate) fn indexed_frames(bytes: &[u8]) -> Result<usize, String> {
    let mut offset = 0_usize;
    let mut serial = None;
    let mut sequence = 0_u32;
    let granule = loop {
        let page = bytes
            .get(offset..offset.saturating_add(27))
            .ok_or("incomplete Ogg page header")?;
        if &page[..4] != b"OggS" || page[4] != 0 {
            return Err("unsupported Ogg page header".into());
        }
        let page_serial = u32::from_le_bytes(page[14..18].try_into().unwrap());
        let page_sequence = u32::from_le_bytes(page[18..22].try_into().unwrap());
        if page_sequence != sequence
            || serial.is_some_and(|serial| serial != page_serial)
            || (sequence == 0 && page[5] & 2 == 0)
            || (sequence != 0 && page[5] & 2 != 0)
        {
            return Err("chained or discontinuous Ogg stream".into());
        }
        serial = Some(page_serial);
        sequence = sequence.checked_add(1).ok_or("too many Ogg pages")?;
        let table_end = offset
            .checked_add(27 + usize::from(page[26]))
            .ok_or("Ogg page offset overflow")?;
        let table = bytes
            .get(offset + 27..table_end)
            .ok_or("incomplete Ogg segment table")?;
        let body: usize = table.iter().map(|&size| usize::from(size)).sum();
        offset = table_end
            .checked_add(body)
            .ok_or("Ogg page length overflow")?;
        if offset > bytes.len() {
            return Err("incomplete Ogg page body".into());
        }
        if page[5] & 4 != 0 {
            if bytes[offset..].iter().any(|&byte| byte != 0) {
                return Err("non-padding bytes after Ogg end page".into());
            }
            break u64::from_le_bytes(page[6..14].try_into().unwrap());
        }
    };
    if granule == 0 || granule == u64::MAX {
        return Err("Ogg end page has no finite audio duration".into());
    }
    let mut format = open_stream(bytes)?;
    let track = format
        .default_track()
        .ok_or("Ogg stream has no audio track")?;
    let track_id = track.id;
    let params = &track.codec_params;
    if params.start_ts != 0 || params.n_frames != Some(granule) {
        return Err("gapless Ogg duration does not prove the end-page frame count".into());
    }
    let mut frames = 0usize;
    loop {
        match format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => {
                let count = usize::try_from(packet.dur())
                    .map_err(|_| "Ogg packet frame count exceeds address space")?;
                frames = frames
                    .checked_add(count)
                    .ok_or("Ogg sample frame count exceeds address space")?;
            }
            Ok(_) => {}
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(format!("cannot index Ogg packets: {error}")),
        }
    }
    if frames == 0 {
        return Err("Ogg stream produces no audio frames".into());
    }
    Ok(frames)
}

fn open_stream(bytes: &[u8]) -> Result<Box<dyn symphonia::core::formats::FormatReader>, String> {
    // The stream owns its bytes: `MediaSourceStream` boxes a `'static` source,
    // and a sample region is a slice of the bank file it was read from.
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                // The granule position of the last page is the sample count;
                // dropping the encoder's padding is what makes loop points
                // land where the bank's own header says they do.
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("not an Ogg Vorbis stream: {error}"))?;
    Ok(probed.format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prepared_frame_limit_bounds_the_actual_vorbis_pcm_producer() {
        let bytes = include_bytes!("../../tests/fixtures/sf3-tone-22050.ogg");
        let (rate, full) = decode(bytes).unwrap();
        let (prepared_rate, prepared) = decode_bounded(bytes, full.len()).unwrap();
        assert_eq!(prepared_rate, rate);
        assert_eq!(prepared, full);
        let error = decode_bounded(bytes, full.len() - 1).unwrap_err();
        assert_eq!(
            error,
            "Vorbis sample exceeds its indexed preparation frame limit"
        );
        assert!(decode_bounded(bytes, 0).is_err());
    }
    fn single_audio_page() -> Vec<u8> {
        let bytes = include_bytes!("../../tests/fixtures/sf3-tone-22050.ogg");
        let mut offset = 0;
        let mut pages = Vec::new();
        while offset < bytes.len() {
            let table_end = offset + 27 + usize::from(bytes[offset + 26]);
            let end = table_end
                + bytes[offset + 27..table_end]
                    .iter()
                    .map(|&n| usize::from(n))
                    .sum::<usize>();
            pages.push((
                &bytes[offset..offset + 27],
                &bytes[offset + 27..table_end],
                &bytes[table_end..end],
            ));
            offset = end;
        }
        let mut merged = pages[2].0.to_vec();
        merged[5] = 4;
        merged[6..14].copy_from_slice(&pages.last().unwrap().0[6..14]);
        merged[22..26].fill(0);
        let segments: usize = pages[2..].iter().map(|page| page.1.len()).sum();
        merged[26] = u8::try_from(segments).unwrap();
        for page in &pages[2..] {
            merged.extend_from_slice(page.1);
        }
        for page in &pages[2..] {
            merged.extend_from_slice(page.2);
        }
        let mut crc = 0u32;
        for &byte in &merged {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        merged[22..26].copy_from_slice(&crc.to_le_bytes());
        let mut stream = Vec::new();
        for page in &pages[..2] {
            stream.extend_from_slice(page.0);
            stream.extend_from_slice(page.1);
            stream.extend_from_slice(page.2);
        }
        stream.extend_from_slice(&merged);
        stream
    }

    #[test]
    fn indexed_single_audio_page_matches_decoded_pcm() {
        let bytes = single_audio_page();
        let (rate, full) = decode(&bytes).unwrap();
        let indexed = indexed_frames(&bytes).unwrap();
        assert_eq!(
            indexed,
            full.len(),
            "packet layout must not change the indexed PCM extent"
        );
        let (prepared_rate, prepared) = decode_bounded(&bytes, indexed).unwrap();
        assert_eq!(prepared_rate, rate);
        assert_eq!(prepared, full);
    }
}
