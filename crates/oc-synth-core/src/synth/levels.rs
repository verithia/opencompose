//! Output-level audit and the worst-case patch matrix.
//!
//! Every stage that can raise the voice path carries a normalisation; this
//! module measures those laws and pins the contract that the instrument stays
//! under its headroom with every stage at maximum. `docs/synth-levels.md`
//! records the per-stage table and the measured numbers behind it.

use super::*;

const RATE: u32 = 48_000;

/// Output gain used while a single stage is measured. At 0.1 the master
/// ceiling is transparent, so the measurement describes the stage itself.
const STAGE_GAIN: f64 = 0.1;

/// Render `notes` held together for `seconds` and return the interleaved
/// stereo buffer.
fn render_notes(patch: &SynthPatch, notes: &[u8], seconds: f64) -> Vec<f32> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let events: Vec<MidiEvent> = notes
        .iter()
        .map(|&note| MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity: 127,
            },
            track_index: 0,
        })
        .collect();
    let refs: Vec<&MidiEvent> = events.iter().collect();
    render_track_with_patch(&refs, frames, RATE, patch.clone(), 120.0)
}

fn peak_dbfs(buffer: &[f32]) -> f64 {
    let peak = buffer
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())));
    if peak <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * peak.log10()
    }
}

fn rms_dbfs(buffer: &[f32]) -> f64 {
    if buffer.is_empty() {
        return f64::NEG_INFINITY;
    }
    let power = buffer
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / buffer.len() as f64;
    if power <= 0.0 {
        f64::NEG_INFINITY
    } else {
        10.0 * power.log10()
    }
}

fn set_fx(fx: &mut SynthFx, name: &str, value: f64) {
    if let Some(slot) = fx.params.iter_mut().find(|(key, _)| key == name) {
        slot.1 = value;
    } else {
        fx.params.push((name.to_string(), value));
    }
}

/// A single oscillator at full level, filter bypassed, unity except for the
/// stage under test.
fn stage_patch() -> SynthPatch {
    SynthPatch {
        gain: STAGE_GAIN,
        oscillator_a_level: 1.0,
        filter_wet: 0.0,
        unison_voices: 1,
        detune_cents: 0.0,
        stereo_width: 0.0,
        ..SynthPatch::default()
    }
}

/// The same patch at the contract's unity output gain.
fn unity_patch() -> SynthPatch {
    let mut patch = stage_patch();
    patch.gain = 1.0;
    patch
}

fn chord_notes(count: usize) -> Vec<u8> {
    const SCALE: [u8; 16] = [
        48, 52, 55, 59, 60, 64, 67, 71, 72, 76, 79, 83, 84, 88, 91, 95,
    ];
    SCALE[..count.min(SCALE.len())].to_vec()
}

/// One module of each FX type with its gain-bearing parameters at maximum.
fn loudest_fx_modules() -> Vec<SynthFx> {
    let mut modules = Vec::new();
    for fx_type in SynthFxType::ALL {
        let mut fx = SynthFx::new(fx_type);
        match fx_type {
            SynthFxType::Drive => {
                set_fx(&mut fx, "drive", 1.0);
                set_fx(&mut fx, "level", 2.0);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Distortion => {
                set_fx(&mut fx, "drive", 1.0);
                set_fx(&mut fx, "level", 2.0);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
                set_fx(&mut fx, "stages", 16.0);
            }
            SynthFxType::Compressor => {
                set_fx(&mut fx, "threshold", 0.0);
                set_fx(&mut fx, "ratio", 1.0);
                set_fx(&mut fx, "makeup", 24.0);
                set_fx(&mut fx, "limiter", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Eq3 => {
                set_fx(&mut fx, "low", 18.0);
                set_fx(&mut fx, "mid", 18.0);
                set_fx(&mut fx, "high", 18.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Delay => {
                // The time control is exponential; 0.05 is 0.5 s, short enough
                // to arrive inside the measured window.
                set_fx(&mut fx, "time", 0.05);
                set_fx(&mut fx, "feedback", 0.95);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Reverb => {
                set_fx(&mut fx, "size", 1.0);
                set_fx(&mut fx, "damp", 0.0);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Chorus => {
                set_fx(&mut fx, "depth", 1.0);
                set_fx(&mut fx, "feedback", 0.9);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Phaser => {
                set_fx(&mut fx, "depth", 1.0);
                set_fx(&mut fx, "feedback", 0.9);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Flanger => {
                set_fx(&mut fx, "depth", 1.0);
                set_fx(&mut fx, "feedback", 0.9);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Filter => {
                set_fx(&mut fx, "cutoff", 425.0);
                set_fx(&mut fx, "resonance", 1.0);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Utils => {
                set_fx(&mut fx, "hpf", 20.0);
                set_fx(&mut fx, "lpf", 20000.0);
                set_fx(&mut fx, "width", 200.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Split => {
                set_fx(&mut fx, "gain1", 2.0);
                set_fx(&mut fx, "gain2", 2.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Split3 => {
                set_fx(&mut fx, "gain1", 2.0);
                set_fx(&mut fx, "gain2", 2.0);
                set_fx(&mut fx, "gain3", 2.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::SplitMs => {
                set_fx(&mut fx, "gain1", 2.0);
                set_fx(&mut fx, "gain2", 2.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Bode => {
                set_fx(&mut fx, "output_mix", 1.0);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::ParallelRack => {
                set_fx(&mut fx, "gain1", 2.0);
                set_fx(&mut fx, "gain2", 2.0);
                set_fx(&mut fx, "gain3", 2.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::HyperD => {
                set_fx(&mut fx, "depth", 1.0);
                set_fx(&mut fx, "feedback", 0.9);
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Conv => {
                set_fx(&mut fx, "mix", 1.0);
                set_fx(&mut fx, "level_out", 2.0);
            }
            SynthFxType::Multiband => {
                set_fx(&mut fx, "makeup", 24.0);
                for band in 1..=3 {
                    set_fx(&mut fx, &format!("band{band}_makeup"), 24.0);
                }
                set_fx(&mut fx, "level_out", 2.0);
            }
        }
        modules.push(fx);
    }
    modules
}

/// The loudest serial chain the rack can hold. Routers are measured on their
/// own so a branch that sums to silence cannot hide the serial modules.
fn loudest_serial_chain() -> Vec<SynthFx> {
    loudest_fx_modules()
        .into_iter()
        .filter(|fx| {
            !matches!(
                fx.fx_type,
                SynthFxType::Split
                    | SynthFxType::Split3
                    | SynthFxType::SplitMs
                    | SynthFxType::ParallelRack
            )
        })
        .take(fx::MAX_SYNTH_FX)
        .collect()
}

/// A parallel rack with one full-scale child, so the branch gain is measured.
fn loudest_parallel_chain() -> Vec<SynthFx> {
    let mut rack = SynthFx::new(SynthFxType::ParallelRack);
    set_fx(&mut rack, "count1", 1.0);
    set_fx(&mut rack, "gain1", 2.0);
    rack.params
        .retain(|(key, _)| key != "count2" && key != "count3");
    set_fx(&mut rack, "count2", 0.0);
    set_fx(&mut rack, "count3", 0.0);
    set_fx(&mut rack, "level_out", 2.0);
    let mut drive = SynthFx::new(SynthFxType::Drive);
    set_fx(&mut drive, "drive", 1.0);
    set_fx(&mut drive, "level", 2.0);
    set_fx(&mut drive, "level_out", 2.0);
    vec![rack, drive]
}

fn all_sources_max() -> SynthPatch {
    let mut patch = unity_patch();
    patch.oscillator_a_level = 1.0;
    patch.oscillator_b_level = 1.0;
    patch.oscillator_c_level = 1.0;
    patch.sub_level = 1.0;
    patch.noise_level = 1.0;
    patch.ring_mod_level = 1.0;
    patch.unison_voices = MAX_UNISON_VOICES;
    patch.oscillator_b_unison = MAX_UNISON_VOICES;
    patch.oscillator_c_unison = MAX_UNISON_VOICES;
    patch.detune_cents = 17.1;
    patch.oscillator_b_detune_cents = 17.1;
    patch.oscillator_c_detune_cents = 17.1;
    patch.stereo_width = 1.0;
    patch.oscillator_b_stereo_width = 1.0;
    patch.oscillator_c_stereo_width = 1.0;
    patch.filter_wet = 1.0;
    patch
}

/// Every stage at its maximum in one patch: all sources and unison, both
/// filters resonant and driven, every macro at full, the loudest FX chain.
fn everything_patch() -> SynthPatch {
    let mut patch = all_sources_max();
    patch.filter_model = FilterModel::Svf;
    patch.filter_cutoff = 425.0;
    patch.filter_resonance = 1.0;
    patch.filter_drive = 1.0;
    patch.filter2_enabled = true;
    patch.filter2_model = FilterModel::Svf;
    patch.filter2_cutoff = 425.0;
    patch.filter2_resonance = 1.0;
    patch.filter2_drive = 1.0;
    patch.filter_routing = FilterRouting::Parallel;
    patch.macros = [1.0; 4];
    patch.extra_macros = [1.0; 4];
    patch.mod_routes = vec![
        ModRoute {
            source: ModSource::Macro1,
            destination: ModDestination::VoiceAmp,
            amount: 1.0,
            ..ModRoute::default()
        },
        ModRoute {
            source: ModSource::Macro2,
            destination: ModDestination::Resonance,
            amount: 1.0,
            ..ModRoute::default()
        },
        ModRoute {
            source: ModSource::Macro3,
            destination: ModDestination::FilterDrive,
            amount: 1.0,
            ..ModRoute::default()
        },
        ModRoute {
            source: ModSource::Macro4,
            destination: ModDestination::OscillatorALevel,
            amount: 1.0,
            ..ModRoute::default()
        },
    ];
    patch.fx = loudest_serial_chain();
    patch
}

/// Highest peak of one stage-maximising patch over an eight-note chord.
fn matrix_peak(patch: &SynthPatch) -> f64 {
    peak_dbfs(&render_notes(patch, &chord_notes(8), 2.0))
}

#[test]
#[ignore = "prints the stage level table quoted in docs/synth-levels.md"]
fn report_stage_levels() {
    let seconds = 2.0;
    println!("\n### Plain source (osc A, filter bypassed, output gain 0.1)\n");
    println!("| patch | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    let plain = render_notes(&stage_patch(), &[60], seconds);
    println!(
        "| saw | {:+.2} | {:+.2} |",
        peak_dbfs(&plain),
        rms_dbfs(&plain)
    );

    println!("\n### Unison (osc A, 17.1 cents, width 0)\n");
    println!("| voices | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    for count in [1, 2, 3, 4, 7, 8, 16] {
        let mut patch = stage_patch();
        patch.unison_voices = count;
        patch.detune_cents = 17.1;
        let buffer = render_notes(&patch, &[60], seconds);
        println!(
            "| {count} | {:+.2} | {:+.2} |",
            peak_dbfs(&buffer),
            rms_dbfs(&buffer)
        );
    }

    println!("\n### Filter resonance (saw at the corner, wet 1)\n");
    println!("| model | res | peak dBFS | rms dBFS |");
    println!("|---|---|---|---|");
    for model in [
        FilterModel::Svf,
        FilterModel::Svf3,
        FilterModel::Svf4,
        FilterModel::Ladder,
        FilterModel::DirtyLadder,
        FilterModel::Moog12,
        FilterModel::Moog24,
    ] {
        for resonance in [0.0, 0.15, 0.5, 1.0] {
            let mut patch = stage_patch();
            patch.filter_wet = 1.0;
            patch.filter_model = model;
            patch.filter_cutoff = 425.0;
            patch.filter_resonance = resonance;
            let buffer = render_notes(&patch, &[60], seconds);
            println!(
                "| {model:?} | {resonance:.2} | {:+.2} | {:+.2} |",
                peak_dbfs(&buffer),
                rms_dbfs(&buffer)
            );
        }
    }

    println!("\n### Filter drive (Svf low-pass at 425 Hz, res 0.15)\n");
    println!("| drive | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    for drive in [0.0, 0.25, 0.5, 1.0] {
        let mut patch = stage_patch();
        patch.filter_wet = 1.0;
        patch.filter_cutoff = 425.0;
        patch.filter_drive = drive;
        let buffer = render_notes(&patch, &[60], seconds);
        println!(
            "| {drive:.2} | {:+.2} | {:+.2} |",
            peak_dbfs(&buffer),
            rms_dbfs(&buffer)
        );
    }

    println!("\n### Warp modes at full amount (osc A)\n");
    println!("| mode | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    for mode in WarpMode::ALL {
        let mut patch = stage_patch();
        patch.oscillator_a_warp_mode = mode;
        patch.oscillator_a_warp_amount = 1.0;
        let buffer = render_notes(&patch, &[60], seconds);
        println!(
            "| {mode:?} | {:+.2} | {:+.2} |",
            peak_dbfs(&buffer),
            rms_dbfs(&buffer)
        );
    }

    println!("\n### FX modules alone, each at its loudest\n");
    println!("| fx | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    for fx in loudest_fx_modules() {
        let mut patch = stage_patch();
        patch.fx = vec![fx.clone()];
        let buffer = render_notes(&patch, &[60], seconds);
        println!(
            "| {:?} | {:+.2} | {:+.2} |",
            fx.fx_type,
            peak_dbfs(&buffer),
            rms_dbfs(&buffer)
        );
    }

    println!("\n### Polyphony (plain saw, distinct notes)\n");
    println!("| notes | peak dBFS | rms dBFS |");
    println!("|---|---|---|");
    for count in [1, 2, 4, 8, 16] {
        let buffer = render_notes(&unity_patch(), &chord_notes(count), seconds);
        println!(
            "| {count} | {:+.2} | {:+.2} |",
            peak_dbfs(&buffer),
            rms_dbfs(&buffer)
        );
    }

    println!("\n### Macros at full routed to every level destination\n");
    let mut patch = all_sources_max();
    patch.macros = [1.0; 4];
    patch.extra_macros = [1.0; 4];
    patch.mod_routes = (0..4)
        .map(|index| ModRoute {
            source: [
                ModSource::Macro1,
                ModSource::Macro2,
                ModSource::Macro3,
                ModSource::Macro4,
            ][index],
            destination: ModDestination::VoiceAmp,
            amount: 1.0,
            ..ModRoute::default()
        })
        .collect();
    let buffer = render_notes(&patch, &chord_notes(8), seconds);
    println!(
        "| macro voice amp | {:+.2} | {:+.2} |",
        peak_dbfs(&buffer),
        rms_dbfs(&buffer)
    );

    println!("\n### Worst-case matrix at unity output (contract: below -6 dBFS)\n");
    println!("| case | peak dBFS |");
    println!("|---|---|");
    for (label, patch) in matrix_cases() {
        println!("| {label} | {:+.2} |", matrix_peak(&patch));
    }
}

fn matrix_cases() -> Vec<(&'static str, SynthPatch)> {
    let mut max_unison = all_sources_max();
    max_unison.filter_wet = 0.0;
    let mut max_resonance = unity_patch();
    max_resonance.filter_wet = 1.0;
    max_resonance.filter_model = FilterModel::Svf;
    max_resonance.filter_cutoff = 440.0;
    max_resonance.filter_resonance = 1.0;
    let mut max_drive = unity_patch();
    max_drive.filter_wet = 1.0;
    max_drive.filter_cutoff = 440.0;
    max_drive.filter_drive = 1.0;
    let mut max_filters = max_resonance.clone();
    max_filters.filter2_enabled = true;
    max_filters.filter2_model = FilterModel::Svf;
    max_filters.filter2_cutoff = 440.0;
    max_filters.filter2_resonance = 1.0;
    max_filters.filter_routing = FilterRouting::Parallel;
    let mut max_fx = unity_patch();
    max_fx.filter_wet = 0.0;
    max_fx.fx = loudest_serial_chain();
    let mut max_router = unity_patch();
    max_router.filter_wet = 0.0;
    max_router.fx = loudest_parallel_chain();
    let mut max_warp = unity_patch();
    max_warp.filter_wet = 0.0;
    // Every warp slot at full amount on all three oscillators.
    max_warp.oscillator_a_warp_mode = WarpMode::BendPos;
    max_warp.oscillator_a_warp_amount = 1.0;
    max_warp.oscillator_b_warp_mode = WarpMode::BendPos;
    max_warp.oscillator_b_warp_amount = 1.0;
    max_warp.oscillator_c_warp_mode = WarpMode::BendPos;
    max_warp.oscillator_c_warp_amount = 1.0;
    max_warp.oscillator_a_level = 1.0;
    max_warp.oscillator_b_level = 1.0;
    max_warp.oscillator_c_level = 1.0;

    vec![
        ("max unison, every source", max_unison),
        ("max resonance, sine at the corner", max_resonance),
        ("max drive", max_drive),
        ("both filters at max resonance", max_filters),
        ("every FX at its loudest", max_fx),
        ("parallel rack at full gain", max_router),
        ("warp at full amount", max_warp),
        ("every stage at once", everything_patch()),
    ]
}

/// The contract: no patch, with any stage at its maximum, leaves the
/// instrument above the headroom at unity output. This is what lets the
/// audition preview start without rendering and measuring the figure.
#[test]
fn worst_case_matrix_stays_below_the_output_ceiling() {
    let ceiling_db = 20.0 * OUTPUT_CEILING.log10();
    for (label, patch) in matrix_cases() {
        let peak = matrix_peak(&patch);
        assert!(
            peak < -6.0,
            "{label}: peak {peak:+.2} dBFS is above the -6 dBFS contract (ceiling {ceiling_db:+.2} dBFS)"
        );
    }
}

/// The headroom contract's lemma: the level law never puts a stack's power more
/// than 1.5 dB above the incoherent normalisation `1/sqrt(N)`. The allowance is
/// the measured partial coherence of a stack, not a fudge; the level itself
/// lands on one voice, see `synth::tests::rendered_unison_grid_carries_one_voice`.
#[test]
fn unison_voice_gain_stays_within_the_coherence_allowance() {
    let allowance_db = 1.5;
    for count in 1..=MAX_UNISON_VOICES {
        for detune in [0.0, 3.0, 6.0, 12.0, 22.0, 50.0] {
            let gain = unison_voice_gain(count, detune);
            let incoherent = 1.0 / (count as f64).sqrt();
            let rise_db = 20.0 * (gain / incoherent).log10();
            assert!(
                rise_db <= allowance_db,
                "N={count} detune={detune}: voice gain {gain:.6} sits {rise_db:+.2} dB over \
                 the incoherent normalisation {incoherent:.6}"
            );
        }
    }
}

/// Q sweep at a fixed input: the resonant peak stays inside the stage budget.
/// The reading is the peak at the corner against the passband, so the
/// low-pass roll-off at the corner does not masquerade as resonance gain.
#[test]
fn resonance_peak_stays_inside_the_stage_budget() {
    let budget_db = 20.0 * MAX_RESONANT_GAIN.log10();
    for model in [FilterModel::Svf, FilterModel::Svf3, FilterModel::Svf4] {
        for resonance in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let mut patch = stage_patch();
            patch.filter_wet = 1.0;
            patch.filter_model = model;
            patch.filter_cutoff = 440.0;
            patch.filter_resonance = resonance;
            patch.oscillator_a_waveform = Waveform::Sine;
            // A1 sits well inside the passband; A4 sits on the corner.
            let passband = peak_dbfs(&render_notes(&patch, &[33], 1.0));
            let peak = peak_dbfs(&render_notes(&patch, &[69], 1.0));
            let rise = peak - passband;
            assert!(
                rise <= budget_db + 0.5,
                "{model:?} res {resonance}: peak {peak:+.2} dBFS rises {rise:+.2} dB above the passband ({passband:+.2}), budget {budget_db:+.2} dB"
            );
        }
    }
}

/// Drive sweep at a fixed input: the output stays bounded. The input gain
/// reaches +24 dB, so the bound comes from the saturator's cubic clamp and the
/// master ceiling, not from a gain cap that would freeze the knob.
#[test]
fn drive_input_sweep_stays_bounded() {
    for drive in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let mut patch = unity_patch();
        patch.filter_wet = 1.0;
        patch.filter_cutoff = 440.0;
        patch.filter_drive = drive;
        let peak = peak_dbfs(&render_notes(&patch, &[69], 1.0));
        assert!(peak < -6.0, "drive {drive}: peak {peak:+.2} dBFS");
    }
    // The stage's own transfer bounds the peak: the input gain is +24 dB at
    // full, and the cubic clamp reaches exactly 1.0.
    assert!((drive_gain(1.0) - 16.0).abs() < 1e-12);
    assert_eq!(saturate(10.0), 1.0);
}

/// One to sixteen notes at unity output stay under the headroom.
#[test]
fn polyphony_peak_stays_below_the_output_ceiling() {
    let mut peaks = Vec::new();
    for count in 1..=16 {
        let peak = peak_dbfs(&render_notes(&unity_patch(), &chord_notes(count), 1.0));
        assert!(peak < -6.0, "{count} notes: peak {peak:+.2} dBFS");
        peaks.push(peak);
    }
    let lowest = peaks.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
        lowest > -60.0,
        "the polyphony sweep went silent at {lowest:+.2} dBFS"
    );
}
