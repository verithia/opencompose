//! Section arrangement render tests: buffers, not just durations.

use super::super::*;
use std::collections::HashMap;
use std::path::Path;

const BODY: &str = r#"@ocp "0.2.0"

@project "section render"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  @phrase body [1:1 - 3:1]
    C4 q mf
    rest w
    rest h
    rest q
"#;

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_minimum_duration(4.0).unwrap();
    cache
        .render_full(
            project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap()
}

fn peak(audio: &[f32], start_seconds: f64, end_seconds: f64) -> f32 {
    let start = ((start_seconds * 48_000.0) as usize * 2).min(audio.len());
    let end = ((end_seconds * 48_000.0) as usize * 2).min(audio.len());
    audio[start..end]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn two_bar_section_repeated_twice_is_the_body_twice() {
    let reference = oc_dsl::parse(BODY).unwrap();
    let body = render(&reference);
    // 120 BPM: two bars = 4 s = 192000 frames; the reference only renders the
    // body plus its tail, so compare the first four seconds.
    let body_frames = 192_000;
    assert!(body.len() >= body_frames * 2);

    let with_section = oc_dsl::parse(&BODY.replace(
        "@track tone",
        "@section body [1:1 - 3:1]\n  repeat: 2\n\n@track tone",
    ))
    .unwrap();
    let expanded = render(&with_section);
    // The rendered duration comes from the arrangement: 8 s even though the
    // last musical event is inside the first two bars.
    assert_eq!(expanded.len(), 384_000 * 2);

    let first = &expanded[..body_frames * 2];
    let second = &expanded[body_frames * 2..];
    assert!(
        second.iter().any(|sample| sample.abs() > 1e-3),
        "the repeated half must contain audio"
    );
    for (index, (a, b)) in first.iter().zip(&body[..body_frames * 2]).enumerate() {
        assert!((a - b).abs() < 1e-5, "first half frame {index}: {a} vs {b}");
    }
    for (index, (a, b)) in second.iter().zip(&body[..body_frames * 2]).enumerate() {
        assert!(
            (a - b).abs() < 1e-5,
            "second half frame {index}: {a} vs {b}"
        );
    }
}

#[test]
fn swing_moves_the_off_beat_onset_and_leaves_the_written_prefix_silent() {
    let project = oc_dsl::parse(
        r#"@ocp "0.2.0"

@project "swing render"
  tempo: 120
  time_signature: 4/4
  swing: 66%

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@track tone | preset: tone, instrument: synth/lead_square
  @phrase offbeat [1:1 - 1:2]
    rest 8th
    C4 8th mf
"#,
    )
    .unwrap();
    let audio = render(&project);
    // 66% moves the 8th off-beat from 12000 to 15840 samples.
    let onset = 15_840;
    assert!(
        audio[..onset * 2].iter().all(|sample| *sample == 0.0),
        "the pre-swing 8th grid must stay silent"
    );
    assert!(
        audio[onset * 2..(onset + 4) * 2]
            .iter()
            .any(|sample| sample.abs() > 1e-3),
        "audio must start at the swung onset"
    );
}

const RINGING_TAIL: &str = r#"@ocp "0.2.0"

@project "seam tail"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.open_synth"

@track lead | preset: tone, instrument: synth/virtual_analog
  @phrase body [1:1 - 3:1]
    @7 C4 w mf
"#;

#[test]
fn a_repeat_seam_releases_the_note_and_lets_its_release_tail_cross() {
    let project = oc_dsl::parse(&RINGING_TAIL.replace(
        "@track lead",
        "@section body [1:1 - 3:1]\n  repeat: 2\n\n@track lead",
    ))
    .unwrap();
    let audio = render(&project);
    // Bar 2 beat 4 starts at 3.5 s; the seam is at 4 s.
    assert!(
        peak(&audio, 3.7, 3.9) > 0.01,
        "the note sounds before the seam"
    );
    assert!(
        peak(&audio, 4.0, 4.05) > 0.002,
        "the release tail crosses the seam instead of being hard-cut"
    );
    // Untrimmed, the sustained note would hold ~0.7 of its level into bar 3.
    assert!(
        peak(&audio, 4.6, 7.0) < 0.001,
        "the first play's note must not bleed into the repeat"
    );
    // The repeat plays its own copy at 7.5 s and rings out to the end.
    assert!(
        peak(&audio, 7.6, 7.8) > 0.01,
        "the repeat plays its own copy"
    );
}
