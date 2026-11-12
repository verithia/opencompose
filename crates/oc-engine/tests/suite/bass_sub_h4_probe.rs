//! Held-note harmonic probe for the bass voice's 4x family.
//!
//! The probe renders one held `C2` through the demo bass patch with the sources
//! muted one at a time, so the fourth harmonic can be attributed to the sub
//! table, the C layer, the voice filter or the rack EQs. It writes the raw
//! stereo f32 to `/tmp/h4-probe`; the analysis (harmonic magnitude and phase)
//! runs outside the test so the tables can be re-derived without rebuilding.
//!
//! Findings pinned here (see `docs/synth-character.md`):
//!
//! - The 4x family is linear: it is the sub saw table's own 4th harmonic,
//!   lifted by the rack EQ's low cut. Bypassing the voice filter changes H4 by
//!   less than 0.5 dB, so the ladder's input stage is not the generator.
//! - The total H4 is the vector sum of the sub's 4th harmonic and the C
//!   layer's fundamental (both at 32.7 Hz). With a shared start phase the pair
//!   lands in anti-phase and nulls by about 12 dB.
//!
//! Run with:
//! `cargo test -p oc-engine --release --test all bass_sub_h4_probe:: -- --ignored --nocapture`

use std::path::{Path, PathBuf};

use oc_synth::synth::{
    FilterModel, ModDestination, OpenSynth, SynthFxType, SynthPatch, load_project_patch,
};

const RATE: u32 = 48_000;
const NOTE: u8 = 36; // C2
const SECONDS: f64 = 10.0;
fn demo_path() -> PathBuf {
    std::env::var_os("OCP_DEMO_PROJECT")
        .map(PathBuf::from)
        .expect("set OCP_DEMO_PROJECT to a demo .ocp")
}
const OUT: &str = "/tmp/h4-probe";

fn bass_patch() -> SynthPatch {
    let demo = demo_path();
    let text = std::fs::read_to_string(&demo).expect("demo project");
    let project = oc_dsl::parse(&text).expect("demo parses");
    let preset = project
        .presets
        .iter()
        .find(|preset| preset.name == "demo_bass")
        .expect("demo_bass");
    load_project_patch(&preset.params, Some(demo.parent().unwrap())).expect("bass patch")
}

/// Render one held note; `tune` edits the patch before the voice starts.
fn render(tag: &str, tune: impl Fn(&mut SynthPatch)) -> Vec<f32> {
    let mut patch = bass_patch();
    tune(&mut patch);
    std::fs::create_dir_all(OUT).expect("out dir");
    let mut synth = OpenSynth::new(patch);
    synth.note_on(0, NOTE, 100);
    let frames = (SECONDS * RATE as f64) as usize;
    let mut samples = vec![0.0_f32; frames * 2];
    synth.render_interleaved(&mut samples, RATE);
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in &samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    let path = Path::new(OUT).join(format!("{tag}.f32"));
    std::fs::write(&path, bytes).expect("write probe");
    println!("{tag}: {} frames -> {}", samples.len() / 2, path.display());
    samples
}

fn mute_all(patch: &mut SynthPatch) {
    patch.oscillator_a_level = 0.0;
    patch.oscillator_b_level = 0.0;
    patch.oscillator_c_level = 0.0;
    patch.sub_level = 0.0;
    patch.noise_level = 0.0;
    patch.ring_mod_level = 0.0;
}

/// Keep only the two rack EQs, which are the modules the in-situ probe uses.
fn eq_only(patch: &mut SynthPatch) {
    patch.fx.retain(|module| module.fx_type == SynthFxType::Eq3);
    patch.fx_mod_routes.clear();
}

/// Remove only the route that drives oscillator C, keeping the matrix intact.
fn mute_c_route(patch: &mut SynthPatch) {
    patch
        .mod_routes
        .retain(|route| route.destination != ModDestination::OscillatorCLevel);
}

fn mute_noise_route(patch: &mut SynthPatch) {
    patch
        .mod_routes
        .retain(|route| route.destination != ModDestination::NoiseLevel);
}

/// Complex amplitude of harmonic `harmonic` in the steady window `4..9 s`.
fn harmonic(samples: &[f32], harmonic: usize) -> (f64, f64) {
    let f0 = 440.0 * 2f64.powf(-69.0 / 12.0);
    let window = &samples[(4.0 * RATE as f64) as usize * 2..(9.0 * RATE as f64) as usize * 2];
    let frames = window.len() / 2;
    let mut real = 0.0;
    let mut imaginary = 0.0;
    let mut weight = 0.0;
    for index in 0..frames {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (frames - 1) as f64).cos();
        let value = 0.5 * (f64::from(window[index * 2]) + f64::from(window[index * 2 + 1]));
        let phase = std::f64::consts::TAU * f0 * harmonic as f64 * index as f64 / f64::from(RATE);
        real += w * value * phase.cos();
        imaginary -= w * value * phase.sin();
        weight += w;
    }
    let scale = 2.0 / weight;
    (real * scale, imaginary * scale)
}

fn level_db(samples: &[f32], harmonic_index: usize) -> f64 {
    let (real, imaginary) = harmonic(samples, harmonic_index);
    let (base_real, base_imaginary) = harmonic(samples, 1);
    let base = (base_real * base_real + base_imaginary * base_imaginary).sqrt();
    20.0 * ((real * real + imaginary * imaginary).sqrt() / base.max(1e-15)).log10()
}

/// Peak / RMS / crest of the first `frames` stereo frames.
fn level(samples: &[f32], frames: usize) -> (f64, f64, f64) {
    let mut peak = 0.0_f64;
    let mut power = 0.0_f64;
    for frame in samples.chunks_exact(2).take(frames) {
        for sample in frame {
            let value = f64::from(*sample);
            peak = peak.max(value.abs());
            power += value * value;
        }
    }
    let rms = (power / (frames * 2) as f64).sqrt();
    (peak, rms, 20.0 * (peak / rms).log10())
}

#[test]
#[ignore = "measurement harness for the bass H4/H12 probe"]
fn render_bass_held_note_variants() {
    let base = bass_patch();

    render("full", |patch| {
        patch.fx.clear();
        patch.fx_mod_routes.clear();
    });
    render("full_fx", |patch| *patch = base.clone());

    render("sub", |patch| {
        eq_only(patch);
        mute_all(patch);
        patch.sub_level = base.sub_level;
    });

    render("c", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_noise_route(patch);
    });

    render("sub_c", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_noise_route(patch);
        patch.sub_level = base.sub_level;
    });

    render("sub_c_noise", |patch| {
        eq_only(patch);
        mute_all(patch);
        patch.sub_level = base.sub_level;
    });

    render("ab", |patch| {
        eq_only(patch);
        mute_all(patch);
        patch.oscillator_a_level = base.oscillator_a_level;
        patch.oscillator_b_level = base.oscillator_b_level;
    });

    // The voice filter is bypassed but the rack EQs stay: any 4x family left
    // here is linear, so the ladder cannot be its source.
    render("sub_filter_off", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_c_route(patch);
        mute_noise_route(patch);
        patch.sub_level = base.sub_level;
        patch.filter_wet = 0.0;
    });

    render("sub_drive0", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_c_route(patch);
        mute_noise_route(patch);
        patch.sub_level = base.sub_level;
        patch.filter_drive = 0.0;
    });

    render("sub_res0", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_c_route(patch);
        mute_noise_route(patch);
        patch.sub_level = base.sub_level;
        patch.filter_resonance = 0.0;
    });

    render("sub_linear", |patch| {
        eq_only(patch);
        mute_all(patch);
        mute_c_route(patch);
        mute_noise_route(patch);
        patch.sub_level = base.sub_level;
        patch.filter_drive = 0.0;
        patch.filter_resonance = 0.0;
        patch.filter_model = FilterModel::Svf;
        patch.filter_cutoff = 4_000.0;
    });
}

/// The in-situ harmonic table: sub + C + noise, voice filter on, rack EQs on.
#[test]
#[ignore = "measurement harness for the bass H4/H12 probe"]
fn render_insitu_variants() {
    let base = bass_patch();
    render("insitu", |patch| {
        mute_all(patch);
        patch.sub_level = base.sub_level;
    });
    render("insitu_no_c", |patch| {
        mute_all(patch);
        mute_c_route(patch);
        patch.sub_level = base.sub_level;
    });
    render("insitu_eqonly", |patch| {
        mute_all(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
    });
    render("insitu_eqonly_no_c", |patch| {
        mute_all(patch);
        mute_c_route(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
    });
    render("insitu_filter_off", |patch| {
        mute_all(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
        patch.filter_wet = 0.0;
    });
    render("insitu_filter_off_no_c", |patch| {
        mute_all(patch);
        mute_c_route(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
        patch.filter_wet = 0.0;
    });
}

/// Regression: the sub's 4th harmonic and the C layer's fundamental must not
/// null each other. Before the 4x phase alignment H4 read -12.2 dB and H12
/// -18.5 dB re H1; the reference reads -1.5 and -8.3.
#[test]
#[ignore = "measurement harness for the bass H4/H12 probe"]
fn sub_4x_partials_are_not_nulled() {
    let base = bass_patch();
    let samples = render("regression_insitu", |patch| {
        mute_all(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
    });
    let h4 = level_db(&samples, 4);
    let h8 = level_db(&samples, 8);
    let h12 = level_db(&samples, 12);
    let h16 = level_db(&samples, 16);
    println!(
        "H4 {h4:.2}  H8 {h8:.2}  H12 {h12:.2}  H16 {h16:.2} (reference -1.5 / 3.8 / -8.3 / -5.7)"
    );
    assert!(h4 > -5.0, "H4 is nulled at {h4:.2} dB");
    assert!(h12 > -12.0, "H12 is nulled at {h12:.2} dB");
    assert!((h8 - 3.8).abs() < 1.5, "H8 moved to {h8:.2} dB");
    assert!((h16 + 5.7).abs() < 1.5, "H16 moved to {h16:.2} dB");

    // With the voice filter bypassed the 4x family is linear: the total must
    // be the vector sum of the isolated sources, so no filter-generated
    // component can be hiding in it.
    let sub_only = render("regression_sub_only", |patch| {
        mute_all(patch);
        mute_c_route(patch);
        mute_noise_route(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
        patch.filter_wet = 0.0;
    });
    let c_only = render("regression_c_only", |patch| {
        mute_all(patch);
        mute_noise_route(patch);
        eq_only(patch);
        patch.filter_wet = 0.0;
        // The 4x step only exists while the sub is audible, so set the C's
        // aligned phase directly for the isolated measurement.
        patch.oscillator_start_phase = 0.5 + 22.0 / 360.0;
    });
    let both = render("regression_both_linear", |patch| {
        mute_all(patch);
        mute_noise_route(patch);
        eq_only(patch);
        patch.sub_level = base.sub_level;
        patch.filter_wet = 0.0;
    });
    let (sr, si) = harmonic(&sub_only, 4);
    let (cr, ci) = harmonic(&c_only, 4);
    let (tr, ti) = harmonic(&both, 4);
    let sum = ((sr + cr) * (sr + cr) + (si + ci) * (si + ci)).sqrt();
    let total = (tr * tr + ti * ti).sqrt();
    let delta = 20.0 * (total / sum).log10();
    println!("filter-off vector sum vs total at H4: {delta:.2} dB");
    assert!(
        delta.abs() < 0.5,
        "H4 is not the vector sum ({delta:.2} dB)"
    );

    // The voice filter's compression is the second half of the law: at this
    // operating point it pulls the coincident partial pair down, so the
    // in-situ target is set with the filter on.
    let (ir, ii) = harmonic(&samples, 4);
    let in_situ = (ir * ir + ii * ii).sqrt();
    let compression = 20.0 * (in_situ / sum).log10();
    println!("voice-filter contribution at H4: {compression:+.2} dB");

    let (peak, rms, crest) = level(&samples, samples.len() / 2);
    println!("held-note peak {peak:.5} rms {rms:.5} crest {crest:.2} dB");
}
