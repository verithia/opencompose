//! Plosive processing uses the production audio path and shared TLS heap counter.
use oc_test_support::operations;

use oc_dsp::AudioEffect;
use oc_dsp::plosive::Plosive;

const RATE: u32 = 48_000;

/// A repeating 30 ms burst followed by silence, so the detector fires on every
/// pass through the buffer.
fn burst_buffer(frames: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; frames * 2];
    let mut state = 0x1234_5678u32;
    let mut low = 0.0;
    for frame in 0..frames {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (state >> 8) as f64 / 8_388_608.0 - 1.0;
        low += (noise - low) * 0.015;
        let phase = std::f64::consts::TAU * 90.0 * frame as f64 / RATE as f64;
        let envelope = if frame % 4_800 < 1_440 { 1.0 } else { 0.0 };
        let value = ((0.5 * low + 0.15 * phase.sin()) * envelope) as f32;
        buffer[frame * 2] = value;
        buffer[frame * 2 + 1] = value;
    }
    buffer
}

#[test]
fn the_audio_path_does_not_allocate_while_rendering() {
    let mut effect = Plosive::new(RATE);
    effect.set_param("amount", 100.0);
    effect.set_param("threshold", 6.0);
    let mut small = burst_buffer(4_800);
    let mut huge = burst_buffer(12_000);
    // Warm up every pass size and both parameter paths before counting.
    effect.process(&mut small, RATE);
    effect.process(&mut huge, RATE);
    effect.set_param("frequency", 110.0);
    effect.set_param("release", 60.0);
    effect.reset();
    let allocations = operations(|| {
        for _ in 0..64 {
            effect.process(&mut small, RATE);
        }
        // Blocks larger than any internal buffer are processed in place, not grown.
        for _ in 0..8 {
            effect.process(&mut huge, RATE);
        }
        effect.set_param("amount", 50.0);
        for _ in 0..16 {
            effect.process(&mut small, RATE);
        }
    });
    assert_eq!(allocations, 0, "the plosive audio path allocated");
}
