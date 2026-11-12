//! SF3 banks: Ogg Vorbis sample data inside the SoundFont 2 container.
//!
//! The fixture is a one-second 22050 Hz tone (441 Hz with 882 Hz and 2205 Hz
//! partials), synthesised for this test and encoded to Ogg Vorbis with ffmpeg's
//! Vorbis encoder; it is our own generated signal, released CC0.

use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::Path;

use oc_engine::interpret::{MidiEvent, MidiMessage};
use oc_engine::sf2::{SampleMode, Sf2Renderer, SoundFont};
use sf2_analysis::{dominant_frequency, mono, rms};
use sf2_builder::{Font, Sample, generators, range};

const TONE: &[u8] = include_bytes!("../fixtures/sf3-tone-22050.ogg");
const TONE_RATE: u32 = 22_050;
const TONE_FRAMES: u32 = 22_050;

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

/// One Ogg sample, one instrument, one preset. `loop_mode` is the
/// `sampleModes` generator: 0 plays once, 1 loops continuously.
fn ogg_font(loop_mode: i16) -> Font {
    let mut font = Font::new("SF3 Test Tone");
    font.compressed = true;
    let sample = font.add_sample(Sample::ogg(
        "Tone",
        TONE_RATE,
        60,
        TONE_FRAMES as usize,
        0,
        TONE_FRAMES,
        TONE.to_vec(),
    ));
    let instrument = font.add_instrument(
        "Tone Instrument",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
            (generators::SAMPLE_MODES, loop_mode),
        ]],
    );
    font.add_preset(
        "Tone Preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    font
}

fn event(time_samples: u64, event: MidiMessage) -> MidiEvent {
    MidiEvent {
        time_samples,
        event,
        track_index: 0,
    }
}

fn note_on(note: u8) -> MidiEvent {
    event(
        0,
        MidiMessage::NoteOn {
            channel: 0,
            note,
            velocity: 100,
        },
    )
}

fn render(path: &Path, events: Vec<MidiEvent>, seconds: f64) -> Vec<f32> {
    let mut renderer = Sf2Renderer::new(48_000, path).expect("bank loads");
    renderer
        .try_render_events(&events, seconds)
        .expect("render")
}

/// Locate the last chunk with this identifier and return its payload offset.
fn chunk_payload(bytes: &[u8], id: &[u8; 4]) -> Option<usize> {
    let position = bytes.windows(4).rposition(|window| window == id)?;
    Some(position + 8)
}

#[test]
fn an_ogg_sample_decodes_into_the_existing_preset_model() {
    let path = ogg_font(0).write_temp();
    let font = SoundFont::load(&path).expect("the SF3 bank loads");

    assert!(font.compressed);
    assert_eq!(font.sample_bank.bit_depth(), 16);
    // The fixture decodes to a whole number of Vorbis blocks; the header's
    // byte span is replaced by the decoded frame count, not by its byte length.
    assert_eq!(font.sample_bank.frames(), 22_080);
    assert_eq!(font.sample_headers.len(), 1);
    let header = &font.sample_headers[0];
    assert_eq!(header.name, "Tone");
    assert_eq!((header.start, header.end), (0, 22_080));
    assert_eq!(header.rate, TONE_RATE);
    // SF3 loop points are relative to the sample start; the header here asks
    // for the whole decoded sample, so it ends at the decoded frame count.
    assert_eq!((header.loop_start, header.loop_end), (0, TONE_FRAMES));

    let preset = &font.presets[0];
    assert_eq!(preset.zones.len(), 1);
    let zone = &preset.zones[0];
    assert_eq!((zone.start, zone.end), (0, 22_080));
    assert_eq!((zone.loop_start, zone.loop_end), (0, 22_050));
    assert_eq!(zone.root_key, 60);
    assert_eq!(zone.sample_mode, SampleMode::NoLoop);
}

#[test]
fn an_ogg_sample_plays_at_the_rate_and_pitch_its_header_declares() {
    let path = ogg_font(0).write_temp();
    let buffer = render(&path, vec![note_on(60)], 0.5);
    let channel = mono(&buffer);
    assert!(rms(&channel) > 0.01, "the decoded sample sounds");
    // Root key 60 plays the 441 Hz tone untransposed.
    let frequency = dominant_frequency(&channel, 48_000.0, 380.0, 520.0, 1.0);
    assert!(
        (frequency - 441.0).abs() < 4.0,
        "measured {frequency:.1} Hz, expected 441 Hz"
    );
}

#[test]
fn a_compressed_loop_keeps_sounding_past_the_sample_end() {
    let path = ogg_font(1).write_temp();
    // The sample lasts one second; the note is held for 1.6 s without a
    // note-off, so only the loop can keep it sounding.
    let buffer = render(&path, vec![note_on(60)], 1.6);
    let channel = mono(&buffer);
    let tail = &channel[52_800..76_800]; // 1.1 s .. 1.6 s
    assert!(
        rms(tail) > 0.01,
        "the loop still sounds after the sample end"
    );
}

#[test]
fn a_bank_can_mix_raw_pcm_and_ogg_samples() {
    let mut font = Font::new("Mixed SF3");
    font.compressed = true;
    let ogg = font.add_sample(Sample::ogg(
        "Ogg Tone",
        TONE_RATE,
        60,
        TONE_FRAMES as usize,
        0,
        TONE_FRAMES,
        TONE.to_vec(),
    ));
    let raw = font.add_sample(Sample::sine("Raw Tone", 44_100, 60, 441.0, 1000));
    font.add_instrument(
        "Ogg Instrument",
        vec![vec![
            (generators::SAMPLE_ID, ogg as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
        ]],
    );
    font.add_instrument(
        "Raw Instrument",
        vec![vec![
            (generators::SAMPLE_ID, raw as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
        ]],
    );
    // Both presets play program 0; the last one wins on a program change, so
    // give them different programs to address them separately.
    font.add_preset("Ogg Preset", 0, 0, vec![vec![(generators::INSTRUMENT, 0)]]);
    font.add_preset("Raw Preset", 0, 1, vec![vec![(generators::INSTRUMENT, 1)]]);

    let path = font.write_temp();
    let parsed = SoundFont::load(&path).expect("the mixed bank loads");
    assert!(parsed.compressed);
    assert_eq!(parsed.sample_headers.len(), 2);
    // Regions stay in file order and become frame-contiguous: the Ogg sample's
    // decoded frames first, then the raw sample's 1000 frames.
    assert_eq!(parsed.sample_headers[0].start, 0);
    assert_eq!(parsed.sample_headers[0].end, 22_080);
    assert_eq!(parsed.sample_headers[1].start, 22_080);
    assert_eq!(parsed.sample_headers[1].end, 23_080);
    assert_eq!(parsed.sample_bank.frames(), 23_080);
    assert_eq!(parsed.presets.len(), 2);
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("raw PCM")),
        "the mixed bank is reported: {:?}",
        parsed.warnings
    );
}

#[test]
fn a_corrupt_vorbis_sample_is_refused_with_its_name() {
    // A stream cut inside its last page still starts with an Ogg page header,
    // so it reaches the decoder as Vorbis and must be refused there.
    let cut = TONE[..TONE.len() * 3 / 5].to_vec();
    let mut font = Font::new("Corrupt SF3");
    font.compressed = true;
    font.single_preset(
        Sample::ogg("Broken Tone", 22050, 60, 11_025, 0, 11_025, cut),
        0,
        127,
    );
    let error = SoundFont::load(&font.write_temp()).expect_err("a cut stream must be refused");
    let text = error.to_string();
    assert!(
        text.contains("Broken Tone"),
        "message names the sample: {text}"
    );
    assert!(text.contains("cut short"), "message says why: {text}");
}

#[test]
fn a_damaged_vorbis_page_is_refused_with_its_name() {
    // Keep the page chain intact, so the sample is still recognised as Vorbis,
    // but destroy packet bytes inside it: the decoder reports the bad page.
    let mut damaged = TONE.to_vec();
    damaged[160..260].fill(0);
    let mut font = Font::new("Damaged SF3");
    font.compressed = true;
    font.single_preset(
        Sample::ogg("Damaged Tone", 22050, 60, 11_025, 0, 11_025, damaged),
        0,
        127,
    );
    let error = SoundFont::load(&font.write_temp()).expect_err("a damaged header must be refused");
    let text = error.to_string();
    assert!(
        text.contains("Damaged Tone"),
        "message names the sample: {text}"
    );
}

#[test]
fn a_truncated_file_is_refused_at_load() {
    let bytes = ogg_font(0).bytes();
    let cut = &bytes[..bytes.len() * 7 / 10];
    let path = std::env::temp_dir().join(format!("oc-sf3-cut-{}.sf2", std::process::id()));
    std::fs::write(&path, cut).expect("write cut bank");
    let error = SoundFont::load(&path).expect_err("a truncated file must be refused");
    let text = error.to_string();
    assert!(
        text.contains("chunk") || text.contains("truncated") || text.contains("cut short"),
        "message explains the cut: {text}"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_sample_that_points_past_the_sample_data_is_refused_by_name() {
    let mut bytes = ogg_font(0).bytes();
    let shdr = chunk_payload(&bytes, b"shdr").expect("the writer emits an shdr chunk");
    // End offset of the first sample record is at +24 from the record start.
    bytes[shdr + 24..shdr + 28].copy_from_slice(&99_999u32.to_le_bytes());
    let path = std::env::temp_dir().join(format!("oc-sf3-past-{}.sf2", std::process::id()));
    std::fs::write(&path, &bytes).expect("write patched bank");
    let error = SoundFont::load(&path).expect_err("a sample past the data must be refused");
    let text = error.to_string();
    assert!(text.contains("Tone"), "message names the sample: {text}");
    assert!(text.contains("truncated"), "message explains why: {text}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_raw_soundfont_still_takes_the_pcm_path_whatever_its_name() {
    let font = SoundFont::load(&fixture()).expect("the raw fixture loads");
    assert!(!font.compressed);
    assert_eq!(font.sample_bank.bit_depth(), 16);
    assert_eq!(font.sample_bank.frames(), 4096);
    assert_eq!(
        (font.sample_headers[0].start, font.sample_headers[0].end),
        (0, 4096)
    );

    // The same bytes under a `.sf3` name are still raw PCM: dispatch is on
    // content, not on the extension.
    let path = std::env::temp_dir().join(format!("oc-raw-as-sf3-{}.sf3", std::process::id()));
    std::fs::copy(fixture(), &path).expect("copy fixture");
    let renamed = SoundFont::load(&path).expect("raw PCM named .sf3 loads");
    let _ = std::fs::remove_file(path);
    assert!(!renamed.compressed);
    assert_eq!(renamed.sample_bank.frames(), font.sample_bank.frames());
    assert_eq!(
        renamed.presets[0].zones[0].end,
        font.presets[0].zones[0].end
    );
    assert!((renamed.sample_bank.point(100) - font.sample_bank.point(100)).abs() < 1e-9);
}

/// Four resources let a middle program prove both address preservation and
/// omitted decoding. The next raw resource is needed for linear interpolation;
/// the first and final resources are unrelated unless a generator crosses them.
fn streaming_subset_font(crosses_previous_sample: bool) -> Font {
    let mut font = Font::new("Prepared SF3 resources");
    font.compressed = true;
    font.add_sample(Sample::sine("Previous raw", TONE_RATE, 60, 330.0, 512));
    font.add_sample(Sample::ogg(
        "Middle Ogg",
        TONE_RATE,
        60,
        TONE_FRAMES as usize,
        0,
        TONE_FRAMES,
        TONE.to_vec(),
    ));
    font.add_sample(Sample::sine(
        "Interpolation neighbor",
        TONE_RATE,
        60,
        550.0,
        512,
    ));
    font.add_sample(Sample::ogg(
        "Unrelated Ogg",
        TONE_RATE,
        60,
        TONE_FRAMES as usize,
        0,
        TONE_FRAMES,
        TONE.to_vec(),
    ));
    for index in 0..4 {
        let mut zone = vec![
            (generators::SAMPLE_ID, index as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
        ];
        if index == 1 && crosses_previous_sample {
            zone.push((generators::START_ADDRS_OFFSET, -25));
            zone.push((generators::END_ADDRS_OFFSET, 40));
        }
        let instrument = font.add_instrument(&format!("Resource {index}"), vec![zone]);
        let (bank, program) = if index == 3 {
            (5, 2)
        } else {
            (0, index as u16)
        };
        font.add_preset(
            &format!("Program {index}"),
            bank,
            program,
            vec![vec![(generators::INSTRUMENT, instrument as i16)]],
        );
    }
    font
}

fn middle_program_events() -> Vec<MidiEvent> {
    vec![
        event(
            0,
            MidiMessage::ProgramChange {
                channel: 0,
                program: 1,
            },
        ),
        note_on(60),
        event(
            16_000,
            MidiMessage::NoteOff {
                channel: 0,
                note: 60,
            },
        ),
    ]
}

#[test]
fn prepared_sf3_middle_program_keeps_absolute_addresses_and_only_decodes_its_closure() {
    let directory = tempfile::tempdir().unwrap();
    let path = streaming_subset_font(false).write(&directory.path().join("subset.sf3"));
    let events = middle_program_events();
    let mut prepared = Sf2Renderer::new_streaming(
        48_000,
        &path,
        events.iter().map(|event| &event.event),
        false,
    )
    .unwrap();
    let full = SoundFont::load(&path).unwrap();
    assert_eq!(
        prepared.font().sample_bank.frames(),
        full.sample_bank.frames()
    );
    for (actual, expected) in prepared
        .font()
        .sample_headers
        .iter()
        .zip(&full.sample_headers)
    {
        assert_eq!(
            (actual.start, actual.end, actual.loop_start, actual.loop_end),
            (
                expected.start,
                expected.end,
                expected.loop_start,
                expected.loop_end
            )
        );
    }
    let first = &full.sample_headers[0];
    assert!(
        (first.start..first.end)
            .all(|frame| prepared.font().sample_bank.point(frame as usize) == 0.0)
    );
    let last = &full.sample_headers[3];
    assert!(
        (last.start..last.end)
            .all(|frame| prepared.font().sample_bank.point(frame as usize) == 0.0)
    );
    let neighbor = &full.sample_headers[2];
    assert!(
        (neighbor.start..neighbor.end).all(|frame| prepared
            .font()
            .sample_bank
            .point(frame as usize)
            == full.sample_bank.point(frame as usize))
    );
    assert!(prepared.font().warnings.iter().any(|warning| warning ==
        "prepared 2 of 4 SF3 samples for admitted streaming programs"));
    let actual = prepared.try_render_events(&events, 0.6).unwrap();
    let expected = Sf2Renderer::from_font(std::sync::Arc::new(full), 48_000)
        .try_render_events(&events, 0.6)
        .unwrap();
    assert_eq!(actual, expected);
    assert!(actual.iter().any(|sample| sample.abs() > 0.001));
}

#[test]
fn prepared_sf3_generator_offsets_include_previous_and_following_resources() {
    let directory = tempfile::tempdir().unwrap();
    let path = streaming_subset_font(true).write(&directory.path().join("crossing.sf3"));
    let events = middle_program_events();
    let mut prepared = Sf2Renderer::new_streaming(
        48_000,
        &path,
        events.iter().map(|event| &event.event),
        false,
    )
    .unwrap();
    assert!(prepared.font().warnings.iter().any(|warning| warning ==
        "prepared 3 of 4 SF3 samples for admitted streaming programs"));
    let actual = prepared.try_render_events(&events, 0.6).unwrap();
    let expected = Sf2Renderer::new(48_000, &path)
        .unwrap()
        .try_render_events(&events, 0.6)
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn prepared_sf3_late_bank_program_reset_and_missing_program_match_the_full_player() {
    let directory = tempfile::tempdir().unwrap();
    let path = streaming_subset_font(false).write(&directory.path().join("future.sf3"));
    let mut events = middle_program_events();
    events.extend([
        event(
            18_000,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 0,
                value: 0,
            },
        ),
        event(
            18_000,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 32,
                value: 5,
            },
        ),
        event(
            18_000,
            MidiMessage::ProgramChange {
                channel: 0,
                program: 2,
            },
        ),
        event(
            18_000,
            MidiMessage::NoteOn {
                channel: 0,
                note: 64,
                velocity: 100,
            },
        ),
        event(
            22_000,
            MidiMessage::ControlChange {
                channel: 0,
                controller: 121,
                value: 0,
            },
        ),
        event(
            23_000,
            MidiMessage::NoteOff {
                channel: 0,
                note: 64,
            },
        ),
        event(
            25_000,
            MidiMessage::ProgramChange {
                channel: 0,
                program: 99,
            },
        ),
        event(
            25_000,
            MidiMessage::NoteOn {
                channel: 0,
                note: 67,
                velocity: 100,
            },
        ),
    ]);
    let mut prepared = Sf2Renderer::new_streaming(
        48_000,
        &path,
        events.iter().map(|event| &event.event),
        false,
    )
    .unwrap();
    let expected = Sf2Renderer::new(48_000, &path)
        .unwrap()
        .try_render_events(&events, 0.8)
        .unwrap();
    assert_eq!(prepared.try_render_events(&events, 0.8).unwrap(), expected);
    prepared.reset();
    assert_eq!(prepared.try_render_events(&events, 0.8).unwrap(), expected);
}

#[test]
fn prepared_sf3_unprepared_program_refusal_preserves_existing_voice_and_channel() {
    let directory = tempfile::tempdir().unwrap();
    let path = streaming_subset_font(false).write(&directory.path().join("refusal.sf3"));
    let events = middle_program_events();
    let mut first = Sf2Renderer::new_streaming(
        48_000,
        &path,
        events.iter().map(|event| &event.event),
        false,
    )
    .unwrap();
    let mut second = Sf2Renderer::new_streaming(
        48_000,
        &path,
        events.iter().map(|event| &event.event),
        false,
    )
    .unwrap();
    for event in &events[..2] {
        first.send(&event.event).unwrap();
        second.send(&event.event).unwrap();
    }
    assert_eq!(first.render_samples(256), second.render_samples(256));
    let voices = first.active_voices();
    let error = first
        .send(&MidiMessage::ProgramChange {
            channel: 0,
            program: 0,
        })
        .unwrap_err();
    assert!(error.contains("requires streaming preparation"));
    assert_eq!(first.active_voices(), voices);
    let retrigger = MidiMessage::NoteOn {
        channel: 0,
        note: 67,
        velocity: 100,
    };
    first.send(&retrigger).unwrap();
    second.send(&retrigger).unwrap();
    assert_eq!(first.render_samples(1024), second.render_samples(1024));
}

#[test]
fn prepared_sf3_missing_drum_kit_remains_silent_without_melodic_substitution() {
    let directory = tempfile::tempdir().unwrap();
    let path = streaming_subset_font(false).write(&directory.path().join("drum.sf3"));
    let events = vec![event(
        0,
        MidiMessage::NoteOn {
            channel: 9,
            note: 36,
            velocity: 100,
        },
    )];
    let mut prepared =
        Sf2Renderer::new_streaming(48_000, &path, events.iter().map(|event| &event.event), true)
            .unwrap();
    let mut full = Sf2Renderer::new(48_000, &path).unwrap();
    full.enable_drum_channel();
    let actual = prepared.try_render_events(&events, 0.05).unwrap();
    assert!(actual.iter().all(|sample| *sample == 0.0));
    assert_eq!(actual, full.try_render_events(&events, 0.05).unwrap());
}

/// A rebuild that admits the same programs must not read, index and decode the
/// bank again: the prepared bank is kept for exactly that admission table.
#[test]
fn a_second_streaming_load_reuses_the_prepared_bank() {
    use std::num::NonZeroUsize;
    let mut font = ogg_font(0);
    // A second preset, so a different program set has a different admission.
    let sample = font.add_sample(Sample::ogg(
        "Tone 2",
        TONE_RATE,
        60,
        TONE_FRAMES as usize,
        0,
        TONE_FRAMES,
        TONE.to_vec(),
    ));
    let instrument = font.add_instrument(
        "Tone Instrument 2",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::KEY_RANGE, range(0, 127) as i16),
        ]],
    );
    font.add_preset(
        "Tone Preset 2",
        0,
        1,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    let path = font.write_temp();
    let options =
        || oc_engine::render_parallel::RenderWorkerOptions::new(NonZeroUsize::new(2).unwrap());
    let program = |program: u8| {
        [MidiMessage::ProgramChange {
            channel: 0,
            program,
        }]
    };

    let first =
        Sf2Renderer::new_streaming_with_options(44_100, &path, program(0).iter(), false, options())
            .unwrap();
    // Another test running in parallel may evict the entry, so allow a retry.
    let reused = (0..4).any(|_| {
        let again = Sf2Renderer::new_streaming_with_options(
            44_100,
            &path,
            program(0).iter(),
            false,
            options(),
        )
        .unwrap();
        std::ptr::eq(first.font(), again.font())
    });
    assert!(
        reused,
        "the prepared bank was prepared again for the same program set"
    );

    let other =
        Sf2Renderer::new_streaming_with_options(44_100, &path, program(1).iter(), false, options())
            .unwrap();
    assert!(
        !std::ptr::eq(first.font(), other.font()),
        "a different program set must decode its own subset"
    );
}
