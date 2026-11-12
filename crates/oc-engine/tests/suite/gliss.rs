//! `gliss`: a slide is the existing note-relative pitch curve, so it is proven
//! by the pitch of the rendered audio; a run is short notes, so it is proven by
//! the note events it expands into.

#[cfg(feature = "plugin-host")]
use std::collections::HashMap;
#[cfg(feature = "plugin-host")]
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::interpret::{InterpretSettings, MidiEvent, MidiMessage, try_interpret};
use oc_synth::synth::NativeVoiceEvent;

const SAMPLE_RATE: u32 = 48_000;

/// C4 and G4, the two pitches every slide test moves between.
#[cfg(feature = "plugin-host")]
const C4_HZ: f64 = 261.63;
#[cfg(feature = "plugin-host")]
const G4_HZ: f64 = 392.00;

/// The mapped-clock slide moves between these two.
#[cfg(feature = "plugin-host")]
const A4_HZ: f64 = 440.00;
#[cfg(feature = "plugin-host")]
const E5_HZ: f64 = 659.26;

fn tempo_project(tempo: &str, tempo_map: &str, phrase: &str) -> Project {
    oc_dsl::parse(&format!(
        r#"@project "Gliss"
  tempo: {tempo}
  time_signature: 4/4
  key: C major
{tempo_map}
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
  params: {{output.loudness_trim_db: 0}}

@track lead | preset: tone, instrument: synth/lead_saw
  @phrase phrase [1:1 - 2:1]
{phrase}"#
    ))
    .unwrap_or_else(|error| panic!("{error}"))
}

fn project(phrase: &str) -> Project {
    tempo_project("60", "", phrase)
}

/// A whole note sliding A4→E5, an open rest, then a plain A4 quarter. The
/// plain note shares the curve's own pitch but never overlaps it.
#[cfg(feature = "plugin-host")]
fn mixed_project() -> Project {
    oc_dsl::parse(
        r#"@project "Gliss"
  tempo: 120
  time_signature: 4/4
  key: C major
@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@track lead | preset: tone, instrument: synth/lead_saw
  @phrase phrase [1:1 - 5:1]
    A4 w mf gliss
    E5 w mf
    rest w
    A4 q mf
"#,
    )
    .unwrap()
}

fn sampled_project(phrase: &str) -> Project {
    oc_dsl::parse(&format!(
        r#"@project "Gliss"
  tempo: 60
  time_signature: 4/4
  key: C major

@preset piano
  source: soundfont
  program: 0

@track keys | preset: piano, instrument: piano/acoustic_grand
  @phrase phrase [1:1 - 2:1]
{phrase}"#
    ))
    .unwrap_or_else(|error| panic!("{error}"))
}

fn settings() -> InterpretSettings {
    InterpretSettings {
        sample_rate: SAMPLE_RATE,
        ..Default::default()
    }
}

#[cfg(feature = "plugin-host")]
fn temp_path(name: &str) -> PathBuf {
    oc_test_support::temp_dir("gliss", name).join("slide.wav")
}

/// The real render path: project in, interleaved stereo out.
#[cfg(feature = "plugin-host")]
fn render(project: &Project) -> Vec<f32> {
    let mut cache = oc_engine::render_cache::RenderCache::new();
    let mut plugins = HashMap::new();
    let missing = Path::new("/nonexistent/gliss-must-not-need-a-bank.sf2");
    cache
        .render_full(project, missing, SAMPLE_RATE, &mut plugins)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// `f0` per 50 ms window of the rendered WAV, or `None` for an unvoiced window.
#[cfg(feature = "plugin-host")]
fn f0_windows(project: &Project, name: &str) -> Vec<(f64, Option<f64>)> {
    let path = temp_path(name);
    oc_engine::wav::write_stereo_wav(&path, &render(project), SAMPLE_RATE).unwrap();
    let buffer = oc_engine::analysis::read_wav(&path).unwrap();
    let mono: Vec<f32> = buffer.mono().iter().map(|value| *value as f32).collect();
    let analysis = oc_dsp::vocal_analysis::analyze(
        &mono,
        SAMPLE_RATE,
        &oc_dsp::vocal_analysis::DetectorParams::default(),
    )
    .unwrap();
    let mut windows: Vec<(f64, Option<f64>)> = Vec::new();
    for frame in analysis.frames.iter().filter(|frame| frame.voiced) {
        let index = (frame.time_s / 0.05).floor() as usize;
        while windows.len() <= index {
            windows.push((windows.len() as f64 * 0.05 + 0.025, None));
        }
        let entry = &mut windows[index];
        entry.1 = Some(
            entry
                .1
                .map_or(frame.f0_hz, |previous| previous.max(frame.f0_hz)),
        );
    }
    windows
}

/// Interval between two frequencies in cents.
#[cfg(feature = "plugin-host")]
fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

#[cfg(feature = "plugin-host")]
fn first_voiced(windows: &[(f64, Option<f64>)]) -> (f64, f64) {
    windows
        .iter()
        .find_map(|(centre, hz)| hz.map(|hz| (*centre, hz)))
        .expect("the render is voiced somewhere")
}

#[cfg(feature = "plugin-host")]
fn window_hz(windows: &[(f64, Option<f64>)], seconds: f64) -> f64 {
    windows
        .iter()
        .filter(|(centre, _)| (*centre - seconds).abs() < 0.05)
        .find_map(|(_, hz)| *hz)
        .unwrap_or_else(|| panic!("no voiced window at {seconds}s"))
}

fn note_ons(events: &[MidiEvent]) -> Vec<(u64, u8)> {
    events
        .iter()
        .filter_map(|event| match event.event {
            MidiMessage::NoteOn { note, .. } => Some((event.time_samples, note)),
            _ => None,
        })
        .collect()
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_slide_moves_the_rendered_pitch_from_one_note_to_the_next() {
    let project = project("    C4 h mf gliss\n    G4 h mf\n");
    let windows = f0_windows(&project, "slide");
    let inside: Vec<(f64, f64)> = windows
        .iter()
        .filter(|(centre, _)| (0.15..=1.85).contains(centre))
        .filter_map(|(centre, hz)| hz.map(|hz| (*centre, hz)))
        .collect();
    assert!(
        inside.len() >= 30,
        "the slide must be voiced across its whole length, got {} windows",
        inside.len()
    );

    // The written curve is linear in cents from C4 to G4 across the half note,
    // so every window has one expected value, not just the two ends.
    for (centre, hz) in &inside {
        let expected = C4_HZ * 2f64.powf((centre / 2.0) * 7.0 / 12.0);
        assert!(
            cents(*hz, expected).abs() < 60.0,
            "at {centre:.2}s the pitch is {hz:.1} Hz, {:.0} cents off the written glide ({expected:.1} Hz)",
            cents(*hz, expected)
        );
    }

    // Endpoints: the trace starts on the written pitch and ends on the next
    // note's pitch, both within a semitone of the analysed window.
    let first = first_voiced(&windows);
    assert!(
        cents(first.1, C4_HZ) > 0.0 && cents(first.1, C4_HZ) < 100.0,
        "the slide starts at {:.1} Hz at {:.2}s, not on C4 ({C4_HZ:.1})",
        first.1,
        first.0
    );
    let last = inside[inside.len() - 1].1;
    assert!(
        cents(last, G4_HZ).abs() < 100.0,
        "the slide ends at {last:.1} Hz, not on G4 ({G4_HZ:.1})"
    );

    // Monotonicity, window to window, not just "the two ends differ".
    for pair in inside.windows(2) {
        let (earlier, later) = (pair[0], pair[1]);
        assert!(
            later.1 > earlier.1 + 1.0,
            "pitch fell back at {:.2}s: {:.1} Hz then {:.1} Hz",
            later.0,
            earlier.1,
            later.1
        );
    }

    // The next note then holds its own pitch for the rest of the phrase.
    for seconds in [2.2, 2.6, 3.2, 3.6] {
        let hz = window_hz(&windows, seconds);
        assert!(
            (hz - G4_HZ).abs() < 6.0,
            "after the slide the pitch is {hz:.1} Hz at {seconds}s, not G4"
        );
    }
}

#[test]
#[cfg(feature = "plugin-host")]
fn a_written_length_arrives_early_and_holds() {
    let project = project("    C4 h mf gliss=250ms\n    G4 h mf\n");
    let windows = f0_windows(&project, "portamento");
    let first = first_voiced(&windows);
    assert!(
        cents(first.1, C4_HZ) > 0.0 && cents(first.1, C4_HZ) < 200.0,
        "the note starts near C4 and bends up, got {:.1} Hz at {:.2}s",
        first.1,
        first.0
    );
    // A full-length slide is still far below G4 at 0.4 s; a 250 ms length has
    // already arrived and holds for the rest of the half note.
    for seconds in [0.4, 0.8, 1.5] {
        let hz = window_hz(&windows, seconds);
        assert!(
            cents(hz, G4_HZ).abs() < 60.0,
            "a 250 ms slide holds G4 by {seconds}s, got {hz:.1} Hz"
        );
    }
}

#[test]
fn a_chromatic_run_expands_to_every_semitone_and_keeps_the_length() {
    let project = project("    C4 q mf gliss=run\n    G4 q mf\n");
    let events = try_interpret(&project, &settings()).unwrap();
    let ons = note_ons(&events);
    let notes: Vec<u8> = ons.iter().map(|(_, note)| *note).collect();
    // C4 up every semitone to G4, then the written G4 that the run lands on.
    assert_eq!(notes, vec![60, 61, 62, 63, 64, 65, 66, 67, 67]);

    // One quarter at 60 bpm is one second, split into eight equal steps.
    let times: Vec<u64> = ons.iter().map(|(time, _)| *time).collect();
    let expected: Vec<u64> = (0..8).map(|step| step * 6_000).collect::<Vec<_>>();
    assert_eq!(&times[..8], expected.as_slice());
    assert_eq!(times[8], 48_000, "the next written note keeps its position");

    // The whole note's duration is preserved: the steps cover it exactly.
    let run_end = ons[7].0 + 6_000;
    assert_eq!(run_end, 48_000);
}

#[test]
fn a_diatonic_run_steps_through_the_key() {
    let project = project("    C4 q mf gliss=run_diatonic\n    G4 q mf\n");
    let events = try_interpret(&project, &settings()).unwrap();
    let notes: Vec<u8> = note_ons(&events).iter().map(|(_, note)| *note).collect();
    // C major from C4 to G4: C D E F G, then the written G4.
    assert_eq!(notes, vec![60, 62, 64, 65, 67, 67]);
    let ons = note_ons(&events);
    assert_eq!(ons[4].0, 4 * 9_600, "five steps of a quarter note each");
    assert_eq!(ons[5].0, 48_000);
}

#[test]
fn a_run_also_plays_on_a_sampled_source() {
    let project = sampled_project("    C4 q mf gliss=run\n    G4 q mf\n");
    let events = try_interpret(&project, &settings()).unwrap();
    assert_eq!(note_ons(&events).len(), 9);
}

#[test]
fn a_slide_on_a_sampled_source_names_the_way_out() {
    let project = sampled_project("    C4 q mf gliss\n    G4 q mf\n");
    let error = try_interpret(&project, &settings()).unwrap_err();
    assert!(error.contains("gliss at @0 in track 'keys'"), "{error}");
    assert!(error.contains("'piano' is a sampled source"), "{error}");
    assert!(error.contains("cannot slide"), "{error}");
    assert!(
        error.contains("use gliss=run for a run of short notes"),
        "{error}"
    );
    assert!(error.contains("builtin.open_synth"), "{error}");
    assert!(error.contains("read_skill('writing-notes')"), "{error}");
}

#[test]
fn a_slide_on_a_hosted_instrument_names_the_way_out() {
    let project = oc_dsl::parse(
        r#"@project "Gliss"
  tempo: 60
@preset hosted
  source: vst3
  plugin: "com.example.hosted_synth"
@track lead | preset: hosted, instrument: synth/lead_saw
  @phrase phrase [1:1 - 2:1]
    C4 q mf gliss=G4
    G4 q mf
"#,
    )
    .unwrap();
    let error = try_interpret(&project, &settings()).unwrap_err();
    assert!(error.contains("'hosted' is a hosted instrument"), "{error}");
    assert!(
        error.contains("use gliss=run for a run of short notes"),
        "{error}"
    );
}

#[test]
fn a_slide_past_two_octaves_is_refused_rather_than_clipped() {
    let project = project("    C4 q mf gliss=C7\n    G4 q mf\n");
    let error = try_interpret(&project, &settings()).unwrap_err();
    assert!(error.contains("more than two octaves"), "{error}");
    assert!(error.contains("gliss=run"), "{error}");
}

/// Quarters elapsed at `seconds` on this take's clock: 120 bpm to quarter 1 at
/// 0.5 s, then 60 bpm. The written slide spans those two quarters.
#[cfg(feature = "plugin-host")]
fn mapped_quarters(seconds: f64) -> f64 {
    if seconds <= 0.5 {
        seconds / 0.5
    } else {
        1.0 + (seconds - 0.5)
    }
}

/// The analysed window nearest `seconds`, with its centre time.
#[cfg(feature = "plugin-host")]
fn window_at(windows: &[(f64, Option<f64>)], seconds: f64) -> (f64, f64) {
    let (centre, hz) = windows
        .iter()
        .filter_map(|(centre, hz)| hz.map(|hz| (*centre, hz)))
        .min_by(|a, b| (a.0 - seconds).abs().total_cmp(&(b.0 - seconds).abs()))
        .unwrap_or_else(|| panic!("the render is voiced at {seconds}s"));
    assert!((centre - seconds).abs() < 0.05, "no window near {seconds}s");
    (centre, hz)
}

/// The slide is written across two quarters: a tempo change at quarter 1 makes
/// the mapped take last 1.5 s, where a flat 120 bpm take lasts 1.0 s.
#[test]
#[cfg(feature = "plugin-host")]
fn a_slide_follows_the_tempo_map_clock() {
    let phrase = "    A4 h mf gliss\n    E5 h mf\n";
    let mapped = f0_windows(
        &tempo_project("120", "  tempo_map:\n    1:2 = 60 bpm\n", phrase),
        "mapped",
    );
    let flat = f0_windows(&tempo_project("120", "", phrase), "flat");

    // Both takes are at quarter 0.8 by 0.4 s, so the change itself is the only
    // difference between the two renders.
    for (name, windows) in [("mapped", &mapped), ("flat", &flat)] {
        let (centre, hz) = window_at(windows, 0.4);
        let expected = A4_HZ * 2f64.powf(350.0 * mapped_quarters(centre) / 1200.0);
        assert!(
            cents(hz, expected).abs() < 30.0,
            "the {name} take is {hz:.1} Hz at {centre:.2}s, {:.0} cents off the written glide ({expected:.1} Hz)",
            cents(hz, expected)
        );
    }

    // At 0.75 s the clock says quarter 1.25, so the mapped pitch is 437.5 cents
    // above A4. A curve stretched linearly over the whole 1.5 s would read 350
    // cents there instead.
    let (centre, hz) = window_at(&mapped, 0.75);
    let expected = A4_HZ * 2f64.powf(350.0 * mapped_quarters(centre) / 1200.0);
    assert!(
        cents(hz, expected).abs() < 30.0,
        "the mapped slide is {hz:.1} Hz at {centre:.2}s, {:.0} cents off the clock ({expected:.1} Hz)",
        cents(hz, expected)
    );
    let linear_in_time = A4_HZ * 2f64.powf(350.0 / 1200.0);
    assert!(
        cents(hz, linear_in_time) > 50.0,
        "at {centre:.2}s the mapped slide is {hz:.1} Hz, which is the linear-in-time 350-cent value; the clock says quarter 1.25"
    );

    // At 1.0 s the flat take has arrived on E5; the mapped take is only at
    // quarter 1.5, still 175 cents short. That gap is the map's length.
    let (flat_centre, flat_hz) = window_at(&flat, 1.0);
    let (mapped_centre, mapped_hz) = window_at(&mapped, 1.0);
    assert!(
        cents(flat_hz, E5_HZ).abs() < 40.0,
        "the flat slide is {flat_hz:.1} Hz at {flat_centre:.2}s, not on E5 ({E5_HZ:.1} Hz)"
    );
    let expected = A4_HZ * 2f64.powf(350.0 * mapped_quarters(mapped_centre) / 1200.0);
    assert!(
        cents(mapped_hz, expected).abs() < 35.0,
        "the mapped slide is {mapped_hz:.1} Hz at {mapped_centre:.2}s, {:.0} cents off the clock ({expected:.1} Hz)",
        cents(mapped_hz, expected)
    );
    assert!(
        cents(flat_hz, mapped_hz) > 100.0,
        "the flat take ({flat_hz:.1} Hz) and the mapped take ({mapped_hz:.1} Hz) should differ by more than a semitone at 1.0 s"
    );

    // The mapped glide reaches E5 before the note-off at 1.5 s and the next
    // note holds it.
    let (centre, hz) = window_at(&mapped, 1.48);
    assert!(
        cents(hz, E5_HZ).abs() < 40.0,
        "the mapped slide ends at {hz:.1} Hz at {centre:.2}s, not on E5 ({E5_HZ:.1} Hz)"
    );
    let (centre, hz) = window_at(&mapped, 1.8);
    assert!(
        (hz - E5_HZ).abs() < 8.0,
        "the next note holds {hz:.1} Hz at {centre:.2}s, not E5"
    );
}

/// The plain A4 is at the curve's own pitch but after the slide is over. Both
/// notes must sound: the curve note at 0 s and the plain note at 6 s, while the
/// curve still reaches E5 before the plain note enters.
#[test]
#[cfg(feature = "plugin-host")]
fn a_plain_note_at_a_curve_pitch_renders_as_a_second_voice() {
    let project = mixed_project();
    let events = try_interpret(&project, &settings()).unwrap();
    let a4_ons = events
        .iter()
        .filter(|event| {
            matches!(
                event.event,
                MidiMessage::NativeVoice(NativeVoiceEvent::On { note: 69, .. })
            )
        })
        .count();
    assert_eq!(
        a4_ons, 2,
        "the curve and the plain A4 each need their own voice id"
    );

    let audio = render(&project);
    let onsets = oc_engine::align::detect_onsets(&audio, SAMPLE_RATE);
    let onset_seconds: Vec<f64> = onsets
        .iter()
        .map(|sample| *sample as f64 / f64::from(SAMPLE_RATE))
        .collect();
    for expected in [0.0, 6.0] {
        assert!(
            onset_seconds
                .iter()
                .any(|seconds| (*seconds - expected).abs() < 0.05),
            "no note onset at {expected}s: {onset_seconds:?}"
        );
    }

    let peak_between = |start: f64, end: f64| {
        let first = (start * f64::from(SAMPLE_RATE)) as usize * 2;
        let last = (end * f64::from(SAMPLE_RATE)) as usize * 2;
        audio[first..last]
            .iter()
            .fold(0.0_f32, |peak, value| peak.max(value.abs()))
    };
    for start in [0.0, 6.0] {
        assert!(
            peak_between(start, start + 0.2) > 0.01,
            "the note at {start}s is silent"
        );
    }

    // The curve moves through the written midpoint and ends on E5.
    let windows = f0_windows(&project, "mixed");
    let halfway = window_hz(&windows, 1.0);
    assert!(
        cents(halfway, A4_HZ * 2f64.powf(350.0 / 1200.0)).abs() < 45.0,
        "at 1.0 s the slide is {halfway:.1} Hz, not the written midpoint"
    );
    let arrival = window_hz(&windows, 1.95);
    assert!(
        cents(arrival, E5_HZ).abs() < 60.0,
        "the slide ends at {arrival:.1} Hz, not on E5 ({E5_HZ:.1} Hz)"
    );
    for seconds in [2.5, 3.5] {
        let hz = window_hz(&windows, seconds);
        assert!(
            (hz - E5_HZ).abs() < 8.0,
            "the next note holds {hz:.1} Hz at {seconds}s, not E5"
        );
    }
    // The plain A4 keeps its own pitch; it is not bent by the earlier curve.
    let plain = window_hz(&windows, 6.2);
    assert!(
        (plain - A4_HZ).abs() < 8.0,
        "the plain note sounds {plain:.1} Hz at 6.2 s, not A4 ({A4_HZ:.1} Hz)"
    );
}

/// The plain A4 sits in a later phrase, so the scan has to cover the whole
/// track rather than only the phrase that carries the curve.
#[test]
fn a_plain_note_at_a_curve_pitch_in_another_phrase_is_identified() {
    let project = oc_dsl::parse(
        r#"@project "Gliss"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.open_synth"
@track lead | preset: tone, instrument: synth/lead_saw
  @phrase slide [1:1 - 2:1]
    A4 q mf gliss=G4
    G4 q mf
  @phrase plain [2:1 - 3:1]
    A4 q mf
"#,
    )
    .unwrap();
    let events = try_interpret(&project, &settings()).unwrap();
    let a4_ons = events
        .iter()
        .filter(|event| {
            matches!(
                event.event,
                MidiMessage::NativeVoice(NativeVoiceEvent::On { note: 69, .. })
            )
        })
        .count();
    assert_eq!(
        a4_ons, 2,
        "the curve and the plain note in the next phrase each need a voice id"
    );
}

/// FNV-1a over the interleaved f32 frames: a flat-tempo slide must render the
/// same samples it rendered before mapped curves were supported. The pin guards
/// the slide, so it moves only when the instrument's level law does.
#[test]
#[cfg(feature = "plugin-host")]
fn a_flat_tempo_slide_keeps_its_rendered_samples() {
    let audio = render(&project("    C4 h mf gliss\n    G4 h mf\n"));
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for value in &audio {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    assert_eq!(audio.len(), 576_000);
    assert_eq!(hash, 0x58d1_a561_3f70_60bc);
}
