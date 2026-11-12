//! Minimal SoundFont 2 writer for the parser and playback tests.
//!
//! It emits the chunk tree and the `pdta` records the reader consumes, one bag
//! per zone, so a test can pin exactly one generator or modulator at a time.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Generator opcodes used by the tests.
pub mod generators {
    pub const START_ADDRS_OFFSET: u16 = 0;
    pub const END_ADDRS_OFFSET: u16 = 1;
    pub const START_LOOP_ADDRS_OFFSET: u16 = 2;
    pub const END_LOOP_ADDRS_OFFSET: u16 = 3;
    pub const MOD_LFO_TO_PITCH: u16 = 5;
    pub const INITIAL_FILTER_FC: u16 = 8;
    pub const INITIAL_FILTER_Q: u16 = 9;
    pub const VIB_LFO_TO_PITCH: u16 = 6;
    pub const MOD_ENV_TO_PITCH: u16 = 7;
    pub const MOD_LFO_TO_FILTER_FC: u16 = 10;
    pub const MOD_ENV_TO_FILTER_FC: u16 = 11;
    pub const MOD_LFO_TO_VOLUME: u16 = 13;
    pub const CHORUS_SEND: u16 = 15;
    pub const REVERB_SEND: u16 = 16;
    pub const PAN: u16 = 17;
    pub const DELAY_MOD_LFO: u16 = 21;
    pub const FREQ_MOD_LFO: u16 = 22;
    pub const DELAY_MOD_ENV: u16 = 25;
    pub const ATTACK_MOD_ENV: u16 = 26;
    pub const HOLD_MOD_ENV: u16 = 27;
    pub const DECAY_MOD_ENV: u16 = 28;
    pub const SUSTAIN_MOD_ENV: u16 = 29;
    pub const RELEASE_MOD_ENV: u16 = 30;
    pub const KEYNUM_TO_MOD_ENV_HOLD: u16 = 31;
    pub const KEYNUM_TO_MOD_ENV_DECAY: u16 = 32;
    pub const DELAY_VOL_ENV: u16 = 33;
    pub const ATTACK_VOL_ENV: u16 = 34;
    pub const HOLD_VOL_ENV: u16 = 35;
    pub const DECAY_VOL_ENV: u16 = 36;
    pub const SUSTAIN_VOL_ENV: u16 = 37;
    pub const RELEASE_VOL_ENV: u16 = 38;
    pub const KEYNUM_TO_VOL_ENV_HOLD: u16 = 39;
    pub const KEYNUM_TO_VOL_ENV_DECAY: u16 = 40;
    pub const DELAY_VIB_LFO: u16 = 23;
    pub const FREQ_VIB_LFO: u16 = 24;
    pub const INSTRUMENT: u16 = 41;
    pub const KEY_RANGE: u16 = 43;
    pub const VEL_RANGE: u16 = 44;
    pub const INITIAL_ATTENUATION: u16 = 48;
    pub const COARSE_TUNE: u16 = 51;
    pub const FINE_TUNE: u16 = 52;
    pub const KEYNUM: u16 = 46;
    pub const VELOCITY: u16 = 47;
    pub const SAMPLE_ID: u16 = 53;
    pub const SAMPLE_MODES: u16 = 54;
    pub const SCALE_TUNING: u16 = 56;
    pub const EXCLUSIVE_CLASS: u16 = 57;
    pub const OVERRIDING_ROOT_KEY: u16 = 58;
}

/// Modulator destination opcodes.
pub mod destinations {
    pub const MOD_LFO_TO_PITCH: u16 = 5;
    pub const VIB_LFO_TO_PITCH: u16 = 6;
    pub const MOD_ENV_TO_PITCH: u16 = 7;
    pub const INITIAL_FILTER_FC: u16 = 8;
    pub const INITIAL_FILTER_Q: u16 = 9;
    pub const MOD_LFO_TO_FILTER_FC: u16 = 10;
    pub const MOD_ENV_TO_FILTER_FC: u16 = 11;
    pub const MOD_LFO_TO_VOLUME: u16 = 13;
    pub const PAN: u16 = 17;
    pub const ATTACK_MOD_ENV: u16 = 26;
    pub const DECAY_MOD_ENV: u16 = 28;
    pub const SUSTAIN_MOD_ENV: u16 = 29;
    pub const RELEASE_MOD_ENV: u16 = 30;
    pub const ATTACK_VOL_ENV: u16 = 34;
    pub const DECAY_VOL_ENV: u16 = 36;
    pub const RELEASE_VOL_ENV: u16 = 38;
    pub const INITIAL_ATTENUATION: u16 = 48;
}

/// Pack a key or velocity range the way a generator amount stores it.
pub fn range(low: u8, high: u8) -> u16 {
    u16::from_le_bytes([low, high])
}

#[derive(Debug, Clone)]
pub enum Points {
    I16(Vec<i16>),
    /// 24-bit points, stored as the specification's `smpl` + `sm24` pair.
    I24(Vec<i32>),
    /// An Ogg Vorbis stream, stored verbatim in `smpl` (an SF3 sample).
    Ogg {
        bytes: Vec<u8>,
        frames: usize,
    },
}

impl Points {
    pub fn len(&self) -> usize {
        match self {
            Self::I16(points) => points.len(),
            Self::I24(points) => points.len(),
            Self::Ogg { frames, .. } => *frames,
        }
    }

    /// Bytes this sample occupies in `smpl`.
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::I16(points) => points.len() * 2,
            Self::I24(points) => points.len() * 2,
            Self::Ogg { bytes, .. } => bytes.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub name: String,
    pub rate: u32,
    pub root_key: u8,
    pub points: Points,
    /// Loop points in frames; `(0, 0)` disables the loop bounds.
    pub loop_start: u32,
    pub loop_end: u32,
}

impl Sample {
    /// A single sine with `cycles` whole cycles across `frames`, so a loop at
    /// the sample end is phase-continuous.
    pub fn sine(name: &str, rate: u32, root_key: u8, frequency: f64, frames: usize) -> Self {
        let amplitude = 0.5;
        let points = (0..frames)
            .map(|frame| {
                let phase = 2.0 * std::f64::consts::PI * frequency * frame as f64 / f64::from(rate);
                (phase.sin() * amplitude * 32767.0) as i16
            })
            .collect();
        Self {
            name: name.to_string(),
            rate,
            root_key,
            points: Points::I16(points),
            loop_start: 0,
            loop_end: frames as u32,
        }
    }

    /// Equal-amplitude partials, each an integer number of cycles per second,
    /// so the one-second loop is phase-continuous.
    pub fn partials(
        name: &str,
        rate: u32,
        root_key: u8,
        frequencies: &[f64],
        frames: usize,
    ) -> Self {
        let amplitude = 0.5 / frequencies.len().max(1) as f64;
        let points = (0..frames)
            .map(|frame| {
                let t = frame as f64 / f64::from(rate);
                let value: f64 = frequencies
                    .iter()
                    .map(|frequency| (2.0 * std::f64::consts::PI * frequency * t).sin())
                    .sum();
                (value * amplitude * 32767.0) as i16
            })
            .collect();
        Self {
            name: name.to_string(),
            rate,
            root_key,
            points: Points::I16(points),
            loop_start: 0,
            loop_end: frames as u32,
        }
    }

    pub fn with_loop(mut self, start: u32, end: u32) -> Self {
        self.loop_start = start;
        self.loop_end = end;
        self
    }

    /// One sample stored as an Ogg Vorbis stream. `frames` is the decoded
    /// frame count the loop points refer to.
    pub fn ogg(
        name: &str,
        rate: u32,
        root_key: u8,
        frames: usize,
        loop_start: u32,
        loop_end: u32,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            name: name.to_string(),
            rate,
            root_key,
            points: Points::Ogg { bytes, frames },
            loop_start,
            loop_end,
        }
    }
}

/// One `pmod`/`imod` record.
#[derive(Debug, Clone, Copy)]
pub struct ModRecord {
    pub source: u16,
    pub destination: u16,
    pub amount: i16,
    pub amount_source: u16,
    pub transform: u16,
}

impl ModRecord {
    /// A MIDI controller source (the source word's controller flag is bit 7).
    pub fn cc(controller: u8, destination: u16, amount: i16) -> Self {
        Self {
            source: 0x80 | u16::from(controller),
            destination,
            amount,
            amount_source: 0,
            transform: 0,
        }
    }

    /// A general controller source: 1 velocity, 2 key number, 4 channel
    /// pressure, 5 pitch wheel.
    pub fn controller(index: u16, destination: u16, amount: i16) -> Self {
        Self {
            source: index,
            destination,
            amount,
            amount_source: 0,
            transform: 0,
        }
    }

    fn bytes(&self) -> [u8; 10] {
        let mut bytes = [0u8; 10];
        bytes[0..2].copy_from_slice(&self.source.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.destination.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.amount.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.amount_source.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.transform.to_le_bytes());
        bytes
    }
}

#[derive(Debug, Clone)]
pub struct Instrument {
    pub name: String,
    /// Each zone is its generator list; `SAMPLE_ID` must be present.
    pub zones: Vec<Vec<(u16, i16)>>,
    /// Per-zone modulator lists; empty entries are written as a terminal only.
    pub zone_modulators: Vec<Vec<ModRecord>>,
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    pub bank: u16,
    pub program: u16,
    /// Each zone is its generator list; `INSTRUMENT` must be present.
    pub zones: Vec<Vec<(u16, i16)>>,
    /// Per-zone modulator lists; empty entries are written as a terminal only.
    pub zone_modulators: Vec<Vec<ModRecord>>,
}

#[derive(Debug, Clone, Default)]
pub struct Font {
    pub name: String,
    pub samples: Vec<Sample>,
    pub instruments: Vec<Instrument>,
    pub presets: Vec<Preset>,
    /// Extra bytes appended to the `phdr` chunk to exercise record padding.
    pub phdr_padding: usize,
    /// Write `shdr` offsets as byte offsets with loop points relative to the
    /// sample start, the way an SF3 bank stores them.
    pub compressed: bool,
}

impl Font {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    pub fn add_sample(&mut self, sample: Sample) -> usize {
        self.samples.push(sample);
        self.samples.len() - 1
    }

    pub fn add_instrument(&mut self, name: &str, zones: Vec<Vec<(u16, i16)>>) -> usize {
        self.instruments.push(Instrument {
            name: name.to_string(),
            zones,
            zone_modulators: Vec::new(),
        });
        self.instruments.len() - 1
    }

    pub fn add_preset(&mut self, name: &str, bank: u16, program: u16, zones: Vec<Vec<(u16, i16)>>) {
        self.presets.push(Preset {
            name: name.to_string(),
            bank,
            program,
            zones,
            zone_modulators: Vec::new(),
        });
    }

    /// One preset over one instrument over one sample, no extra generators.
    pub fn single_preset(&mut self, sample: Sample, key_low: u8, key_high: u8) {
        let sample_index = self.add_sample(sample);
        let instrument = self.add_instrument(
            "Instrument",
            vec![vec![
                (generators::SAMPLE_ID, sample_index as i16),
                (generators::KEY_RANGE, range(key_low, key_high) as i16),
            ]],
        );
        self.add_preset(
            "Preset",
            0,
            0,
            vec![vec![(generators::INSTRUMENT, instrument as i16)]],
        );
    }

    pub fn bytes(&self) -> Vec<u8> {
        let smpl = self.smpl_chunk();
        let sm24 = self.sm24_chunk();

        let mut sdta = Vec::new();
        sdta.extend_from_slice(&chunk(b"smpl", &smpl));
        if let Some(sm24) = &sm24 {
            sdta.extend_from_slice(&chunk(b"sm24", sm24));
        }
        let sdta = list(b"sdta", &sdta);

        let pdta = list(b"pdta", &self.pdta_chunks());
        let mut info = Vec::new();
        info.extend_from_slice(&chunk(b"ifil", &[2, 0, 1, 0]));
        let mut name = self.name.as_bytes().to_vec();
        // RIFF strings are even-length; strict readers reject an odd INAM.
        if name.len() & 1 == 1 {
            name.push(0);
        }
        info.extend_from_slice(&chunk(b"INAM", &name));
        let info = list(b"INFO", &info);

        let mut body = Vec::new();
        body.extend_from_slice(&info);
        body.extend_from_slice(&sdta);
        body.extend_from_slice(&pdta);

        let mut bytes = Vec::with_capacity(body.len() + 12);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        bytes.extend_from_slice(b"sfbk");
        bytes.extend_from_slice(&body);
        bytes
    }

    pub fn write(&self, path: &Path) -> PathBuf {
        std::fs::write(path, self.bytes()).expect("write test font");
        path.to_path_buf()
    }

    /// Write to a uniquely named file under the test root.
    pub fn write_temp(&self) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let name = format!("oc-test-{}-{nanos}-{count}.sf2", std::process::id());
        self.write(&oc_test_support::temp_path("oc-test", &name))
    }

    fn smpl_chunk(&self) -> Vec<u8> {
        let len: usize = self
            .samples
            .iter()
            .map(|sample| sample.points.encoded_len())
            .sum();
        let mut bytes = Vec::with_capacity(len);
        for sample in &self.samples {
            match &sample.points {
                Points::I16(points) => {
                    for point in points {
                        bytes.extend_from_slice(&point.to_le_bytes());
                    }
                }
                Points::I24(points) => {
                    // High 16 bits here, low byte in `sm24`.
                    for point in points {
                        bytes.extend_from_slice(&((point >> 8) as i16).to_le_bytes());
                    }
                }
                Points::Ogg { bytes: stream, .. } => bytes.extend_from_slice(stream),
            }
        }
        bytes
    }

    fn sm24_chunk(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut any = false;
        for sample in &self.samples {
            match &sample.points {
                Points::I16(points) => bytes.extend(std::iter::repeat_n(0u8, points.len())),
                Points::I24(points) => {
                    any = true;
                    for point in points {
                        bytes.push((point & 0xff) as u8);
                    }
                }
                Points::Ogg { .. } => {}
            }
        }
        any.then_some(bytes)
    }

    fn pdta_chunks(&self) -> Vec<u8> {
        let mut pbag = Vec::new();
        let mut pgen = Vec::new();
        let mut pmod = Vec::new();
        let mut preset_bags = Vec::with_capacity(self.presets.len());
        for preset in &self.presets {
            preset_bags.push((pbag.len() / 4) as u16);
            for (zone_index, zone) in preset.zones.iter().enumerate() {
                pbag.extend_from_slice(&bag_record(
                    (pgen.len() / 4) as u16,
                    (pmod.len() / 10) as u16,
                ));
                for (opcode, amount) in ordered_zone(zone) {
                    pgen.extend_from_slice(&generator_record(opcode, amount));
                }
                push_modulator_list(&mut pmod, preset.zone_modulators.get(zone_index));
            }
        }
        let terminal_preset_bag = (pbag.len() / 4) as u16;
        pbag.extend_from_slice(&bag_record(
            (pgen.len() / 4) as u16,
            (pmod.len() / 10) as u16,
        ));

        let mut phdr = Vec::new();
        for (preset, bag) in self.presets.iter().zip(&preset_bags) {
            phdr.extend_from_slice(&preset_record(
                &preset.name,
                preset.program,
                preset.bank,
                *bag,
            ));
        }
        phdr.extend_from_slice(&preset_record("EOP", 0, 0, terminal_preset_bag));
        phdr.extend_from_slice(&vec![0u8; self.phdr_padding]);

        let mut ibag = Vec::new();
        let mut igen = Vec::new();
        let mut imod = Vec::new();
        let mut instrument_bags = Vec::with_capacity(self.instruments.len());
        for instrument in &self.instruments {
            instrument_bags.push((ibag.len() / 4) as u16);
            for (zone_index, zone) in instrument.zones.iter().enumerate() {
                ibag.extend_from_slice(&bag_record(
                    (igen.len() / 4) as u16,
                    (imod.len() / 10) as u16,
                ));
                for (opcode, amount) in ordered_zone(zone) {
                    igen.extend_from_slice(&generator_record(opcode, amount));
                }
                push_modulator_list(&mut imod, instrument.zone_modulators.get(zone_index));
            }
        }
        let terminal_instrument_bag = (ibag.len() / 4) as u16;
        ibag.extend_from_slice(&bag_record(
            (igen.len() / 4) as u16,
            (imod.len() / 10) as u16,
        ));

        let mut inst = Vec::new();
        for (instrument, bag) in self.instruments.iter().zip(&instrument_bags) {
            inst.extend_from_slice(&instrument_record(&instrument.name, *bag));
        }
        inst.extend_from_slice(&instrument_record("EOI", terminal_instrument_bag));

        let mut shdr = Vec::new();
        let mut start = 0u32;
        for sample in &self.samples {
            let frames = sample.points.len() as u32;
            // An SF3 header offsets samples by bytes and its loop points are
            // relative to the sample start; an SF2 header uses frames.
            let span = if self.compressed {
                sample.points.encoded_len() as u32
            } else {
                frames
            };
            let end = start + span;
            let (loop_start, loop_end) = if sample.loop_end == 0 {
                if self.compressed {
                    (0, 0)
                } else {
                    (start, start)
                }
            } else if self.compressed {
                (sample.loop_start, sample.loop_end)
            } else {
                (start + sample.loop_start, start + sample.loop_end)
            };
            shdr.extend_from_slice(&sample_record(
                &sample.name,
                start,
                end,
                loop_start,
                loop_end,
                sample.rate,
                sample.root_key,
            ));
            start = end;
        }
        shdr.extend_from_slice(&sample_record("EOS", 0, 0, 0, 0, 0, 0));

        let mut chunks = Vec::new();
        chunks.extend_from_slice(&chunk(b"phdr", &phdr));
        chunks.extend_from_slice(&chunk(b"pbag", &pbag));
        chunks.extend_from_slice(&chunk(b"pmod", &pmod));
        chunks.extend_from_slice(&chunk(b"pgen", &pgen));
        chunks.extend_from_slice(&chunk(b"inst", &inst));
        chunks.extend_from_slice(&chunk(b"ibag", &ibag));
        chunks.extend_from_slice(&chunk(b"imod", &imod));
        chunks.extend_from_slice(&chunk(b"igen", &igen));
        chunks.extend_from_slice(&chunk(b"shdr", &shdr));
        chunks
    }
}

fn name_record(name: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; 20];
    let name = name.as_bytes();
    bytes[..name.len().min(20)].copy_from_slice(&name[..name.len().min(20)]);
    bytes
}

fn preset_record(name: &str, program: u16, bank: u16, bag: u16) -> Vec<u8> {
    let mut bytes = name_record(name);
    bytes.extend_from_slice(&program.to_le_bytes());
    bytes.extend_from_slice(&bank.to_le_bytes());
    bytes.extend_from_slice(&bag.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(bytes.len(), 38);
    bytes
}

fn instrument_record(name: &str, bag: u16) -> Vec<u8> {
    let mut bytes = name_record(name);
    bytes.extend_from_slice(&bag.to_le_bytes());
    debug_assert_eq!(bytes.len(), 22);
    bytes
}

#[allow(clippy::too_many_arguments)]
fn sample_record(
    name: &str,
    start: u32,
    end: u32,
    loop_start: u32,
    loop_end: u32,
    rate: u32,
    root_key: u8,
) -> Vec<u8> {
    let mut bytes = name_record(name);
    bytes.extend_from_slice(&start.to_le_bytes());
    bytes.extend_from_slice(&end.to_le_bytes());
    bytes.extend_from_slice(&loop_start.to_le_bytes());
    bytes.extend_from_slice(&loop_end.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.push(root_key);
    bytes.push(0);
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    debug_assert_eq!(bytes.len(), 46);
    bytes
}

/// A zone's generators with the identity generator last, as the specification
/// requires; strict readers discard everything after `sampleID`/`instrument`.
fn ordered_zone(zone: &[(u16, i16)]) -> Vec<(u16, i16)> {
    let identity = [generators::SAMPLE_ID, generators::INSTRUMENT];
    let mut ordered: Vec<(u16, i16)> = zone
        .iter()
        .copied()
        .filter(|(opcode, _)| !identity.contains(opcode))
        .collect();
    ordered.extend(
        zone.iter()
            .copied()
            .filter(|(opcode, _)| identity.contains(opcode)),
    );
    ordered
}

fn bag_record(generator_index: u16, modulator_index: u16) -> Vec<u8> {
    let mut bytes = generator_index.to_le_bytes().to_vec();
    bytes.extend_from_slice(&modulator_index.to_le_bytes());
    bytes
}

/// A zone's modulator list followed by its terminal record.
fn push_modulator_list(records: &mut Vec<u8>, modulators: Option<&Vec<ModRecord>>) {
    for record in modulators.map(Vec::as_slice).unwrap_or_default() {
        records.extend_from_slice(&record.bytes());
    }
    records.extend_from_slice(&[0u8; 10]);
}

fn generator_record(opcode: u16, amount: i16) -> Vec<u8> {
    let mut bytes = opcode.to_le_bytes().to_vec();
    bytes.extend_from_slice(&amount.to_le_bytes());
    bytes
}

fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len() + 9);
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    if payload.len() & 1 == 1 {
        bytes.push(0);
    }
    bytes
}

fn list(form: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut body = form.to_vec();
    body.extend_from_slice(payload);
    chunk(b"LIST", &body)
}
