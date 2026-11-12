//! SMF container scanning and typed parse errors.
//!
//! `midly` does not expose byte offsets, so this module validates the chunk
//! layout first and locates the failing track when event parsing fails.

use midly::{EventBytemapIter, Format, Smf, Timing};

use super::import::{PARSE_END, Progress};
use super::{ImportLimit, ImportLimits, ImportStage, MidiError};

pub(crate) struct RawChunk<'a> {
    pub offset: usize,
    pub data: &'a [u8],
}

pub(crate) struct RawFile<'a> {
    pub format: Format,
    pub ppq: u16,
    pub chunks: Vec<RawChunk<'a>>,
    pub unknown_chunks: usize,
}

pub(crate) fn scan<'a>(data: &'a [u8], progress: &Progress<'_>) -> Result<RawFile<'a>, MidiError> {
    progress.check()?;
    let limits = progress.limits();
    ImportLimits::enforce(
        ImportLimit::Bytes,
        limits.max_bytes as u64,
        data.len() as u64,
        0,
    )?;
    if data.len() < 8 {
        return Err(MidiError::truncated(
            data.len(),
            "file is shorter than a MIDI header",
        ));
    }
    if &data[0..4] != b"MThd" {
        return Err(MidiError::malformed(0, "missing MThd header"));
    }
    let header_len = read_u32(data, 4) as usize;
    if header_len < 6 {
        return Err(MidiError::malformed(
            4,
            "MIDI header is shorter than 6 bytes",
        ));
    }
    let header_end = checked_add(8, header_len, data.len())?;
    if header_end > data.len() {
        return Err(MidiError::truncated(
            data.len(),
            "MIDI header runs past the end of the file",
        ));
    }
    let format = match read_u16(data, 8) {
        0 => Format::SingleTrack,
        1 => Format::Parallel,
        2 => Format::Sequential,
        other => {
            return Err(MidiError::malformed(
                8,
                format!("unknown SMF format {other}"),
            ));
        }
    };
    let declared_tracks = usize::from(read_u16(data, 10));
    ImportLimits::enforce(
        ImportLimit::Tracks,
        limits.max_tracks as u64,
        declared_tracks as u64,
        10,
    )?;
    let division = read_u16(data, 12);
    if division & 0x8000 != 0 {
        return Err(MidiError::UnsupportedTiming {
            offset: 12,
            message: "SMPTE timing has no PPQ grid".into(),
        });
    }
    if division == 0 {
        return Err(MidiError::malformed(12, "zero ticks per quarter note"));
    }

    let mut chunks = Vec::with_capacity(declared_tracks);
    let mut unknown_chunks = 0;
    let mut pos = header_end;
    while pos < data.len() {
        progress.check()?;
        if data.len() - pos < 8 {
            return Err(MidiError::truncated(pos, "truncated chunk header"));
        }
        let id = &data[pos..pos + 4];
        let length = read_u32(data, pos + 4) as usize;
        let start = checked_add(pos, 8, data.len())?;
        let end = checked_add(start, length, data.len())?;
        if end > data.len() {
            return Err(MidiError::truncated(
                data.len(),
                "track chunk runs past the end of the file",
            ));
        }
        ImportLimits::enforce(
            ImportLimit::Events,
            limits.max_events as u64,
            chunks
                .len()
                .saturating_add(unknown_chunks)
                .saturating_add(1) as u64,
            pos,
        )?;
        if id == b"MTrk" {
            ImportLimits::enforce(
                ImportLimit::Tracks,
                limits.max_tracks as u64,
                chunks.len().saturating_add(1) as u64,
                pos,
            )?;
            chunks.push(RawChunk {
                offset: start,
                data: &data[start..end],
            });
        } else {
            unknown_chunks += 1;
        }
        pos = end;
    }

    if chunks.len() < declared_tracks {
        return Err(MidiError::truncated(
            data.len(),
            format!(
                "header declares {declared_tracks} tracks but only {} are present",
                chunks.len()
            ),
        ));
    }
    if chunks.len() > declared_tracks {
        return Err(MidiError::malformed(
            chunks[declared_tracks].offset,
            "more track chunks than the header declares",
        ));
    }

    Ok(RawFile {
        format,
        ppq: division,
        chunks,
        unknown_chunks,
    })
}

pub(crate) fn parse<'a>(
    data: &'a [u8],
    raw: &RawFile<'a>,
    progress: &mut Progress<'_>,
) -> Result<Smf<'a>, MidiError> {
    let total = raw
        .chunks
        .iter()
        .map(|chunk| chunk.data.len())
        .sum::<usize>()
        .max(1);
    let limits = progress.limits();
    let mut events = 0u64;
    let mut notes = 0u64;

    let mut scanned = 0_usize;
    for chunk in &raw.chunks {
        progress.check()?;
        let mut consumed = 0_usize;
        let mut next_report = 0_usize;
        let mut tick = 0u64;
        for entry in EventBytemapIter::new(chunk.data) {
            let (bytes, event) = entry.map_err(|_| {
                MidiError::malformed(
                    chunk.offset + consumed,
                    "track contains an unparsable event",
                )
            })?;
            progress.check()?;
            events = events.saturating_add(1);
            ImportLimits::enforce(
                ImportLimit::Events,
                limits.max_events as u64,
                events,
                chunk.offset + consumed,
            )?;
            tick = tick
                .checked_add(u64::from(event.delta.as_int()))
                .ok_or_else(|| MidiError::InvalidEvent {
                    offset: chunk.offset + consumed,
                    message: "MIDI tick overflow".into(),
                })?;
            if u128::from(tick) > u128::from(limits.max_quarter_notes) * u128::from(raw.ppq) {
                return Err(MidiError::LimitExceeded {
                    kind: ImportLimit::Extent,
                    limit: limits.max_quarter_notes,
                    actual: tick.div_ceil(u64::from(raw.ppq)),
                    offset: chunk.offset + consumed,
                });
            }
            ImportLimits::enforce(
                ImportLimit::Payload,
                limits.max_event_payload_bytes as u64,
                bytes.len() as u64,
                chunk.offset + consumed,
            )?;
            match event.kind {
                midly::TrackEventKind::Midi {
                    message: midly::MidiMessage::NoteOn { vel, .. },
                    ..
                } if vel.as_int() != 0 => {
                    notes = notes.saturating_add(1);
                    ImportLimits::enforce(
                        ImportLimit::Notes,
                        limits.max_notes as u64,
                        notes,
                        chunk.offset + consumed,
                    )?;
                    progress.charge_output(1, chunk.offset + consumed)?;
                }
                midly::TrackEventKind::Midi {
                    message:
                        midly::MidiMessage::Controller { .. }
                        | midly::MidiMessage::PitchBend { .. }
                        | midly::MidiMessage::Aftertouch { .. }
                        | midly::MidiMessage::ChannelAftertouch { .. },
                    ..
                } => {
                    progress.charge_output(1, chunk.offset + consumed)?;
                }
                midly::TrackEventKind::Meta(message) => {
                    progress.charge_metadata(bytes.len() as u64, chunk.offset + consumed)?;
                    if matches!(
                        message,
                        midly::MetaMessage::Tempo(_)
                            | midly::MetaMessage::TimeSignature(..)
                            | midly::MetaMessage::KeySignature(..)
                    ) {
                        progress.charge_output(1, chunk.offset + consumed)?;
                    }
                }
                _ => {}
            }
            consumed += varlen_len(event.delta.as_int()) + bytes.len();
            if consumed >= next_report {
                progress.span(
                    ImportStage::Parse,
                    0.0,
                    PARSE_END,
                    scanned + consumed,
                    total,
                );
                next_report = consumed + 256;
            }
        }
        if consumed < chunk.data.len() {
            return Err(MidiError::malformed(
                chunk.offset + consumed,
                "track contains an unparsable event",
            ));
        }
        scanned += chunk.data.len();
    }
    progress.check()?;
    progress.charge_work(events.saturating_add((data.len() / 16) as u64))?;
    match Smf::parse(data) {
        Ok(smf) => {
            if smf.tracks.len() != raw.chunks.len() {
                let offset = raw
                    .chunks
                    .get(smf.tracks.len())
                    .map(|chunk| chunk.offset)
                    .unwrap_or(data.len());
                return Err(MidiError::malformed(
                    offset,
                    "a track chunk could not be parsed",
                ));
            }
            Ok(smf)
        }
        Err(error) => {
            for chunk in &raw.chunks {
                progress.check()?;
                progress.charge_work((chunk.data.len() / 16) as u64)?;
                let synthetic = single_track_file(chunk.data);
                if Smf::parse(&synthetic).is_err() {
                    return Err(MidiError::malformed(chunk.offset, error.to_string()));
                }
            }
            Err(MidiError::malformed(8, error.to_string()))
        }
    }
}

pub(crate) fn write(smf: &Smf<'_>) -> Result<Vec<u8>, MidiError> {
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)
        .map_err(|error| MidiError::InvalidExport {
            message: format!("cannot encode MIDI: {error}"),
        })?;
    Ok(bytes)
}

pub(crate) fn header(format: Format, ppq: u16) -> Result<midly::Header, MidiError> {
    if ppq == 0 || ppq > 32767 {
        return Err(MidiError::InvalidExport {
            message: format!("PPQ {ppq} is outside 1..=32767"),
        });
    }
    Ok(midly::Header::new(format, Timing::Metrical(ppq.into())))
}

fn single_track_file(track: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(22 + track.len());
    bytes.extend_from_slice(b"MThd");
    bytes.extend_from_slice(&6_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&480_u16.to_be_bytes());
    bytes.extend_from_slice(b"MTrk");
    bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
    bytes.extend_from_slice(track);
    bytes
}

fn varlen_len(value: u32) -> usize {
    if value < 0x80 {
        1
    } else if value < 0x4000 {
        2
    } else if value < 0x20_0000 {
        3
    } else {
        4
    }
}

fn checked_add(base: usize, length: usize, limit: usize) -> Result<usize, MidiError> {
    let end = base
        .checked_add(length)
        .ok_or_else(|| MidiError::truncated(limit, "chunk length overflow"))?;
    Ok(end)
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}
