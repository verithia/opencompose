//! The SoundFont adapter: every render path reaches the same player.
//!
//! The adapter is exercised where the shipping code uses it — the offline
//! renderer, the stems export, the cache's per-font groups, the streaming block
//! source — and the native player's own guarantees (voice cap, determinism) are
//! pinned here rather than in the player module.
#![cfg(feature = "plugin-host")]

// The all.rs integration binary owns these shared SoundFont helpers once.
use crate::sf2_analysis;
use crate::sf2_builder;

use std::path::{Path, PathBuf};

use oc_engine::block_source::{BlockSource, MAX_SOURCE_BLOCK_FRAMES, SourceOutput};
use oc_engine::interpret::{self, InterpretSettings, MidiEvent, MidiMessage};
use oc_engine::render::{
    render_to_buffer_per_track, render_to_buffer_with_plugins, render_with_stems,
    write_stems_export,
};
use oc_engine::render_cache::RenderCache;
use oc_engine::sf2::Sf2Renderer;
use oc_engine::soundfont::SoundFontRenderer;
use sf2_analysis::{dominant_frequency, left, mono, right, rms};
use sf2_builder::{Font, Sample, generators};

const SAMPLE_RATE: u32 = 48_000;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

fn temp_dir(name: &str) -> PathBuf {
    oc_test_support::temp_dir("ocp-sf2-engine", name)
}

fn copy_fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::copy(fixture(), &path).expect("copy fixture font");
    path
}

const ONE_TRACK: &str = r#"@ocp "0.2.0"

@project "SoundFont one track"
  tempo: 120
  time_signature: 4/4

@preset keys
  source: soundfont
  program: 0

@track keys | preset: keys, instrument: piano/acoustic_grand
  @phrase held [1:1 - 2:1]
    C4 w ff
"#;

/// Two tracks that declare different font files, panned apart so a render
/// makes it visible which groups produced audio.
fn two_font_project(left_font: &Path, right_font: &Path) -> String {
    format!(
        r#"@ocp "0.2.0"

@project "SoundFont two fonts"
  tempo: 120
  time_signature: 4/4

@preset left_font
  source: soundfont
  program: 0
  file: "{left}"

@preset right_font
  source: soundfont
  program: 0
  file: "{right}"

@track left | preset: left_font, instrument: piano/acoustic_grand
  mix: pan=-1
  @phrase held [1:1 - 2:1]
    C4 w ff

@track right | preset: right_font, instrument: piano/acoustic_grand
  mix: pan=1
  @phrase held [1:1 - 2:1]
    E4 w ff
"#,
        left = left_font.display(),
        right = right_font.display(),
    )
}

fn interpret_events(project: &oc_dsl::ast::Project) -> Vec<MidiEvent> {
    interpret::try_interpret(
        project,
        &InterpretSettings {
            sample_rate: SAMPLE_RATE,
            ..Default::default()
        },
    )
    .expect("project interprets")
}

/// The offline scheduler: dispatch every event at or before the next frame,
/// render small blocks, repeat.
fn render_events_offline(
    renderer: &mut SoundFontRenderer,
    events: &[MidiEvent],
    frames: usize,
) -> Vec<f32> {
    let duration = frames as f64 / f64::from(SAMPLE_RATE);
    let audio = renderer
        .try_render_events(events, duration)
        .expect("offline render");
    assert_eq!(audio.len(), frames * 2, "offline frame count");
    audio
}

/// The streaming scheduler: same event rule, caller-reserved block buffer.
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
        let transport = oc_host::PluginTransport::new(position as u64, SAMPLE_RATE, 120.0);
        source.render_into(count, SAMPLE_RATE, transport, &mut output);
        audio.extend_from_slice(output.main());
        position += count;
    }
    audio
}

#[test]
fn the_engine_adapter_loads_a_bank_at_the_output_rate() {
    let mut renderer = SoundFontRenderer::new(SAMPLE_RATE, &fixture()).expect("bank loads");
    assert_eq!(renderer.sample_rate(), SAMPLE_RATE);
    renderer
        .send(&MidiMessage::ProgramChange {
            channel: 0,
            program: 0,
        })
        .expect("program");
    renderer
        .send(&MidiMessage::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        })
        .expect("note on");
    let audio = renderer.render_samples(4_800);
    assert_eq!(audio.len(), 9_600);
    assert!(
        audio.iter().any(|sample| sample.abs() > 1e-6),
        "the adapter rendered silence"
    );
}

#[test]
fn offline_render_plays_a_soundfont_track_through_the_native_engine() {
    let project = oc_dsl::parse(ONE_TRACK).expect("project parses");
    oc_dsl::validate(&project).expect("project validates");
    let audio = render_to_buffer_per_track(&project, &fixture(), SAMPLE_RATE)
        .expect("offline render succeeds");
    assert!(audio.len() >= SAMPLE_RATE as usize, "too few frames");
    assert!(audio.iter().all(|sample| sample.is_finite()));
    let level = rms(&left(&audio));
    assert!(level > 0.004, "soundfont track is silent: {level}");
}

#[test]
fn stems_export_carries_the_soundfont_track() {
    let project = oc_dsl::parse(ONE_TRACK).expect("project parses");
    let dir = temp_dir("stems");
    let mut plugins = std::collections::HashMap::new();
    let export = render_with_stems(
        &project,
        &fixture(),
        SAMPLE_RATE,
        &mut plugins,
        None,
        0.0,
        0.0,
    )
    .expect("stems render");
    assert!(!export.mix.is_empty());
    let manifest = write_stems_export(
        &project,
        &dir,
        "soundfont",
        &export,
        oc_engine::encode::EncodeSpec::for_project(&project, SAMPLE_RATE),
    )
    .expect("stems written");
    let manifest_text = std::fs::read_to_string(&manifest).expect("manifest is readable");
    assert!(
        manifest_text.contains("keys"),
        "track stem missing from manifest: {manifest_text}"
    );
    let stem = dir.join("soundfont-keys.wav");
    let mut reader = hound::WavReader::open(&stem).expect("track stem exists");
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|sample| f32::from(sample.expect("sample")) / 32768.0)
        .collect();
    assert_eq!(reader.spec().channels, 2);
    assert!(
        rms(&left(&samples)) > 0.004,
        "track stem is silent after the engine switch: {}",
        rms(&left(&samples))
    );
}

#[test]
fn tracks_declaring_different_fonts_render_in_separate_groups() {
    let dir = temp_dir("groups");
    let first = copy_fixture(&dir, "first.sf2");
    let second = copy_fixture(&dir, "second.sf2");
    let project = oc_dsl::parse(&two_font_project(&first, &second)).expect("project parses");
    oc_dsl::validate(&project).expect("project validates");
    // The cache groups tracks by the font their preset names, so the fallback
    // path is never opened: passing a missing one proves the grouping.
    let mut plugins = std::collections::HashMap::new();
    let audio = render_to_buffer_with_plugins(
        &project,
        Path::new("missing-fallback.sf2"),
        SAMPLE_RATE,
        &mut plugins,
    )
    .expect("two-font render succeeds");
    let left_level = rms(&left(&audio));
    let right_level = rms(&right(&audio));
    assert!(
        left_level > 0.004,
        "the first font group rendered nothing: {left_level}"
    );
    assert!(
        right_level > 0.004,
        "the second font group rendered nothing: {right_level}"
    );

    // The same font on both tracks must still sum, not cancel.
    let same = two_font_project(&first, &first);
    let project = oc_dsl::parse(&same).expect("project parses");
    let audio = render_to_buffer_with_plugins(
        &project,
        Path::new("missing-fallback.sf2"),
        SAMPLE_RATE,
        &mut plugins,
    )
    .expect("single-font render");
    assert!(rms(&left(&audio)) > 0.004 && rms(&right(&audio)) > 0.004);
}

#[test]
fn streamed_blocks_match_the_offline_render_of_the_same_range() {
    let project = oc_dsl::parse(ONE_TRACK).expect("project parses");
    let events = interpret_events(&project);
    let frames = SAMPLE_RATE as usize * 3;

    let mut offline = SoundFontRenderer::new(SAMPLE_RATE, &fixture()).expect("bank loads");
    let expected = render_events_offline(&mut offline, &events, frames);

    let track = &project.tracks[0];
    let preset = project
        .presets
        .iter()
        .find(|preset| Some(&preset.name) == track.preset.as_ref())
        .expect("track preset");
    let mut source = BlockSource::new(
        Some(preset),
        &Vec::new(),
        &fixture(),
        SAMPLE_RATE,
        project.properties.tempo,
        None,
    )
    .expect("streaming source builds");
    let streamed = stream_blocks(&mut source, &events, frames, MAX_SOURCE_BLOCK_FRAMES);

    assert_eq!(streamed.len(), expected.len(), "block underrun");
    assert!(
        expected.iter().any(|sample| sample.abs() > 1e-4),
        "the parity range is silent"
    );
    let mut worst = 0.0f32;
    for (streamed, offline) in streamed.iter().zip(&expected) {
        worst = worst.max((streamed - offline).abs());
    }
    assert_eq!(
        worst, 0.0,
        "streamed blocks differ from the offline render by {worst}"
    );
}

#[test]
fn the_drum_channel_plays_the_bank_128_kit_while_streaming() {
    let mut font = Font::new("Kit");
    let kick = font.add_sample(Sample::sine("kick", 44_100, 36, 880.0, 4096));
    font.add_sample(Sample::sine("melodic", 44_100, 60, 220.0, 4096));
    let kit = font.add_instrument(
        "kit",
        vec![vec![
            (generators::SAMPLE_ID, kick as i16),
            (generators::KEY_RANGE, sf2_builder::range(36, 36) as i16),
        ]],
    );
    font.add_preset(
        "kit",
        128,
        0,
        vec![vec![(generators::INSTRUMENT, kit as i16)]],
    );
    let path = font.write_temp();

    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Drum"
  tempo: 120
@preset kit
  source: soundfont
  program: 0
@track drums | preset: kit, instrument: drums/kit
  @phrase hit [1:1 - 1:2]
    C4 q mf
"#,
    )
    .expect("project parses");
    let track = &project.tracks[0];
    assert_eq!(track.instrument.category, "drums");
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "kit")
        .expect("preset");
    let mut source = BlockSource::new(Some(preset), &Vec::new(), &path, SAMPLE_RATE, 120.0, None)
        .expect("streaming source builds");
    // Channel 10 reaches the kit only because the source enables the drum channel.
    let events = vec![
        MidiEvent {
            time_samples: 0,
            track_index: 0,
            event: MidiMessage::ProgramChange {
                channel: 9,
                program: 0,
            },
        },
        MidiEvent {
            time_samples: 0,
            track_index: 0,
            event: MidiMessage::NoteOn {
                channel: 9,
                note: 36,
                velocity: 100,
            },
        },
    ];
    let audio = stream_blocks(&mut source, &events, SAMPLE_RATE as usize / 2, 128);
    let measured = dominant_frequency(&mono(&audio), f64::from(SAMPLE_RATE), 600.0, 1200.0, 1.0);
    assert!(
        (measured - 880.0).abs() < 20.0,
        "drum channel played {measured:.1} Hz"
    );
}

#[test]
fn voice_stealing_keeps_rendering_inside_the_reserved_block() {
    // One zone per note, so every note-on allocates a voice: well past the cap.
    let mut player = Sf2Renderer::new(SAMPLE_RATE, &fixture()).expect("bank loads");
    player.enable_drum_channel();
    for channel in (0..16u8).filter(|channel| *channel != 9) {
        player
            .send(&MidiMessage::ProgramChange {
                channel,
                program: 0,
            })
            .expect("program");
    }
    for index in 0..400u16 {
        player
            .send(&MidiMessage::NoteOn {
                channel: (index % 15) as u8,
                note: 48 + (index % 24) as u8,
                velocity: 100,
            })
            .expect("note on");
    }
    assert!(
        player.active_voices() <= 128,
        "voice pool overran its cap: {}",
        player.active_voices()
    );
    // Render into exactly one reserved block's worth of frames, repeatedly.
    let mut block = [0.0f32; MAX_SOURCE_BLOCK_FRAMES * 2];
    for _ in 0..32 {
        player.render_into(&mut block);
        assert!(block.iter().all(|sample| sample.is_finite()));
    }

    // The same pressure through the streaming source keeps every block's size.
    let project = oc_dsl::parse(ONE_TRACK).expect("project parses");
    let track = &project.tracks[0];
    let preset = project
        .presets
        .iter()
        .find(|preset| Some(&preset.name) == track.preset.as_ref())
        .expect("preset");
    let mut source = BlockSource::new(
        Some(preset),
        &Vec::new(),
        &fixture(),
        SAMPLE_RATE,
        project.properties.tempo,
        None,
    )
    .expect("streaming source builds");
    for index in 0..400u16 {
        source
            .send(&MidiMessage::NoteOn {
                channel: (index % 15) as u8,
                note: 48 + (index % 24) as u8,
                velocity: 100,
            })
            .expect("note on");
    }
    let mut output = SourceOutput::new(0, &[]);
    for _ in 0..8 {
        let transport = oc_host::PluginTransport::new(0, SAMPLE_RATE, 120.0);
        source.render_into(MAX_SOURCE_BLOCK_FRAMES, SAMPLE_RATE, transport, &mut output);
        assert_eq!(output.main().len(), MAX_SOURCE_BLOCK_FRAMES * 2);
        assert!(output.main().iter().all(|sample| sample.is_finite()));
    }
}

#[test]
fn the_same_project_renders_byte_identical_twice() {
    let project = oc_dsl::parse(ONE_TRACK).expect("project parses");
    let first = render_to_buffer_per_track(&project, &fixture(), SAMPLE_RATE).expect("render");
    let second = render_to_buffer_per_track(&project, &fixture(), SAMPLE_RATE).expect("render");
    assert_eq!(first.len(), second.len());
    assert!(
        first == second,
        "two renders of one project are not byte-identical"
    );
}

#[test]
fn pattern_previews_reach_the_engine() {
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"
@project "Preview"
  tempo: 120
@preset keys
  source: soundfont
  program: 0
@pattern riff | instrument: piano/acoustic_grand, preset: keys
  events:
    C4 8th mf
    E4 8th mf
@track keys | preset: keys, instrument: piano/acoustic_grand
  @phrase held [1:1 - 2:1]
    C4 w mf
"#,
    )
    .expect("project parses");
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(true);
    let mut plugins = std::collections::HashMap::new();
    let audio = cache
        .render_full(&project, &fixture(), SAMPLE_RATE, &mut plugins)
        .expect("pattern preview render");
    assert!(!audio.is_empty());
    assert!(rms(&left(&audio)) > 0.004);
}
