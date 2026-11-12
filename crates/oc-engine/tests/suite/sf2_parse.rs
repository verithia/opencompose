//! Parser tests: the checked-in 8 KB fixture, plus synthetic banks for the
//! generator cases the fixture does not carry.

use crate::sf2_builder;

use std::path::PathBuf;
use std::sync::Arc;

use oc_engine::sf2::font::Error;
use oc_engine::sf2::modulation::{Destination, ModSource};
use oc_engine::sf2::{SampleMode, SoundFont};
use sf2_builder::{Font, ModRecord, Points, Sample, destinations, generators, range};

fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

#[test]
fn fixture_parses_into_one_playable_preset() {
    let font = SoundFont::load(&fixture()).expect("fixture parses");
    assert_eq!(font.name.as_deref(), Some("OpenCompose Test Tone"));
    assert_eq!(font.version, (2, 1));
    assert_eq!(font.sample_bank.bit_depth(), 16);
    assert_eq!(font.instruments.len(), 1);
    assert_eq!(font.instruments[0].name, "TestInst");

    assert_eq!(font.presets.len(), 1);
    let preset = &font.presets[0];
    assert_eq!(preset.name, "TestPreset");
    assert_eq!((preset.bank, preset.program), (0, 0));
    assert_eq!(preset.zones.len(), 1);

    let zone = &preset.zones[0];
    assert_eq!((zone.key_low, zone.key_high), (0, 127));
    assert_eq!((zone.vel_low, zone.vel_high), (0, 127));
    assert_eq!(zone.sample, 0);
    assert_eq!((zone.start, zone.end), (0, 4096));
    assert_eq!((zone.loop_start, zone.loop_end), (0, 4096));
    assert_eq!(zone.root_key, 60);
    assert_eq!(zone.scale_tuning, 100);
    assert_eq!(zone.attenuation_cb, 0);
    assert_eq!(zone.sample_mode, SampleMode::NoLoop);
    // The fixture declares no envelope generator; each default is the
    // specification's -12000 timecents, i.e. about one millisecond.
    let one_millisecond = 2f64.powi(-10);
    assert!((zone.envelope.attack - one_millisecond).abs() < 1e-9);
    assert!((zone.envelope.release - one_millisecond).abs() < 1e-9);
    assert!((zone.envelope.sustain_cb - 0.0).abs() < f32::EPSILON);
}

#[test]
fn fixture_sample_data_matches_its_header() {
    let font = SoundFont::load(&fixture()).unwrap();
    assert_eq!(font.sample_headers.len(), 1);
    let header = &font.sample_headers[0];
    assert_eq!(header.name, "TestSample");
    assert_eq!((header.start, header.end), (0, 4096));
    assert_eq!((header.loop_start, header.loop_end), (0, 4096));
    assert_eq!(header.rate, 44_100);
    assert_eq!(header.root_key, 60);
    assert!(!header.is_stereo());
    assert_eq!(font.sample_bank.frames(), 4096);

    // The tone is a 0.366-amplitude sine; out-of-range frames read as silence.
    let peak = (0..4096)
        .map(|frame| font.sample_bank.point(frame).abs())
        .fold(0.0f32, f32::max);
    assert!((peak - 0.36618).abs() < 0.001, "peak {peak}");
    assert_eq!(font.sample_bank.point(4096), 0.0);
    // Linear interpolation between neighbours stays inside the waveform.
    let interpolated = font.sample_bank.read(0.5);
    let expected = (font.sample_bank.point(0) + font.sample_bank.point(1)) * 0.5;
    assert!((interpolated - expected).abs() < 1e-6);
}

#[test]
fn fixture_reports_the_modulators_it_does_not_apply() {
    // The fixture's two records are terminal records: nothing to apply.
    let font = SoundFont::load(&fixture()).unwrap();
    assert!(
        font.warnings
            .iter()
            .any(|warning| warning.contains("honoured 0 of 2 modulator records")),
        "{:?}",
        font.warnings
    );
}

#[test]
fn missing_pdta_is_reported_as_a_structural_error() {
    let bytes = std::fs::read(fixture()).unwrap();
    let pdta = bytes
        .windows(4)
        .position(|window| window == b"pdta")
        .expect("fixture has a pdta list");
    // Cut the file where the pdta list starts: INFO and sdta survive.
    let truncated = &bytes[..pdta - 4];
    let error = SoundFont::parse(truncated).unwrap_err();
    assert!(matches!(error, Error::Missing("pdta list")), "{error}");
    assert!(error.to_string().contains("pdta"));
}

#[test]
fn a_truncated_chunk_names_itself() {
    let mut font = Font::new("Short");
    font.single_preset(Sample::sine("tone", 44_100, 60, 441.0, 64), 0, 127);
    let mut bytes = font.bytes();
    let smpl = bytes
        .windows(4)
        .position(|window| window == b"smpl")
        .expect("smpl chunk");
    let declared = (bytes.len() as u32) * 2;
    bytes[smpl + 4..smpl + 8].copy_from_slice(&declared.to_le_bytes());
    let error = SoundFont::parse(&bytes).unwrap_err();
    let text = error.to_string();
    assert!(matches!(error, Error::Structure(_)), "{text}");
    assert!(text.contains("smpl"), "{text}");
}

#[test]
fn padded_preset_records_are_tolerated() {
    let mut font = Font::new("Padding");
    font.phdr_padding = 7;
    font.single_preset(Sample::sine("tone", 44_100, 60, 441.0, 64), 0, 127);
    let parsed = SoundFont::load(&font.write_temp()).expect("padding is tolerated");
    assert_eq!(parsed.presets.len(), 1);
    assert_eq!(parsed.presets[0].zones[0].end, 64);
}

#[test]
fn preset_and_instrument_ranges_intersect() {
    let mut font = Font::new("Ranges");
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 128));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::KEY_RANGE, range(48, 64) as i16),
            (generators::VEL_RANGE, range(0, 100) as i16),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![
            (generators::INSTRUMENT, instrument as i16),
            (generators::KEY_RANGE, range(60, 96) as i16),
            (generators::VEL_RANGE, range(40, 127) as i16),
        ]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    let zone = &parsed.presets[0].zones[0];
    assert_eq!((zone.key_low, zone.key_high), (60, 64));
    assert_eq!((zone.vel_low, zone.vel_high), (40, 100));
}

#[test]
fn an_inverted_range_drops_the_zone_with_a_warning() {
    let mut font = Font::new("Inverted");
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 128));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::KEY_RANGE, range(70, 60) as i16),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    assert!(parsed.presets[0].zones.is_empty());
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("empty key or velocity range")),
        "{:?}",
        parsed.warnings
    );
}

#[test]
fn a_zone_naming_a_missing_sample_is_dropped_with_a_warning() {
    let mut font = Font::new("Missing");
    font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 128));
    let instrument = font.add_instrument("inst", vec![vec![(generators::SAMPLE_ID, 9)]]);
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    assert!(parsed.presets[0].zones.is_empty());
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("sample 9")),
        "{:?}",
        parsed.warnings
    );
}

#[test]
fn tuning_and_envelope_generators_become_playback_values() {
    let mut font = Font::new("Generators");
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 256));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::COARSE_TUNE, 12),
            (generators::FINE_TUNE, -50),
            (generators::SCALE_TUNING, 50),
            (generators::OVERRIDING_ROOT_KEY, 48),
            (generators::INITIAL_ATTENUATION, 60),
            (generators::PAN, -250),
            (generators::SAMPLE_MODES, 1),
            (generators::DELAY_VOL_ENV, -2400),
            (generators::ATTACK_VOL_ENV, -1200),
            (generators::HOLD_VOL_ENV, -600),
            (generators::DECAY_VOL_ENV, -1800),
            (generators::SUSTAIN_VOL_ENV, 200),
            (generators::RELEASE_VOL_ENV, -900),
            (generators::KEYNUM_TO_VOL_ENV_HOLD, 50),
            (generators::KEYNUM_TO_VOL_ENV_DECAY, 100),
            (generators::INITIAL_FILTER_FC, 9000),
            (generators::INITIAL_FILTER_Q, 60),
            (generators::EXCLUSIVE_CLASS, 3),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    let zone = &parsed.presets[0].zones[0];

    assert_eq!(zone.root_key, 48, "overridingRootKey wins over the header");
    assert_eq!(zone.coarse_tune, 12);
    assert_eq!(zone.fine_tune, -50);
    assert_eq!(zone.scale_tuning, 50);
    assert_eq!(zone.attenuation_cb, 60);
    assert!((zone.pan + 0.5).abs() < 1e-6);
    assert_eq!(zone.sample_mode, SampleMode::Continuous);
    assert_eq!(zone.exclusive_class, 3);

    assert!((zone.envelope.delay - 0.25).abs() < 1e-9);
    assert!((zone.envelope.attack - 0.5).abs() < 1e-9);
    assert!((zone.envelope.hold - 2f64.powf(-0.5)).abs() < 1e-9);
    assert!((zone.envelope.decay - 2f64.powf(-1.5)).abs() < 1e-9);
    assert!((zone.envelope.sustain_cb - 200.0).abs() < f32::EPSILON);
    assert!((zone.envelope.release - 2f64.powf(-0.75)).abs() < 1e-9);
    assert!((zone.envelope.hold_key_scale - 50.0).abs() < f32::EPSILON);
    assert!((zone.envelope.decay_key_scale - 100.0).abs() < f32::EPSILON);

    // The filter is 9000 cents above 8.176 Hz, with 6 dB of resonance.
    let cutoff = zone.filter_cutoff_hz().expect("filter is active");
    assert!((cutoff - 8.176 * 2f64.powf(7.5)).abs() < 0.01);
    assert!((zone.filter_q() - 10f64.powf(0.3)).abs() < 1e-6);

    // Keynum scaling: key 72 shortens the hold by 50 timecents per key.
    assert!(zone.envelope.hold_for(72) < zone.envelope.hold_for(60));
    assert!(zone.envelope.decay_for(72) < zone.envelope.decay_for(60));
}

#[test]
fn address_generators_shift_the_play_region_inside_the_sample() {
    let mut font = Font::new("Offsets");
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 1000).with_loop(200, 800));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::START_ADDRS_OFFSET, 100),
            (generators::END_ADDRS_OFFSET, -200),
            (generators::START_LOOP_ADDRS_OFFSET, 50),
            (generators::END_LOOP_ADDRS_OFFSET, -50),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    let zone = &parsed.presets[0].zones[0];
    assert_eq!((zone.start, zone.end), (100, 800));
    assert_eq!((zone.loop_start, zone.loop_end), (250, 750));
}

#[test]
fn loop_points_outside_the_play_region_are_clamped() {
    let mut font = Font::new("Clamp");
    let sample = font.add_sample(Sample::sine("tone", 44_100, 60, 441.0, 100).with_loop(90, 300));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::END_ADDRS_OFFSET, -95),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    let zone = &parsed.presets[0].zones[0];
    assert_eq!(zone.end, 5);
    assert!(zone.loop_start < zone.loop_end || zone.loop_end == 0);
}

#[test]
fn twenty_four_bit_low_bytes_are_combined() {
    let mut font = Font::new("Wide");
    let points = vec![0, 1 << 20, -(1 << 20), (1 << 23) - 1, -(1 << 23)];
    font.add_sample(Sample {
        name: "wide".to_string(),
        rate: 44_100,
        root_key: 60,
        points: Points::I24(points.clone()),
        loop_start: 0,
        loop_end: 0,
    });
    let instrument = font.add_instrument("inst", vec![vec![(generators::SAMPLE_ID, 0)]]);
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let parsed = SoundFont::load(&font.write_temp()).unwrap();
    assert_eq!(parsed.sample_bank.bit_depth(), 24);
    assert_eq!(parsed.sample_bank.frames(), 5);
    for (frame, point) in points.iter().enumerate() {
        let expected = *point as f32 / 8_388_608.0;
        let actual = parsed.sample_bank.point(frame);
        assert!(
            (actual - expected).abs() < 1e-7,
            "frame {frame}: {actual} != {expected}"
        );
    }
}

#[test]
fn preset_lookup_falls_back_for_a_missing_program() {
    let mut font = Font::new("Lookup");
    font.add_sample(Sample::sine("low", 44_100, 60, 220.0, 64));
    font.add_sample(Sample::sine("high", 44_100, 60, 880.0, 64));
    font.add_instrument("inst", vec![vec![(generators::SAMPLE_ID, 0)]]);
    font.add_instrument("kit", vec![vec![(generators::SAMPLE_ID, 1)]]);
    font.add_preset("zero", 0, 0, vec![vec![(generators::INSTRUMENT, 0)]]);
    font.add_preset("forty", 0, 40, vec![vec![(generators::INSTRUMENT, 0)]]);
    font.add_preset("kit", 128, 0, vec![vec![(generators::INSTRUMENT, 1)]]);
    let parsed = SoundFont::load(&font.write_temp()).unwrap();

    assert_eq!(parsed.preset_index(0, 40), Some(1));
    assert_eq!(parsed.find_preset(0, 40), Some(1));
    assert_eq!(parsed.drum_preset_index(0), Some(2));
    // An undeclared program falls back to the lowest program of bank 0.
    assert_eq!(parsed.find_preset(0, 7), Some(0));
    // The drum channel never falls back onto a melodic preset.
    assert_eq!(parsed.drum_preset_index(9), None);
}

#[test]
fn shared_loading_parses_a_bank_once() {
    let mut font = Font::new("Shared");
    font.single_preset(Sample::sine("tone", 44_100, 60, 441.0, 64), 0, 127);
    let path = font.write_temp();
    let first = SoundFont::load_shared(&path).unwrap();
    let second = SoundFont::load_shared(&path).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn a_foreign_file_is_rejected_with_a_useful_message() {
    let error = SoundFont::parse(b"not a soundfont at all").unwrap_err();
    assert!(error.to_string().contains("RIFF"), "{error}");
    let error = SoundFont::parse(b"RIFF\x10\0\0\0WAVEfmt ").unwrap_err();
    assert!(error.to_string().contains("sfbk"), "{error}");
}

#[test]
fn attenuation_and_pan_records_are_parsed_onto_the_zone() {
    // The two destinations a GM bank leans on beyond pitch and cutoff: a
    // velocity-shaped `initialAttenuation` and a CC-shaped `pan`.
    let mut font = Font::new("Modulators");
    font.single_preset(Sample::sine("tone", 44_100, 60, 441.0, 64), 0, 127);
    font.presets[0].zone_modulators = vec![vec![
        ModRecord {
            source: 0x0102,
            destination: destinations::INITIAL_ATTENUATION,
            amount: 400,
            amount_source: 0,
            transform: 0,
        },
        ModRecord::cc(2, destinations::PAN, 250),
    ]];
    let parsed = SoundFont::load(&font.write_temp()).expect("the records parse");
    let zone = &parsed.presets[0].zones[0];
    assert_eq!(zone.modulators.len(), 2);
    assert_eq!(
        zone.modulators[0].destination,
        Destination::InitialAttenuation
    );
    assert_eq!(zone.modulators[0].source, ModSource::Velocity);
    assert_eq!(zone.modulators[0].amount, 400.0);
    assert_eq!(zone.modulators[1].destination, Destination::Pan);
    assert_eq!(zone.modulators[1].source, ModSource::MidiController(2));
    assert_eq!(zone.modulators[1].amount, 250.0);
    // Both records count as honoured. The terminal preset bag is not a zone,
    // so it must not produce a phantom "without an instrument" warning either.
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("honoured 2 of")),
        "{:?}",
        parsed.warnings
    );
    assert!(
        !parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("without an instrument")),
        "{:?}",
        parsed.warnings
    );
}
