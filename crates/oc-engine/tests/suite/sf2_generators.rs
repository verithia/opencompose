//! The velocity-to-cutoff default modulator and the fixed `keynum`/`velocity`
//! generators.
//!
//! Brightness is measured as the magnitude-weighted spectral centroid of the
//! rendered audio; envelope times are read off the level track. Nothing is read
//! from the engine's state.

use crate::sf2_analysis;
use crate::sf2_builder;
use crate::sf2_spectral;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::Sf2Renderer;
use sf2_builder::{Font, ModRecord, Sample, destinations, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
/// The fixtures' sample is a 440 Hz sine at its root key.
const SAMPLE_HZ: f64 = 440.0;
/// The probe partial, well above the cutoffs used here.
const HIGH_HZ: f64 = 3520.0;

/// One preset over one instrument over one sample. `generators` lands in a
/// global instrument zone, `modulators` on the preset zone.
fn font(
    sample: Sample,
    generators: Vec<(u16, i16)>,
    modulators: Vec<ModRecord>,
    preset_generators: Vec<(u16, i16)>,
) -> PathBuf {
    let mut font = Font::new("generators");
    font.add_sample(sample);
    font.add_instrument(
        "instrument",
        vec![vec![
            (g::SAMPLE_ID, 0),
            (g::SAMPLE_MODES, 1),
            (g::OVERRIDING_ROOT_KEY, i16::from(NOTE)),
            (g::KEY_RANGE, sf2_builder::range(0, 127) as i16),
        ]],
    );
    if !generators.is_empty() {
        font.instruments[0].zones.insert(0, generators);
    }
    let mut zone = vec![(g::INSTRUMENT, 0)];
    zone.extend(preset_generators);
    font.add_preset("preset", 0, 0, vec![zone]);
    if !modulators.is_empty() {
        font.presets[0].zone_modulators = vec![modulators];
    }
    font.write_temp()
}

fn sine_font(
    generators: Vec<(u16, i16)>,
    modulators: Vec<ModRecord>,
    preset_generators: Vec<(u16, i16)>,
) -> PathBuf {
    font(
        Sample::sine("sine", RATE, NOTE, SAMPLE_HZ, RATE as usize).with_loop(0, RATE),
        generators,
        modulators,
        preset_generators,
    )
}

fn partials_font(generators: Vec<(u16, i16)>, modulators: Vec<ModRecord>) -> PathBuf {
    font(
        Sample::partials("partials", RATE, NOTE, &[SAMPLE_HZ, HIGH_HZ], RATE as usize)
            .with_loop(0, RATE),
        generators,
        modulators,
        Vec::new(),
    )
}

fn at(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

fn render(font: &Path, note: u8, velocity: u8, hold: f64, total: f64) -> Vec<f32> {
    let events = vec![
        MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: at(hold),
            event: MidiMessage::NoteOff { channel: 0, note },
            track_index: 0,
        },
    ];
    let mut player = Sf2Renderer::new(RATE, font).unwrap();
    player.render_events(&events, total)
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

/// Centroid of the steady part of a held note, 100 Hz .. 12 kHz.
fn centroid(buffer: &[f32]) -> f64 {
    let mono = sf2_analysis::mono(buffer);
    sf2_spectral::spectral_centroid(
        &mono[at(0.3) as usize..at(1.1) as usize],
        f64::from(RATE),
        100.0,
        12_000.0,
    )
}

#[test]
fn a_soft_note_is_darker_than_a_hard_one() {
    // The format's default modulator closes the cutoff by 2400 cents of concave
    // curve, so a note at velocity 20 sits about 800 cents below one at 110.
    // The bank declares a 9500-cent (1174 Hz) cutoff; the 3520 Hz partial moves
    // from about -33 dB at velocity 20 to -15 dB at 110.
    let font = partials_font(vec![(g::INITIAL_FILTER_FC, 9500)], Vec::new());
    let soft = centroid(&render(&font, NOTE, 20, 1.2, 1.2));
    let hard = centroid(&render(&font, NOTE, 110, 1.2, 1.2));
    println!("velocity 20: {soft:.1} Hz; velocity 110: {hard:.1} Hz");
    assert!(
        hard > soft + 150.0,
        "a hard note must be brighter: {soft:.1} vs {hard:.1} Hz"
    );
    // Velocity 127 is the format's anchor: no offset at all.
    let full = centroid(&render(&font, NOTE, 127, 1.2, 1.2));
    println!("velocity 127: {full:.1} Hz");
    assert!(
        (full - hard).abs() < 60.0,
        "velocities 110 and 127 are 62 cents apart: {hard:.1} vs {full:.1} Hz"
    );
}

#[test]
fn a_zone_without_a_filter_renders_the_same_at_velocity_64() {
    // The pinned hash was captured before the velocity modulator existed: a
    // zone whose cutoff is at the bypass value never builds a biquad, so the
    // default case is bit-identical.
    let font = sine_font(Vec::new(), Vec::new(), Vec::new());
    let buffer = render(&font, NOTE, 64, 1.0, 1.0);
    assert_eq!(
        fnv1a(&buffer),
        FILTERLESS_VELOCITY_64_HASH,
        "the filterless path drifted"
    );
}

const FILTERLESS_VELOCITY_64_HASH: u64 = 0xb6c7_a927_36ca_2d09;

#[test]
fn full_velocity_keeps_the_cutoff_the_bank_declares() {
    // Velocity 127 is the curve's zero, so a filtered zone renders exactly as
    // it did before the modulator existed (hash captured pre-change), while a
    // mid velocity is already darker.
    let font = sine_font(vec![(g::INITIAL_FILTER_FC, 6000)], Vec::new(), Vec::new());
    let full = render(&font, NOTE, 127, 1.0, 1.0);
    assert_eq!(
        fnv1a(&full),
        FILTERED_VELOCITY_127_HASH,
        "velocity 127 moved"
    );
    let mid = render(&font, NOTE, 100, 1.0, 1.0);
    assert_ne!(
        fnv1a(&mid),
        fnv1a(&full),
        "velocity 100 must close the filter"
    );
}

const FILTERED_VELOCITY_127_HASH: u64 = 0x89a0_ea1c_1331_5905;

#[test]
fn key_number_scaling_lengthens_the_low_key_envelope() {
    // 100 timecents per key measured from key 60: C2 holds four times as long
    // as the 1 s nominal hold and C6 a quarter, a 16x ratio over 48 keys.
    let font = sine_font(
        vec![
            (g::ATTACK_VOL_ENV, -12000),
            (g::HOLD_VOL_ENV, 0),
            (g::DECAY_VOL_ENV, -3986),
            (g::SUSTAIN_VOL_ENV, 1440),
            (g::KEYNUM_TO_VOL_ENV_HOLD, 100),
        ],
        Vec::new(),
        Vec::new(),
    );
    let hold = |note: u8| {
        let buffer = render(&font, note, 100, 5.0, 5.0);
        let mono = sf2_analysis::mono(&buffer);
        sf2_spectral::level_drop_seconds(&mono, f64::from(RATE), 0.02, 6.0)
    };
    let low = hold(36);
    let high = hold(84);
    println!(
        "C2 holds {low:.3} s, C6 {high:.3} s, ratio {:.2}",
        low / high
    );
    assert!(low > high, "C2 holds {low:.3} s, C6 {high:.3} s");
    assert!(
        (low / high - 16.0).abs() < 1.5,
        "48 keys at 100 timecents is 16x, measured {:.2}",
        low / high
    );
    assert!((low - 4.0).abs() < 0.1, "C2 hold {low:.3} s");
    assert!((high - 0.25).abs() < 0.03, "C6 hold {high:.3} s");
}

#[test]
fn the_keynum_generator_fixes_the_key() {
    // `keynum` 60 makes every key synthesize at key 60: same pitch, same
    // envelope scaling, same key-number modulator source. The bank records a
    // key-number-to-cutoff modulator, so all three paths are in the render.
    let generators = vec![
        (g::KEYNUM, 60),
        (g::ATTACK_VOL_ENV, -12000),
        (g::HOLD_VOL_ENV, 0),
        (g::DECAY_VOL_ENV, -3986),
        (g::SUSTAIN_VOL_ENV, 1440),
        (g::KEYNUM_TO_VOL_ENV_HOLD, 100),
    ];
    let modulators = vec![ModRecord::controller(
        0x03,
        destinations::INITIAL_FILTER_FC,
        1200,
    )];
    let font = partials_font(generators, modulators.clone());
    let c2 = render(&font, 36, 100, 2.0, 2.0);
    let c6 = render(&font, 84, 100, 2.0, 2.0);
    assert_eq!(
        fnv1a(&c2),
        fnv1a(&c6),
        "a fixed key makes C2 and C6 identical"
    );

    // The fixed key is the pitch reference: key 60 over a root-60 sample.
    let mono = sf2_analysis::mono(&c2);
    let frequency = sf2_analysis::dominant_frequency(&mono, f64::from(RATE), 300.0, 700.0, 1.0);
    assert!((frequency - SAMPLE_HZ).abs() < 2.0, "{frequency:.1} Hz");

    // Without the generator the two keys differ in pitch and envelope time.
    let plain = partials_font(
        vec![
            (g::ATTACK_VOL_ENV, -12000),
            (g::HOLD_VOL_ENV, 0),
            (g::DECAY_VOL_ENV, -3986),
            (g::SUSTAIN_VOL_ENV, 1440),
            (g::KEYNUM_TO_VOL_ENV_HOLD, 100),
        ],
        modulators,
    );
    assert_ne!(
        fnv1a(&render(&plain, 36, 100, 2.0, 2.0)),
        fnv1a(&render(&plain, 84, 100, 2.0, 2.0))
    );
}

#[test]
fn a_preset_zone_cannot_fix_the_key() {
    // The format defines `keynum` at the instrument level; a preset value is
    // ignored rather than added.
    let font = sine_font(Vec::new(), Vec::new(), vec![(g::KEYNUM, 60)]);
    let c2 = render(&font, 36, 100, 1.0, 1.0);
    let c6 = render(&font, 84, 100, 1.0, 1.0);
    assert_ne!(fnv1a(&c2), fnv1a(&c6));
}

#[test]
fn the_velocity_generator_fixes_the_level_and_the_filter() {
    // A velocity-to-cutoff record and the format's velocity-to-level law both
    // read the fixed value, so the played velocity cannot reach the voice.
    let modulators = vec![ModRecord::controller(
        0x02,
        destinations::INITIAL_FILTER_FC,
        1200,
    )];
    let fixed = partials_font(
        vec![(g::INITIAL_FILTER_FC, 6000), (g::VELOCITY, 100)],
        modulators.clone(),
    );
    let quiet = render(&fixed, NOTE, 20, 1.0, 1.0);
    let loud = render(&fixed, NOTE, 110, 1.0, 1.0);
    assert_eq!(
        fnv1a(&quiet),
        fnv1a(&loud),
        "a fixed velocity makes both renders equal"
    );

    let plain = partials_font(vec![(g::INITIAL_FILTER_FC, 6000)], modulators);
    assert_ne!(
        fnv1a(&render(&plain, NOTE, 20, 1.0, 1.0)),
        fnv1a(&render(&plain, NOTE, 110, 1.0, 1.0)),
        "the played velocity must matter without the generator"
    );
}

#[test]
fn the_generators_are_inert_when_they_are_absent() {
    // A bank that declares neither generator renders exactly as a bank that
    // declares them at their -1 sentinel.
    let plain = sine_font(Vec::new(), Vec::new(), Vec::new());
    let sentinel = sine_font(
        vec![(g::KEYNUM, -1), (g::VELOCITY, -1)],
        Vec::new(),
        Vec::new(),
    );
    let first = render(&plain, 72, 90, 1.0, 1.0);
    let second = render(&sentinel, 72, 90, 1.0, 1.0);
    assert_eq!(fnv1a(&first), fnv1a(&second));
}
