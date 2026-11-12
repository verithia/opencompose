//! Retained aligned Atom ports and checked sequence decoding.

use super::super::abi::*;
use std::ops::{Deref, DerefMut};
use std::ptr;

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct AtomWord(u64);

pub(super) struct AtomBuffer {
    words: Box<[AtomWord]>,
    bytes: usize,
}

impl AtomBuffer {
    pub(super) fn new(bytes: usize) -> Self {
        Self {
            words: vec![AtomWord(0); bytes.div_ceil(8)].into_boxed_slice(),
            bytes,
        }
    }
}
impl Deref for AtomBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: initialized AtomWord storage covers bytes bytes, remains
        // allocated for this borrow, and u8 permits every initialized bit pattern.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.bytes) }
    }
}
impl DerefMut for AtomBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: exclusive owner access covers initialized retained words;
        // writing any byte pattern preserves validity of the underlying u64s.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.bytes) }
    }
}

pub(super) fn write_empty_sequence(buffer: &mut [u8], sequence_urid: u32) {
    if buffer.len() < ATOM_SEQUENCE_HEADER_SIZE {
        return;
    }
    let sequence = AtomSequence {
        atom: Atom {
            size: 8,
            type_: sequence_urid,
        },
        body: AtomSequenceBody { unit: 0, pad: 0 },
    };
    // SAFETY: the slice contains the entire header; unaligned writing also
    // permits bounded byte slices used by the checked decoder's tests.
    unsafe { ptr::write_unaligned(buffer.as_mut_ptr().cast::<AtomSequence>(), sequence) };
}

pub(super) fn write_output_chunk(buffer: &mut [u8], chunk_urid: u32) {
    if buffer.len() < std::mem::size_of::<Atom>() {
        return;
    }
    let capacity = buffer.len() - std::mem::size_of::<Atom>();
    let Ok(size) = u32::try_from(capacity) else {
        return;
    };
    // SAFETY: the slice holds an Atom header; its advertised body capacity is
    // exactly the remaining retained storage, not the preceding sequence size.
    unsafe {
        ptr::write_unaligned(
            buffer.as_mut_ptr().cast::<Atom>(),
            Atom {
                size,
                type_: chunk_urid,
            },
        )
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct EventHeader {
    pub(super) frames: i64,
    pub(super) body: Atom,
}

fn header_at(buffer: &[u8], offset: usize) -> Option<EventHeader> {
    if offset.checked_add(ATOM_EVENT_HEADER_SIZE)? > buffer.len() {
        return None;
    }
    // SAFETY: a complete initialized header is contained within the borrowed
    // byte slice; the copied value has no invalid bit patterns or references.
    Some(unsafe { ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<EventHeader>()) })
}

fn validated_end(buffer: &[u8], sequence_urid: u32, frames: u32) -> Option<usize> {
    if buffer.len() < ATOM_SEQUENCE_HEADER_SIZE {
        return None;
    }
    // SAFETY: the input slice holds a complete initialized header, copied
    // without assuming that an arbitrary decoder input slice is aligned.
    let sequence = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<AtomSequence>()) };
    if sequence.atom.type_ != sequence_urid
        || sequence.atom.size < 8
        || sequence.body.unit != 0
        || sequence.body.pad != 0
    {
        return None;
    }
    let end = std::mem::size_of::<Atom>().checked_add(sequence.atom.size as usize)?;
    if end > buffer.len() {
        return None;
    }
    let mut offset = ATOM_SEQUENCE_HEADER_SIZE;
    let mut previous = 0;
    while offset < end {
        let header = header_at(&buffer[..end], offset)?;
        let time = u32::try_from(header.frames).ok()?;
        if time >= frames || time < previous {
            return None;
        }
        let padded = (header.body.size as usize).checked_add(7)? & !7;
        let next = offset
            .checked_add(ATOM_EVENT_HEADER_SIZE)?
            .checked_add(padded)?;
        if next > end {
            return None;
        }
        previous = time;
        offset = next;
    }
    (offset == end).then_some(end)
}

pub(super) fn read_sequence_events(
    buffer: &[u8],
    sequence_urid: u32,
    midi_urid: u32,
    frames: u32,
) -> Vec<(u32, Vec<u8>)> {
    let Some(end) = validated_end(buffer, sequence_urid, frames) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let mut offset = ATOM_SEQUENCE_HEADER_SIZE;
    while offset < end {
        let header = header_at(&buffer[..end], offset).expect("validated complete event header");
        let payload = offset + ATOM_EVENT_HEADER_SIZE;
        let size = header.body.size as usize;
        if header.body.type_ == midi_urid {
            events.push((
                header.frames as u32,
                buffer[payload..payload + size].to_vec(),
            ));
        }
        offset = payload + size.div_ceil(8) * 8;
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    const SEQUENCE: u32 = 1;
    const MIDI: u32 = 2;
    fn sequence(bytes: usize, time: i64, payload: &[u8]) -> AtomBuffer {
        let mut buffer = AtomBuffer::new(bytes);
        write_empty_sequence(&mut buffer, SEQUENCE);
        let header = EventHeader {
            frames: time,
            body: Atom {
                size: payload.len() as u32,
                type_: MIDI,
            },
        };
        // SAFETY: fixtures reserve header plus padded payload before writing.
        unsafe {
            ptr::write_unaligned(buffer.as_mut_ptr().add(16).cast::<EventHeader>(), header);
        }
        buffer[32..32 + payload.len()].copy_from_slice(payload);
        let body = (24 + payload.len().div_ceil(8) * 8) as u32;
        buffer[..4].copy_from_slice(&body.to_ne_bytes());
        buffer
    }
    #[test]
    fn lv2_atom_owner_alignment_header_and_exact_capacity_boundary() {
        for bytes in [16, 17, 40, 8192] {
            let mut buffer = AtomBuffer::new(bytes);
            assert_eq!(buffer.as_ptr() as usize % 8, 0);
            assert_eq!(buffer.len(), bytes);
            write_output_chunk(&mut buffer, 3);
            assert_eq!(
                u32::from_ne_bytes(buffer[..4].try_into().unwrap()),
                bytes as u32 - 8
            );
            assert_eq!(u32::from_ne_bytes(buffer[4..8].try_into().unwrap()), 3);
        }
        let full = sequence(40, 7, &[0x90, 60, 100]);
        assert_eq!(
            read_sequence_events(&full, SEQUENCE, MIDI, 8),
            vec![(7, vec![0x90, 60, 100])]
        );
    }
    #[test]
    fn lv2_atom_owner_refuses_declared_truncation_padding_and_invalid_time() {
        let valid = sequence(48, 7, &[0x90, 60, 100]);
        for body in [8_u32, 16, 24, 27, 31, 41, u32::MAX] {
            let mut bad = valid.to_vec();
            bad[..4].copy_from_slice(&body.to_ne_bytes());
            assert!(
                read_sequence_events(&bad, SEQUENCE, MIDI, 8).is_empty(),
                "body {body}"
            );
        }
        for time in [-1, 8, i64::MAX] {
            let bad = sequence(40, time, &[0x90, 60, 100]);
            assert!(read_sequence_events(&bad, SEQUENCE, MIDI, 8).is_empty());
        }
        let mut bad = valid.to_vec();
        bad[24..28].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(read_sequence_events(&bad, SEQUENCE, MIDI, 8).is_empty());
        for bytes in 0..40 {
            assert!(read_sequence_events(&valid[..bytes], SEQUENCE, MIDI, 8).is_empty());
        }
    }
}
