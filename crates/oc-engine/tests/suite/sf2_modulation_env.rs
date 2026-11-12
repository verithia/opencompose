//! The mod LFO, the modulation envelope and the filter cutoff they move.
//!
//! Every figure here is measured from the rendered audio: pitch from rising
//! zero crossings, volume from short-window RMS, brightness from the level of
//! a 3520 Hz partial next to a 440 Hz one. The expected values come from the
//! format's units (cents, centibels, absolute cents for the cutoff) and from
//! the analytic two-pole response, never from the engine's own state.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::soundfont::SoundFontRenderer;
use sf2_builder::{Font, ModRecord, Sample, destinations, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
/// The fixtures' sample is a 440 Hz sine at its root key.
const SAMPLE_HZ: f64 = 440.0;
/// The probe partial's frequency, well above every cutoff used here.
const HIGH_HZ: f64 = 3520.0;

fn font_with(sample: Sample, generators: Vec<(u16, i16)>, modulators: Vec<ModRecord>) -> PathBuf {
    let mut font = Font::new("modulation");
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
    font.add_preset("preset", 0, 0, vec![vec![(g::INSTRUMENT, 0)]]);
    if !generators.is_empty() {
        font.instruments[0].zones.insert(0, generators);
    }
    if !modulators.is_empty() {
        font.presets[0].zone_modulators = vec![modulators];
    }
    font.write_temp()
}

fn sine_font(generators: Vec<(u16, i16)>, modulators: Vec<ModRecord>) -> PathBuf {
    font_with(
        Sample::sine("sine", RATE, NOTE, SAMPLE_HZ, RATE as usize).with_loop(0, RATE),
        generators,
        modulators,
    )
}

fn partials_font(generators: Vec<(u16, i16)>) -> PathBuf {
    font_with(
        Sample::partials("partials", RATE, NOTE, &[SAMPLE_HZ, HIGH_HZ], RATE as usize),
        generators,
        Vec::new(),
    )
}

fn note_on(time: u64, note: u8, velocity: u8) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity,
        },
        track_index: 0,
    }
}

fn note_off(time: u64, note: u8) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOff { channel: 0, note },
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

fn at(seconds: f64) -> u64 {
    (seconds * f64::from(RATE)).round() as u64
}

fn render(font: &Path, events: &[MidiEvent], seconds: f64) -> Vec<f32> {
    let mut renderer = SoundFontRenderer::new(RATE, font).unwrap();
    renderer.render_events(events, seconds)
}

/// One held note with an optional controller event before it.
fn held(seconds: f64, velocity: u8, setup: Option<MidiEvent>) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    if let Some(setup) = setup {
        events.push(setup);
    }
    events.push(note_on(0, NOTE, velocity));
    events.push(note_off(at(seconds), NOTE));
    events
}

fn cents_at(samples: &[f32], start: f64, window: f64, note: u8) -> f64 {
    let reference = SAMPLE_HZ * 2f64.powf(f64::from(note - NOTE) / 12.0);
    sf2_analysis::pitch_cents_at(samples, f64::from(RATE), start, window, reference)
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
fn the_mod_lfo_pitch_depth_and_rate_come_from_the_generators() {
    let font = sine_font(vec![(g::MOD_LFO_TO_PITCH, 100)], Vec::new());
    let buffer = native_held(&font, 1.5);
    let mono = sf2_analysis::mono(&buffer);
    let settled = &mono[at(0.25) as usize..];
    let depth = sf2_analysis::pitch_depth_cents(settled, f64::from(RATE));
    // The format's LFO origin is 8.176 Hz; a sine's sampled peak is a little
    // under the nominal depth, as with the vibrato path.
    assert!((depth - 100.0).abs() < 3.0, "depth {depth:.2} cents");

    let times = sf2_analysis::rising_crossings(settled, f64::from(RATE));
    let track = sf2_analysis::cents_track(&times);
    let cycles = track
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    let seconds = 1.5 - 0.25;
    let rate = cycles as f64 / seconds;
    assert!((rate - 8.176).abs() < 0.6, "{rate:.2} Hz");

    // `freqModLFO` is an absolute cents offset from that origin.
    let fast = sine_font(
        vec![(g::MOD_LFO_TO_PITCH, 100), (g::FREQ_MOD_LFO, 1200)],
        Vec::new(),
    );
    let buffer = native_held(&fast, 1.5);
    let mono = sf2_analysis::mono(&buffer);
    let times = sf2_analysis::rising_crossings(&mono[at(0.25) as usize..], f64::from(RATE));
    let track = sf2_analysis::cents_track(&times);
    let cycles = track
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    let rate = cycles as f64 / seconds;
    assert!((rate - 16.352).abs() < 1.0, "one octave up: {rate:.2} Hz");
}

fn native_held(font: &Path, seconds: f64) -> Vec<f32> {
    render(font, &held(seconds, 100, None), seconds)
}

#[test]
fn the_mod_lfo_volume_depth_is_bipolar_and_matches_its_centibels() {
    // 100 centibels is 10 dB each way around the unmodulated level.
    let font = sine_font(vec![(g::MOD_LFO_TO_VOLUME, 100)], Vec::new());
    let buffer = native_held(&font, 1.2);
    let mono = sf2_analysis::mono(&buffer);
    let range = sf2_analysis::level_range_db(&mono, f64::from(RATE), 0.3, 1.1, 0.005);
    assert!((range - 20.0).abs() < 1.5, "tremolo range {range:.2} dB");

    // The rate is the LFO rate: count crossings of the mean in the level track.
    let mean = sf2_analysis::level_db_at(&mono, f64::from(RATE), 0.3, 0.8);
    let mut levels = Vec::new();
    let mut t = 0.3;
    while t < 1.1 {
        levels.push(sf2_analysis::level_db_at(&mono, f64::from(RATE), t, 0.005) - mean);
        t += 0.005;
    }
    let rising = levels
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    let rate = rising as f64 / 0.8;
    assert!((rate - 8.176).abs() < 1.0, "tremolo rate {rate:.2} Hz");

    // No depth, no tremolo. The longer window holds whole cycles of the
    // 440 Hz carrier, so RMS ripple does not read as modulation.
    let flat = sine_font(Vec::new(), Vec::new());
    let buffer = native_held(&flat, 1.2);
    let mono = sf2_analysis::mono(&buffer);
    let range = sf2_analysis::level_range_db(&mono, f64::from(RATE), 0.3, 1.1, 0.05);
    assert!(range < 0.2, "unmodulated level moves {range:.2} dB");
}

#[test]
fn the_mod_env_filter_sweep_reaches_the_requested_cutoff() {
    // 6000 cents is 261 Hz; +4000 cents at the peak is 2610 Hz. The 3520 Hz
    // partial sits above both, so its level tracks the cutoff.
    let font = partials_font(vec![
        (g::INITIAL_FILTER_FC, 6000),
        (g::MOD_ENV_TO_FILTER_FC, 4000),
        (g::ATTACK_MOD_ENV, 0),
        (g::HOLD_VOL_ENV, 3000),
    ]);
    let buffer = native_held(&font, 1.6);
    let mono = sf2_analysis::mono(&buffer);

    let open = partials_font(vec![(g::INITIAL_FILTER_FC, 13_500)]);
    let open_buffer = native_held(&open, 1.6);
    let open_mono = sf2_analysis::mono(&open_buffer);
    let open_high = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.8, 0.2, HIGH_HZ);
    let open_low = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.8, 0.2, SAMPLE_HZ);

    let early_high = sf2_analysis::band_level(&mono, f64::from(RATE), 0.05, 0.1, HIGH_HZ);
    let late_high = sf2_analysis::band_level(&mono, f64::from(RATE), 1.3, 0.2, HIGH_HZ);
    let late_low = sf2_analysis::band_level(&mono, f64::from(RATE), 1.3, 0.2, SAMPLE_HZ);
    assert!(
        late_high > early_high * 10.0,
        "brightness rises: {:.1} dB -> {:.1} dB",
        sf2_analysis::db(early_high),
        sf2_analysis::db(late_high)
    );

    // The requested cutoff at the sustain: 6000 + 4000 cents.
    let requested = 8.176 * 2f64.powf(10_000.0 / 1200.0);
    let predicted_high = sf2_analysis::lowpass_db(HIGH_HZ, requested, 1.0, f64::from(RATE));
    let predicted_low = sf2_analysis::lowpass_db(SAMPLE_HZ, requested, 1.0, f64::from(RATE));
    let measured_high = sf2_analysis::db(late_high / open_high);
    let measured_low = sf2_analysis::db(late_low / open_low);
    assert!(
        (measured_high - predicted_high).abs() < 5.0,
        "3520 Hz: measured {measured_high:.1} dB, predicted {predicted_high:.1} dB"
    );
    assert!(
        (measured_low - predicted_low).abs() < 3.0,
        "440 Hz: measured {measured_low:.1} dB, predicted {predicted_low:.1} dB"
    );
}

#[test]
fn the_mod_lfo_moves_the_filter_cutoff_both_ways() {
    let font = partials_font(vec![
        (g::INITIAL_FILTER_FC, 6000),
        (g::MOD_LFO_TO_FILTER_FC, 2400),
        (g::FREQ_MOD_LFO, -1200),
    ]);
    let buffer = native_held(&font, 1.2);
    let mono = sf2_analysis::mono(&buffer);
    let mut levels = Vec::new();
    let mut t = 0.25;
    while t < 1.15 {
        levels.push(sf2_analysis::band_level(
            &mono,
            f64::from(RATE),
            t,
            0.005,
            HIGH_HZ,
        ));
        t += 0.005;
    }
    let high = levels.iter().cloned().fold(0.0f64, f64::max);
    let low = levels.iter().cloned().fold(f64::MAX, f64::min);
    let swing = sf2_analysis::db(high / low);
    // 3600..8400 cents is 65..1046 Hz; the analytic swing at 3520 Hz is 49 dB.
    assert!(swing > 35.0, "cutoff swing {swing:.1} dB");

    let open = partials_font(vec![(g::INITIAL_FILTER_FC, 13_500)]);
    let open_buffer = native_held(&open, 1.2);
    let open_mono = sf2_analysis::mono(&open_buffer);
    let open_high = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.3, 0.2, HIGH_HZ);
    let top = sf2_analysis::db(high / open_high);
    let predicted_top = sf2_analysis::lowpass_db(
        HIGH_HZ,
        8.176 * 2f64.powf(8400.0 / 1200.0),
        1.0,
        f64::from(RATE),
    );
    assert!(
        (top - predicted_top).abs() < 6.0,
        "top of the sweep {top:.1} dB vs predicted {predicted_top:.1} dB"
    );
}

#[test]
fn the_mod_env_pitch_follows_a_fast_and_a_slow_attack() {
    for (attack, times) in [
        (-2400i16, [0.05f64, 0.1, 0.3, 1.2]),
        (0, [0.05, 0.1, 0.6, 1.2]),
    ] {
        let font = sine_font(
            vec![(g::MOD_ENV_TO_PITCH, 1200), (g::ATTACK_MOD_ENV, attack)],
            Vec::new(),
        );
        let buffer = render(&font, &held(1.6, 100, None), 1.6);
        let mono = sf2_analysis::mono(&buffer);
        let attack_seconds = 2f64.powf(f64::from(attack) / 1200.0);
        let mut previous = -10.0;
        for t in times {
            let window = 0.02;
            let measured = cents_at(&mono, t, window, NOTE);
            // The tracker's median period describes the window's middle, and
            // the envelope is a straight ramp there.
            let progress = ((t + window * 0.5 - 0.005) / attack_seconds).clamp(0.0, 1.0);
            let expected = 1200.0 * progress;
            assert!(
                (measured - expected).abs() < 60.0,
                "attack {attack_seconds:.2} s at {t:.2} s: {measured:.1} cents, expected {expected:.1}"
            );
            assert!(measured + 1.0 >= previous, "not monotonic at {t:.2} s");
            previous = measured;
        }
        let full = cents_at(&mono, 1.45, 0.1, NOTE);
        assert!((full - 1200.0).abs() < 5.0, "full depth {full:.1} cents");
    }
}

#[test]
fn the_mod_env_sustain_holds_and_its_release_falls() {
    let font = sine_font(
        vec![
            (g::MOD_ENV_TO_PITCH, 1200),
            (g::ATTACK_MOD_ENV, -12000),
            (g::DECAY_MOD_ENV, 0),
            (g::SUSTAIN_MOD_ENV, 300),
            // Keep the voice alive through the modulation envelope's release.
            (g::RELEASE_VOL_ENV, 0),
        ],
        Vec::new(),
    );
    let events = vec![note_on(0, NOTE, 100), note_off(at(1.5), NOTE)];
    let buffer = render(&font, &events, 2.6);
    let mono = sf2_analysis::mono(&buffer);
    // 300 centibels is 28.8 dB on the engine's measured sustain scale.
    let sustain = 1200.0 * 10f64.powf(-28.8 / 20.0);
    let measured = cents_at(&mono, 1.0, 0.1, NOTE);
    assert!(
        (measured - sustain).abs() < 6.0,
        "sustain {measured:.1} cents, expected {sustain:.1}"
    );
    // Note off at 1.5 s, release time 1 s, from the sustain level.
    let released = cents_at(&mono, 2.3, 0.15, NOTE);
    assert!(released < 20.0, "release left {released:.1} cents");
}

#[test]
fn keynum_scaling_shortens_the_mod_env_hold() {
    let font = sine_font(
        vec![
            (g::MOD_ENV_TO_PITCH, 1200),
            (g::ATTACK_MOD_ENV, -12000),
            (g::HOLD_MOD_ENV, 0),
            (g::DECAY_MOD_ENV, 0),
            (g::SUSTAIN_MOD_ENV, 1200),
            (g::KEYNUM_TO_MOD_ENV_HOLD, 100),
            (g::RELEASE_VOL_ENV, 0),
        ],
        Vec::new(),
    );
    let duration = 1.4;
    let low = render(&font, &held(duration, 100, None), duration);
    let high_events = vec![note_on(0, 72, 100), note_off(at(duration), 72)];
    let high = render(&font, &high_events, duration);
    let low_mono = sf2_analysis::mono(&low);
    let high_mono = sf2_analysis::mono(&high);
    // Key 72's one-second hold is halved by the +100 timecents per key.
    let low_offset = cents_at(&low_mono, 0.7, 0.05, NOTE);
    let high_offset = cents_at(&high_mono, 0.7, 0.05, 72);
    assert!(
        (low_offset - 1200.0).abs() < 10.0,
        "key 60 still holds: {low_offset:.1} cents"
    );
    assert!(
        high_offset < 300.0,
        "key 72 already decays: {high_offset:.1} cents"
    );
}

#[test]
fn keynum_scaling_shortens_the_mod_env_decay() {
    let font = sine_font(
        vec![
            (g::MOD_ENV_TO_PITCH, 1200),
            (g::ATTACK_MOD_ENV, -12000),
            (g::DECAY_MOD_ENV, 0),
            (g::SUSTAIN_MOD_ENV, 300),
            (g::KEYNUM_TO_MOD_ENV_DECAY, 100),
            (g::RELEASE_VOL_ENV, 0),
        ],
        Vec::new(),
    );
    let duration = 0.6;
    let low = render(&font, &held(duration, 100, None), duration);
    let high_events = vec![note_on(0, 72, 100), note_off(at(duration), 72)];
    let high = render(&font, &high_events, duration);
    let low_offset = cents_at(&sf2_analysis::mono(&low), 0.2, 0.02, NOTE);
    let high_offset = cents_at(&sf2_analysis::mono(&high), 0.2, 0.02, 72);
    // The 96 dB-per-stage decay reaches the shallow sustain at 0.3 s on key 60
    // and at 0.15 s on key 72.
    assert!(low_offset > 90.0, "key 60 mid-decay: {low_offset:.1} cents");
    // Key 72's decay is twice as fast, so it already sits at the 300-centibel
    // sustain (43.6 cents) while key 60 is still falling.
    assert!(
        high_offset < 60.0,
        "key 72 at sustain: {high_offset:.1} cents"
    );
}

#[test]
fn generator_and_bank_records_sum_in_cents() {
    // The zone's 100 cents plus the bank's own CC2 record, 50 cents at full.
    let font = sine_font(
        vec![(g::MOD_LFO_TO_PITCH, 100)],
        vec![ModRecord::cc(2, destinations::MOD_LFO_TO_PITCH, 50)],
    );
    for (value, expected) in [(0u8, 100.0), (64, 125.0), (127, 150.0)] {
        let events = held(1.5, 100, Some(controller(0, 2, value)));
        let buffer = render(&font, &events, 1.5);
        let mono = sf2_analysis::mono(&buffer);
        let depth = sf2_analysis::pitch_depth_cents(&mono[at(0.25) as usize..], f64::from(RATE));
        assert!(
            (depth - expected).abs() < 4.0,
            "CC2 {value}: {depth:.1} cents, expected {expected:.1}"
        );
    }
}

#[test]
fn a_negative_unipolar_record_complements_its_source() {
    // The format's negative unipolar mapping is 1 - x, not -x: full velocity is
    // the quiet end. The same fixture measures 21/84 cents.
    let mut record = ModRecord::controller(0x02, destinations::MOD_LFO_TO_PITCH, 100);
    record.source |= 0x0100;
    let font = sine_font(Vec::new(), vec![record]);
    for (velocity, expected) in [(100u8, 100.0 * 27.0 / 127.0), (20, 100.0 * 107.0 / 127.0)] {
        let events = held(1.5, velocity, None);
        let buffer = render(&font, &events, 1.5);
        let mono = sf2_analysis::mono(&buffer);
        let depth = sf2_analysis::pitch_depth_cents(&mono[at(0.25) as usize..], f64::from(RATE));
        assert!(
            (depth - expected).abs() < 4.0,
            "velocity {velocity}: {depth:.1} cents, expected {expected:.1}"
        );
    }
}

#[test]
fn a_vibrato_and_a_mod_lfo_sum_into_one_pitch_offset() {
    // CC1's default 50 cents and the mod LFO's 100 cents both peak together
    // because both oscillators start at phase zero at note-on.
    let font = sine_font(vec![(g::MOD_LFO_TO_PITCH, 100)], Vec::new());
    let events = held(1.5, 100, Some(controller(0, 1, 127)));
    let buffer = render(&font, &events, 1.5);
    let mono = sf2_analysis::mono(&buffer);
    let depth = sf2_analysis::pitch_depth_cents(&mono[at(0.25) as usize..], f64::from(RATE));
    assert!((depth - 150.0).abs() < 4.0, "summed depth {depth:.1} cents");
}

#[test]
fn the_filter_cutoff_clamps_to_the_formats_legal_range() {
    // A target above the top clamps at 13500 cents, which is open.
    let open = partials_font(vec![(g::INITIAL_FILTER_FC, 13_500)]);
    let open_buffer = native_held(&open, 1.0);
    let open_mono = sf2_analysis::mono(&open_buffer);
    let open_high = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.3, 0.2, HIGH_HZ);
    let open_low = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.3, 0.2, SAMPLE_HZ);

    let top = partials_font(vec![
        (g::INITIAL_FILTER_FC, 10_000),
        (g::MOD_ENV_TO_FILTER_FC, 8000),
        (g::ATTACK_MOD_ENV, -12000),
    ]);
    let top_buffer = native_held(&top, 1.0);
    let top_mono = sf2_analysis::mono(&top_buffer);
    let top_high = sf2_analysis::band_level(&top_mono, f64::from(RATE), 0.3, 0.2, HIGH_HZ);
    let top_low = sf2_analysis::band_level(&top_mono, f64::from(RATE), 0.3, 0.2, SAMPLE_HZ);
    assert!(
        (sf2_analysis::db(top_high / open_high)).abs() < 1.0,
        "clamped filter stays open"
    );
    assert!(
        (sf2_analysis::db(top_low / open_low)).abs() < 1.0,
        "clamped filter leaves the fundamental alone"
    );

    // A target below the bottom clamps at 1500 cents (19.4 Hz).
    let bottom = partials_font(vec![
        (g::INITIAL_FILTER_FC, 2000),
        (g::MOD_ENV_TO_FILTER_FC, -8000),
        (g::ATTACK_MOD_ENV, -12000),
    ]);
    let bottom_buffer = native_held(&bottom, 1.0);
    let bottom_mono = sf2_analysis::mono(&bottom_buffer);
    let bottom_low = sf2_analysis::band_level(&bottom_mono, f64::from(RATE), 0.3, 0.2, SAMPLE_HZ);
    let floor = sf2_analysis::lowpass_db(SAMPLE_HZ, 19.4, 1.0, f64::from(RATE));
    let measured = sf2_analysis::db(bottom_low / open_low);
    assert!(
        (measured - floor).abs() < 4.0,
        "bottom clamp: {measured:.1} dB vs predicted {floor:.1} dB"
    );
}

#[test]
fn a_static_filter_response_matches_the_two_pole_law() {
    let open = partials_font(vec![(g::INITIAL_FILTER_FC, 13_500)]);
    let open_buffer = native_held(&open, 1.0);
    let open_mono = sf2_analysis::mono(&open_buffer);
    let open_high = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.3, 0.2, HIGH_HZ);
    let open_low = sf2_analysis::band_level(&open_mono, f64::from(RATE), 0.3, 0.2, SAMPLE_HZ);

    for cents in [6000i16, 8000] {
        let font = partials_font(vec![(g::INITIAL_FILTER_FC, cents)]);
        let buffer = native_held(&font, 1.0);
        let mono = sf2_analysis::mono(&buffer);
        let high = sf2_analysis::band_level(&mono, f64::from(RATE), 0.3, 0.2, HIGH_HZ);
        let low = sf2_analysis::band_level(&mono, f64::from(RATE), 0.3, 0.2, SAMPLE_HZ);
        let cutoff = 8.176 * 2f64.powf(f64::from(cents) / 1200.0);
        let expected_high = sf2_analysis::lowpass_db(HIGH_HZ, cutoff, 1.0, f64::from(RATE));
        let expected_low = sf2_analysis::lowpass_db(SAMPLE_HZ, cutoff, 1.0, f64::from(RATE));
        let measured_high = sf2_analysis::db(high / open_high);
        let measured_low = sf2_analysis::db(low / open_low);
        assert!(
            (measured_high - expected_high).abs() < 2.0,
            "{cents} cents: 3520 Hz {measured_high:.1} vs {expected_high:.1} dB"
        );
        assert!(
            (measured_low - expected_low).abs() < 2.0,
            "{cents} cents: 440 Hz {measured_low:.1} vs {expected_low:.1} dB"
        );
    }
}

#[test]
fn a_bank_without_modulation_renders_byte_identically() {
    // The same sample and envelope; one bank carries modulation generators and
    // records whose sources are at zero. Nothing may move.
    let plain = sine_font(Vec::new(), Vec::new());
    let declared = sine_font(
        vec![
            (g::MOD_LFO_TO_PITCH, 0),
            (g::MOD_ENV_TO_PITCH, 0),
            (g::MOD_LFO_TO_FILTER_FC, 0),
            (g::MOD_ENV_TO_FILTER_FC, 0),
            (g::MOD_LFO_TO_VOLUME, 0),
            (g::FREQ_MOD_LFO, 0),
            (g::DELAY_MOD_LFO, -12000),
        ],
        vec![
            ModRecord::cc(2, destinations::MOD_LFO_TO_PITCH, 0),
            ModRecord::cc(3, destinations::MOD_LFO_TO_VOLUME, 0),
        ],
    );
    let events = held(1.0, 100, None);
    let first = render(&plain, &events, 1.0);
    let second = render(&declared, &events, 1.0);
    assert_eq!(fnv1a(&first), fnv1a(&second));
    assert!(first == second, "samples differ");
    // The pinned hash from before the modulation work must not move.
    assert_eq!(fnv1a(&first), UNMODULATED_HASH);
}

const UNMODULATED_HASH: u64 = 4_826_522_779_913_708_261;

#[test]
fn the_modulation_state_is_per_voice() {
    let font = sine_font(
        vec![
            (g::MOD_LFO_TO_PITCH, 100),
            (g::MOD_ENV_TO_PITCH, 600),
            (g::ATTACK_MOD_ENV, -1200),
        ],
        Vec::new(),
    );
    let first_note = at(0.6);
    let second_note = at(0.87);
    let alone = |start: u64, held: u64| {
        let events = vec![note_on(start, NOTE, 100), note_off(start + held, NOTE)];
        render(&font, &events, 1.6)
    };
    let first = alone(0, first_note);
    let second = alone(second_note, at(0.5));
    let mut events = vec![note_on(0, NOTE, 100), note_off(first_note, NOTE)];
    events.push(note_on(second_note, NOTE, 100));
    events.push(note_off(second_note + at(0.5), NOTE));
    let together = render(&font, &events, 1.6);

    let mut expected = first.clone();
    for (slot, value) in expected.iter_mut().zip(&second) {
        *slot += value;
    }
    let worst = together
        .iter()
        .zip(&expected)
        .fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
    assert_eq!(worst, 0.0, "per-voice modulation sum differs by {worst}");
}

#[test]
fn the_same_events_render_byte_identically_twice() {
    let font = sine_font(
        vec![
            (g::MOD_LFO_TO_PITCH, 120),
            (g::MOD_LFO_TO_VOLUME, 80),
            (g::MOD_ENV_TO_FILTER_FC, 3000),
            (g::INITIAL_FILTER_FC, 8000),
            (g::MOD_ENV_TO_PITCH, 400),
        ],
        vec![ModRecord::cc(2, destinations::MOD_LFO_TO_FILTER_FC, 600)],
    );
    let events = held(1.2, 100, Some(controller(0, 2, 90)));
    let first = render(&font, &events, 1.2);
    let second = render(&font, &events, 1.2);
    assert_eq!(fnv1a(&first), fnv1a(&second));
}
