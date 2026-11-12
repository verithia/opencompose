//! The rendered arrangement: section spans and repeats decide how long the
//! audio is, so a repeat plan is audible, not only declarative.

use std::path::{Path, PathBuf};

const RATE: u32 = 22_050;

/// 8 written bars of content in one section; `repeat` decides how many plays.
const VERSE: &str = r#"@ocp "0.2.0"

@project "verse repeated"
  tempo: 120
  time_signature: 4/4

@preset pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {synth_version: 2, osc.a.waveform: 1, osc.a.level: 0.55, filter.cutoff: 2200Hz, amp.attack: 0.01s, amp.decay: 0.2s, amp.sustain: 0.7, amp.release: 0.05s, output.gain: 0.36}

@pattern riff | instrument: synth/pad, preset: pad
  events:
    @0 A3 w mp, C4 w mp, E4 w mp
    @4 F3 w mp, A3 w mp, C4 w mp

@track pad | instrument: synth/pad, preset: pad
  clip: riff [1:1 - 9:1]

@section verse [1:1 - 9:1]
  repeat: 2
  label: "Verse"
"#;

fn soundfont() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
}

fn verse(repeat: Option<u32>) -> oc_dsl::ast::Project {
    let mut project = oc_dsl::parse(VERSE).expect("the verse fixture parses");
    oc_dsl::validate(&project).expect("the verse fixture validates");
    project.sections[0].repeat = repeat;
    project
}

fn render(project: &oc_dsl::ast::Project) -> Vec<f32> {
    oc_engine::render::render_to_buffer(project, &soundfont(), RATE).expect("the verse renders")
}

/// Loudest absolute sample in `[from_s, to_s)`.
fn peak_between(audio: &[f32], from_s: f64, to_s: f64) -> f32 {
    let frames = audio.len() / 2;
    let from = ((from_s * f64::from(RATE)) as usize).min(frames);
    let to = ((to_s * f64::from(RATE)) as usize).min(frames);
    audio[from * 2..to * 2]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn a_repeat_plays_the_span_again_and_lengthens_the_render() {
    let once = render(&verse(None));
    let twice = render(&verse(Some(2)));

    // 8 bars at 120 bpm is 16 s of arrangement per play.
    let play_seconds = 16.0;
    assert!(
        twice.len() > once.len(),
        "a repeated section renders longer: {} vs {} samples",
        twice.len(),
        once.len()
    );
    assert!(
        (twice.len() as f64 / 2.0 / f64::from(RATE)) >= play_seconds * 2.0,
        "the render covers both plays: {:.2} s",
        twice.len() as f64 / 2.0 / f64::from(RATE)
    );

    // The first play ends at 16 s; the second one is the same music again.
    assert!(
        peak_between(&twice, 0.1, play_seconds - 0.1) > 1e-3,
        "the first play is audible"
    );
    assert!(
        peak_between(&twice, play_seconds + 0.1, play_seconds * 2.0 - 0.1) > 1e-3,
        "the repeated play is audible"
    );
    // The single-play render is silent where the repeat is.
    assert!(
        peak_between(&once, play_seconds + 0.1, play_seconds * 2.0 - 0.1) < 1e-3,
        "without repeat the span after 16 s is silent"
    );
}

#[test]
fn removing_the_repeat_restores_the_written_length() {
    let once = render(&verse(None));
    let written_quarters = 32.0;
    let written_seconds = written_quarters * 60.0 / 120.0;
    assert_eq!(written_seconds, 16.0);
    assert!(
        (once.len() as f64 / 2.0 / f64::from(RATE)) >= written_seconds,
        "a single play still covers its written span"
    );
    assert!(
        (once.len() as f64 / 2.0 / f64::from(RATE)) < written_seconds * 2.0,
        "a single play does not render the second play"
    );
}

/// The showcase's shape: a repeated section sits before a tempo and meter map.
const REPEATED_CLOCK: &str = r#"@ocp "0.2.0"

@project "repeated clock"
  tempo: 100
  time_signature: 4/4
  tempo_map:
    7:1 = 88
    9:1 = 104
  time_sig_map:
    9:1 = 3/4

@preset keys
  source: soundfont
  program: 0

@track keys | instrument: piano/acoustic_grand, preset: keys
  @phrase head [1:1 - 3:1]
    C4 w mf
    E4 w mf

@section a [1:1 - 3:1]
@section b [3:1 - 5:1]
  repeat: 2
@section c [5:1 - 7:1]
@section d [7:1 - 9:1]
@section e [9:1 - 13:1]
  repeat: 2
"#;

/// Two tempo points inside one repeated section, plus the tempo its play starts
/// at, so every play is 2 bars at 60 then 2 bars at 120.
const TWO_TEMPO_POINTS: &str = r#"@ocp "0.2.0"

@project "two tempo points"
  tempo: 60
  time_signature: 4/4
  tempo_map:
    1:1 = 60
    2:1 = 60
    3:1 = 120

@preset keys
  source: soundfont
  program: 0

@track keys | instrument: piano/acoustic_grand, preset: keys
  @phrase line [1:1 - 5:1]
    C4 16th mf
    @4 E4 16th mf
    @8 G4 16th mf
    @12 C5 16th mf

@section verse [1:1 - 5:1]
  repeat: 2
"#;

#[test]
fn two_tempo_points_in_one_repeated_section_render_each_play() {
    let project = oc_dsl::parse(TWO_TEMPO_POINTS).expect("the two-point fixture parses");
    oc_dsl::validate(&project).expect("the two-point fixture validates");
    let audio = render(&project);
    let seconds = audio.len() as f64 / 2.0 / f64::from(RATE);

    // 2 bars at 60 BPM and 2 bars at 120 BPM are 12 s per play; the repeat
    // plays both maps, so the render covers 24 s plus the final note's gate.
    // Reading the map at its written position would give a 12 s render.
    assert!(
        (24.0..24.3).contains(&seconds),
        "the repeated map renders {seconds:.3} s"
    );

    // Onsets at quarters 0, 4, 8, 12 of each play: 0, 4, 8, 10 s then
    // 12, 16, 20, 22 s. Each note sounds at its onset and is silent before it.
    for (index, onset) in [0.0, 4.0, 8.0, 10.0, 12.0, 16.0, 20.0, 22.0]
        .into_iter()
        .enumerate()
    {
        assert!(
            peak_between(&audio, onset + 0.02, onset + 0.3) > 1e-3,
            "note {index} starts at {onset:.2} s"
        );
        if onset > 0.0 {
            assert!(
                peak_between(&audio, onset - 0.35, onset - 0.02) < 1e-3,
                "note {index} leaves a rest before {onset:.2} s"
            );
        }
    }
}

#[test]
fn a_tempo_and_meter_map_repeats_with_the_section_that_holds_it() {
    let project = oc_dsl::parse(REPEATED_CLOCK).expect("the repeated clock fixture parses");
    oc_dsl::validate(&project).expect("the repeated clock fixture validates");
    let audio = render(&project);
    let seconds = audio.len() as f64 / 2.0 / f64::from(RATE);
    // 64 rendered quarters: 32 at 100 BPM, 8 at 88, then 24 at 104. A map read
    // at its written bar instead of its played one renders a different length.
    assert!(
        (seconds - 38.5007).abs() < 0.01,
        "the repeated clock renders {seconds:.3} s"
    );

    // The meter change repeats with the outro, so the last play is 3/4 too.
    let expanded = oc_dsl::arrangement::expand(&project).expect("the fixture expands");
    let clock = oc_dsl::timing::ProjectClock::new(&expanded.properties).unwrap();
    assert_eq!(clock.meter_at_bar(11).unwrap().numerator, 3);
    assert_eq!(clock.meter_at_bar(14).unwrap().numerator, 3);
    assert_eq!(clock.meter_at_bar(15).unwrap().numerator, 3);
    assert_eq!(clock.tempo_at_quarter(32.0).unwrap(), 88.0);
    assert_eq!(clock.tempo_at_quarter(40.0).unwrap(), 104.0);
}
