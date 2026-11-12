//! The FX tree must not allocate while audio is running.

use oc_synth::synth::{OpenSynth, SynthFx, SynthFxType, SynthPatch};

/// Counts heap operations on the measuring thread; rendering here is single-threaded.
#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

fn tree_patch() -> SynthPatch {
    let mut split = SynthFx::new(SynthFxType::Split);
    split.set_param("freq", 500.0);
    split.set_param("count2", 1.0);
    let mut filter = SynthFx::new(SynthFxType::Filter);
    filter.set_param("cutoff", 200.0);
    filter.set_param("resonance", 0.0);
    filter.set_param("mix", 1.0);
    let mut parallel = SynthFx::new(SynthFxType::ParallelRack);
    parallel.set_param("count2", 1.0);
    parallel.set_param("gain2", 0.5);
    let mut delay = SynthFx::new(SynthFxType::Delay);
    delay.set_param("mix", 0.25);
    SynthPatch {
        fx: vec![split, filter, parallel, delay],
        ..Default::default()
    }
}

#[test]
fn rendering_a_parallel_tree_does_not_allocate() {
    let mut synth = OpenSynth::new(tree_patch());
    synth.note_on(0, 60, 100);
    let mut warmup = [0.0f32; 1024];
    synth.render_interleaved(&mut warmup, 48_000);

    let mut output = [0.0f32; 8192];
    let allocations = oc_test_support::operations(|| synth.render_interleaved(&mut output, 48_000));
    assert_eq!(allocations, 0, "FX processing allocated");
    assert!(output.iter().all(|sample| sample.is_finite()));
}

fn single_fx_patch(fx_type: SynthFxType) -> SynthPatch {
    let mut fx = SynthFx::new(fx_type);
    fx.set_param("mix", 0.5);
    SynthPatch {
        fx: vec![fx],
        ..Default::default()
    }
}

fn assert_renders_without_allocations(fx_type: SynthFxType, sample_rate: u32) {
    let mut synth = OpenSynth::new(single_fx_patch(fx_type));
    synth.note_on(0, 60, 100);
    // The first block may re-derive a rate-dependent state; keep it outside.
    let mut warmup = [0.0f32; 2048];
    synth.render_interleaved(&mut warmup, sample_rate);

    let mut output = [0.0f32; 8192];
    // About two seconds of stereo audio per FX family.
    let allocations = oc_test_support::operations(|| {
        for _ in 0..24 {
            synth.render_interleaved(&mut output, sample_rate);
        }
    });
    assert_eq!(allocations, 0, "{fx_type:?} allocated while playing");
    assert!(output.iter().all(|sample| sample.is_finite()));
}

#[test]
fn hyperd_does_not_allocate_while_playing() {
    assert_renders_without_allocations(SynthFxType::HyperD, 48_000);
}

#[test]
fn convolution_does_not_allocate_while_playing() {
    assert_renders_without_allocations(SynthFxType::Conv, 44_100);
}
