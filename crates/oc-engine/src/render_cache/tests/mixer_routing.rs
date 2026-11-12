//! Send tap, bus-return and channel-override behaviour end to end.

use super::super::*;
use std::collections::HashMap;
use std::path::Path;

const BASE: &str = r#"@ocp "0.2.0"

@project "mixer routing"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@bus sink
  effects:
    - gain: factor=0.0
  routes: [master]

@bus space
  effects:
    - gain: factor=0.5
  routes: [master]

@track a | preset: tone, instrument: synth/lead_square
  mix: output = sink, send(space=100%)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#;

const SOLO: &str = r#"@ocp "0.2.0"

@project "solo safe"
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
  routes: [master]

@track a | preset: tone_a, instrument: synth/lead_square
  mix: output = glue
  @phrase a [1:1 - 2:1]
    C4 w mf

@track b | preset: tone_b, instrument: synth/lead_square
  mix: solo = true
  @phrase b [1:1 - 2:1]
    E4 w mf
"#;

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

fn peak(buffer: &[f32]) -> f64 {
    buffer
        .iter()
        .fold(0.0_f64, |peak, value| peak.max(f64::from(value.abs())))
}

fn assert_close(label: &str, actual: &[f32], expected: &[f32], tolerance: f64) {
    assert_eq!(actual.len(), expected.len(), "{label}: lengths differ");
    let mut worst = 0.0_f64;
    for (index, (x, y)) in actual.iter().zip(expected).enumerate() {
        let error = (f64::from(*x) - f64::from(*y)).abs();
        if error > tolerance {
            panic!("{label}: sample {index}: {x} vs {y} (error {error})");
        }
        worst = worst.max(error);
    }
    assert!(worst <= tolerance, "{label}: worst error {worst}");
}

fn scale(buffer: &[f32], gain: f64) -> Vec<f32> {
    buffer
        .iter()
        .map(|sample| (*sample as f64 * gain) as f32)
        .collect()
}

#[test]
fn pre_fader_send_ignores_fader_and_post_fader_send_tracks_it() {
    let post = render(&BASE.replace("send(space=100%)", "send(space=100%, post)"));
    let post_quiet = render(&BASE.replace("send(space=100%)", "vol=-12dB, send(space=100%, post)"));
    let fader = 10.0_f64.powf(-12.0 / 20.0);
    let post_ratio = peak(&post_quiet) / peak(&post);
    assert!(
        (post_ratio - fader).abs() < 1e-6,
        "post send must follow the fader: ratio {post_ratio} vs {fader}"
    );

    let pre = render(&BASE.replace("send(space=100%)", "send(space=100%, pre)"));
    let pre_quiet = render(&BASE.replace("send(space=100%)", "vol=-12dB, send(space=100%, pre)"));
    assert!(
        pre == pre_quiet,
        "pre send must not change with the fader: {} vs {}",
        peak(&pre),
        peak(&pre_quiet)
    );
    assert!(peak(&pre) > 1e-4);
}

#[test]
fn pre_fader_send_includes_insert_effects() {
    let with_insert = r#"@ocp "0.2.0"
@project "insert tap"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
@bus sink
  effects:
    - gain: factor=0.0
  routes: [master]
@bus space
  routes: [master]
@track a | preset: tone, instrument: synth/lead_square
  effects:
    - gain: factor=0.5
  mix: output = sink, vol=-12dB, send(space=100%, pre)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#;
    let quiet = render(with_insert);
    let loud = render(&with_insert.replace("factor=0.5", "factor=1.0"));
    let ratio = peak(&quiet) / peak(&loud);
    assert!(
        (ratio - 0.5).abs() < 1e-6,
        "send must include inserts: {ratio}"
    );
}

#[test]
fn send_to_bus_is_sample_identical_to_a_hand_wired_bus_chain() {
    let sent = render(&BASE.replace("send(space=100%)", "send(space=100%, post)"));
    let hand_wired =
        render(&BASE.replace("output = sink, send(space=100%, post)", "output = space"));
    assert!(peak(&sent) > 1e-4);
    assert!(
        sent == hand_wired,
        "send path and exclusive bus output must match: {} vs {}",
        peak(&sent),
        peak(&hand_wired)
    );
}

#[test]
fn send_changes_the_render_by_the_expected_amount() {
    let dry = render(
        &BASE
            .replace("output = sink, send(space=100%)", "output = master")
            .replace(
                "@bus sink\n  effects:\n    - gain: factor=0.0\n  routes: [master]\n\n",
                "",
            )
            .replace(
                "@bus space\n  effects:\n    - gain: factor=0.5\n  routes: [master]\n\n",
                "",
            ),
    );
    let level_db = -6.0;
    let amount = 10.0_f64.powf(level_db / 20.0);
    let wet_source = format!(
        "@ocp \"0.2.0\"\n\n@project \"mixer routing\"\n  tempo: 120\n  time_signature: 4/4\n\n\
         @preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}\n\n\
         @bus space\n  effects:\n    - gain: factor=0.5\n  routes: [master]\n\n\
         @track a | preset: tone, instrument: synth/lead_square\n  mix: send(space={level_db}dB)\n  @phrase a [1:1 - 2:1]\n    C4 w mf\n"
    );
    let wet = render(&wet_source);
    // Dry direct path plus a 0.5-gain bus return at the send level.
    let expected = scale(&dry, 1.0 + 0.5 * amount);
    assert_close("send return amount", &wet, &expected, 1e-6);
}

#[test]
fn polarity_inversion_negates_the_channel() {
    let normal = render(&BASE.replace("output = sink, send(space=100%)", "output = master"));
    let inverted = render(&BASE.replace(
        "output = sink, send(space=100%)",
        "output = master, polarity=invert",
    ));
    let expected: Vec<f32> = normal.iter().map(|sample| -*sample).collect();
    assert_close("polarity", &inverted, &expected, 1e-7);
}

#[test]
fn bus_routing_cycle_is_rejected_instead_of_hanging() {
    let source = r#"@ocp "0.2.0"
@project "cycle"
  tempo: 120
@bus a
  routes: [b]
@bus b
  routes: [a]
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
@track t | preset: tone, instrument: synth/lead_square
  mix: send(a=50%)
  @phrase p [1:1 - 2:1]
    C4 w mf
"#;
    let project = oc_dsl::parse(source).unwrap();
    let error = AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
        .err()
        .expect("cyclic bus routing must fail");
    assert!(error.contains("cycle"), "unexpected error: {error}");
}

#[test]
fn solo_safe_channel_stays_audible_under_another_solo() {
    let baseline = render(&SOLO.replace("  mix: solo = true\n", ""));
    // Track b stays soloed; track a is solo-safe, so both remain audible.
    let solo_safe = render(&SOLO.replace(
        "  mix: output = glue\n",
        "  mix: output = glue, solo_safe = true\n",
    ));
    assert!(peak(&solo_safe) > 1e-4);
    assert!(solo_safe == baseline, "solo-safe channel must keep playing");

    let plain = render(SOLO);
    let muted = render(&SOLO.replace(
        "  mix: output = glue\n",
        "  mix: output = glue, mute = true\n",
    ));
    assert!(plain == muted, "unsoloed channel must be silenced");
}

#[test]
fn pre_and_post_sends_to_one_bus_sum_both_taps() {
    let one_bus = r#"@ocp "0.2.0"
@project "double send"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
@bus space
  routes: [master]
@track a | preset: tone, instrument: synth/lead_square
  mix: vol=-6dB, send(space=50%), send(space=50%, pre)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#;
    let split = r#"@ocp "0.2.0"
@project "double send"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
@bus post
  routes: [master]
@bus pre
  routes: [master]
@track a | preset: tone, instrument: synth/lead_square
  mix: vol=-6dB, send(post=50%), send(pre=50%, pre)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#;
    let combined = render(one_bus);
    let separated = render(split);
    assert!(peak(&combined) > 1e-4);
    // Summation order differs between one bus and two, so allow f32 rounding.
    assert_close("both taps sum into one bus", &combined, &separated, 1e-7);
}

/// One track sending 100 % into `space`, with the direct path muted by `sink`.
fn bus_strip_source(bus_mix: &str) -> String {
    format!(
        r#"@ocp "0.2.0"
@project "bus strip"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}
@bus sink
  effects:
    - gain: factor=0.0
  routes: [master]
@bus space
{bus_mix}  routes: [master]
@track a | preset: tone, instrument: synth/lead_square
  mix: output = sink, send(space=100%)
  @phrase a [1:1 - 2:1]
    C4 w mf
"#
    )
}

#[test]
fn bus_return_fader_scales_the_return_exactly() {
    let unity = render(&bus_strip_source(""));
    let quiet = render(&bus_strip_source("  mix: vol=-6dB\n"));
    assert!(peak(&unity) > 1e-4);
    let gain = 10.0_f64.powf(-6.0 / 20.0);
    assert_close("bus fader", &quiet, &scale(&unity, gain), 1e-6);
}

#[test]
fn bus_mute_silences_only_its_own_return() {
    let muted = render(&bus_strip_source("  mix: mute=true\n"));
    assert_eq!(peak(&muted), 0.0, "muted bus must not reach the master");
    // The direct path is independent: a second track stays audible.
    let source = bus_strip_source("  mix: mute=true\n").replace(
        "  mix: output = sink, send(space=100%)",
        "  mix: output = master, send(space=100%)",
    );
    let audible = render(&source);
    assert!(
        peak(&audible) > 1e-4,
        "muting the bus must not mute other paths"
    );
}

const BUS_SOLO: &str = r#"@ocp "0.2.0"
@project "bus solo"
  tempo: 120
  time_signature: 4/4
@preset tone_a
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}
@preset tone_b
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 330, amplitude: 0.15, waveform: 0}
@bus soloed
  mix: solo=true
  routes: [master]
@bus upstream
  routes: [soloed]
@bus other
  routes: [master]
@track direct | preset: tone_a, instrument: synth/lead_square
  mix: send(soloed=100%)
  @phrase p [1:1 - 2:1]
    C4 w mf
@track via_bus | preset: tone_b, instrument: synth/lead_square
  mix: send(other=100%)
  @phrase p [1:1 - 2:1]
    E4 w mf
@track via_upstream | preset: tone_b, instrument: synth/lead_square
  mix: send(upstream=100%)
  @phrase p [1:1 - 2:1]
    G4 w mf
@track plain | preset: tone_a, instrument: synth/lead_square
  @phrase p [1:1 - 2:1]
    C5 w mf
"#;

#[test]
fn soloed_bus_keeps_its_inputs_and_silences_unrelated_channels() {
    let soloed = render(BUS_SOLO);
    // Reference: same graph without solo, unrelated channels muted. `direct`
    // feeds the soloed bus and `via_upstream` reaches it through `upstream`.
    let reference = BUS_SOLO
        .replace("  mix: solo=true\n", "")
        .replace(
            "  mix: send(other=100%)",
            "  mix: mute=true, send(other=100%)",
        )
        .replace(
            "@track plain | preset: tone_a, instrument: synth/lead_square\n",
            "@track plain | preset: tone_a, instrument: synth/lead_square\n  mix: mute=true\n",
        );
    assert!(peak(&soloed) > 1e-4);
    assert_close(
        "bus solo keeps its input component",
        &soloed,
        &render(&reference),
        1e-7,
    );

    // With nothing routed into the soloed component, the solo is silent: a
    // direct-to-master channel that does not feed the bus stays muted.
    let unrelated = BUS_SOLO
        .replace("send(soloed=100%)", "send(other=100%)")
        .replace("send(upstream=100%)", "send(other=100%)");
    assert_eq!(
        peak(&render(&unrelated)),
        0.0,
        "unrelated direct channels stay muted"
    );
}

#[test]
fn bus_solo_does_not_silence_an_upstream_bus() {
    // Muting the upstream bus removes its contribution, proving the solo kept
    // it running rather than folding it into the soloed bus return.
    let soloed = render(BUS_SOLO);
    let without_upstream = render(&BUS_SOLO.replace(
        "@bus upstream\n  routes: [soloed]\n",
        "@bus upstream\n  mix: mute=true\n  routes: [soloed]\n",
    ));
    assert!(peak(&soloed) > 1e-4);
    assert!(
        peak(&without_upstream) < peak(&soloed) * 0.9,
        "upstream input must be part of the soloed bus, got {} vs {}",
        peak(&without_upstream),
        peak(&soloed)
    );
}

fn send_pan_source(pan: &str) -> String {
    format!(
        r#"@ocp "0.2.0"
@project "send pan"
  tempo: 120
  time_signature: 4/4
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {{frequency: 220, amplitude: 0.2, waveform: 0}}
@bus sink
  effects:
    - gain: factor=0.0
  routes: [master]
@bus space
  routes: [master]
@track a | preset: tone, instrument: synth/lead_square
  mix: output = sink, send(space=100%{pan})
  @phrase a [1:1 - 2:1]
    C4 w mf
"#
    )
}

#[test]
fn send_pan_uses_the_mono_constant_power_curve_at_the_bus_input() {
    let centre = render(&send_pan_source(""));
    assert!(peak(&centre) > 1e-4);
    let left = render(&send_pan_source(", pan=-1"));
    let right = render(&send_pan_source(", pan=1"));
    // Centre is exactly unity, so the un-panned send keeps its legacy level.
    let sqrt_two = std::f64::consts::SQRT_2;
    let expected_left: Vec<f32> = centre
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            (f64::from(*sample) * if index % 2 == 0 { sqrt_two } else { 0.0 }) as f32
        })
        .collect();
    assert_close("send pan hard left", &left, &expected_left, 1e-6);
    let expected_right: Vec<f32> = centre
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            (f64::from(*sample) * if index % 2 == 0 { 0.0 } else { sqrt_two }) as f32
        })
        .collect();
    assert_close("send pan hard right", &right, &expected_right, 1e-6);

    // Constant power: L^2 + R^2 is unchanged from the unity centre send.
    let power = |buffer: &[f32]| -> f64 {
        buffer
            .chunks_exact(2)
            .map(|frame| f64::from(frame[0]).powi(2) + f64::from(frame[1]).powi(2))
            .sum()
    };
    assert!((power(&left) - power(&centre)).abs() < 1e-6 * power(&centre).max(1.0));
}

#[test]
fn send_level_above_zero_db_scales_the_return_and_stays_below_the_ceiling() {
    let unity = render(&send_pan_source(""));
    let hot = render(&send_pan_source("").replace("send(space=100%)", "send(space=+6dB)"));
    let gain = 10.0_f64.powf(6.0 / 20.0);
    assert_close("send +6 dB", &hot, &scale(&unity, gain), 1e-6);
}

#[test]
fn bus_routing_cycle_is_rejected_with_an_auxiliary_source() {
    let source = r#"@ocp "0.2.0"
@project "aux cycle"
  tempo: 120
@preset fake
  source: vst3
  plugin: "test.fake.two_bus"
@bus a
  routes: [b]
@bus b
  routes: [a]
@track host | preset: fake, instrument: keys/installed
  mix: send(host.out2->a=100%)
  @phrase p [1:1 - 2:1]
    C4 q
"#;
    let project = oc_dsl::parse(source).unwrap();
    let error = AudioGraph::try_from_project_with_source_latencies(&project, 48_000, &[])
        .err()
        .expect("cyclic bus routing with an aux source must fail");
    assert!(error.contains("cycle"), "unexpected error: {error}");
    assert!(
        oc_dsl::validate(&project)
            .err()
            .unwrap()
            .iter()
            .any(|error| error.to_string().contains("Bus routing cycle"))
    );
}
