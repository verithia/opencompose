//! Guards the EQ response table shared with the GUI curve editor.
//! Regenerate with `OC_EQ_RESPONSE_FIXTURE=write cargo test -p oc-dsp --test all devices::eq_response_fixture::`.

use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;

use oc_dsp::AudioEffect;
use oc_dsp::biquad::{BiquadCoefficients, FilterType};
use oc_dsp::dynamic_eq::{BandKind, ChannelTarget, DynamicEq};
use serde::Serialize;

const SR: u32 = 48_000;
const POINTS: usize = 32;
const TOLERANCE_DB: f64 = 0.1;

#[derive(Serialize, Clone)]
struct FixtureBand {
    slot: &'static str,
    shape: &'static str,
    freq: f64,
    q: f64,
    gain_db: f64,
}

#[derive(Serialize, Clone)]
struct Case {
    name: &'static str,
    bands: Vec<FixtureBand>,
    analytic_db: Vec<f64>,
    measured_db: Vec<f64>,
}

#[derive(Serialize)]
struct Fixture {
    sample_rate: u32,
    frequencies: Vec<f64>,
    cases: Vec<Case>,
}

fn band(slot: &'static str, shape: &'static str, freq: f64, q: f64, gain_db: f64) -> FixtureBand {
    FixtureBand {
        slot,
        shape,
        freq,
        q,
        gain_db,
    }
}

fn filter_type(shape: &str) -> FilterType {
    match shape {
        "bell" => FilterType::Peak,
        "low_shelf" => FilterType::LowShelf,
        "high_shelf" => FilterType::HighShelf,
        "low_cut" => FilterType::HighPass,
        "high_cut" => FilterType::LowPass,
        "notch" => FilterType::Notch,
        "band_pass" => FilterType::BandPass,
        other => panic!("unknown shape {other}"),
    }
}

fn band_kind(shape: &str) -> BandKind {
    match shape {
        "bell" => BandKind::Bell,
        "low_shelf" => BandKind::LowShelf,
        "high_shelf" => BandKind::HighShelf,
        "low_cut" => BandKind::LowCut,
        "high_cut" => BandKind::HighCut,
        "notch" => BandKind::Notch,
        "band_pass" => BandKind::BandPass,
        other => panic!("unknown shape {other}"),
    }
}

fn cases() -> Vec<Case> {
    let definitions: Vec<(&'static str, Vec<FixtureBand>)> = vec![
        ("flat", vec![]),
        ("bell_boost", vec![band("band1", "bell", 1_000.0, 1.0, 9.0)]),
        (
            "bell_cut_wide",
            vec![band("band1", "bell", 250.0, 2.0, -6.0)],
        ),
        (
            "low_shelf_boost",
            vec![band("band1", "low_shelf", 200.0, 0.707, 6.0)],
        ),
        (
            "high_shelf_cut",
            vec![band("band1", "high_shelf", 8_000.0, 0.707, -6.0)],
        ),
        (
            "cuts",
            vec![
                band("low_cut", "low_cut", 35.0, 0.707, 0.0),
                band("high_cut", "high_cut", 18_000.0, 0.707, 0.0),
            ],
        ),
        (
            "notch_and_band_pass",
            vec![
                band("band1", "notch", 1_200.0, 4.0, 0.0),
                band("band2", "band_pass", 3_000.0, 2.0, 0.0),
            ],
        ),
        (
            "mastering_chain",
            vec![
                band("band1", "low_shelf", 120.0, 0.707, 3.0),
                band("band2", "bell", 400.0, 1.6, -4.0),
                band("band3", "bell", 2_500.0, 1.2, 5.0),
                band("band4", "high_shelf", 6_000.0, 0.707, -4.0),
                band("band5", "low_cut", 30.0, 0.707, 0.0),
                band("band6", "high_cut", 19_000.0, 0.707, 0.0),
            ],
        ),
    ];

    definitions
        .into_iter()
        .map(|(name, bands)| {
            let analytic_db = bands
                .iter()
                .map(|entry| analytic_band_db(entry, FREQUENCIES.as_slice()))
                .collect::<Vec<_>>();
            let analytic_db = (0..POINTS)
                .map(|index| analytic_db.iter().map(|rows| rows[index]).sum())
                .collect::<Vec<_>>();
            let measured_db = measure_case(&bands);
            Case {
                name,
                bands,
                analytic_db,
                measured_db,
            }
        })
        .collect()
}

fn analytic_band_db(entry: &FixtureBand, probes: &[f64]) -> Vec<f64> {
    let coefficients = BiquadCoefficients::design(
        filter_type(entry.shape),
        entry.freq,
        entry.q,
        entry.gain_db,
        SR as f64,
    );
    probes
        .iter()
        .map(|probe| coefficients.response_db(*probe, SR as f64))
        .collect()
}

fn measure_case(bands: &[FixtureBand]) -> Vec<f64> {
    let mut eq = DynamicEq::new(SR);
    for entry in bands {
        let (prefix, shape) = match entry.slot {
            "low_cut" => ("low_cut", "low_cut"),
            "high_cut" => ("high_cut", "high_cut"),
            slot => (slot, entry.shape),
        };
        match prefix {
            "low_cut" | "high_cut" => {
                eq.set_param(&format!("{prefix}_enabled"), 1.0);
                eq.set_param(&format!("{prefix}_freq"), entry.freq);
                eq.set_param(&format!("{prefix}_q"), entry.q);
            }
            _ => {
                eq.set_param(&format!("{prefix}_enabled"), 1.0);
                eq.set_param(&format!("{prefix}_type"), band_kind(shape).code());
                eq.set_param(&format!("{prefix}_freq"), entry.freq);
                eq.set_param(&format!("{prefix}_q"), entry.q);
                eq.set_param(&format!("{prefix}_gain"), entry.gain_db);
                eq.set_param(&format!("{prefix}_channel"), ChannelTarget::Stereo.code());
            }
        }
    }
    FREQUENCIES
        .iter()
        .map(|probe| measured_gain_db(&mut eq, *probe))
        .collect()
}

fn measured_gain_db(eq: &mut DynamicEq, freq: f64) -> f64 {
    let amplitude = 0.25;
    let settle = (SR as f64 * 0.75) as usize;
    let window = (SR as f64 * 0.5) as usize;
    let frames = settle + window;
    let mut buffer = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        let value = (TAU * freq * frame as f64 / SR as f64).sin() * amplitude;
        buffer[frame * 2] = value as f32;
        buffer[frame * 2 + 1] = value as f32;
    }
    eq.reset();
    eq.process(&mut buffer, SR);

    let mut real = 0.0;
    let mut imaginary = 0.0;
    let mut weight_sum = 0.0;
    for offset in 0..window {
        let t = offset as f64 / (window - 1) as f64;
        let weight = 0.5 - 0.5 * (TAU * t).cos();
        let phase = TAU * freq * offset as f64 / SR as f64;
        let sample = buffer[(settle + offset) * 2] as f64;
        real += weight * sample * phase.cos();
        imaginary -= weight * sample * phase.sin();
        weight_sum += weight;
    }
    let measured = 2.0 * (real * real + imaginary * imaginary).sqrt() / weight_sum;
    20.0 * (measured / amplitude).log10()
}

fn frequencies() -> Vec<f64> {
    (0..POINTS)
        .map(|index| 20.0 * (1_000.0f64).powf(index as f64 / (POINTS - 1) as f64))
        .collect()
}

static FREQUENCIES: std::sync::LazyLock<Vec<f64>> = std::sync::LazyLock::new(frequencies);
static CASES: std::sync::LazyLock<Vec<Case>> = std::sync::LazyLock::new(cases);

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../oc-gui/frontend/src/devices/__fixtures__/eqResponseTable.json")
}

#[test]
fn response_table_matches_the_measured_device() {
    for case in CASES.iter() {
        for (index, probe) in FREQUENCIES.iter().enumerate() {
            let delta = (case.measured_db[index] - case.analytic_db[index]).abs();
            assert!(
                delta < TOLERANCE_DB,
                "{} at {probe:.1} Hz: measured {:.3} dB, analytic {:.3} dB",
                case.name,
                case.measured_db[index],
                case.analytic_db[index]
            );
        }
    }
}

#[test]
fn response_table_file_is_current() {
    let fixture = Fixture {
        sample_rate: SR,
        frequencies: FREQUENCIES.to_vec(),
        cases: CASES.clone(),
    };
    let generated = serde_json::to_string_pretty(&fixture).unwrap() + "\n";
    let path = fixture_path();
    if std::env::var("OC_EQ_RESPONSE_FIXTURE").as_deref() == Ok("write") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
        return;
    }
    let existing = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} is missing ({error}); regenerate with OC_EQ_RESPONSE_FIXTURE=write",
            path.display()
        )
    });
    assert_eq!(
        existing,
        generated,
        "{} is stale; regenerate with OC_EQ_RESPONSE_FIXTURE=write",
        path.display()
    );
}
