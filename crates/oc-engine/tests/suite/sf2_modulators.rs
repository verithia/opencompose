//! Modulator records that reach the filter Q, the envelope times,
//! `initialAttenuation` and `pan`.
//!
//! Every assertion reads the rendered buffers: a level ratio, a sample-identical
//! pair, a pitch reading or the left/right balance. Nothing is read from the
//! engine's state, so a record that parses but never reaches the voice fails
//! here.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::Sf2Renderer;
use sf2_builder::{Font, ModRecord, Sample, destinations, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
/// The fixture's sample is a 440 Hz sine at its root key.
const SAMPLE_HZ: f64 = 440.0;

/// One preset over one looped sine, optional preset-level generators and
/// optional modulator records on its zone.
fn font(records: Vec<ModRecord>, preset_generators: Vec<(u16, i16)>) -> PathBuf {
    let mut font = Font::new("modulators");
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
    let mut zone = vec![(g::INSTRUMENT, 0)];
    zone.extend(preset_generators);
    font.add_preset("preset", 0, 0, vec![zone]);
    if !records.is_empty() {
        font.presets[0].zone_modulators = vec![records];
    }
    font.write_temp()
}

/// `initialAttenuation` from velocity, negative unipolar: full velocity adds
/// nothing and zero velocity adds the whole amount.
fn velocity_attenuation(amount: i16) -> ModRecord {
    ModRecord {
        source: 0x0102,
        destination: destinations::INITIAL_ATTENUATION,
        amount,
        amount_source: 0,
        transform: 0,
    }
}

fn at(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

/// The cutoff that sits exactly on the fixture's 440 Hz sine, so a resonance
/// change shows up as a level change instead of a spectral-centroid shift.
fn resonant_cutoff() -> i16 {
    (1200.0 * (SAMPLE_HZ / 8.176).log2()).round() as i16
}

fn note_on(velocity: u8) -> MidiEvent {
    MidiEvent {
        time_samples: 0,
        event: MidiMessage::NoteOn {
            channel: 0,
            note: NOTE,
            velocity,
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

/// A note held from frame zero, with any controllers sent before it.
fn held(velocity: u8, controllers: &[(u8, u8)], hold: f64) -> Vec<MidiEvent> {
    let mut events: Vec<MidiEvent> = controllers
        .iter()
        .map(|(controller, value)| MidiEvent {
            time_samples: 0,
            event: MidiMessage::ControlChange {
                channel: 0,
                controller: *controller,
                value: *value,
            },
            track_index: 0,
        })
        .collect();
    events.push(note_on(velocity));
    events.push(note_off(at(hold)));
    events
}

fn render(font: &Path, events: &[MidiEvent], seconds: f64) -> Vec<f32> {
    let mut player = Sf2Renderer::new(RATE, font).unwrap();
    player.render_events(events, seconds)
}

fn mono(buffer: &[f32]) -> Vec<f32> {
    sf2_analysis::mono(buffer)
}

fn level_db(buffer: &[f32], start: f64, end: f64) -> f64 {
    let mono = mono(buffer);
    sf2_analysis::db(sf2_analysis::rms(
        &mono[at(start) as usize..at(end) as usize],
    ))
}

fn channel_db(buffer: &[f32], left: bool, start: f64, end: f64) -> f64 {
    let channel = if left {
        sf2_analysis::left(buffer)
    } else {
        sf2_analysis::right(buffer)
    };
    sf2_analysis::db(sf2_analysis::rms(
        &channel[at(start) as usize..at(end) as usize],
    ))
}

#[test]
fn a_velocity_attenuation_record_follows_its_curve_and_leaves_full_velocity_alone() {
    let plain = font(Vec::new(), Vec::new());
    let shaped = font(vec![velocity_attenuation(400)], Vec::new());

    // At full velocity the record contributes nothing, so the two renders are
    // sample-identical: a record that only carries its own curve cannot move a
    // bank that plays every note at 127.
    let full_plain = render(&plain, &held(127, &[], 0.5), 0.6);
    let full_shaped = render(&shaped, &held(127, &[], 0.5), 0.6);
    assert_eq!(full_plain, full_shaped);

    // At velocity 64 a 400 cB negative-unipolar record adds
    // 400 * (1 - 64/127) = 198.4 cB = 19.84 dB of attenuation.
    let soft_plain = render(&plain, &held(64, &[], 0.5), 0.6);
    let soft_shaped = render(&shaped, &held(64, &[], 0.5), 0.6);
    let depth = level_db(&soft_plain, 0.1, 0.45) - level_db(&soft_shaped, 0.1, 0.45);
    let expected = 400.0 * (1.0 - 64.0 / 127.0) / 10.0;
    assert!(
        (depth - expected).abs() < 0.1,
        "attenuation {depth} dB, expected {expected}"
    );
}

#[test]
fn a_cc_attenuation_record_reaches_a_sounding_note() {
    let shaped = font(
        vec![ModRecord::cc(2, destinations::INITIAL_ATTENUATION, 200)],
        Vec::new(),
    );

    // CC2 = 0 contributes nothing; CC2 = 127 contributes the full 200 cB.
    let open = render(&shaped, &held(100, &[(2, 0)], 0.8), 0.9);
    let closed = render(&shaped, &held(100, &[(2, 127)], 0.8), 0.9);
    let depth = level_db(&open, 0.15, 0.7) - level_db(&closed, 0.15, 0.7);
    assert!((depth - 20.0).abs() < 0.1, "depth {depth} dB");

    // A controller that arrives mid-note moves the voice that is already
    // sounding, not only the next note-on.
    let events = [
        controller(0, 2, 0),
        note_on(100),
        controller(at(0.35), 2, 127),
        note_off(at(0.8)),
    ];
    let buffer = render(&shaped, &events, 0.9);
    let before = level_db(&buffer, 0.15, 0.3);
    let after = level_db(&buffer, 0.45, 0.7);
    assert!(
        (before - after - 20.0).abs() < 0.5,
        "mid-note move {before} -> {after} dB"
    );
}

#[test]
fn a_pan_record_moves_the_image_and_sums_with_the_zone_pan() {
    let shaped = font(vec![ModRecord::cc(2, destinations::PAN, 250)], Vec::new());

    // CC2 = 0 leaves the image centred: the two channels are equal.
    let centre = render(&shaped, &held(100, &[(2, 0)], 0.5), 0.6);
    let centre_balance =
        channel_db(&centre, false, 0.1, 0.45) - channel_db(&centre, true, 0.1, 0.45);
    assert!(
        centre_balance.abs() < 0.05,
        "centre balance {centre_balance} dB"
    );

    // CC2 = 127 adds 250 generator units, i.e. pan +0.5. The constant-power
    // law puts the right side at tan(3 pi / 8) = 1 + sqrt(2) over the left.
    let panned = render(&shaped, &held(100, &[(2, 127)], 0.5), 0.6);
    let balance = channel_db(&panned, false, 0.1, 0.45) - channel_db(&panned, true, 0.1, 0.45);
    let expected = 20.0 * (1.0 + 2.0f64.sqrt()).log10();
    assert!(
        (balance - expected).abs() < 0.05,
        "panned balance {balance} dB, expected {expected}"
    );

    // A zone that already sits at pan 250 plus the record at CC2 = 127 reaches
    // the hard-right end, so the left channel is silent.
    let summed = font(
        vec![ModRecord::cc(2, destinations::PAN, 250)],
        vec![(g::PAN, 250)],
    );
    let hard = render(&summed, &held(100, &[(2, 127)], 0.5), 0.6);
    let left = channel_db(&hard, true, 0.1, 0.45);
    assert!(left < -100.0, "left channel {left} dBFS");
}

#[test]
fn a_zero_amount_record_leaves_the_render_untouched() {
    // Both destinations are parsed, but each amount is zero, so the voice's
    // extra attenuation is exactly unity and its pan sum is zero. The render
    // must equal the record-free one sample for sample; this is the same
    // guard the pinned hashes in `sf2_sends` and `sf2_modulation` carry.
    let plain = font(Vec::new(), Vec::new());
    let zero = font(
        vec![
            velocity_attenuation(0),
            ModRecord::cc(2, destinations::PAN, 0),
        ],
        Vec::new(),
    );
    let a = render(&plain, &held(80, &[(2, 90)], 0.5), 0.6);
    let b = render(&zero, &held(80, &[(2, 90)], 0.5), 0.6);
    assert_eq!(a, b);
}

/// A velocity-sourced record with the positive unipolar mapping: zero at zero
/// velocity and the whole amount at full velocity.
fn velocity_record(destination: u16, amount: i16) -> ModRecord {
    ModRecord {
        source: 0x0002,
        destination,
        amount,
        amount_source: 0,
        transform: 0,
    }
}

#[test]
fn a_velocity_q_record_sharpens_the_resonant_peak_at_note_on() {
    // A low-pass passes a tone at its own cutoff at exactly its Q. The zone's
    // generator leaves Q at 1, so a 300 cB record (Q = 10^1.5 = 31.6) lifts the
    // 440 Hz sine by 20 log10(31.6) = 30 dB.
    let cutoff = resonant_cutoff();
    let plain = font(Vec::new(), vec![(g::INITIAL_FILTER_FC, cutoff)]);
    let shaped = font(
        vec![velocity_record(destinations::INITIAL_FILTER_Q, 300)],
        vec![(g::INITIAL_FILTER_FC, cutoff)],
    );
    let plain = render(&plain, &held(127, &[], 0.5), 0.6);
    let shaped = render(&shaped, &held(127, &[], 0.5), 0.6);
    let lift = level_db(&shaped, 0.1, 0.45) - level_db(&plain, 0.1, 0.45);
    assert!((lift - 30.0).abs() < 0.5, "resonant lift {lift} dB");
}

#[test]
fn a_q_record_is_read_at_note_on_and_not_refreshed_mid_note() {
    let cutoff = resonant_cutoff();
    let shaped = font(
        vec![ModRecord::cc(2, destinations::INITIAL_FILTER_Q, 300)],
        vec![(g::INITIAL_FILTER_FC, cutoff)],
    );

    // CC2 = 127 at note-on builds the sharp biquad; CC2 = 0 leaves Q at 1.
    let opened = render(&shaped, &held(127, &[(2, 127)], 0.5), 0.6);
    let quiet = render(&shaped, &held(127, &[(2, 0)], 0.5), 0.6);
    let full = level_db(&opened, 0.1, 0.45) - level_db(&quiet, 0.1, 0.45);
    assert!((full - 30.0).abs() < 0.5, "note-on Q move {full} dB");

    // The same controller arriving mid-note cannot re-aim a biquad that was
    // built with the value it read at note-on, so the level stays put.
    let events = [
        controller(0, 2, 0),
        note_on(127),
        controller(at(0.25), 2, 127),
        note_off(at(0.55)),
    ];
    let buffer = render(&shaped, &events, 0.65);
    let before = level_db(&buffer, 0.05, 0.2);
    let after = level_db(&buffer, 0.35, 0.5);
    assert!(
        (before - after).abs() < 0.05,
        "mid-note Q {before} -> {after} dB"
    );
}

#[test]
fn a_velocity_volume_envelope_time_record_stretches_the_attack_at_note_on() {
    // The generator's 0 timecents is a one-second linear attack; a +1200
    // timecent record doubles it. At half a second the ramps sit at 0.5 and
    // 0.25, so the level difference is 20 log10(2) = 6.02 dB, and both attacks
    // are finished before the late window.
    let plain = font(Vec::new(), vec![(g::ATTACK_VOL_ENV, 0)]);
    let shaped = font(
        vec![velocity_record(destinations::ATTACK_VOL_ENV, 1200)],
        vec![(g::ATTACK_VOL_ENV, 0)],
    );
    let plain = render(&plain, &held(127, &[], 2.4), 2.5);
    let shaped = render(&shaped, &held(127, &[], 2.4), 2.5);
    let early = level_db(&plain, 0.4, 0.6) - level_db(&shaped, 0.4, 0.6);
    assert!((early - 6.02).abs() < 0.2, "attack stretch {early} dB");
    let late = level_db(&plain, 2.1, 2.3) - level_db(&shaped, 2.1, 2.3);
    assert!(late.abs() < 0.1, "settled levels differ by {late} dB");
}

#[test]
fn a_sustain_mod_env_record_holds_the_modulation_envelope_at_note_on() {
    // The modulation envelope feeds a 1200-cent pitch depth, so its sustain
    // level is a pitch reading. The generator holds full scale; a 250-unit
    // record attenuates the sustain by 250 * 0.096 = 24 dB, leaving
    // 1200 * 10^(-24/20) = 75.7 cents.
    let generators = vec![
        (g::MOD_ENV_TO_PITCH, 1200),
        (g::ATTACK_MOD_ENV, 0),
        (g::DECAY_MOD_ENV, -12000),
        (g::SUSTAIN_MOD_ENV, 0),
    ];
    let plain = font(Vec::new(), generators.clone());
    let shaped = font(
        vec![ModRecord::cc(2, destinations::SUSTAIN_MOD_ENV, 250)],
        generators,
    );

    let cents = |font: &Path, cc: u8| {
        let buffer = render(font, &held(127, &[(2, cc)], 1.6), 1.7);
        sf2_analysis::pitch_cents_at(&mono(&buffer), f64::from(RATE), 1.2, 0.2, SAMPLE_HZ)
    };
    let open = cents(&plain, 0);
    let held_low = cents(&shaped, 127);
    assert!(
        (open - 1200.0).abs() < 5.0,
        "modulation envelope {open} cents"
    );
    let expected = 1200.0 * 10f64.powf(-24.0 / 20.0);
    assert!(
        (held_low - expected).abs() < 5.0,
        "sustained pitch {held_low} cents, expected {expected}"
    );
}
