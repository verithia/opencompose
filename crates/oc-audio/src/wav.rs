//! Bounded PCM and Broadcast WAV reads from seekable RIFF files.

use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Open PCM or IEEE-float WAV for sample-accurate reads. Hound handles the
/// sample format. When RIFF metadata precedes the samples, it receives just
/// the format and data headers while audio streams from the original offset.
pub fn open_reader(path: &Path) -> hound::Result<hound::WavReader<impl Read + Seek>> {
    open_file_reader(File::open(path)?)
}

/// Rewind and open an already-held WAV file.
pub fn open_file_reader(file: File) -> hound::Result<hound::WavReader<impl Read + Seek>> {
    let mut file = BufReader::new(file);
    file.rewind()?;
    let (header, data_offset, data_len) = projected_header(&mut file)?.unwrap_or_default();
    file.seek(SeekFrom::Start(data_offset))?;
    hound::WavReader::new(WavInput {
        file,
        header: Cursor::new(header),
        data_offset,
        data_len,
        data_position: 0,
    })
}

struct WavInput {
    file: BufReader<File>,
    header: Cursor<Vec<u8>>,
    data_offset: u64,
    data_len: u64,
    data_position: u64,
}

impl Read for WavInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let header_len = self.header.get_ref().len() as u64;
        if header_len == 0 {
            return self.file.read(buffer);
        }
        if self.header.position() < header_len {
            return self.header.read(buffer);
        }
        let remaining = self.data_len.saturating_sub(self.data_position);
        if remaining > 0 {
            let limit = buffer.len().min(remaining as usize);
            let read = self.file.read(&mut buffer[..limit])?;
            self.data_position += read as u64;
            return Ok(read);
        }
        // The projected RIFF includes the required alignment byte after data.
        if self.data_position == self.data_len
            && !self.data_len.is_multiple_of(2)
            && !buffer.is_empty()
        {
            buffer[0] = 0;
            self.data_position += 1;
            return Ok(1);
        }
        Ok(0)
    }
}

impl Seek for WavInput {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let header_len = self.header.get_ref().len() as u64;
        if header_len == 0 {
            return self.file.seek(position);
        }
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(delta) => {
                i128::from(self.header.position())
                    + i128::from(self.data_position)
                    + i128::from(delta)
            }
            SeekFrom::End(delta) => {
                i128::from(header_len + self.data_len + self.data_len % 2) + i128::from(delta)
            }
        };
        let target = u64::try_from(target)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid WAV seek"))?;
        let data_position = target.saturating_sub(header_len);
        let source = self
            .data_offset
            .checked_add(data_position.min(self.data_len))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid WAV seek"))?;
        self.file.seek(SeekFrom::Start(source))?;
        self.header.set_position(target.min(header_len));
        self.data_position = data_position;
        Ok(target)
    }
}

struct RiffChunk {
    tag: [u8; 4],
    data: u64,
    size: u32,
    complete: bool,
}

/// Visit top-level RIFF chunks by offset. The visitor can stop early.
/// Returns false when the scan reaches its chunk limit before the end.
fn scan_wave_chunks(
    reader: &mut BufReader<File>,
    mut visit: impl FnMut(&mut BufReader<File>, RiffChunk) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Ok(true);
    }
    let riff_end = (8 + u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())))
        .min(reader.get_ref().metadata()?.len());
    let mut cursor = 12;
    for _ in 0..4096 {
        if cursor + 8 > riff_end {
            return Ok(true);
        }
        reader.seek(SeekFrom::Start(cursor))?;
        let mut bytes = [0; 8];
        reader.read_exact(&mut bytes)?;
        let size = u32::from_le_bytes(bytes[4..].try_into().unwrap());
        let data = cursor + 8;
        let end = data + u64::from(size);
        if !visit(
            reader,
            RiffChunk {
                tag: bytes[..4].try_into().unwrap(),
                data,
                size,
                complete: end <= riff_end,
            },
        )? {
            return Ok(true);
        }
        cursor = end + u64::from(size % 2);
    }
    Ok(cursor + 8 > riff_end)
}

fn projected_header(file: &mut BufReader<File>) -> io::Result<Option<(Vec<u8>, u64, u64)>> {
    let mut header = b"RIFF\0\0\0\0WAVE".to_vec();
    let mut audio = None;
    let mut needs_projection = false;
    scan_wave_chunks(file, |reader, chunk| {
        if !chunk.complete {
            return Ok(false);
        }
        match &chunk.tag {
            b"fmt " if header.len() == 12 && (16..=4096).contains(&chunk.size) => {
                header.extend_from_slice(b"fmt ");
                header.extend_from_slice(&chunk.size.to_le_bytes());
                let start = header.len();
                header.resize(start + chunk.size as usize, 0);
                reader.read_exact(&mut header[start..])?;
            }
            b"data" => {
                if needs_projection && header.len() > 12 {
                    let size = (header.len() as u32)
                        .checked_add(chunk.size)
                        .and_then(|size| size.checked_add(chunk.size % 2))
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "WAV size overflow")
                        })?;
                    header[4..8].copy_from_slice(&size.to_le_bytes());
                    header.extend_from_slice(b"data");
                    header.extend_from_slice(&chunk.size.to_le_bytes());
                    audio = Some((chunk.data, u64::from(chunk.size)));
                }
                return Ok(false);
            }
            _ => needs_projection = true,
        }
        needs_projection |= chunk.size % 2 != 0;
        Ok(true)
    })?;
    Ok(audio.map(|(offset, len)| (header, offset, len)))
}

/// EBU Tech 3285 first-sample timestamp. The chunk may follow audio data.
pub fn broadcast_time_reference(path: &Path) -> io::Result<Option<u64>> {
    const BEXT_FIXED_BYTES: u32 = 602;
    const TIME_REFERENCE_OFFSET: u64 = 338;
    let mut reader = BufReader::new(File::open(path)?);
    let mut timestamp = None;
    let complete = scan_wave_chunks(&mut reader, |reader, chunk| {
        if &chunk.tag == b"bext" {
            if chunk.size < BEXT_FIXED_BYTES || !chunk.complete || timestamp.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete or duplicate bext chunk",
                ));
            }
            reader.seek(SeekFrom::Start(chunk.data + TIME_REFERENCE_OFFSET))?;
            let mut samples = [0; 8];
            reader.read_exact(&mut samples)?;
            timestamp = Some(u64::from_le_bytes(samples));
        }
        Ok(true)
    })?;
    if !complete {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many WAV chunks to verify Broadcast WAV timing",
        ));
    }
    Ok(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_already_open_file_projects_even_metadata_before_format() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prefixed.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(-12_000_i16).unwrap();
        writer.write_sample(24_000_i16).unwrap();
        writer.finalize().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.splice(12..12, *b"JUNK\x02\0\0\0AB");
        let size = (bytes.len() as u32 - 8).to_le_bytes();
        bytes[4..8].copy_from_slice(&size);
        std::fs::write(&path, &bytes).unwrap();

        let mut file = File::open(&path).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        let mut reader = open_file_reader(file).unwrap();
        assert_eq!(reader.duration(), 2);
        reader.seek(1).unwrap();
        assert_eq!(reader.samples::<i16>().next().unwrap().unwrap(), 24_000);
        let mut stream = reader.into_inner();
        stream.rewind().unwrap();
        let mut header = [0; 16];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(&header[12..], b"fmt ");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
