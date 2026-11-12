//! Sample-less kits must render without touching the heap.

use oc_dsl::ast::ParamValue;
use oc_synth::drumkit::DrumKit;

/// Counts heap operations on the measuring thread; rendering here is single-threaded.
#[global_allocator]
static ALLOCATOR: oc_test_support::CountingAllocator = oc_test_support::CountingAllocator;

fn synth_kit() -> Vec<(String, ParamValue)> {
    let mut params = Vec::new();
    for (name, note) in [
        ("kick", 36.0),
        ("snare", 38.0),
        ("hat_closed", 42.0),
        ("hat_open", 46.0),
        ("tom", 41.0),
        ("clap", 39.0),
        ("rim", 37.0),
    ] {
        params.push((format!("pad.{name}.note"), ParamValue::Number(note)));
    }
    params
}

#[test]
fn synthesised_voices_render_without_allocating() {
    let params = synth_kit();
    let mut kit = DrumKit::from_params(&params, None, 48_000).unwrap();
    for note in [36u8, 38, 42, 46, 41, 39, 37] {
        kit.note_on(note, 100);
    }

    let mut peak = 0.0f32;
    let allocations = oc_test_support::operations(|| {
        for _ in 0..4_096 {
            let [left, right] = kit.next_stereo(48_000);
            peak = peak.max(left.abs()).max(right.abs());
        }
    });

    assert_eq!(allocations, 0, "synth render allocated {allocations} times");
    assert!(peak > 0.01, "synth render was silent");
}
