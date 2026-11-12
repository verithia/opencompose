//! A pattern's `transpose` moves its pitched notes on every render path.
//!
//! The fixture bank holds one 4096-frame tone at its root key C4, so the
//! rendered pitch names the MIDI key the engine actually sent.

#[cfg(feature = "plugin-host")]
use std::collections::HashMap;
#[cfg(feature = "plugin-host")]
use std::path::{Path, PathBuf};

#[cfg(feature = "plugin-host")]
use oc_engine::block_source::{BlockSource, SourceOutput};
use oc_engine::interpret::InterpretSettings;
#[cfg(feature = "plugin-host")]
use oc_engine::interpret::MidiEvent;
#[cfg(feature = "plugin-host")]
use oc_engine::render::render_to_buffer_per_track;
#[cfg(feature = "plugin-host")]
use oc_engine::render_cache::{RangeRenderRequest, RenderCache};
#[cfg(feature = "plugin-host")]
use oc_engine::soundfont::SoundFontRenderer;

const RATE: u32 = 44_100;
/// The fixture tone's frequency at root key C4.
#[cfg(feature = "plugin-host")]
const FIXTURE_FREQUENCY: f64 = 261.66;

#[cfg(feature = "plugin-host")]
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

/// One bar with a single written C4, played through the track's `keys` preset.
/// `pattern_preset` is the preset the pattern itself names; naming `keys` keeps
/// the render on one source lane, naming `bright` splits it.
#[cfg(feature = "plugin-host")]
fn project(transpose: Option<i8>, pattern_preset: &str) -> oc_dsl::ast::Project {
    let transpose = transpose.map_or_else(String::new, |value| format!(", transpose: {value}"));
    let text = format!(
        r#"@ocp "0.2.0"
@project "pattern transpose"
  tempo: 120
  time_signature: 4/4
@preset keys
  source: soundfont
  program: 0
@preset bright
  source: soundfont
  program: 0
@pattern riff | instrument: piano/acoustic_grand, preset: {pattern_preset}{transpose}
  events:
    C4 q mf
@track keys | instrument: piano/acoustic_grand, preset: keys
  clip: riff [1:1 - 2:1]
"#
    );
    oc_dsl::parse(&text).expect("the transpose fixture parses")
}

#[cfg(feature = "plugin-host")]
fn render(project: &oc_dsl::ast::Project) -> Vec<f32> {
    render_to_buffer_per_track(project, &fixture(), RATE).expect("the fixture renders")
}

/// Frequency from positive-going zero crossings of the left channel.
#[cfg(feature = "plugin-host")]
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

/// Left channel of the first 50 ms — inside the fixture tone even when the
/// transposition shortens it, and before the chain's tail can ring.
#[cfg(feature = "plugin-host")]
fn measured_frequency(audio: &[f32]) -> f64 {
    let frames = (RATE as usize / 20).min(audio.len() / 2);
    let mono: Vec<f32> = audio[..frames * 2]
        .chunks(2)
        .map(|frame| frame[0])
        .collect();
    zero_crossing_frequency(&mono, f64::from(RATE))
}

#[cfg(feature = "plugin-host")]
fn expected_frequency(semitones: f64) -> f64 {
    FIXTURE_FREQUENCY * 2f64.powf(semitones / 12.0)
}

#[cfg(feature = "plugin-host")]
fn assert_pitch(measured: f64, semitones: f64, context: &str) {
    let expected = expected_frequency(semitones);
    assert!(
        (measured - expected).abs() / expected < 0.01,
        "{context}: measured {measured:.2} Hz, expected {expected:.2} Hz for {semitones} semitones"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_negative_transpose_sounds_the_written_note_two_semitones_lower() {
    assert_pitch(
        measured_frequency(&render(&project(Some(-2), "keys"))),
        -2.0,
        "-2",
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_positive_transpose_sounds_the_written_note_seven_semitones_higher() {
    assert_pitch(
        measured_frequency(&render(&project(Some(7), "keys"))),
        7.0,
        "+7",
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_zero_transpose_renders_the_same_bytes_as_an_absent_field() {
    assert_eq!(
        render(&project(Some(0), "keys")),
        render(&project(None, "keys")),
        "transpose: 0 and no field at all must render identically"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_range_render_carries_the_pattern_transposition() {
    let project = project(Some(-2), "keys");
    let mut cache = RenderCache::new();
    let request = RangeRenderRequest {
        start_sample: 0,
        end_sample: u64::from(RATE) / 4,
        tracks: None,
        stems: false,
    };
    let mut plugins = HashMap::new();
    let output = cache
        .render_range(&project, &fixture(), RATE, &mut plugins, request)
        .expect("the range renders");
    assert_eq!(output.frames, RATE as usize / 4);
    assert_pitch(measured_frequency(&output.audio), -2.0, "range render");
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_clip_whose_pattern_names_another_preset_still_transposes() {
    // The pattern's preset differs from the hosting track's, so the renderer
    // splits the track into source lanes before it plays the clip.
    let project = project(Some(-2), "bright");
    let mut cache = RenderCache::new();
    let request = RangeRenderRequest {
        start_sample: 0,
        end_sample: u64::from(RATE) / 4,
        tracks: None,
        stems: false,
    };
    let mut plugins = HashMap::new();
    let output = cache
        .render_range(&project, &fixture(), RATE, &mut plugins, request)
        .expect("the split-lane range renders");
    assert_pitch(measured_frequency(&output.audio), -2.0, "split source lane");
}

/// The streaming scheduler: events are sent when their sample time arrives and
/// the source renders caller-reserved blocks.
#[cfg(feature = "plugin-host")]
fn stream_blocks(
    source: &mut BlockSource,
    events: &[MidiEvent],
    frames: usize,
    block: usize,
) -> Vec<f32> {
    let mut output = SourceOutput::new(0, &[]);
    let mut audio = Vec::with_capacity(frames * 2);
    let mut cursor = 0usize;
    let mut position = 0usize;
    while position < frames {
        while cursor < events.len() && events[cursor].time_samples as usize <= position {
            source
                .send(&events[cursor].event)
                .expect("streaming event dispatch");
            cursor += 1;
        }
        let next = events
            .get(cursor)
            .map(|event| event.time_samples as usize)
            .unwrap_or(frames);
        let count = block.min(frames - position).min((next - position).max(1));
        let transport = oc_host::PluginTransport::new(position as u64, RATE, 120.0);
        source.render_into(count, RATE, transport, &mut output);
        audio.extend_from_slice(output.main());
        position += count;
    }
    audio
}

#[test]
#[cfg(feature = "plugin-host")]
fn streaming_playback_sounds_the_same_transposed_pitch_as_the_offline_render() {
    let project = project(Some(7), "keys");
    let settings = InterpretSettings {
        sample_rate: RATE,
        ..Default::default()
    };
    let events =
        oc_engine::interpret::try_interpret(&project, &settings).expect("the project plays");
    let frames = RATE as usize / 4;

    let mut renderer = SoundFontRenderer::new(RATE, &fixture()).expect("the bank loads");
    let offline = renderer
        .try_render_events(&events, frames as f64 / f64::from(RATE))
        .expect("the offline player renders");
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "keys")
        .expect("the track preset");
    let mut source = BlockSource::new(Some(preset), &Vec::new(), &fixture(), RATE, 120.0, None)
        .expect("the streaming source builds");
    let streamed = stream_blocks(&mut source, &events, frames, 256);

    assert_eq!(streamed.len(), offline.len(), "streaming block underrun");
    let mut worst = 0.0f32;
    for (streamed, offline) in streamed.iter().zip(&offline) {
        worst = worst.max((streamed - offline).abs());
    }
    assert_eq!(worst, 0.0, "streaming differs from the offline player");
    assert_pitch(measured_frequency(&streamed), 7.0, "streaming");
    assert_pitch(
        measured_frequency(&render(&project)),
        7.0,
        "offline full render",
    );
}

#[test]
fn drum_hits_ignore_the_pattern_transposition() {
    let _warnings = crate::RENDER_WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let source = |transpose: &str| {
        format!(
            r#"@ocp "0.2.0"
@project "hits"
  tempo: 120
@preset kit
  source: soundfont
@pattern beat | instrument: drums/acoustic_kit, preset: kit{transpose}
  hits:
    kick 1:0 mf
    snare 1:2 mf
@track drums | instrument: drums/acoustic_kit, preset: kit
  clip: beat [1:1 - 2:1]
"#
        )
    };
    let settings = InterpretSettings {
        sample_rate: RATE,
        ..Default::default()
    };
    let keys = |text: &str| {
        let project = oc_dsl::parse(text).expect("the hit fixture parses");
        oc_engine::interpret::try_interpret(&project, &settings)
            .expect("the hit fixture plays")
            .into_iter()
            .filter_map(|event| match event.event {
                oc_engine::interpret::MidiMessage::NoteOn { note, .. } => Some(note),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    oc_engine::render_warnings::clear();
    let plain = keys(&source(""));
    let transposed = keys(&source(", transpose: -2"));
    assert_eq!(plain, transposed, "a hit is a sample key, not a pitch");
    let warnings = oc_engine::render_warnings::take();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("transpose") && warning.contains("hits")),
        "the render must report the ignored transposition: {warnings:?}"
    );
}
