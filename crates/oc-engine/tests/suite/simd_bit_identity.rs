//! A forced backend must reproduce the default render bit for bit.
//!
//! The buffers come from the real render path (built-in source, pan law, sends,
//! buses, polarity and the master fader), not from a synthetic generator.

use std::collections::HashMap;
use std::path::Path;

use oc_dsp::simd::{self, SimdLevel};
use oc_engine::render_cache::RenderCache;

const RATE: u32 = 48_000;

/// Fader, pan, polarity, pre/post sends, bus gains and the master fader.
const PROJECT: &str = r#"@ocp "0.2.0"

@project "SIMD identity"
  tempo: 132
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.35, waveform: 0}

@bus space
  mix: vol=-4.5dB
  routes: [master]

@bus delay
  mix: vol=-2dB
  routes: [master]

@track lead | preset: tone, instrument: synth/lead_square
  mix: vol=-6.2dB, pan=0.35, send(space=-6dB, pre), send(delay=25%, pan=-0.4), polarity=invert
  @phrase line [1:1 - 2:1]
    C4 8th mf
    E4 8th mf
    G4 q mf
    B4 q mf

@track pad | preset: tone, instrument: synth/pad_warm
  mix: vol=-9dB, pan=-0.6
  @phrase bed [1:1 - 2:1]
    C3 w mp
    G2 w mp

@master
  volume: -1.5dB
"#;

fn render() -> Vec<f32> {
    let project = oc_dsl::parse(PROJECT).expect("project parses");
    let mut cache = RenderCache::new();
    cache
        .render_full(
            &project,
            Path::new("/nonexistent/soundfont.sf2"),
            RATE,
            &mut HashMap::new(),
        )
        .expect("render")
}

fn assert_same_bits(reference: &[f32], actual: &[f32], context: &str) {
    assert_eq!(reference.len(), actual.len(), "{context} length");
    for (index, (reference, actual)) in reference.iter().zip(actual).enumerate() {
        assert_eq!(
            reference.to_bits(),
            actual.to_bits(),
            "{context} differs at {index}: {reference} vs {actual}"
        );
    }
}

#[test]
fn forced_backends_match_the_default_render_bit_for_bit() {
    let default = render();
    assert!(
        default.iter().any(|sample| *sample != 0.0),
        "the test project must produce audio"
    );

    assert!(simd::select(SimdLevel::Scalar));
    let scalar = render();
    assert_same_bits(&default, &scalar, "scalar");

    for level in [SimdLevel::Sse2, SimdLevel::Avx2, SimdLevel::Neon] {
        if !simd::select(level) {
            continue;
        }
        let forced = render();
        assert_same_bits(&default, &forced, level.name());
    }

    assert!(simd::select(SimdLevel::detect()));
}
