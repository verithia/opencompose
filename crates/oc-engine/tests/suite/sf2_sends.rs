//! Effect sends: the chorus and reverb buses, their level law and the
//! controllers that move them.
//!
//! Every figure is measured from the rendered audio. The reverb is read as the
//! energy after note-off, the chorus as the pitch wobble of the wet signal (the
//! send render minus the zero-send render, which is the same dry path), and the
//! dry path as the first milliseconds, before any wet return can arrive.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::soundfont::SoundFontRenderer;
use sf2_builder::{Font, ModRecord, Sample, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;
/// The fixture's sample is a 440 Hz sine at its root key.
const SAMPLE_HZ: f64 = 440.0;

/// Held note with a 3 s render, so the reverb tail is inside the file.
const NOTE_SECONDS: f64 = 1.0;
const RENDER_SECONDS: f64 = 3.0;

fn send_font(chorus_send: i16, reverb_send: i16, modulators: Vec<ModRecord>) -> PathBuf {
    let mut font = Font::new("sends");
    font.add_sample(Sample::sine("sine", RATE, NOTE, SAMPLE_HZ, RATE as usize).with_loop(0, RATE));
    font.add_instrument(
        "instrument",
        vec![vec![
            (g::SAMPLE_ID, 0),
            (g::SAMPLE_MODES, 1),
            (g::OVERRIDING_ROOT_KEY, i16::from(NOTE)),
            (g::KEY_RANGE, sf2_builder::range(0, 127) as i16),
            (g::CHORUS_SEND, chorus_send),
            (g::REVERB_SEND, reverb_send),
        ]],
    );
    font.add_preset("preset", 0, 0, vec![vec![(g::INSTRUMENT, 0)]]);
    if !modulators.is_empty() {
        font.presets[0].zone_modulators = vec![modulators];
    }
    font.write_temp()
}

fn note_on(channel: u8, time: u64) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOn {
            channel,
            note: NOTE,
            velocity: 100,
        },
        track_index: 0,
    }
}

fn note_off(channel: u8, time: u64) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::NoteOff {
            channel,
            note: NOTE,
        },
        track_index: 0,
    }
}

fn controller(channel: u8, time: u64, controller: u8, value: u8) -> MidiEvent {
    MidiEvent {
        time_samples: time,
        event: MidiMessage::ControlChange {
            channel,
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

/// A note held for `seconds`, optionally after controller events. The render
/// loop dispatches in list order, so the list is time-sorted like an
/// interpreted event list.
fn held(seconds: f64, setup: &[MidiEvent]) -> Vec<MidiEvent> {
    let mut events = setup.to_vec();
    events.push(note_on(0, 0));
    events.push(note_off(0, at(seconds)));
    events.sort_by_key(|event| event.time_samples);
    events
}

fn window(interleaved: &[f32], start: f64, end: f64) -> &[f32] {
    let low = ((start * f64::from(RATE)) as usize * 2).min(interleaved.len());
    let high = ((end * f64::from(RATE)) as usize * 2).min(interleaved.len());
    &interleaved[low..high]
}

fn level_db(interleaved: &[f32], start: f64, end: f64) -> f64 {
    sf2_analysis::db(sf2_analysis::rms(window(interleaved, start, end)))
}

/// Energy of the reverb tail: everything after the note-off and its release.
fn tail_db(interleaved: &[f32]) -> f64 {
    level_db(interleaved, NOTE_SECONDS + 0.05, RENDER_SECONDS - 0.5)
}

/// The dry path before any wet return: the reverb's first comb delay is longer
/// than this and the chorus's shortest delay is 4 ms.
fn early_db(interleaved: &[f32]) -> f64 {
    level_db(interleaved, 0.0, 0.003)
}

fn difference(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(left, right)| left - right).collect()
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

/// The wobble's rate: how often the pitch track crosses its median.
fn wobble_hz(samples: &[f32], seconds: f64) -> f64 {
    let times = sf2_analysis::rising_crossings(samples, f64::from(RATE));
    let track = sf2_analysis::cents_track(&times);
    let crossings = track
        .windows(2)
        .filter(|pair| (pair[0] <= 0.0) != (pair[1] <= 0.0))
        .count();
    crossings as f64 / (seconds * 2.0)
}

// ============================================================================
// The level law
// ============================================================================

#[test]
fn the_reverb_tail_grows_linearly_with_the_send_and_the_dry_stays_put() {
    let mut tails = Vec::new();
    let mut zero_send = None;
    for send in [0i16, 250, 500, 1000] {
        let font = send_font(0, send, Vec::new());
        let output = render(&font, &held(NOTE_SECONDS, &[]), RENDER_SECONDS);
        let tail = tail_db(&output);
        tails.push(tail);
        if send == 0 {
            zero_send = Some(output);
        }
    }
    // The tail rises with every step, and doubling the send adds 6 dB: the
    // send is a linear amplitude, not a power.
    for pair in tails.windows(2) {
        assert!(pair[1] > pair[0] + 3.0, "not monotone: {tails:?}");
    }
    assert!((tails[2] - tails[3] + 6.02).abs() < 0.6, "{tails:?}");
    assert!((tails[1] - tails[3] + 12.04).abs() < 0.6, "{tails:?}");
    // A bank whose send is zero has no tail at all.
    assert!(
        tails[0] < -200.0,
        "the zero send produced a tail: {tails:?}"
    );
    // The dry path is untouched: the first 3 ms are sample-identical at every
    // send, because no wet return has arrived yet.
    let zero_send = zero_send.unwrap();
    for send in [250i16, 500, 1000] {
        let font = send_font(0, send, Vec::new());
        let output = render(&font, &held(NOTE_SECONDS, &[]), RENDER_SECONDS);
        let frames = (0.003 * f64::from(RATE)) as usize * 2;
        assert_eq!(
            &output[..frames],
            &zero_send[..frames],
            "the dry path moved at send {send}"
        );
        assert!(
            (early_db(&output) - early_db(&zero_send)).abs() < 0.1,
            "the dry level moved at send {send}"
        );
    }
}

/// Chorus depth in the rendered note: the sidebands the delay wobble puts
/// around the carrier, relative to the carrier, over a Hann-windowed window.
/// The dry note is a pure sine, so every sideband comes from the wet copy, and
/// the window's first null sits at the first sideband, which keeps the carrier
/// from leaking into the measurement.
fn sideband_db(samples: &[f32]) -> f64 {
    let mut mono = sf2_analysis::mono(window(samples, 0.2, 3.0));
    let frames = mono.len() as f64;
    for (index, sample) in mono.iter_mut().enumerate() {
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / frames).cos();
        *sample *= window as f32;
    }
    let carrier = sf2_analysis::goertzel(&mono, f64::from(RATE), SAMPLE_HZ).powi(2);
    if carrier <= 1e-30 {
        return -240.0;
    }
    let mut side = 0.0;
    for harmonic in 1..=12 {
        for sign in [-1.0f64, 1.0] {
            let frequency = SAMPLE_HZ + sign * CHORUS_RATE_HZ * f64::from(harmonic);
            side += sf2_analysis::goertzel(&mono, f64::from(RATE), frequency).powi(2);
        }
    }
    sf2_analysis::db((side / carrier).sqrt())
}

/// The chorus device's default rate, for the sideband probe.
const CHORUS_RATE_HZ: f64 = 1.5;

#[test]
fn the_chorus_wet_grows_linearly_and_deepens_with_the_send() {
    // A three-second note, so the wobble has several cycles inside the window.
    let dry = render(&send_font(0, 0, Vec::new()), &held(3.0, &[]), 3.5);
    let mut levels = Vec::new();
    let mut depths = Vec::new();
    let mut wet = Vec::new();
    for send in [0i16, 250, 500, 1000] {
        let font = send_font(send, 0, Vec::new());
        let output = render(&font, &held(3.0, &[]), 3.5);
        let difference = difference(&output, &dry);
        levels.push(level_db(&difference, 0.2, 2.8));
        depths.push(sideband_db(&output));
        let frames = (0.003 * f64::from(RATE)) as usize * 2;
        assert_eq!(
            &output[..frames],
            &dry[..frames],
            "the dry path moved at chorus send {send}"
        );
        wet = difference;
    }
    // No send, no chorus: the carrier stands alone and only the window's own
    // floor is left.
    assert!(depths[0] < -45.0, "{depths:?}");
    for pair in levels[1..].windows(2) {
        assert!(pair[1] > pair[0] + 3.0, "wet not monotone: {levels:?}");
    }
    assert!((levels[3] - levels[2] - 6.02).abs() < 0.6, "{levels:?}");
    // The modulation depth of the rendered note follows the send: a quarter of
    // the send leaves the sidebands about five decibels below half of it.
    for pair in depths[1..].windows(2) {
        assert!(pair[1] > pair[0] + 3.0, "{depths:?}");
    }
    // The wobble itself is slow: the pitch track of the wet copy crosses its
    // median a handful of times over the held note.
    let rate = wobble_hz(&sf2_analysis::mono(window(&wet, 0.2, 2.8)), 2.6);
    assert!((0.2..8.0).contains(&rate), "chorus rate {rate} Hz");
}

// ============================================================================
// The controllers
// ============================================================================

#[test]
fn cc91_moves_the_reverb_before_and_during_a_note() {
    // The bank declares no send, so every bit of tail comes from CC91 alone.
    let font = send_font(0, 0, Vec::new());
    let dry = render(&font, &held(NOTE_SECONDS, &[]), RENDER_SECONDS);
    assert!(tail_db(&dry) < -200.0, "a send-free bank has a tail");
    let wet_during = |events: &[MidiEvent]| {
        let output = render(&font, &held(NOTE_SECONDS, events), RENDER_SECONDS);
        (
            level_db(&difference(&output, &dry), 0.2, NOTE_SECONDS),
            tail_db(&output),
        )
    };
    let (before_level, before_tail) = wet_during(&[controller(0, 0, 91, 127)]);
    let (middle_level, middle_tail) = wet_during(&[controller(0, at(0.5), 91, 127)]);
    let (half_level, _) = wet_during(&[controller(0, 0, 91, 64)]);
    // The send is live from the note's first sample when the CC precedes it,
    // and joins mid-note when it arrives later: the wet is there in both cases
    // and it is louder the earlier the CC lands.
    assert!(before_tail > -100.0, "no tail: {before_tail}");
    assert!(middle_tail > -100.0, "no tail: {middle_tail}");
    assert!(
        before_level > middle_level + 2.0,
        "a CC before the note and one mid-note are not distinguished: \
         {before_level} vs {middle_level}"
    );
    // Full travel is twice the send of CC91 = 64: 6 dB more wet.
    assert!(
        (before_level - half_level - 6.02).abs() < 1.0,
        "{before_level} vs {half_level}"
    );
    // CC91 on another channel does not reach this one.
    let other_channel = render(
        &font,
        &held(NOTE_SECONDS, &[controller(1, 0, 91, 127)]),
        RENDER_SECONDS,
    );
    assert!(
        tail_db(&other_channel) < -200.0,
        "the send crossed channels: {}",
        tail_db(&other_channel)
    );
}

#[test]
fn cc93_moves_the_chorus_at_runtime() {
    let font = send_font(0, 0, Vec::new());
    let dry = render(&font, &held(NOTE_SECONDS, &[]), RENDER_SECONDS);
    let mut levels = Vec::new();
    for (time, value) in [(0u64, 32u8), (0, 64), (0, 127), (at(0.5), 127)] {
        let output = render(
            &font,
            &held(NOTE_SECONDS, &[controller(0, time, 93, value)]),
            RENDER_SECONDS,
        );
        levels.push(level_db(&difference(&output, &dry), 0.2, NOTE_SECONDS));
    }
    // 32 < 64 < 127 before the note, and the same value arriving mid-note
    // leaves less wet energy than one that was there from the start.
    assert!(levels[0] < levels[1] - 3.0, "{levels:?}");
    assert!(levels[1] < levels[2] - 3.0, "{levels:?}");
    assert!(levels[3] < levels[2] - 1.0, "{levels:?}");
    assert!(levels[3] > levels[1], "{levels:?}");
}

// ============================================================================
// Byte identity and determinism
// ============================================================================

const UNMODULATED_HASH: u64 = 4_826_522_779_913_708_261;

#[test]
fn a_bank_with_no_sends_keeps_the_pinned_render() {
    // The same fixture the modulation tests pin: no send generators, no send
    // controllers, so the buses never run and the samples are the old ones.
    let font = send_font(0, 0, Vec::new());
    let events = held(1.3, &[]);
    let output = render(&font, &events, 1.0);
    assert_eq!(
        fnv1a(&output),
        UNMODULATED_HASH,
        "the send-free render drifted"
    );
}

#[test]
fn a_send_controller_at_zero_keeps_the_pinned_render() {
    // An explicit CC91/CC93 at zero asks for the same silence a bank without
    // sends gets, and must not switch the buses on.
    let font = send_font(0, 0, Vec::new());
    let events = held(1.3, &[controller(0, 0, 91, 0), controller(0, 0, 93, 0)]);
    assert_eq!(fnv1a(&render(&font, &events, 1.0)), UNMODULATED_HASH);
}

#[test]
fn the_same_send_events_render_identically_twice() {
    let font = send_font(500, 1000, Vec::new());
    let events = held(NOTE_SECONDS, &[controller(0, 0, 91, 96)]);
    let first = render(&font, &events, RENDER_SECONDS);
    let second = render(&font, &events, RENDER_SECONDS);
    assert_eq!(fnv1a(&first), fnv1a(&second));
    assert!(first == second);
}

#[test]
fn a_recorded_send_and_the_controller_sum_to_the_same_send() {
    // 20 % from the bank plus 20 % from CC91 is the same send as 40 % from the
    // bank alone, because the destinations are summed in their own unit.
    let recorded = render(
        &send_font(0, 400, Vec::new()),
        &held(NOTE_SECONDS, &[]),
        RENDER_SECONDS,
    );
    let controller = render(
        &send_font(0, 200, Vec::new()),
        &held(NOTE_SECONDS, &[controller(0, 0, 91, 127)]),
        RENDER_SECONDS,
    );
    assert!(
        (tail_db(&recorded) - tail_db(&controller)).abs() < 0.2,
        "{} vs {}",
        tail_db(&recorded),
        tail_db(&controller)
    );
}

// ============================================================================
// Polyphony
// ============================================================================

fn polyphonic_render(sends: (i16, i16), notes: &[u8], seconds: f64) -> Vec<f32> {
    let font = send_font(sends.0, sends.1, Vec::new());
    let mut events = Vec::new();
    for note in notes {
        events.push(MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note: *note,
                velocity: 100,
            },
            track_index: 0,
        });
    }
    for note in notes {
        events.push(MidiEvent {
            time_samples: at(seconds),
            event: MidiMessage::NoteOff {
                channel: 0,
                note: *note,
            },
            track_index: 0,
        });
    }
    render(&font, &events, seconds + 2.0)
}

#[test]
fn many_voices_share_one_bus_without_smearing_or_clipping() {
    // Eight notes at once on one player: the buses are shared, and both devices
    // are linear, so the wet return is the sum of the single-note returns.
    let notes: [u8; 8] = [48, 52, 55, 59, 60, 64, 67, 72];
    let together = polyphonic_render((1000, 1000), &notes, 0.8);
    let mut expected = vec![0.0f32; together.len()];
    for note in notes {
        let single = polyphonic_render((1000, 1000), &[note], 0.8);
        for (slot, value) in expected.iter_mut().zip(&single) {
            *slot += value;
        }
    }
    let worst = together
        .iter()
        .zip(&expected)
        .fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
    let peak = sf2_analysis::peak(&together);
    assert!(
        worst < 2e-3,
        "the shared bus is not the sum of the voices: {worst}"
    );
    assert!(peak < 4.0, "the send return clipped: peak {peak}");
    assert!(
        together.iter().all(|sample| sample.is_finite()),
        "non-finite sample"
    );
    // The tail of the chord is real, not a single smeared voice.
    assert!(tail_db(&together) > -80.0);
}

// ============================================================================
// The measurement table
// ============================================================================

/// Prints the numbers the docs quote; run explicitly with `--ignored`.
#[test]
#[ignore = "prints the measured send table"]
fn the_send_table() {
    let dry = render(
        &send_font(0, 0, Vec::new()),
        &held(NOTE_SECONDS, &[]),
        RENDER_SECONDS,
    );
    let dry_level = level_db(&dry, NOTE_SECONDS + 0.05, RENDER_SECONDS - 0.5);
    let dry_early = early_db(&dry);
    println!("reverb: send | tail dBFS | dry first 3 ms | wet/dry dB");
    for send in [0i16, 250, 500, 1000] {
        let output = render(
            &send_font(0, send, Vec::new()),
            &held(NOTE_SECONDS, &[]),
            RENDER_SECONDS,
        );
        println!(
            "{send:>13} | {:>9.1} | {:>14.2} | {:>10.2}",
            tail_db(&output),
            early_db(&output),
            tail_db(&output) - dry_level
        );
    }
    let _ = dry_early;
    let chorus_dry = render(&send_font(0, 0, Vec::new()), &held(3.0, &[]), 3.5);
    let chorus_dry_level = level_db(&chorus_dry, 0.2, 2.8);
    println!("chorus: send | wet dBFS | wet/dry dB | sideband dB | dry first 3 ms");
    for send in [0i16, 250, 500, 1000] {
        let output = render(&send_font(send, 0, Vec::new()), &held(3.0, &[]), 3.5);
        let wet = difference(&output, &chorus_dry);
        println!(
            "{send:>12} | {:>8.1} | {:>10.2} | {:>11.2} | {:>14.2}",
            level_db(&wet, 0.2, 2.8),
            level_db(&wet, 0.2, 2.8) - chorus_dry_level,
            sideband_db(&output),
            early_db(&output)
        );
    }
    let font = send_font(0, 0, Vec::new());
    let dry = render(&font, &held(NOTE_SECONDS, &[]), RENDER_SECONDS);
    println!("cc | when | value | reverb tail dBFS | chorus wet dBFS");
    for (when, time, value) in [
        ("before", 0u64, 127u8),
        ("mid-note", at(0.5), 127),
        ("before", 0, 64),
    ] {
        let reverb = render(
            &font,
            &held(NOTE_SECONDS, &[controller(0, time, 91, value)]),
            RENDER_SECONDS,
        );
        let chorus = render(
            &font,
            &held(NOTE_SECONDS, &[controller(0, time, 93, value)]),
            RENDER_SECONDS,
        );
        println!(
            "{:>2} | {when:>8} | {value:>5} | {:>16.1} | {:>15.1}",
            91,
            tail_db(&reverb),
            level_db(&difference(&chorus, &dry), 0.2, NOTE_SECONDS)
        );
    }
}
