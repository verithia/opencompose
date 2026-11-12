//! Export precision: sample formats, dither, and the project setting that
//! selects them.

use std::path::{Path, PathBuf};

use oc_engine::encode::{
    EncodeSpec, encode_to_file,
    wav::{SampleFormat, WavOptions},
};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

const RATE: u32 = 48_000;
const TONE_HZ: usize = 1_000;

fn temp_path(name: &str) -> PathBuf {
    oc_test_support::temp_dir("ocp-export-format", name).join("out.wav")
}

fn spec(options: WavOptions) -> EncodeSpec {
    EncodeSpec {
        container: oc_dsl::ast::ExportContainer::Wav,
        wav: options,
        flac: oc_engine::encode::FlacOptions::default(),
        mp3: oc_engine::encode::Mp3Options::default(),
        sample_rate: RATE,
        channels: 2,
    }
}

fn write(path: &Path, samples: &[f32], options: WavOptions) {
    encode_to_file(path, &spec(options), samples).expect("wav written");
}

fn read_i16(path: &Path) -> Vec<i32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 16);
    assert_eq!(reader.spec().sample_format, hound::SampleFormat::Int);
    reader
        .samples::<i16>()
        .map(|s| i32::from(s.unwrap()))
        .collect()
}

fn read_i24(path: &Path) -> Vec<i32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 24);
    assert_eq!(reader.spec().sample_format, hound::SampleFormat::Int);
    reader.samples::<i32>().map(|s| s.unwrap()).collect()
}

fn read_f32(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().bits_per_sample, 32);
    assert_eq!(reader.spec().sample_format, hound::SampleFormat::Float);
    reader.samples::<f32>().map(|s| s.unwrap()).collect()
}

/// A bin-centred 1 kHz sine, so its harmonics land on whole FFT bins.
fn sine(amplitude: f64, frames: usize) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let phase = std::f64::consts::TAU * TONE_HZ as f64 * frame as f64 / RATE as f64;
        let value = (amplitude * phase.sin()) as f32;
        samples.push(value);
        samples.push(value);
    }
    samples
}

/// RMS of everything that is not the tone or one of its harmonics.
///
/// The tone is bin-centred, and the 16-bit quantiser's error on a periodic
/// signal is itself periodic, so the undithered error lands only on harmonic
/// bins. Dither spreads energy across the whole band.
fn noise_floor_dbfs(samples: &[i32]) -> f64 {
    let frames = samples.len() / 2;
    let mut buffer: Vec<Complex<f64>> = (0..frames)
        .map(|frame| Complex::new(f64::from(samples[frame * 2]) / 32768.0, 0.0))
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(frames);
    fft.process(&mut buffer);
    let scale = 2.0 / frames as f64;
    let mut noise_power = 0.0;
    for (bin, value) in buffer.iter().enumerate().take(frames / 2 + 1) {
        let harmonic = bin % TONE_HZ == 0;
        if harmonic {
            continue;
        }
        let amplitude = value.norm() * scale;
        noise_power += amplitude * amplitude / 2.0;
    }
    let rms = noise_power.sqrt();
    if rms == 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

fn boundary_samples() -> Vec<f32> {
    let mut samples = vec![0.0, 1.0, -1.0, 0.5, -0.5];
    let mut state = 0x1234_5678u64;
    for _ in 0..4_001 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = ((state >> 11) as f64 / (1u64 << 53) as f64) as f32;
        samples.push(unit * 2.0 - 1.0);
    }
    samples
}

#[test]
fn dithered_i16_raises_the_noise_floor_but_keeps_it_below_minus_80_dbfs() {
    let samples = sine(10.0_f64.powf(-90.0 / 20.0), RATE as usize);
    let plain = temp_path("plain");
    let dithered = temp_path("dithered");
    write(
        &plain,
        &samples,
        WavOptions {
            depth: SampleFormat::I16,
            dither: false,
            ..WavOptions::default()
        },
    );
    write(
        &dithered,
        &samples,
        WavOptions {
            depth: SampleFormat::I16,
            dither: true,
            ..WavOptions::default()
        },
    );

    let plain_floor = noise_floor_dbfs(&read_i16(&plain));
    let dithered_floor = noise_floor_dbfs(&read_i16(&dithered));
    println!(
        "i16 -90 dBFS sine noise floor: plain {plain_floor:.1} dBFS, dithered {dithered_floor:.1} dBFS"
    );
    assert!(
        dithered_floor > plain_floor,
        "dither must raise the broadband floor: {dithered_floor:.1} vs {plain_floor:.1} dBFS"
    );
    assert!(
        dithered_floor < -80.0,
        "1 LSB TPDF must stay below -80 dBFS: {dithered_floor:.1} dBFS"
    );
}

#[test]
fn i16_without_dither_keeps_the_historical_truncation() {
    let samples = boundary_samples();
    let path = temp_path("legacy");
    write(&path, &samples, WavOptions::default());
    let written = read_i16(&path);
    assert_eq!(written.len(), samples.len());
    for (index, (&sample, &value)) in samples.iter().zip(&written).enumerate() {
        let expected = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
        assert_eq!(value, i32::from(expected), "sample {index} moved");
    }
}

#[test]
fn i24_and_f32_round_trip_the_internal_samples() {
    let samples = boundary_samples();
    let i24_path = temp_path("i24");
    let f32_path = temp_path("f32");
    write(
        &i24_path,
        &samples,
        WavOptions {
            depth: SampleFormat::I24,
            ..WavOptions::default()
        },
    );
    write(
        &f32_path,
        &samples,
        WavOptions {
            depth: SampleFormat::F32,
            ..WavOptions::default()
        },
    );

    let round_tripped = read_f32(&f32_path);
    assert_eq!(round_tripped.len(), samples.len());
    for (index, (&expected, &actual)) in samples.iter().zip(&round_tripped).enumerate() {
        assert_eq!(
            expected.to_bits(),
            actual.to_bits(),
            "f32 sample {index} changed"
        );
    }

    let quantised = read_i24(&i24_path);
    assert_eq!(quantised.len(), samples.len());
    for (index, (&sample, &value)) in samples.iter().zip(&quantised).enumerate() {
        let expected = f64::from(sample) * 8_388_607.0;
        let error = (f64::from(value) - expected).abs();
        assert!(
            error <= 1.0,
            "i24 sample {index} is {value}, expected {expected:.3} (error {error:.3} LSB)"
        );
    }
}

#[test]
fn the_same_seed_is_byte_identical_and_a_new_seed_moves_only_the_low_bits() {
    let samples = boundary_samples();
    let first = temp_path("seed-a");
    let again = temp_path("seed-b");
    let perturbed = temp_path("seed-c");
    let options = WavOptions {
        depth: SampleFormat::I16,
        dither: true,
        dither_seed: 7,
    };
    write(&first, &samples, options);
    write(&again, &samples, options);
    write(
        &perturbed,
        &samples,
        WavOptions {
            dither_seed: options.dither_seed + 1,
            ..options
        },
    );

    let first_bytes = std::fs::read(&first).unwrap();
    let again_bytes = std::fs::read(&again).unwrap();
    assert_eq!(
        first_bytes, again_bytes,
        "the same dither seed must repeat byte for byte"
    );

    let base = read_i16(&first);
    let other = read_i16(&perturbed);
    assert_eq!(base.len(), other.len());
    // The dither spans two LSBs, so a sample sitting between two rounding
    // boundaries can move by two steps. Everything above the second bit stays.
    let mut changed = 0;
    let mut double_steps = 0;
    for (index, (&base, &other)) in base.iter().zip(&other).enumerate() {
        let step = (base - other).abs();
        assert!(step <= 2, "sample {index} moved by {step} LSB");
        if step > 0 {
            changed += 1;
            double_steps += usize::from(step == 2);
        }
    }
    assert!(changed > 0, "a different seed must change some LSBs");
    assert!(
        double_steps * 10 < changed,
        "{double_steps} of {changed} changed samples moved two steps"
    );
}

#[test]
fn dither_is_refused_for_float_output() {
    let path = temp_path("refused");
    let error = encode_to_file(
        &path,
        &spec(WavOptions {
            depth: SampleFormat::F32,
            dither: true,
            ..WavOptions::default()
        }),
        &[0.0, 0.0],
    )
    .expect_err("float dither must be refused");
    assert!(
        error.contains("meaningless for f32"),
        "unreadable refusal: {error}"
    );
    assert!(!path.exists(), "a refused write must not create a file");
}

#[test]
fn the_project_header_selects_the_format_and_dither() {
    let source = r#"@ocp "0.2.0"

@project "export settings"
  tempo: 120
  time_signature: 4/4
  export: {container: wav, wav: {depth: i24, dither: true}}
"#;
    let project = oc_dsl::parse(source).expect("parses");
    let project_spec = EncodeSpec::for_project(&project, RATE);
    assert_eq!(project_spec.container, oc_dsl::ast::ExportContainer::Wav);
    assert_eq!(project_spec.wav.depth, SampleFormat::I24);
    assert!(project_spec.wav.dither);

    let plain = oc_dsl::parse("@project \"no export\"\n  tempo: 120\n").expect("parses");
    let defaults = EncodeSpec::for_project(&plain, RATE);
    assert_eq!(defaults.wav.depth, SampleFormat::I16);
    assert!(!defaults.wav.dither);

    // The historical flat block still selects the same options.
    let legacy =
        oc_dsl::parse("@project\n  export: {format: i24, dither: true}\n").expect("parses");
    let spec = EncodeSpec::for_project(&legacy, RATE);
    assert_eq!(spec.wav.depth, SampleFormat::I24);
    assert!(spec.wav.dither);
}
