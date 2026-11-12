//! Solo end-to-end: non-soloed sources are silent, routing and FX still run.

use super::super::*;
use std::collections::HashMap;
use std::path::Path;

const MASTER: &str = r#"@ocp "0.2.0"

@project "solo"
  tempo: 120
  time_signature: 4/4

@preset tone_a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@preset tone_b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2, waveform: 0}

@track a | preset: tone_a, instrument: synth/lead_square
  @phrase a [1:1 - 2:1]
    C4 w mf

@track b | preset: tone_b, instrument: synth/lead_square
  @phrase b [1:1 - 2:1]
    E4 w mf
"#;

const BUS: &str = r#"@ocp "0.2.0"

@project "solo bus"
  tempo: 120
  time_signature: 4/4

@preset tone_a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@preset tone_b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2, waveform: 0}

@bus glue
  effects:
    - eq: low_gain=+6dB, low_freq=80Hz

@master
  effects:
    - eq: high_gain=-6dB, high_freq=2kHz

@track a | preset: tone_a, instrument: synth/lead_square
  mix: output = glue
  @phrase a [1:1 - 2:1]
    C4 w mf

@track b | preset: tone_b, instrument: synth/lead_square
  @phrase b [1:1 - 2:1]
    E4 w mf
"#;

fn assert_same(label: &str, a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "{label}: lengths differ");
    if let Some(index) = a.iter().zip(b).position(|(x, y)| x != y) {
        panic!(
            "{label}: first difference at sample {index}: {} vs {}",
            a[index], b[index]
        );
    }
}

fn render(source: &str) -> Vec<f32> {
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    RenderCache::new()
        .render_full(
            &project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap()
}

#[test]
fn soloing_a_direct_track_matches_muting_the_other() {
    let soloed = render(&MASTER.replace(
        "@track a | preset: tone_a, instrument: synth/lead_square",
        "@track a | preset: tone_a, instrument: synth/lead_square\n  mix: solo = true",
    ));
    let muted = render(&MASTER.replace(
        "@track b | preset: tone_b, instrument: synth/lead_square",
        "@track b | preset: tone_b, instrument: synth/lead_square\n  mix: mute = true",
    ));
    assert!(soloed.iter().any(|sample| sample.abs() > 1e-3));
    assert_same("solo vs mute", &soloed, &muted);
}

#[test]
fn soloing_a_bus_track_keeps_bus_and_master_fx_on_the_survivor() {
    let muted = render(&BUS.replace(
        "@track b | preset: tone_b, instrument: synth/lead_square",
        "@track b | preset: tone_b, instrument: synth/lead_square\n  mix: mute = true",
    ));
    let soloed = render(&BUS.replace(
        "  mix: output = glue\n",
        "  mix: output = glue, solo = true\n",
    ));
    assert!(soloed.iter().any(|sample| sample.abs() > 1e-3));
    assert_same("solo vs mute", &soloed, &muted);

    let direct = render(&BUS.replace("  mix: output = glue\n", ""));
    let wet = render(BUS);
    assert!(
        direct != wet,
        "bus routing and master FX must change the audible result"
    );
}

#[test]
fn soloing_the_direct_track_silences_the_bus_track() {
    let muted = render(&BUS.replace(
        "  mix: output = glue\n",
        "  mix: output = glue, mute = true\n",
    ));
    let soloed = render(&BUS.replace(
        "@track b | preset: tone_b, instrument: synth/lead_square",
        "@track b | preset: tone_b, instrument: synth/lead_square\n  mix: solo = true",
    ));
    assert!(soloed.iter().any(|sample| sample.abs() > 1e-3));
    assert_same("solo vs mute", &soloed, &muted);
}

#[test]
fn nothing_soloed_plays_every_track() {
    let baseline = render(MASTER);
    let soloed = render(&MASTER.replace(
        "@track a | preset: tone_a, instrument: synth/lead_square",
        "@track a | preset: tone_a, instrument: synth/lead_square\n  mix: solo = true",
    ));
    assert!(baseline.iter().any(|sample| sample.abs() > 1e-3));
    assert_ne!(baseline, soloed);
}

#[test]
fn a_soloed_track_with_no_peers_still_plays() {
    let mut project = oc_dsl::parse(MASTER).unwrap();
    project.tracks.retain(|track| track.name == "a");
    project.tracks[0].mix.solo = true;
    let audio = RenderCache::new()
        .render_full(
            &project,
            Path::new("unused.sf2"),
            48_000,
            &mut HashMap::new(),
        )
        .unwrap();
    assert!(audio.iter().any(|sample| sample.abs() > 1e-3));
}

/// Two sources into `bus_a`, which fans out to `main` (then `side`) and to the
/// `reverb` return; `src_c` feeds only the reverb and `src_d` feeds no bus.
const MODES: &str = r#"@ocp "0.2.0"

@project "solo modes"
  tempo: 120
  time_signature: 4/4

@preset tone_a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@preset tone_b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.2, waveform: 0}

@preset tone_c
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 440, amplitude: 0.2, waveform: 0}

@preset tone_d
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 550, amplitude: 0.2, waveform: 0}

@bus bus_a
  routes: [main, reverb]

@bus main
  routes: [side]

@bus side
  routes: [master]

@bus reverb
  effects:
    - reverb: decay=0.8s, mix=100%
  routes: [master]

@master

@track src_a | preset: tone_a, instrument: synth/lead_square
  mix: output = bus_a, send(reverb = -6dB, pre)
  @phrase src_a [1:1 - 2:1]
    C4 w mf

@track src_b | preset: tone_b, instrument: synth/lead_square
  mix: output = main, send(reverb = -6dB, post)
  @phrase src_b [1:1 - 2:1]
    E4 w mf

@track src_c | preset: tone_c, instrument: synth/lead_square
  mix: output = reverb
  @phrase src_c [1:1 - 2:1]
    G4 w mf

@track src_d | preset: tone_d, instrument: synth/lead_square
  @phrase src_d [1:1 - 2:1]
    C5 w mf
"#;

fn with_solo_mode(source: &str, mode: &str) -> String {
    source.replace(
        "  time_signature: 4/4\n",
        &format!("  time_signature: 4/4\n  solo_mode: {mode}\n"),
    )
}

fn solo_bus(source: &str, bus: &str) -> String {
    source.replace(
        &format!("@bus {bus}\n"),
        &format!("@bus {bus}\n  mix: solo = true\n"),
    )
}

fn mute_bus(source: &str, bus: &str) -> String {
    source.replace(
        &format!("@bus {bus}\n"),
        &format!("@bus {bus}\n  mix: mute = true\n"),
    )
}

fn track_mix_end(source: &str, track: &str) -> Option<(usize, usize)> {
    let header = source.find(&format!("@track {track} |"))?;
    let mix = source[header..]
        .find("  mix: ")
        .map(|offset| header + offset)?;
    let end = source[mix..].find('\n').map(|offset| mix + offset)?;
    Some((mix, end))
}

fn mute_track(source: &str, track: &str) -> String {
    let mut out = source.to_string();
    if let Some((_, end)) = track_mix_end(&out, track) {
        out.insert_str(end, ", mute = true");
        return out;
    }
    let header = out.find(&format!("@track {track} |")).unwrap();
    let end = out[header..].find('\n').unwrap() + header + 1;
    out.insert_str(end, "  mix: mute = true\n");
    out
}

fn solo_track(source: &str, track: &str) -> String {
    let (_, end) = track_mix_end(source, track).expect("track with a mix line");
    let mut out = source.to_string();
    out.insert_str(end, ", solo = true");
    out
}

#[test]
fn default_solo_mode_keeps_the_historical_bus_closure() {
    let soloed = solo_bus(MODES, "main");
    let default = render(&soloed);
    assert!(default.iter().any(|sample| sample.abs() > 1e-3));
    assert_same(
        "default vs explicit both",
        &default,
        &render(&with_solo_mode(&soloed, "both")),
    );
    // Today's set for this score: every source except src_d, every bus.
    assert_same(
        "default vs historical audible set",
        &default,
        &render(&mute_track(MODES, "src_d")),
    );
}

#[test]
fn upstream_drops_the_return_that_both_keeps() {
    let soloed = solo_bus(MODES, "main");
    let both = render(&with_solo_mode(&soloed, "both"));
    let upstream = render(&with_solo_mode(&soloed, "upstream"));
    assert!(both.iter().any(|sample| sample.abs() > 1e-3));
    assert!(upstream.iter().any(|sample| sample.abs() > 1e-3));

    // Upstream keeps src_a and src_b; src_c and the reverb return drop out.
    let expected = render(&mute_bus(
        &mute_track(&mute_track(MODES, "src_c"), "src_d"),
        "reverb",
    ));
    assert_same("upstream vs explicit mutes", &upstream, &expected);

    let delta: Vec<f32> = both
        .iter()
        .zip(&upstream)
        .map(|(left, right)| left - right)
        .collect();
    let max_delta = delta
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let rms_delta =
        (delta.iter().map(|sample| sample * sample).sum::<f32>() / delta.len() as f32).sqrt();
    assert!(
        max_delta > 1e-3,
        "the return must be audible: max delta {max_delta}"
    );
    assert!(rms_delta > 1e-4, "rms difference {rms_delta}");
}

#[test]
fn a_soloed_channel_keeps_both_send_taps_in_either_mode() {
    let soloed = solo_track(MODES, "src_a");
    let expected = render(&mute_track(
        &mute_track(&mute_track(MODES, "src_b"), "src_c"),
        "src_d",
    ));
    assert!(expected.iter().any(|sample| sample.abs() > 1e-3));

    let mut reference: Option<Vec<f32>> = None;
    for mode in ["both", "upstream"] {
        let audio = render(&with_solo_mode(&soloed, mode));
        assert!(audio.iter().any(|sample| sample.abs() > 1e-3), "{mode}");
        assert_same(&format!("{mode} vs explicit mutes"), &expected, &audio);
        if let Some(reference) = &reference {
            assert_same(&format!("{mode} vs the previous mode"), reference, &audio);
        } else {
            reference = Some(audio);
        }
    }
}
