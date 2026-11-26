//! Turn WAVE cue points into ordinary, editable sampler regions.

use serde::Serialize;
use std::fmt::Write as _;
use std::path::Path;

const MAX_CUES: u16 = 129; // MIDI keys plus an optional end marker.
/// How a mapped slice responds to note-off.
#[derive(Clone, Copy, Debug)]
pub enum SlicePlayback {
    OneShot,
    Gated,
}

/// One key and its half-open sample-frame range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WaveSlice {
    pub cue_id: u32,
    pub key: u8,
    pub start_frame: u32,
    pub end_frame: u32,
}

/// A mapping from increasing cue positions to consecutive MIDI notes.
#[derive(Debug, Serialize)]
pub struct WaveSliceMap {
    pub sample_rate: u32,
    pub source_frames: u32,
    pub slices: Vec<WaveSlice>,
}

impl WaveSliceMap {
    /// Read metadata without decoding the sample data.
    pub fn from_wave(path: &Path, first_key: u8) -> Result<Self, String> {
        let read = oc_audio::wav::read_wave_cues(path, MAX_CUES)
            .and_then(|wave| Self::map(wave, first_key));
        read.map_err(|error| format!("{}: {error}", path.display()))
    }

    fn map(wave: oc_audio::wav::WaveCues, first_key: u8) -> Result<Self, String> {
        let mut cues = wave.cues;
        cues.sort_unstable_by_key(|cue| cue.frame);
        if cues.last().is_some_and(|cue| cue.frame > wave.frames) {
            return Err("WAVE cue lies after the audio ends".into());
        }
        if cues.windows(2).any(|pair| pair[0].frame == pair[1].frame) {
            return Err("multiple WAVE cues identify the same sample frame".into());
        }
        if cues.last().is_some_and(|cue| cue.frame == wave.frames) {
            cues.pop();
        }
        if cues.is_empty() {
            return Err("no playable WAVE cue points found".into());
        }
        if usize::from(first_key) + cues.len() > 128 {
            return Err("WAVE slices do not fit the MIDI key range".into());
        }
        Ok(Self {
            sample_rate: wave.sample_rate,
            source_frames: wave.frames,
            slices: cues
                .iter()
                .enumerate()
                .map(|(index, cue)| WaveSlice {
                    cue_id: cue.id,
                    key: first_key + index as u8,
                    start_frame: cue.frame,
                    end_frame: cues.get(index + 1).map_or(wave.frames, |next| next.frame),
                })
                .collect(),
        })
    }

    /// Write a sampler patch that stays editable and refers to one original WAVE file.
    pub fn to_sfz(
        &self,
        sample_reference: &str,
        playback: SlicePlayback,
    ) -> Result<String, String> {
        if sample_reference.is_empty()
            || sample_reference.trim() != sample_reference
            || sample_reference.chars().any(char::is_control)
            || sample_reference.contains('\"') && sample_reference.contains('\'')
        {
            return Err("WAVE reference cannot be represented as an SFZ sample path".into());
        }
        let quote = if sample_reference.contains('\"') {
            '\''
        } else {
            '\"'
        };
        let mode = match playback {
            SlicePlayback::OneShot => "one_shot",
            SlicePlayback::Gated => "no_loop",
        };
        let mut text = format!("<group> loop_mode={mode}\n");
        for slice in &self.slices {
            writeln!(
                text,
                "<region> sample={quote}{sample_reference}{quote} key={} offset={} end={}",
                slice.key, slice.start_frame, slice.end_frame
            )
            .expect("writing to a string");
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::{Sampler, patch_from_sfz_text};

    fn chunk(bytes: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
        bytes.extend(name);
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(data);
        if data.len() % 2 == 1 {
            bytes.push(0);
        }
    }

    fn wave_with_cues(cues: &[(u32, u32)], declared_count: u32) -> Vec<u8> {
        let mut wave = b"WAVE".to_vec();
        // Mono PCM16 at 8 kHz.
        chunk(
            &mut wave,
            b"fmt ",
            &[1, 0, 1, 0, 64, 31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0],
        );
        let pcm: Vec<u8> = [4096_i16; 4]
            .into_iter()
            .chain([-16384_i16; 4])
            .flat_map(i16::to_le_bytes)
            .collect();
        chunk(&mut wave, b"data", &pcm);
        chunk(&mut wave, b"JUNK", &[0]);
        let mut cue_bytes = declared_count.to_le_bytes().to_vec();
        for &(cue_id, offset) in cues {
            cue_bytes.extend(cue_id.to_le_bytes());
            cue_bytes.extend(offset.to_le_bytes());
            cue_bytes.extend(b"data");
            cue_bytes.extend([0; 8]);
            cue_bytes.extend(offset.to_le_bytes());
        }
        chunk(&mut wave, b"cue ", &cue_bytes);
        let mut riff = Vec::new();
        chunk(&mut riff, b"RIFF", &wave);
        riff
    }

    #[test]
    fn cues_after_pcm_map_to_editable_regions_and_play_without_next_slice_bleed() {
        let cues = [(25, 4), (40, 8), (12, 0)];
        let bytes = wave_with_cues(&cues, 3);
        let root = oc_test_support::temp_dir("wav-slices", "boundaries");
        let sample_name = "chopped 音.wav";
        let path = root.join(sample_name);
        std::fs::write(&path, bytes).unwrap();
        let map = WaveSliceMap::from_wave(&path, 70).unwrap();
        assert_eq!(map.sample_rate, 8000);
        assert_eq!(map.source_frames, 8);
        assert_eq!(
            map.slices,
            [
                WaveSlice {
                    cue_id: 12,
                    key: 70,
                    start_frame: 0,
                    end_frame: 4
                },
                WaveSlice {
                    cue_id: 25,
                    key: 71,
                    start_frame: 4,
                    end_frame: 8
                },
            ]
        );
        let sfz = map.to_sfz(sample_name, SlicePlayback::OneShot).unwrap();
        let patch = patch_from_sfz_text(&sfz, &[root]);
        assert!(patch.diagnostics.is_empty(), "{:?}", patch.diagnostics);
        assert_eq!(patch.regions.len(), 2);
        let mut sampler = Sampler::from_patch(&patch).unwrap();
        sampler.note_on(0, 70, 127);
        assert_eq!(sampler.next_stereo(16000), [0.125; 2]);
        sampler.note_off(0, 70);
        assert_eq!(
            sampler.render_interleaved(8, 16000),
            [
                0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125, 0.125,
                0.125, 0.125, 0.0, 0.0,
            ]
        );
        sampler.note_on(0, 71, 127);
        assert_eq!(sampler.next_stereo(16000), [-0.5; 2]);
        let gated = map.to_sfz(sample_name, SlicePlayback::Gated).unwrap();
        assert!(gated.starts_with("<group> loop_mode=no_loop\n"));
        assert!(map.to_sfz("bad\npath.wav", SlicePlayback::Gated).is_err());
    }

    #[test]
    fn truncated_out_of_bounds_overlapping_and_oversized_tables_are_refused() {
        let root = oc_test_support::temp_dir("wav-slices", "malformed");
        let path = root.join("input.wav");
        for (cues, count, key) in [
            (vec![(1, 9)], 1, 60),
            (vec![(1, 2), (2, 2)], 2, 60),
            (vec![(1, 2), (2, 3)], 2, 127),
            (vec![(1, 2)], 129, 60),
            (vec![(1, 2)], u32::MAX, 60),
        ] {
            std::fs::write(&path, wave_with_cues(&cues, count)).unwrap();
            assert!(WaveSliceMap::from_wave(&path, key).is_err());
        }
        let mut truncated = wave_with_cues(&[(1, 0)], 1);
        truncated.pop();
        std::fs::write(&path, truncated).unwrap();
        assert!(WaveSliceMap::from_wave(&path, 60).is_err());
    }
}
