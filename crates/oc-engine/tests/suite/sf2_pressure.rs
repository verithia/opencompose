//! Polyphonic key pressure through the modulator engine.
//!
//! Two notes are held at once over one bank whose preset-zone record routes poly
//! pressure to the filter cutoff. The 3520 Hz partial of each note is measured
//! from the rendered audio, so the test reads what the listener hears rather
//! than the engine's per-voice state.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::Sf2Renderer;
use sf2_builder::{Font, ModRecord, Sample, destinations, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
const OTHER: u8 = 72;
const SAMPLE_HZ: f64 = 440.0;
const HIGH_HZ: f64 = 3520.0;
/// Pressure at full travel opens the cutoff by the format's own 2400 cents.
const PRESSURE_CENTS: i16 = 2400;

fn font() -> PathBuf {
    let mut font = Font::new("pressure");
    font.add_sample(
        Sample::partials("partials", RATE, NOTE, &[SAMPLE_HZ, HIGH_HZ], RATE as usize)
            .with_loop(0, RATE),
    );
    font.add_instrument(
        "instrument",
        vec![vec![
            (g::SAMPLE_ID, 0),
            (g::SAMPLE_MODES, 1),
            (g::OVERRIDING_ROOT_KEY, i16::from(NOTE)),
            (g::KEY_RANGE, sf2_builder::range(0, 127) as i16),
            (g::INITIAL_FILTER_FC, 6000),
        ]],
    );
    font.add_preset("preset", 0, 0, vec![vec![(g::INSTRUMENT, 0)]]);
    font.presets[0].zone_modulators = vec![vec![ModRecord::controller(
        0x0A,
        destinations::INITIAL_FILTER_FC,
        PRESSURE_CENTS,
    )]];
    font.write_temp()
}

fn at(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

fn note_on(time: f64, note: u8) -> MidiEvent {
    MidiEvent {
        time_samples: at(time),
        event: MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        },
        track_index: 0,
    }
}

fn note_off(time: f64, note: u8) -> MidiEvent {
    MidiEvent {
        time_samples: at(time),
        event: MidiMessage::NoteOff { channel: 0, note },
        track_index: 0,
    }
}

fn pressure(time: f64, note: u8, pressure: u8) -> MidiEvent {
    MidiEvent {
        time_samples: at(time),
        event: MidiMessage::PolyPressure {
            channel: 0,
            note,
            pressure,
        },
        track_index: 0,
    }
}

fn render(font: &Path, events: &[MidiEvent], seconds: f64) -> Vec<f32> {
    let mut player = Sf2Renderer::new(RATE, font).unwrap();
    player.render_events(events, seconds)
}

/// Level of one partial in one window of the rendered audio.
fn partial(buffer: &[f32], start: f64, window: f64, hz: f64) -> f64 {
    let mono = sf2_analysis::mono(buffer);
    sf2_analysis::band_level(&mono, f64::from(RATE), start, window, hz)
}

#[test]
fn poly_pressure_moves_only_the_note_it_names() {
    let font = font();
    // Both notes sound from zero; pressure reaches note 60 alone at 0.8 s.
    let touched = render(
        &font,
        &[
            note_on(0.0, NOTE),
            note_on(0.0, OTHER),
            pressure(0.8, NOTE, 127),
            note_off(2.0, NOTE),
            note_off(2.0, OTHER),
        ],
        2.0,
    );
    // Note 60's second partial is 3520 Hz; note 72's is an octave up.
    let before = partial(&touched, 0.4, 0.3, HIGH_HZ);
    let after = partial(&touched, 1.0, 0.3, HIGH_HZ);
    let rise = sf2_analysis::db(after / before);
    println!("note 60 partial rises {rise:.2} dB");

    let other_before = partial(&touched, 0.4, 0.3, 2.0 * HIGH_HZ);
    let other_after = partial(&touched, 1.0, 0.3, 2.0 * HIGH_HZ);
    let other_move = sf2_analysis::db(other_after / other_before);
    println!("note 72 partial moves {other_move:.2} dB");

    assert!(rise > 3.0, "pressure must open note 60: {rise:.2} dB");
    assert!(
        other_move.abs() < 0.1,
        "note 72 must be untouched: {other_move:.3} dB"
    );

    // The mirror image: pressure on note 72 only moves the 7040 Hz partial.
    let mirrored = render(
        &font,
        &[
            note_on(0.0, NOTE),
            note_on(0.0, OTHER),
            pressure(0.8, OTHER, 127),
            note_off(2.0, NOTE),
            note_off(2.0, OTHER),
        ],
        2.0,
    );
    let low_before = partial(&mirrored, 0.4, 0.3, HIGH_HZ);
    let low_after = partial(&mirrored, 1.0, 0.3, HIGH_HZ);
    let low_move = sf2_analysis::db(low_after / low_before);
    let high_before = partial(&mirrored, 0.4, 0.3, 2.0 * HIGH_HZ);
    let high_after = partial(&mirrored, 1.0, 0.3, 2.0 * HIGH_HZ);
    let high_rise = sf2_analysis::db(high_after / high_before);
    println!("mirror: note 60 moves {low_move:.3} dB, note 72 rises {high_rise:.2} dB");
    assert!(
        low_move.abs() < 0.1,
        "note 60 must be untouched: {low_move:.3} dB"
    );
    assert!(
        high_rise > 3.0,
        "pressure must open note 72: {high_rise:.2} dB"
    );
}

#[test]
fn a_new_note_on_clears_the_notes_pressure() {
    // The same two notes in succession; only the first one receives pressure.
    let font = font();
    let events = |pressed: bool| {
        let mut events = vec![note_on(0.0, NOTE)];
        if pressed {
            events.push(pressure(0.5, NOTE, 127));
        }
        events.push(note_off(1.0, NOTE));
        events.push(note_on(1.5, NOTE));
        events.push(note_off(2.5, NOTE));
        events
    };
    let plain = render(&font, &events(false), 2.6);
    let pressed = render(&font, &events(true), 2.6);
    let first_plain = partial(&plain, 0.55, 0.3, HIGH_HZ);
    let first_pressed = partial(&pressed, 0.55, 0.3, HIGH_HZ);
    let second_plain = partial(&plain, 1.7, 0.3, HIGH_HZ);
    let second_pressed = partial(&pressed, 1.7, 0.3, HIGH_HZ);
    let first_move = sf2_analysis::db(first_pressed / first_plain);
    let second_move = sf2_analysis::db(second_pressed / second_plain);
    println!("first note {first_move:.2} dB, retriggered note {second_move:.3} dB");
    assert!(
        first_move > 3.0,
        "pressure moved the first note: {first_move:.2} dB"
    );
    assert!(
        second_move.abs() < 0.05,
        "the retriggered note must start clean: {second_move:.3} dB"
    );
}

#[test]
fn poly_pressure_is_per_channel_and_per_note() {
    // Note 60 on channel 0 receives pressure; the same note held on channel 1
    // must not. Both channels share the preset.
    let font = font();
    let events = vec![
        MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: NOTE,
                velocity: 100,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 1,
                note: OTHER,
                velocity: 100,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: at(0.8),
            event: MidiMessage::PolyPressure {
                channel: 0,
                note: NOTE,
                pressure: 127,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: at(2.0),
            event: MidiMessage::NoteOff {
                channel: 0,
                note: NOTE,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: at(2.0),
            event: MidiMessage::NoteOff {
                channel: 1,
                note: OTHER,
            },
            track_index: 0,
        },
    ];
    let buffer = render(&font, &events, 2.0);
    let before = partial(&buffer, 0.4, 0.3, HIGH_HZ);
    let after = partial(&buffer, 1.0, 0.3, HIGH_HZ);
    let other_before = partial(&buffer, 0.4, 0.3, 2.0 * HIGH_HZ);
    let other_after = partial(&buffer, 1.0, 0.3, 2.0 * HIGH_HZ);
    let rise = sf2_analysis::db(after / before);
    let other_move = sf2_analysis::db(other_after / other_before);
    println!("channel 0 {rise:.2} dB, channel 1 {other_move:.3} dB");
    assert!(rise > 3.0);
    assert!(other_move.abs() < 0.1);
}
