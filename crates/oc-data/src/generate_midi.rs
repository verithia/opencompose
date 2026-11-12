//! Synthetic MIDI generator
//!
//! Generates musically valid MIDI files from music theory rules:
//! - Common chord progressions (I-IV-V-I, ii-V-I, I-vi-IV-V, etc.)
//! - Melodies following scales and chord tones
//! - Bass lines (root notes, walking bass)
//! - Drum patterns (rock, pop, jazz, electronic)
//!
//! Used to bootstrap test data when real MIDI files are not available.

use std::path::Path;

/// Simple MIDI file writer (Standard MIDI Format 1)
struct MidiWriter {
    tracks: Vec<Vec<u8>>,
    ticks_per_beat: u16,
}

/// A note event to be written
struct NoteEvent {
    tick: u32,
    channel: u8,
    note: u8,
    velocity: u8,
    duration_ticks: u32,
}

impl MidiWriter {
    fn new(ticks_per_beat: u16) -> Self {
        Self {
            tracks: Vec::new(),
            ticks_per_beat,
        }
    }

    fn add_track(&mut self, events: &[NoteEvent], program: u8, channel: u8) {
        let mut track_data = Vec::new();

        // Program change at tick 0
        write_vlq(&mut track_data, 0);
        track_data.push(0xC0 | (channel & 0x0F));
        track_data.push(program);

        // Sort events by tick
        let mut sorted_events: Vec<&NoteEvent> = events.iter().collect();
        sorted_events.sort_by_key(|e| e.tick);

        // Convert to delta-time note on/off pairs
        let mut all_msgs: Vec<(u32, u8, u8, u8)> = Vec::new(); // (tick, status, data1, data2)
        for ev in &sorted_events {
            all_msgs.push((ev.tick, 0x90 | (ev.channel & 0x0F), ev.note, ev.velocity));
            all_msgs.push((
                ev.tick + ev.duration_ticks,
                0x80 | (ev.channel & 0x0F),
                ev.note,
                0,
            ));
        }
        all_msgs.sort_by_key(|m| m.0);

        let mut last_tick: u32 = 0;
        for (tick, status, d1, d2) in &all_msgs {
            let delta = tick.saturating_sub(last_tick);
            write_vlq(&mut track_data, delta);
            track_data.push(*status);
            track_data.push(*d1);
            track_data.push(*d2);
            last_tick = *tick;
        }

        // End of track
        write_vlq(&mut track_data, 0);
        track_data.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        self.tracks.push(track_data);
    }

    fn add_tempo_track(&mut self, tempo_bpm: u32, time_sig_num: u8, time_sig_den: u8) {
        let mut track_data = Vec::new();

        // Tempo meta event
        let us_per_beat = 60_000_000 / tempo_bpm;
        write_vlq(&mut track_data, 0);
        track_data.extend_from_slice(&[0xFF, 0x51, 0x03]);
        track_data.push(((us_per_beat >> 16) & 0xFF) as u8);
        track_data.push(((us_per_beat >> 8) & 0xFF) as u8);
        track_data.push((us_per_beat & 0xFF) as u8);

        // Time signature meta event
        let den_pow = match time_sig_den {
            2 => 1,
            4 => 2,
            8 => 3,
            16 => 4,
            _ => 2,
        };
        write_vlq(&mut track_data, 0);
        track_data.extend_from_slice(&[0xFF, 0x58, 0x04, time_sig_num, den_pow, 24, 8]);

        // End of track
        write_vlq(&mut track_data, 0);
        track_data.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        self.tracks.insert(0, track_data);
    }

    fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;

        // Header chunk
        file.write_all(b"MThd")?;
        file.write_all(&[0, 0, 0, 6])?; // chunk length
        file.write_all(&[0, 1])?; // format 1
        file.write_all(&(self.tracks.len() as u16).to_be_bytes())?;
        file.write_all(&self.ticks_per_beat.to_be_bytes())?;

        // Track chunks
        for track in &self.tracks {
            file.write_all(b"MTrk")?;
            file.write_all(&(track.len() as u32).to_be_bytes())?;
            file.write_all(track)?;
        }

        Ok(())
    }
}

fn write_vlq(buf: &mut Vec<u8>, mut value: u32) {
    if value == 0 {
        buf.push(0);
        return;
    }
    let mut bytes = Vec::new();
    while value > 0 {
        bytes.push((value & 0x7F) as u8);
        value >>= 7;
    }
    bytes.reverse();
    for (i, b) in bytes.iter().enumerate() {
        if i < bytes.len() - 1 {
            buf.push(b | 0x80);
        } else {
            buf.push(*b);
        }
    }
}

// ============================================================================
// Music theory primitives
// ============================================================================

/// Chord quality (semitone intervals from root)
const MAJOR: [u8; 3] = [0, 4, 7];
const MINOR: [u8; 3] = [0, 3, 7];
#[allow(dead_code)]
const DIM: [u8; 3] = [0, 3, 6];
#[allow(dead_code)]
const DOM7: [u8; 4] = [0, 4, 7, 10];

/// Common chord progressions (scale degrees, 0-indexed)
const PROGRESSIONS: &[&[u8]] = &[
    &[0, 3, 4, 0], // I-IV-V-I
    &[0, 5, 3, 4], // I-vi-IV-V (pop)
    &[0, 4, 5, 3], // I-V-vi-IV (axis)
    &[1, 4, 0, 0], // ii-V-I-I (jazz)
    &[0, 3, 0, 4], // I-IV-I-V (blues-ish)
    &[0, 2, 3, 4], // I-iii-IV-V
    &[5, 3, 0, 4], // vi-IV-I-V
    &[0, 6, 3, 4], // I-viidim-IV-V
];

/// Major scale intervals
const MAJOR_SCALE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
/// Minor scale intervals
const MINOR_SCALE: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];

/// Drum patterns (kick, snare, hihat positions in 16th notes over 1 bar)
const DRUM_ROCK: &[(u8, &[u8])] = &[
    (36, &[0, 8]),                      // kick on 1, 3
    (38, &[4, 12]),                     // snare on 2, 4
    (42, &[0, 2, 4, 6, 8, 10, 12, 14]), // hihat on every 8th
];

const DRUM_POP: &[(u8, &[u8])] = &[
    (36, &[0, 6, 8]), // kick
    (38, &[4, 12]),   // snare
    (42, &[0, 2, 4, 6, 8, 10, 12, 14]),
];

const DRUM_ELECTRONIC: &[(u8, &[u8])] = &[
    (36, &[0, 3, 8, 11]), // kick (four-on-floor-ish)
    (38, &[4, 12]),       // snare
    (42, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]), // hihat 16ths
];

const DRUM_JAZZ: &[(u8, &[u8])] = &[
    (36, &[0, 10]),                 // kick sparse
    (38, &[7, 15]),                 // snare ghost notes
    (51, &[0, 3, 6, 8, 9, 12, 15]), // ride swing pattern
];

// ============================================================================
// Simple deterministic RNG (no dependencies needed)
// ============================================================================

struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(1),
        }
    }

    fn next_u32(&mut self) -> u32 {
        // xorshift64
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        (self.state >> 32) as u32
    }

    fn range(&mut self, min: u32, max: u32) -> u32 {
        if max <= min {
            return min;
        }
        min + (self.next_u32() % (max - min))
    }

    fn choose<'a, T>(&mut self, slice: &'a [T]) -> &'a T {
        let idx = self.range(0, slice.len() as u32) as usize;
        &slice[idx]
    }

    fn chance(&mut self, probability: f32) -> bool {
        (self.next_u32() as f32 / u32::MAX as f32) < probability
    }
}

// ============================================================================
// Generator
// ============================================================================

/// Configuration for a generated piece
struct PieceConfig {
    key: u8, // root note (0=C, 1=C#, ... 11=B)
    is_minor: bool,
    tempo: u32,
    time_sig_num: u8,
    time_sig_den: u8,
    bars: u32,
    has_melody: bool,
    has_chords: bool,
    has_bass: bool,
    has_drums: bool,
    melody_program: u8,
    chord_program: u8,
    bass_program: u8,
}

fn generate_piece(config: &PieceConfig, rng: &mut SimpleRng) -> MidiWriter {
    let tpb: u16 = 480;
    let ticks_per_bar = tpb as u32 * 4 * config.time_sig_num as u32 / config.time_sig_den as u32;
    let sixteenth = tpb as u32 / 4;

    let mut midi = MidiWriter::new(tpb);

    let scale = if config.is_minor {
        &MINOR_SCALE
    } else {
        &MAJOR_SCALE
    };
    let progression = rng.choose(PROGRESSIONS);

    // Generate chords
    if config.has_chords {
        let mut events = Vec::new();
        for bar in 0..config.bars {
            let degree = progression[(bar as usize) % progression.len()];
            let root = config.key + scale[degree as usize % 7];
            let chord = if config.is_minor && (degree == 0 || degree == 3 || degree == 4) {
                &MINOR[..]
            } else {
                &MAJOR[..]
            };
            let velocity = rng.range(60, 90) as u8;
            let duration = ticks_per_bar - sixteenth; // hold almost full bar

            for &interval in chord {
                events.push(NoteEvent {
                    tick: bar * ticks_per_bar,
                    channel: 0,
                    note: 48 + root + interval, // octave 3-4
                    velocity,
                    duration_ticks: duration,
                });
            }
        }
        midi.add_track(&events, config.chord_program, 0);
    }

    // Generate melody
    if config.has_melody {
        let mut events = Vec::new();
        let mut current_note: i8 = (config.key + 60) as i8; // start at middle octave

        for bar in 0..config.bars {
            let notes_in_bar = rng.range(4, 9); // 4-8 notes per bar
            let beat_division = ticks_per_bar / notes_in_bar;

            for beat in 0..notes_in_bar {
                // Move by step or skip (scale-wise)
                let step: i8 = match rng.range(0, 10) {
                    0..=3 => 1,  // step up
                    4..=6 => -1, // step down
                    7..=8 => 2,  // skip up
                    _ => -2,     // skip down
                };

                let scale_idx = ((current_note - config.key as i8).rem_euclid(12)) as usize;
                let nearest_scale = scale
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, s)| (*s as i8 - scale_idx as i8).unsigned_abs())
                    .map(|(i, _)| i)
                    .unwrap_or(0);

                let new_idx = (nearest_scale as i8 + step).clamp(0, 6) as usize;
                current_note = config.key as i8 + scale[new_idx] as i8 + 60;
                current_note = current_note.clamp(48, 84); // keep in range

                let velocity = rng.range(64, 100) as u8;
                let duration = beat_division * rng.range(60, 95) / 100; // slight variation

                // Occasional rest
                if !rng.chance(0.15) {
                    events.push(NoteEvent {
                        tick: bar * ticks_per_bar + beat * beat_division,
                        channel: 1,
                        note: current_note as u8,
                        velocity,
                        duration_ticks: duration,
                    });
                }
            }
        }
        midi.add_track(&events, config.melody_program, 1);
    }

    // Generate bass
    if config.has_bass {
        let mut events = Vec::new();
        for bar in 0..config.bars {
            let degree = progression[(bar as usize) % progression.len()];
            let root = config.key + scale[degree as usize % 7];

            // Simple bass: root on 1, fifth on 3 (or walking)
            let bass_note = 36 + root; // octave 2
            let velocity = rng.range(70, 100) as u8;

            events.push(NoteEvent {
                tick: bar * ticks_per_bar,
                channel: 2,
                note: bass_note,
                velocity,
                duration_ticks: ticks_per_bar / 2,
            });

            // Sometimes add a fifth
            if rng.chance(0.6) {
                events.push(NoteEvent {
                    tick: bar * ticks_per_bar + ticks_per_bar / 2,
                    channel: 2,
                    note: bass_note + 7,
                    velocity: velocity - 10,
                    duration_ticks: ticks_per_bar / 2,
                });
            }
        }
        midi.add_track(&events, config.bass_program, 2);
    }

    // Generate drums
    if config.has_drums {
        let mut events = Vec::new();
        let pattern = match rng.range(0, 4) {
            0 => DRUM_ROCK,
            1 => DRUM_POP,
            2 => DRUM_ELECTRONIC,
            _ => DRUM_JAZZ,
        };

        for bar in 0..config.bars {
            for &(note, positions) in pattern {
                for &pos in positions {
                    let velocity = rng.range(60, 120) as u8;
                    events.push(NoteEvent {
                        tick: bar * ticks_per_bar + pos as u32 * sixteenth,
                        channel: 9, // drum channel
                        note,
                        velocity,
                        duration_ticks: sixteenth,
                    });
                }
            }
        }
        midi.add_track(&events, 0, 9);
    }

    // Add tempo track
    midi.add_tempo_track(config.tempo, config.time_sig_num, config.time_sig_den);
    midi
}

// ============================================================================
// Public API
// ============================================================================

/// Generate `count` synthetic MIDI files in the given directory.
/// Returns the number of files successfully generated.
pub fn generate_dataset(output_dir: &Path, count: u32, base_seed: u64) -> u32 {
    let _ = std::fs::create_dir_all(output_dir);

    let keys: &[u8] = &[0, 2, 4, 5, 7, 9, 11]; // C, D, E, F, G, A, B
    let tempos: &[u32] = &[72, 80, 90, 100, 110, 120, 128, 140, 160];
    let melody_programs: &[u8] = &[0, 4, 24, 25, 40, 56, 65, 73, 80]; // piano, ep, guitar, violin, trumpet, sax, flute, synth
    let chord_programs: &[u8] = &[0, 4, 24, 48, 89]; // piano, ep, guitar, strings, pad
    let bass_programs: &[u8] = &[32, 33, 34, 38]; // acoustic/electric/fretless/synth bass

    let mut generated = 0;

    for i in 0..count {
        let mut rng = SimpleRng::new(base_seed + i as u64);

        let config = PieceConfig {
            key: *rng.choose(keys),
            is_minor: rng.chance(0.4),
            tempo: *rng.choose(tempos),
            time_sig_num: if rng.chance(0.85) { 4 } else { 3 },
            time_sig_den: 4,
            bars: rng.range(8, 32),
            has_melody: rng.chance(0.8),
            has_chords: rng.chance(0.7),
            has_bass: rng.chance(0.7),
            has_drums: rng.chance(0.6),
            melody_program: *rng.choose(melody_programs),
            chord_program: *rng.choose(chord_programs),
            bass_program: *rng.choose(bass_programs),
        };

        let midi = generate_piece(&config, &mut rng);
        let filename = format!("synth_{:04}.mid", i);
        let path = output_dir.join(&filename);

        match midi.write_to_file(&path) {
            Ok(()) => generated += 1,
            Err(e) => eprintln!("Failed to write {}: {}", filename, e),
        }
    }

    generated
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_generate_single() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_output");
        let count = generate_dataset(&dir, 5, 42);
        assert_eq!(count, 5);

        // Verify files exist and are valid MIDI (start with MThd)
        for i in 0..5 {
            let path = dir.join(format!("synth_{:04}.mid", i));
            assert!(path.exists());
            let data = std::fs::read(&path).unwrap();
            assert_eq!(&data[0..4], b"MThd");
            assert!(data.len() > 100); // non-trivial content
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
