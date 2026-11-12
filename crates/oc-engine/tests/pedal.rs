//! Sustain pedal: `@2.0 pedal down` / `@4.0 pedal up` must be audible in both
//! render paths — the built-in synth's CC64 hold and the SoundFont player's.
//!
//! Every measurement is RMS in a window after the note-off, so a held pedal has
//! to show more energy there and a pedal up has to end that tail.

#[cfg(feature = "plugin-host")]
use std::collections::HashMap;
#[cfg(feature = "plugin-host")]
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::interpret::{self, InterpretSettings, MidiMessage};
#[cfg(feature = "plugin-host")]
use oc_engine::render_cache::RenderCache;

#[cfg(feature = "plugin-host")]
use crate::sf2_builder;

#[cfg(feature = "plugin-host")]
use sf2_builder::{Font, Sample, generators};

#[cfg(feature = "plugin-host")]
const RATE: u32 = 48_000;
/// 120 bpm: a beat is half a second, so a half note is off at 1.0 s.
const BEAT_SECONDS: f64 = 0.5;

/// A looping 882 Hz tone with a short release, so a held note rings past its
/// sample end and a released one is gone inside a measurable window.
#[cfg(feature = "plugin-host")]
fn looped_font() -> PathBuf {
    let mut font = Font::new("Pedal");
    let sample = font.add_sample(Sample::sine("loop", 44_100, 60, 882.0, 200).with_loop(0, 200));
    let instrument = font.add_instrument(
        "inst",
        vec![vec![
            (generators::SAMPLE_ID, sample as i16),
            (generators::SAMPLE_MODES, 1),
            (generators::SUSTAIN_VOL_ENV, 0),
            (generators::RELEASE_VOL_ENV, -600),
        ]],
    );
    font.add_preset(
        "preset",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    font.write_temp()
}

fn builtin_project(marks: &str, polyphony: u32) -> Project {
    let source = format!(
        r#"@ocp "0.2.0"
@project "pedal builtin"
  tempo: 120
  time_signature: 4/4

@preset pedal_pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {{ osc.a.waveform: 1, osc.a.level: 0.8, amp.attack: 0.01, amp.decay: 0.1, amp.sustain: 0.9, amp.release: 0.05, filter.cutoff: 12000, filter.env.amount: 0, voice.polyphony: {polyphony} }}

@track piano | preset: pedal_pad, instrument: piano/acoustic_grand
  @phrase line [1:1 - 4:1]
{marks}"#
    );
    let project = oc_dsl::parse(&source).expect("the builtin fixture parses");
    oc_dsl::validate(&project).expect("the builtin fixture validates");
    project
}

#[cfg(feature = "plugin-host")]
fn soundfont_project(marks: &str) -> Project {
    let source = format!(
        r#"@ocp "0.2.0"
@project "pedal soundfont"
  tempo: 120
  time_signature: 4/4

@preset pedal_keys
  source: soundfont
  program: 0

@track piano | preset: pedal_keys, instrument: piano/acoustic_grand
  @phrase line [1:1 - 4:1]
{marks}"#
    );
    let project = oc_dsl::parse(&source).expect("the SoundFont fixture parses");
    oc_dsl::validate(&project).expect("the SoundFont fixture validates");
    project
}

/// The half note is written once; the marks are the only difference between
/// the pedalled and the plain take.
#[cfg(feature = "plugin-host")]
const HALF_NOTE: &str = "    C4 h mf\n    rest h\n";

#[cfg(feature = "plugin-host")]
fn held_take() -> String {
    format!("    @0 pedal down\n{HALF_NOTE}    @3 pedal up\n    rest w\n")
}

#[cfg(feature = "plugin-host")]
fn plain_take() -> String {
    format!("{HALF_NOTE}    rest w\n")
}

#[cfg(feature = "plugin-host")]
fn render(project: &Project, font: &Path) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    let mut plugins = HashMap::new();
    cache
        .render_full(project, font, RATE, &mut plugins)
        .expect("the project renders")
}

#[cfg(feature = "plugin-host")]
fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    (sum / samples.len() as f64).sqrt()
}

#[cfg(feature = "plugin-host")]
fn db(level: f64) -> f64 {
    20.0 * level.max(1e-12).log10()
}

/// Mono window between two absolute seconds.
#[cfg(feature = "plugin-host")]
fn window(audio: &[f32], start: f64, end: f64) -> Vec<f32> {
    let first = (start * f64::from(RATE)) as usize;
    let last = ((end * f64::from(RATE)) as usize).min(audio.len() / 2);
    let mut mono = Vec::with_capacity(last.saturating_sub(first));
    for frame in first..last {
        let left = audio.get(frame * 2).copied().unwrap_or(0.0);
        let right = audio.get(frame * 2 + 1).copied().unwrap_or(0.0);
        mono.push((left + right) * 0.5);
    }
    mono
}

/// After the half note's note-off (1.0 s) and before the pedal up (1.5 s).
#[cfg(feature = "plugin-host")]
fn after_note_off(audio: &[f32]) -> f64 {
    rms(&window(audio, 1.3, 1.5))
}

/// Long after the pedal up: the release has run out for both engines.
#[cfg(feature = "plugin-host")]
fn after_pedal_up(audio: &[f32]) -> f64 {
    rms(&window(audio, 2.8, 3.2))
}

#[test]
fn the_interpreter_emits_cc64_and_lifts_it_at_the_phrase_end() {
    // No pedal up written: the phrase placement ends the pedal, not the note.
    let project = builtin_project("    @0 pedal down\n    C4 w mf\n", 16);
    let events = interpret::try_interpret(&project, &InterpretSettings::default()).unwrap();
    let pedal: Vec<(u64, u8)> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::ControlChange {
                controller: 64,
                value,
                ..
            } => Some((event.time_samples, value)),
            _ => None,
        })
        .collect();
    // Printed at 44.1 kHz; positions are beats, so the phrase end is 16 beats.
    let rate = f64::from(InterpretSettings::default().sample_rate);
    let beats = |samples: u64| samples as f64 / rate / BEAT_SECONDS;
    assert_eq!(pedal.len(), 2, "{pedal:?}");
    assert_eq!(pedal[0].1, 127);
    assert_eq!(pedal[1].1, 0);
    assert!((beats(pedal[0].0) - 0.0).abs() < 1e-6, "{:?}", pedal);
    assert!((beats(pedal[1].0) - 12.0).abs() < 1e-6, "{:?}", pedal);
}

#[test]
fn a_clipped_pattern_places_its_pedal_at_the_clip_start() {
    let source = r#"@ocp "0.2.0"
@project "pedal clip"
  tempo: 120
  time_signature: 4/4

@preset pedal_keys
  source: soundfont
  program: 0

@pattern riff | preset: pedal_keys, instrument: piano/acoustic_grand
  events:
    @0 pedal down
    C4 h mf
    rest h
    @2 pedal up

@track piano | preset: pedal_keys, instrument: piano/acoustic_grand
  clip: riff [3:1 - 4:1]
"#;
    let project = oc_dsl::parse(source).expect("the clip fixture parses");
    oc_dsl::validate(&project).expect("the clip fixture validates");
    let events = interpret::try_interpret(&project, &InterpretSettings::default()).unwrap();
    let pedal: Vec<(u64, u8)> = events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::ControlChange {
                controller: 64,
                value,
                ..
            } => Some((event.time_samples, value)),
            _ => None,
        })
        .collect();
    // Bar 3 beat 1 is beat 8, so the marks sit 4.0 s and 5.0 s into the take.
    let rate = f64::from(InterpretSettings::default().sample_rate);
    let at = |samples: u64| samples as f64 / rate;
    assert_eq!(pedal.len(), 2, "{pedal:?}");
    assert_eq!(pedal[0].1, 127);
    assert!((at(pedal[0].0) - 4.0).abs() < 1e-6, "{pedal:?}");
    assert!((at(pedal[1].0) - 5.0).abs() < 1e-6, "{pedal:?}");
}

#[test]
#[cfg(feature = "plugin-host")]
fn builtin_pedal_holds_the_note_past_its_note_off() {
    let font = Path::new("/nonexistent/soundfont.sf2");
    let held = render(&builtin_project(&held_take(), 16), font);
    let plain = render(&builtin_project(&plain_take(), 16), font);
    let (held_level, plain_level) = (after_note_off(&held), after_note_off(&plain));
    println!(
        "builtin after note-off: pedal {:.1} dB, no pedal {:.1} dB",
        db(held_level),
        db(plain_level)
    );
    assert!(
        plain_level < 1e-3,
        "the plain note should be gone: {plain_level}"
    );
    assert!(
        held_level > plain_level * 20.0,
        "the pedal did not hold the note: {:.1} dB vs {:.1} dB",
        db(held_level),
        db(plain_level)
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn soundfont_pedal_holds_the_note_past_its_note_off() {
    let font = looped_font();
    let held = render(&soundfont_project(&held_take()), &font);
    let plain = render(&soundfont_project(&plain_take()), &font);
    let (held_level, plain_level) = (after_note_off(&held), after_note_off(&plain));
    println!(
        "soundfont after note-off: pedal {:.1} dB, no pedal {:.1} dB",
        db(held_level),
        db(plain_level)
    );
    assert!(
        plain_level < 1e-3,
        "the plain note should be gone: {plain_level}"
    );
    assert!(
        held_level > plain_level * 20.0,
        "the pedal did not hold the note: {:.1} dB vs {:.1} dB",
        db(held_level),
        db(plain_level)
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn builtin_pedal_up_ends_the_tail() {
    let font = Path::new("/nonexistent/soundfont.sf2");
    let held = render(&builtin_project(&held_take(), 16), font);
    let (sounding, tail) = (after_note_off(&held), after_pedal_up(&held));
    println!(
        "builtin after pedal up: {:.1} dB (held was {:.1} dB)",
        db(tail),
        db(sounding)
    );
    assert!(sounding > 1e-3, "the held note never sounded");
    assert!(
        tail < sounding * 0.01,
        "the pedal up did not end the tail: {:.1} dB vs {:.1} dB",
        db(tail),
        db(sounding)
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn soundfont_pedal_up_ends_the_tail() {
    let font = looped_font();
    let held = render(&soundfont_project(&held_take()), &font);
    let (sounding, tail) = (after_note_off(&held), after_pedal_up(&held));
    println!(
        "soundfont after pedal up: {:.1} dB (held was {:.1} dB)",
        db(tail),
        db(sounding)
    );
    assert!(sounding > 1e-3, "the held note never sounded");
    assert!(
        tail < sounding * 0.01,
        "the pedal up did not end the tail: {:.1} dB vs {:.1} dB",
        db(tail),
        db(sounding)
    );
}

/// Two chords under one pedal: the first chord's note-offs happen while the
/// pedal is down, so six voices are sounding together in the second window.
#[cfg(feature = "plugin-host")]
fn two_chords_take() -> &'static str {
    "    @0 pedal down\n    C4 q mf, E4 q mf, G4 q mf\n    @1 F4 q mf, A4 q mf, C5 q mf\n    @2 pedal up\n    rest w\n    rest w\n"
}

#[test]
#[cfg(feature = "plugin-host")]
fn builtin_two_pedalled_chords_stack_without_breaking_the_voice_count() {
    // Four voices for six held notes: the synth has to steal, not explode.
    let font = Path::new("/nonexistent/soundfont.sf2");
    let audio = render(&builtin_project(two_chords_take(), 4), font);
    let first = rms(&window(&audio, 0.1, 0.4));
    let second = rms(&window(&audio, 0.6, 0.9));
    println!(
        "builtin two pedalled chords: first {:.1} dB, second {:.1} dB",
        db(first),
        db(second)
    );
    assert!(first > 1e-3, "the first chord never sounded");
    assert!(
        second > first * 0.5,
        "the second chord was lost under the voice cap: {:.1} dB vs {:.1} dB",
        db(second),
        db(first)
    );
    assert!(
        audio
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() < 4.0),
        "the voice cap produced a runaway level"
    );
}

#[test]
#[cfg(feature = "plugin-host")]
fn soundfont_two_pedalled_chords_stack_without_breaking_the_voice_count() {
    let font = looped_font();
    let audio = render(&soundfont_project(two_chords_take()), &font);
    let first = rms(&window(&audio, 0.1, 0.4));
    let second = rms(&window(&audio, 0.6, 0.9));
    println!(
        "soundfont two pedalled chords: first {:.1} dB, second {:.1} dB",
        db(first),
        db(second)
    );
    assert!(first > 1e-3, "the first chord never sounded");
    assert!(
        second > first * 0.5,
        "the second chord was lost under the voice cap: {:.1} dB vs {:.1} dB",
        db(second),
        db(first)
    );
    assert!(
        audio
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() < 4.0),
        "the voice cap produced a runaway level"
    );
}
