//! Playback tests: pitch, loop modes, drum bank, velocity, release, pan, filter.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::{Sf2Renderer, SoundFont};
use sf2_analysis::{db, dominant_frequency, left, mono, peak, right, rms};
use sf2_builder::{Font, Sample, generators, range};

fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

fn event(time_samples: u64, event: MidiMessage) -> MidiEvent {
    MidiEvent {
        time_samples,
        event,
        track_index: 0,
    }
}

fn render(path: &Path, sample_rate: u32, events: Vec<MidiEvent>, seconds: f64) -> Vec<f32> {
    let mut renderer = Sf2Renderer::new(sample_rate, path).expect("bank loads");
    renderer.enable_drum_channel();
    renderer
        .try_render_events(&events, seconds)
        .expect("render")
}

/// Frequency from positive-going zero crossings over the sounding part.
fn zero_crossing_frequency(samples: &[f32], sample_rate: f64) -> f64 {
    let mut first = None;
    let mut last = None;
    let mut crossings = 0usize;
    for index in 1..samples.len() {
        if samples[index - 1] <= 0.0 && samples[index] > 0.0 {
            if first.is_none() {
                first = Some(index);
            } else {
                last = Some(index);
                crossings += 1;
            }
        }
    }
    match (first, last) {
        (Some(first), Some(last)) if crossings > 0 && last > first => {
            crossings as f64 * sample_rate / (last - first) as f64
        }
        _ => 0.0,
    }
}

/// The fixture's 4096-frame tone measures 261.66 Hz (24.3 cycles) by zero
/// crossings, with root key 60 and a 44.1 kHz bank rate.
const FIXTURE_FREQUENCY: f64 = 261.66;

#[test]
fn fixture_notes_sound_at_the_sample_pitch() {
    let sample_frequency = FIXTURE_FREQUENCY;
    for note in [48u8, 60, 67, 72] {
        let events = vec![
            event(
                0,
                MidiMessage::ProgramChange {
                    channel: 0,
                    program: 0,
                },
            ),
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note,
                    velocity: 100,
                },
            ),
        ];
        let buffer = render(&fixture(), 44_100, events, 0.1);
        let channel = left(&buffer);
        let expected = sample_frequency * 2f64.powf(f64::from(note) / 12.0 - 5.0);
        let measured = zero_crossing_frequency(&channel, f64::from(44_100u32));
        assert!(
            (measured - expected).abs() / expected < 0.01,
            "note {note}: {measured:.1} Hz, expected {expected:.1} Hz"
        );
    }
}

#[test]
fn pitch_survives_a_different_output_rate() {
    let sample_frequency = FIXTURE_FREQUENCY;
    let events = vec![event(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    )];
    let buffer = render(&fixture(), 48_000, events, 0.1);
    let measured = zero_crossing_frequency(&left(&buffer), f64::from(48_000u32));
    assert!(
        (measured - sample_frequency).abs() / sample_frequency < 0.01,
        "{measured:.1} Hz at 48 kHz, expected {sample_frequency:.1} Hz"
    );
}

/// A looping tone: 200-frame loop of an 882 Hz sine at 44.1 kHz.
fn looped_font(mode: i16) -> (PathBuf, f64) {
    let mut font = Font::new("Loop");
    let sample = font.add_sample(Sample::sine("loop", 44_100, 60, 882.0, 200).with_loop(0, 200));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::SAMPLE_MODES, mode),
            (generators::SUSTAIN_VOL_ENV, 0),
            (generators::RELEASE_VOL_ENV, -1200),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let path = font.write_temp();
    (path, 882.0)
}

#[test]
fn a_continuous_loop_keeps_sounding_past_the_sample_end() {
    let (path, frequency) = looped_font(1);
    let events = vec![
        event(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        ),
        event(
            44_100,
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            },
        ),
    ];
    let buffer = render(&path, 44_100, events, 1.2);
    let channel = left(&buffer);
    // The sample is 4.5 ms long; without a loop it would stop there.
    let early = rms(&channel[1_000..5_000]);
    let late = rms(&channel[40_000..44_000]);
    assert!(early > 0.01, "early window {early}");
    assert!(
        late > early * 0.5,
        "loop stopped: early {early}, late {late}"
    );
    // A continuous loop keeps running into the release stage; the envelope
    // fades it, but the sample must not stop looping at the note-off.
    let after_release = rms(&channel[44_540..45_200]);
    assert!(after_release > early * 0.3, "release killed the loop");
    let measured = dominant_frequency(
        &channel[40_000..48_000],
        44_100.0,
        frequency * 0.9,
        frequency * 1.1,
        1.0,
    );
    assert!(
        (measured - frequency).abs() / frequency < 0.01,
        "{measured:.1} Hz"
    );
}

#[test]
fn loop_until_release_runs_to_the_sample_end_after_note_off() {
    let (path, _) = looped_font(3);
    let events = vec![
        event(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        ),
        event(
            4_410,
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            },
        ),
    ];
    let buffer = render(&path, 44_100, events, 0.5);
    let channel = left(&buffer);
    // While the key is down the loop sustains.
    assert!(rms(&channel[1_000..4_000]) > 0.01);
    // After the key is up the sample plays out (200 frames) and then stops.
    assert!(rms(&channel[4_500..5_200]) > 0.005, "tail was cut");
    let silent = rms(&channel[8_000..12_000]);
    assert!(silent < 1e-4, "voice did not stop: {silent}");
}

#[test]
fn drum_channel_note_selects_the_bank_128_kit() {
    let mut font = Font::new("Kit");
    let melodic = font.add_sample(Sample::sine("melodic", 44_100, 60, 220.0, 4096));
    // The kit sample is rooted on its own key so the note plays it at unity.
    let kick = font.add_sample(Sample::sine("kick", 44_100, 36, 880.0, 4096));
    let melodic_instrument = font.add_instrument(
        "melodic",
        vec![vec![
            (generators::SAMPLE_ID, melodic as i16),
            (generators::KEY_RANGE, sf2_builder::range(0, 127) as i16),
        ]],
    );
    let kit = font.add_instrument(
        "kit",
        vec![vec![
            (generators::SAMPLE_ID, kick as i16),
            (generators::KEY_RANGE, sf2_builder::range(36, 36) as i16),
        ]],
    );
    font.add_preset(
        "melodic",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, melodic_instrument as i16)]],
    );
    font.add_preset(
        "kit",
        128,
        0,
        vec![vec![(generators::INSTRUMENT, kit as i16)]],
    );
    let path = font.write_temp();

    let drum = render(
        &path,
        44_100,
        vec![
            event(
                0,
                MidiMessage::ProgramChange {
                    channel: 9,
                    program: 0,
                },
            ),
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 9,
                    note: 36,
                    velocity: 100,
                },
            ),
        ],
        0.2,
    );
    let measured = dominant_frequency(&mono(&drum), 44_100.0, 600.0, 1200.0, 1.0);
    assert!(
        (measured - 880.0).abs() < 20.0,
        "drum note played {measured:.1} Hz"
    );

    // The same key on a melodic channel selects the melodic preset.
    let melodic_note = render(
        &path,
        44_100,
        vec![
            event(
                0,
                MidiMessage::ProgramChange {
                    channel: 0,
                    program: 0,
                },
            ),
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            ),
        ],
        0.2,
    );
    let measured = dominant_frequency(&mono(&melodic_note), 44_100.0, 100.0, 400.0, 1.0);
    assert!(
        (measured - 220.0).abs() < 20.0,
        "melodic note played {measured:.1} Hz"
    );
}

#[test]
fn velocity_uses_the_square_amplitude_law() {
    let (path, _) = looped_font(1);
    let level = |velocity: u8| {
        let buffer = render(
            &path,
            44_100,
            vec![event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity,
                },
            )],
            0.2,
        );
        rms(&left(&buffer)[2_000..7_000])
    };
    let full = level(127);
    let half = level(64);
    let ratio = half / full;
    // (64/127)^2 = 0.254; allow for the measurement window.
    assert!((ratio - 0.254).abs() < 0.02, "velocity ratio {ratio:.3}");
}

#[test]
fn channel_volume_uses_the_square_amplitude_law() {
    let (path, _) = looped_font(1);
    let level = |volume: u8| {
        let buffer = render(
            &path,
            44_100,
            vec![
                event(
                    0,
                    MidiMessage::ControlChange {
                        channel: 0,
                        controller: 7,
                        value: volume,
                    },
                ),
                event(
                    0,
                    MidiMessage::NoteOn {
                        channel: 0,
                        note: 60,
                        velocity: 127,
                    },
                ),
            ],
            0.2,
        );
        rms(&left(&buffer)[2_000..7_000])
    };
    let ratio = level(64) / level(127);
    assert!((ratio - 0.254).abs() < 0.02, "cc7 ratio {ratio:.3}");
}

#[test]
fn notes_start_at_their_event_time() {
    let (path, _) = looped_font(1);
    let buffer = render(
        &path,
        44_100,
        vec![event(
            4_410,
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        )],
        0.3,
    );
    let channel = left(&buffer);
    assert!(rms(&channel[..4_000]) < 1e-6, "note started early");
    assert!(rms(&channel[4_600..8_000]) > 0.01, "note never started");
}

#[test]
fn release_frees_the_voice() {
    let (path, _) = looped_font(1);
    let mut renderer = Sf2Renderer::new(44_100, &path).unwrap();
    renderer
        .send(&MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        })
        .unwrap();
    assert_eq!(renderer.active_voices(), 1);
    renderer.render_samples(1_000);
    renderer
        .send(&MidiMessage::NoteOff {
            channel: 0,
            note: 60,
        })
        .unwrap();
    // One second is far past the 1 ms release.
    renderer.render_samples(44_100);
    assert_eq!(renderer.active_voices(), 0);
}

/// A looping tone whose zone pan is `pan` (in generator units).
fn panned_font(name: &str, pan: Option<i16>) -> PathBuf {
    let mut font = Font::new(name);
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 1024).with_loop(0, 1024));
    let mut zone = vec![
        (generators::SAMPLE_ID, sample as i16),
        (generators::SAMPLE_MODES, 1),
        (generators::SUSTAIN_VOL_ENV, 0),
    ];
    if let Some(pan) = pan {
        zone.push((generators::PAN, pan));
    }
    let instrument = font.add_instrument("inst", vec![zone]);
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    font.write_temp()
}

#[test]
fn the_zone_pan_places_the_note() {
    let path = panned_font("Zone pan", Some(500));
    let buffer = render(
        &path,
        44_100,
        vec![event(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        )],
        0.2,
    );
    let left_level = rms(&left(&buffer)[2_000..7_000]);
    let right_level = rms(&right(&buffer)[2_000..7_000]);
    assert!(right_level > 0.01, "right side silent");
    assert!(
        left_level < 1e-4,
        "hard right leaked into the left side: {left_level}"
    );
}

#[test]
fn cc10_pan_is_applied_on_top_of_the_zone_pan() {
    let path = panned_font("CC10 pan", None);
    let buffer = render(
        &path,
        44_100,
        vec![
            event(
                0,
                MidiMessage::ControlChange {
                    channel: 0,
                    controller: 10,
                    value: 0,
                },
            ),
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            ),
        ],
        0.2,
    );
    let left_level = rms(&left(&buffer)[2_000..7_000]);
    let right_level = rms(&right(&buffer)[2_000..7_000]);
    assert!(left_level > 0.01, "CC10 left did not pan: {left_level}");
    assert!(right_level < 1e-4, "CC10 left leaked right: {right_level}");

    // A centred zone with CC10 127 lands hard right, and the centre is unity.
    let buffer = render(
        &path,
        44_100,
        vec![
            event(
                0,
                MidiMessage::ControlChange {
                    channel: 0,
                    controller: 10,
                    value: 127,
                },
            ),
            event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            ),
        ],
        0.2,
    );
    let left_level = rms(&left(&buffer)[2_000..7_000]);
    let right_level = rms(&right(&buffer)[2_000..7_000]);
    assert!(right_level > 0.01, "CC10 right did not pan");
    // CC10 127 is one step short of the hard right end, so a little leaks.
    assert!(
        left_level < right_level * 0.05,
        "CC10 right leaked left: {left_level} vs {right_level}"
    );
}

#[test]
fn filter_cutoff_removes_high_harmonics() {
    let build = |generators: Vec<(u16, i16)>| {
        let mut font = Font::new("Filter");
        let frames = 1024;
        let points: Vec<i16> = (0..frames)
            .map(|frame| {
                let phase = frame as f32 / frames as f32;
                ((phase * 2.0 - 1.0) * 16000.0) as i16
            })
            .collect();
        font.add_sample(Sample {
            name: "saw".to_string(),
            rate: 44_100,
            root_key: 60,
            points: sf2_builder::Points::I16(points),
            loop_start: 0,
            loop_end: frames as u32,
        });
        let mut zone = vec![
            (generators::SAMPLE_ID, 0),
            (generators::SAMPLE_MODES, 1),
            (generators::SUSTAIN_VOL_ENV, 0),
        ];
        zone.extend(generators);
        let instrument = font.add_instrument("inst", vec![zone]);
        font.add_preset(
            "preset",
            0,
            0,
            vec![vec![(generators::INSTRUMENT, instrument as i16)]],
        );
        font.write_temp()
    };

    let bright = build(vec![]);
    let dark = build(vec![
        (generators::INITIAL_FILTER_FC, 6000),
        (generators::INITIAL_FILTER_Q, 0),
    ]);
    let render_bank = |path: &PathBuf| {
        let buffer = render(
            path,
            44_100,
            vec![event(
                0,
                MidiMessage::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                },
            )],
            0.2,
        );
        let channel = mono(&buffer);
        let window = &channel[2_000..8_000];
        let low = sf2_analysis::goertzel(window, 44_100.0, 110.0);
        let high = sf2_analysis::goertzel(window, 44_100.0, 1_760.0);
        (low, high)
    };
    let (bright_low, bright_high) = render_bank(&bright);
    let (dark_low, dark_high) = render_bank(&dark);
    assert!(
        db(dark_high / bright_high) < -6.0,
        "filter did not cut the 16th harmonic: {:.1} dB",
        db(dark_high / bright_high)
    );
    assert!(
        db(dark_low / bright_low) > -3.0,
        "filter cut the fundamental: {:.1} dB",
        db(dark_low / bright_low)
    );
}

#[test]
fn a_drum_kit_chokes_notes_in_the_same_exclusive_class() {
    let mut font = Font::new("Choke");
    let sample =
        font.add_sample(Sample::sine("hat", 44_100, 60, 1_000.0, 4_096).with_loop(0, 4_096));
    let instrument = font.add_instrument(
        "inst",
        vec![
            vec![
                (generators::SAMPLE_ID, sample as i16),
                (generators::SAMPLE_MODES, 1),
                (generators::KEY_RANGE, sf2_builder::range(42, 42) as i16),
                (generators::EXCLUSIVE_CLASS, 1),
            ],
            vec![
                (generators::SAMPLE_ID, sample as i16),
                (generators::SAMPLE_MODES, 1),
                (generators::KEY_RANGE, sf2_builder::range(46, 46) as i16),
                (generators::EXCLUSIVE_CLASS, 1),
            ],
        ],
    );
    font.add_preset(
        "kit",
        128,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let path = font.write_temp();

    let mut renderer = Sf2Renderer::new(44_100, &path).unwrap();
    renderer.enable_drum_channel();
    renderer
        .send(&MidiMessage::ProgramChange {
            channel: 9,
            program: 0,
        })
        .unwrap();
    renderer
        .send(&MidiMessage::NoteOn {
            channel: 9,
            note: 42,
            velocity: 100,
        })
        .unwrap();
    renderer.render_samples(2_000);
    assert_eq!(renderer.active_voices(), 1);
    renderer
        .send(&MidiMessage::NoteOn {
            channel: 9,
            note: 46,
            velocity: 100,
        })
        .unwrap();
    renderer.render_samples(4_410);
    // The open hat is released and its tail ends; only the new note is left.
    assert_eq!(renderer.active_voices(), 1);
}

#[test]
fn a_full_scale_note_stays_below_clipping() {
    let (path, _) = looped_font(1);
    let events = vec![
        event(
            0,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 7,
                value: 127,
            },
        ),
        event(
            0,
            MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 127,
            },
        ),
    ];
    let buffer = render(&path, 44_100, events, 0.2);
    let peak = peak(&buffer);
    assert!(peak > 0.05, "note too quiet: {peak}");
    assert!(peak < 1.0, "note clipped: {peak}");
}

#[test]
fn the_shared_bank_is_reused_across_renderers() {
    let mut font = Font::new("Shared render");
    font.single_preset(Sample::sine("tone", 44_100, 60, 441.0, 256), 0, 127);
    let path = font.write_temp();
    let first = Sf2Renderer::new(44_100, &path).unwrap();
    let second = Sf2Renderer::new(44_100, &path).unwrap();
    assert!(std::ptr::eq(first.font(), second.font()));
    assert_eq!(first.font().presets.len(), 1);
}

#[test]
#[ignore = "requires OCP_TEST_SOUNDFONT; run explicitly with --ignored"]
fn a_real_general_midi_bank_parses_and_renders() {
    let path =
        PathBuf::from(std::env::var_os("OCP_TEST_SOUNDFONT").expect("set OCP_TEST_SOUNDFONT"));
    let font = SoundFont::load(&path).expect("bank parses");
    assert!(
        font.presets.len() > 8,
        "only {} presets",
        font.presets.len()
    );
    assert!(font.presets.iter().any(|preset| preset.bank == 128));
    let mut renderer = Sf2Renderer::new(48_000, &path).unwrap();
    renderer.enable_drum_channel();
    for preset in [0u8, 4, 24, 32, 40, 48, 61, 73] {
        renderer
            .send(&MidiMessage::ProgramChange {
                channel: 0,
                program: preset,
            })
            .unwrap();
        renderer
            .send(&MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            })
            .unwrap();
        let buffer = renderer.render_samples(4_800);
        assert!(rms(&left(&buffer)) > 1e-4, "program {preset} is silent");
        renderer
            .send(&MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            })
            .unwrap();
        renderer.render_samples(48_000);
    }
}

/// Add one preset on its own sine, at a bank and program of the caller's choice.
fn add_tone(font: &mut Font, name: &str, bank: u16, program: u16, frequency: f64) {
    let sample = font.add_sample(Sample::sine(name, 44_100, 60, frequency, 8_192));
    let instrument = font.add_instrument(
        name,
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
        ]],
    );
    font.add_preset(
        name,
        bank,
        program,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
}

/// The fundamental of a short note rendered after `prefix` MIDI messages.
fn selected_tone(prefix: &[MidiMessage]) -> f64 {
    let mut font = Font::new("Bank select");
    add_tone(&mut font, "bank 0", 0, 0, 441.0);
    add_tone(&mut font, "bank 1", 1, 0, 882.0);
    add_tone(&mut font, "bank 17", 17, 0, 1_323.0);
    add_tone(&mut font, "bank 128", 128, 0, 1_764.0);
    add_tone(&mut font, "bank 129", 129, 0, 2_205.0);
    let path = font.write_temp();

    let mut events: Vec<MidiEvent> = prefix
        .iter()
        .cloned()
        .map(|message| event(0, message))
        .collect();
    events.push(event(
        0,
        MidiMessage::ProgramChange {
            channel: 0,
            program: 0,
        },
    ));
    events.push(event(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    ));
    let buffer = render(&path, 44_100, events, 0.15);
    dominant_frequency(&mono(&buffer), 44_100.0, 300.0, 2_500.0, 1.0)
}

#[test]
fn bank_select_reaches_the_preset_the_channel_asked_for() {
    let cc = |controller: u8, value: u8| MidiMessage::ControlChange {
        channel: 0,
        controller,
        value,
    };
    // Without a bank select the channel plays bank 0.
    assert!((selected_tone(&[]) - 441.0).abs() < 4.0);
    // CC32 is the bank LSB, where banks 1 and 17 live.
    assert!((selected_tone(&[cc(32, 17)]) - 1_323.0).abs() < 4.0);
    assert!((selected_tone(&[cc(32, 1)]) - 882.0).abs() < 4.0);
    // CC0 is the MSB: one step is bank 128.
    assert!((selected_tone(&[cc(0, 1)]) - 1_764.0).abs() < 4.0);
    // Both halves compose, and clearing them returns to bank 0.
    assert!((selected_tone(&[cc(0, 1), cc(32, 1)]) - 2_205.0).abs() < 4.0);
    assert!((selected_tone(&[cc(0, 1), cc(32, 17), cc(0, 0), cc(32, 0)]) - 441.0).abs() < 4.0);
}
