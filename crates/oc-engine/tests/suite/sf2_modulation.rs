//! Modulator behaviour: the default vibrato set, bank modulator records and the
//! per-voice LFO. Depths are measured from the rendered audio, never from the
//! engine's own state.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::Sf2Renderer;
use oc_engine::soundfont::SoundFontRenderer;
use sf2_builder::{Font, ModRecord, Sample, destinations, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
/// The fixture's sample is a 440 Hz sine at its root key, so note 60 is 440 Hz.
const SAMPLE_HZ: f64 = 440.0;
/// Skip the attack and the LFO delay before measuring.
const SETTLE_SECONDS: f64 = 0.2;

/// One preset over one looped sine, with optional modulator records on the
/// preset zone.
fn vibrato_font(modulators: Vec<ModRecord>, generators: Vec<(u16, i16)>) -> PathBuf {
    let mut font = Font::new("vibrato");
    font.add_sample(Sample::sine("sine", RATE, NOTE, SAMPLE_HZ, RATE as usize).with_loop(0, RATE));
    font.add_instrument(
        "instrument",
        vec![vec![
            (g::SAMPLE_ID, 0),
            (g::SAMPLE_MODES, 1),
            (g::OVERRIDING_ROOT_KEY, i16::from(NOTE)),
            (g::KEY_RANGE, sf2_builder::range(0, 127) as i16),
        ]],
    );
    font.add_preset("preset", 0, 0, vec![vec![(g::INSTRUMENT, 0)]]);
    if !modulators.is_empty() {
        font.presets[0].zone_modulators = vec![modulators];
    }
    if !generators.is_empty() {
        // A global instrument zone carries the vibrato LFO generators.
        font.instruments[0].zones.insert(0, generators);
    }
    font.write_temp()
}

/// CC1 and channel pressure at their defaults, plus a bank-only CC2 record.
fn explicit_modulators() -> Vec<ModRecord> {
    vec![
        ModRecord::cc(1, destinations::VIB_LFO_TO_PITCH, 50),
        ModRecord::controller(0x0D, destinations::VIB_LFO_TO_PITCH, 50),
        ModRecord::cc(2, destinations::VIB_LFO_TO_PITCH, 150),
    ]
}

fn note_on(time: u64) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOn {
            channel: 0,
            note: NOTE,
            velocity: 100,
        },
        track_index: 0,
    }
}

fn note_off(time: u64) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOff {
            channel: 0,
            note: NOTE,
        },
        track_index: 0,
    }
}

fn controller(time: u64, controller: u8, value: u8) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::ControlChange {
            channel: 0,
            controller,
            value,
        },
        track_index: 0,
    }
}

fn pressure(time: u64, value: u8) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::ChannelPressure {
            channel: 0,
            pressure: value,
        },
        track_index: 0,
    }
}

fn render(font: &Path, events: &[MidiEvent], seconds: f64) -> Vec<f32> {
    let mut renderer = SoundFontRenderer::new(RATE, font).unwrap();
    renderer.render_events(events, seconds)
}

/// Mono view of a note held for `seconds`, after skipping the settle window.
fn held_note(font: &Path, events: &[MidiEvent], duration: f64) -> Vec<f32> {
    let buffer = render(font, events, duration);
    let mono: Vec<f32> = buffer
        .chunks(2)
        .map(|frame| (frame[0] + frame[1]) * 0.5)
        .collect();
    mono[(seconds(SETTLE_SECONDS) as usize)..].to_vec()
}

fn seconds(value: f64) -> u64 {
    (value * f64::from(RATE)) as u64
}

fn depth_cents(font: &Path, events: &[MidiEvent]) -> f64 {
    let samples = held_note(font, events, 1.4);
    sf2_analysis::pitch_depth_cents(&samples, f64::from(RATE))
}

/// Note on at zero, held until the end of the render, with an optional
/// controller event sent just before it.
fn held_events(setup: Option<MidiEvent>) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    if let Some(setup) = setup {
        events.push(setup);
    }
    events.push(note_on(0));
    events.push(note_off(seconds(1.3)));
    events
}

fn fnv1a(samples: &[f32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for sample in samples {
        for byte in sample.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[test]
fn cc1_raises_the_vibrato_depth_monotonically() {
    // The fixture's own records add another 50 cents on top of the default set.
    let font = vibrato_font(explicit_modulators(), vec![]);
    let mut previous = -1.0;
    for cc1 in [0u8, 32, 64, 96, 127] {
        let events = held_events(Some(controller(0, 1, cc1)));
        let depth = depth_cents(&font, &events);
        let expected = 100.0 * f64::from(cc1) / 127.0;
        assert!(
            (depth - expected).abs() < 2.0,
            "CC1 {cc1}: measured {depth:.2} cents, expected {expected:.2}"
        );
        assert!(depth > previous, "CC1 {cc1}: {depth:.2} <= {previous:.2}");
        previous = depth;
    }
}

#[test]
fn channel_pressure_raises_the_vibrato_depth_monotonically() {
    let font = vibrato_font(explicit_modulators(), vec![]);
    let mut previous = -1.0;
    for value in [0u8, 32, 64, 96, 127] {
        let events = held_events(Some(pressure(0, value)));
        let depth = depth_cents(&font, &events);
        let expected = 100.0 * f64::from(value) / 127.0;
        assert!(
            (depth - expected).abs() < 2.0,
            "pressure {value}: measured {depth:.2} cents, expected {expected:.2}"
        );
        assert!(
            depth > previous,
            "pressure {value}: {depth:.2} <= {previous:.2}"
        );
        previous = depth;
    }
}

#[test]
fn the_default_modulators_vibrato_a_bank_without_its_own_records() {
    let font = vibrato_font(Vec::new(), vec![]);
    assert!(
        depth_cents(&font, &held_events(None)) < 0.5,
        "no wheel, no vibrato"
    );
    let depth = depth_cents(&font, &held_events(Some(controller(0, 1, 127))));
    assert!((depth - 50.0).abs() < 2.0, "default set: {depth:.2} cents");
    let depth = depth_cents(&font, &held_events(Some(pressure(0, 127))));
    assert!(
        (depth - 50.0).abs() < 2.0,
        "pressure default: {depth:.2} cents"
    );
}

#[test]
fn bank_modulator_records_are_honoured() {
    // CC2 is not in the default set, so any depth here comes from the bank.
    let font = vibrato_font(explicit_modulators(), vec![]);
    let depth = depth_cents(&font, &held_events(Some(controller(0, 2, 127))));
    assert!(
        (depth - 150.0).abs() < 2.0,
        "bank CC2 record: {depth:.2} cents"
    );
    let half = depth_cents(&font, &held_events(Some(controller(0, 2, 64))));
    assert!((half - 75.0).abs() < 2.0, "bank CC2 at 64: {half:.2} cents");
}

#[test]
fn the_lfo_rate_and_delay_come_from_the_zone_generators() {
    // 1200 timecents is one octave above the format's 8.176 Hz origin.
    let font = vibrato_font(explicit_modulators(), vec![(g::FREQ_VIB_LFO, 1200)]);
    let events = held_events(Some(controller(0, 1, 127)));
    let samples = held_note(&font, &events, 1.4);
    let times = sf2_analysis::rising_crossings(&samples, f64::from(RATE));
    let cents = sf2_analysis::cents_track(&times);
    let crossings = cents
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    // One second of track at 16.35 Hz.
    assert!(
        (15..=18).contains(&crossings),
        "{crossings} cycles in a second"
    );

    // A one-second delay silences the LFO until it elapses.
    let delayed = vibrato_font(explicit_modulators(), vec![(g::DELAY_VIB_LFO, 0)]);
    let buffer = render(&delayed, &held_events(Some(controller(0, 1, 127))), 2.2);
    let mono: Vec<f32> = buffer
        .chunks(2)
        .map(|frame| (frame[0] + frame[1]) * 0.5)
        .collect();
    let early = sf2_analysis::pitch_depth_cents(
        &mono[seconds(0.05) as usize..seconds(0.9) as usize],
        f64::from(RATE),
    );
    let late = sf2_analysis::pitch_depth_cents(&mono[seconds(1.1) as usize..], f64::from(RATE));
    assert!(early < 1.0, "LFO before its delay: {early:.2} cents");
    assert!(late > 40.0, "LFO after its delay: {late:.2} cents");
}

#[test]
fn a_zero_controller_render_is_byte_identical_to_a_bank_without_modulators() {
    // Same sample and envelope; one bank carries modulator records, the other
    // does not. With CC1 and channel pressure at zero nothing modulates, so the
    // two renders must agree to the bit.
    let plain = vibrato_font(Vec::new(), vec![]);
    let modulated = vibrato_font(explicit_modulators(), vec![]);
    let events = held_events(None);
    let plain_render = render(&plain, &events, 1.0);
    let modulated_render = render(&modulated, &events, 1.0);
    assert_eq!(fnv1a(&plain_render), fnv1a(&modulated_render));
    assert!(plain_render == modulated_render, "samples differ");
}

#[test]
fn the_unmodulated_render_hashes_to_its_pinned_value() {
    // Captured with the modulator crate present but inert: a regression guard
    // for the unmodulated path (envelope, pan, gain and sample readout).
    let font = vibrato_font(Vec::new(), vec![]);
    let buffer = render(&font, &held_events(None), 1.0);
    assert_eq!(
        fnv1a(&buffer),
        UNMODULATED_HASH,
        "unmodulated render drifted"
    );
}

const UNMODULATED_HASH: u64 = 4_826_522_779_913_708_261;

#[test]
fn the_same_events_render_byte_identically_twice() {
    let font = vibrato_font(explicit_modulators(), vec![]);
    let events = held_events(Some(controller(0, 1, 96)));
    let first = render(&font, &events, 1.0);
    let second = render(&font, &events, 1.0);
    assert_eq!(fnv1a(&first), fnv1a(&second));
    assert!(first == second);
}

#[test]
fn the_lfo_state_is_per_voice() {
    let font = vibrato_font(explicit_modulators(), vec![]);
    let wheel = controller(0, 1, 127);
    let first_note = seconds(0.6);
    let second_note = seconds(0.87);

    let alone = |start: u64, held: u64| {
        let mut events = vec![wheel.clone()];
        events.push(note_on(start));
        events.push(note_off(start + held));
        render(&font, &events, 1.4)
    };
    let first = alone(0, first_note);
    let second = alone(second_note, seconds(0.5));

    let mut events = vec![wheel];
    events.push(note_on(0));
    events.push(note_off(first_note));
    events.push(note_on(second_note));
    events.push(note_off(second_note + seconds(0.5)));
    let together = render(&font, &events, 1.4);

    // The two-note render is the sum of the notes rendered alone, sample for
    // sample. A shared oscillator would give the second note the phase the
    // first one reached, and the sum would not match.
    let mut expected = first.clone();
    for (slot, value) in expected.iter_mut().zip(&second) {
        *slot += value;
    }
    let worst = together
        .iter()
        .zip(&expected)
        .fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
    assert_eq!(worst, 0.0, "per-voice LFO sum differs by {worst}");
    // Guard against a trivially equal pair: the wheel must actually be moving
    // the pitch.
    let left: Vec<f32> = first.chunks(2).map(|frame| frame[0]).collect();
    let deviated = sf2_analysis::pitch_depth_cents(&left[seconds(0.1) as usize..], f64::from(RATE));
    assert!(deviated > 40.0, "vibrato depth {deviated:.2} cents");
}

#[test]
fn the_engine_adapter_reaches_the_same_depth() {
    // The render paths go through `Sf2Renderer` directly; make sure the adapter
    // and the raw player agree on a modulated render.
    let font = vibrato_font(explicit_modulators(), vec![]);
    let events = held_events(Some(controller(0, 1, 127)));
    let mut player = Sf2Renderer::new(RATE, &font).unwrap();
    let direct = player.render_events(&events, 1.0);
    let adapted = render(&font, &events, 1.0);
    assert!(direct == adapted);
}
