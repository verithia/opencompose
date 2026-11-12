//! A preset that leaves its voice filter off must render with its top octave
//! intact: the import may not fall back to the engine's default low-pass.

use std::path::{Path, PathBuf};

use oc_synth_core::synth::{OpenSynth, SynthPatch};
use oc_synth_import::foreign_preset::{ForeignPreset, MAGIC};

const RATE: f64 = 48_000.0;
const NOTE: u8 = 48;
const FUNDAMENTAL: f64 = 130.812_782_877_572_55;

/// A `.SerumPreset` container around one JSON state tree, as the format stores it.
fn write_preset(dir: &Path, state: &serde_json::Value) -> PathBuf {
    let metadata = serde_json::json!({ "presetName": "filter off" });
    let metadata = serde_json::to_vec(&metadata).expect("metadata must encode");
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(state, &mut cbor).expect("state must encode");
    let packed = zstd::stream::encode_all(&cbor[..], 3).expect("state must compress");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(&(cbor.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&packed);
    let path = dir.join("filter-off.SerumPreset");
    std::fs::write(&path, bytes).expect("preset must write");
    path
}

/// A one-frame band-limited saw table, so the readout has a full top octave.
fn write_saw_table(dir: &Path) -> PathBuf {
    const SIZE: usize = 2048;
    let mut samples = vec![0.0f32; SIZE];
    for (index, sample) in samples.iter_mut().enumerate() {
        let phase = index as f64 / SIZE as f64;
        let mut value = 0.0;
        for harmonic in 1..SIZE / 2 {
            value += (std::f64::consts::TAU * phase * harmonic as f64).sin() / harmonic as f64;
        }
        *sample = value as f32;
    }
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    for sample in &mut samples {
        *sample /= peak;
    }
    let path = dir.join("saw.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&path, spec).expect("table must write");
    for sample in samples {
        writer
            .write_sample(sample)
            .expect("table sample must write");
    }
    writer.finalize().expect("table must finalize");
    path
}

/// The state tree of a preset whose filter is off: the enable key is absent
/// and the controls beside it are stale UI values.
fn filter_off_state(enable: Option<f64>) -> serde_json::Value {
    let mut filter = serde_json::json!({
        "kParamFreq": 0.431_685_775_518_417_36,
        "kParamReso": 10.0,
    });
    if let Some(enable) = enable {
        filter["kParamEnable"] = serde_json::json!(enable);
    }
    serde_json::json!({
        "Oscillator0": {
            "WTOsc0": { "plainParams": { "kParamTablePos": 1.0 }, "relativePathToWT": "saw.wav" },
            "plainParams": { "kParamEnable": 1.0, "kParamVolume": 0.7 }
        },
        "VoiceFilter0": { "plainParams": filter },
        "Env0": { "plainParams": { "kParamAttack": 0.16571, "kParamDecay": 0.41629 } }
    })
}

fn render(patch: SynthPatch) -> Vec<f32> {
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, NOTE, 100);
    (0..(RATE as usize * 3 / 2))
        .map(|_| synth.next_stereo(RATE as u32)[0])
        .collect()
}

/// Energy in `lo..hi` of the steady part, from a Hann-windowed spectrum.
fn band_power(samples: &[f32], lo: f64, hi: f64) -> f64 {
    let fft = rustfft::FftPlanner::new().plan_fft_forward(32_768);
    let start = (RATE as usize) / 5;
    let mut buffer: Vec<rustfft::num_complex::Complex<f64>> = samples[start..start + 32_768]
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let window = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / 32_767.0).cos();
            rustfft::num_complex::Complex::new(f64::from(*sample) * window, 0.0)
        })
        .collect();
    fft.process(&mut buffer);
    let bin_hz = RATE / 32_768.0;
    buffer
        .iter()
        .enumerate()
        .filter(|(bin, _)| {
            let hz = *bin as f64 * bin_hz;
            (*bin > 0 && *bin < 16_384) && hz >= lo && hz < hi
        })
        .map(|(_, value)| value.norm_sqr())
        .sum()
}

fn top_octave_ratio(samples: &[f32]) -> f64 {
    let mid = band_power(samples, 1_000.0, 4_000.0);
    let top = band_power(samples, 16_000.0, 20_000.0);
    10.0 * (top / mid).log10()
}

/// The same ratio for an ideal band-limited saw at the note's fundamental.
fn ideal_ratio() -> f64 {
    let band = |lo: f64, hi: f64| {
        let mut power = 0.0;
        let mut harmonic = 1;
        loop {
            let hz = FUNDAMENTAL * f64::from(harmonic);
            if hz >= RATE / 2.0 {
                break;
            }
            if hz >= lo && hz < hi {
                power += 1.0 / f64::from(harmonic).powi(2);
            }
            harmonic += 1;
        }
        power
    };
    10.0 * (band(16_000.0, 20_000.0) / band(1_000.0, 4_000.0)).log10()
}

#[test]
fn a_filter_off_preset_renders_its_top_octave() {
    let dir = std::env::temp_dir().join(format!("ocp-filter-off-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir must exist");
    write_saw_table(&dir);

    let off = ForeignPreset::load(&write_preset(&dir, &filter_off_state(None)))
        .expect("filter-off preset must parse")
        .to_synth_patch(Some(&dir));
    assert_eq!(
        off.filter_wet, 0.0,
        "a stored filter without an enable key is off"
    );

    let ours = top_octave_ratio(&render(off));
    let ideal = ideal_ratio();
    assert!(
        (ours - ideal).abs() <= 1.4,
        "filter-off top octave {ours:.2} dB is not within 1.4 dB of {ideal:.2} dB"
    );

    // The same preset with the filter engaged is the sound the mapping must not
    // produce: the stale 0.43 cutoff would bury the band far below the target.
    let engaged = ForeignPreset::load(&write_preset(&dir, &filter_off_state(Some(1.0))))
        .expect("engaged preset must parse")
        .to_synth_patch(Some(&dir));
    assert_eq!(engaged.filter_wet, 1.0, "an enabled filter still maps");
    let dark = top_octave_ratio(&render(engaged));
    assert!(
        ours - dark > 20.0,
        "the engaged filter should be far darker: {ours:.2} vs {dark:.2} dB"
    );

    std::fs::remove_dir_all(&dir).expect("temp dir must clean up");
}
