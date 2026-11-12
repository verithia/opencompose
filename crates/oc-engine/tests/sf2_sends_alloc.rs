//! The send path's render loop must not allocate, whatever the block size.
//! Measurement uses the shared thread-local allocator in the test aggregator.

use oc_test_support::operations;
use std::path::PathBuf;

use oc_engine::interpret::MidiMessage;
use oc_engine::soundfont::SoundFontRenderer;

use crate::sf2_builder;

use sf2_builder::{Font, Sample, generators as g};

const RATE: u32 = 48_000;
const NOTE: u8 = 60;

fn send_font(chorus_send: i16, reverb_send: i16) -> PathBuf {
    let mut font = Font::new("sends");
    font.add_sample(Sample::sine("sine", RATE, NOTE, 440.0, RATE as usize).with_loop(0, RATE));
    font.add_instrument(
        "instrument",
        vec![vec![
            (g::SAMPLE_ID, 0),
            (g::SAMPLE_MODES, 1),
            (g::OVERRIDING_ROOT_KEY, i16::from(NOTE)),
            (g::KEY_RANGE, sf2_builder::range(0, 127) as i16),
            (g::CHORUS_SEND, chorus_send),
            (g::REVERB_SEND, reverb_send),
        ]],
    );
    font.add_preset("preset", 0, 0, vec![vec![(g::INSTRUMENT, 0)]]);
    font.write_temp()
}

#[test]
fn the_send_path_does_not_allocate_while_rendering() {
    let font = send_font(1000, 1000);
    let mut renderer = SoundFontRenderer::new(RATE, &font).unwrap();
    renderer
        .send(&MidiMessage::NoteOn {
            channel: 0,
            note: NOTE,
            velocity: 100,
        })
        .unwrap();
    let mut block = vec![0.0f32; 128 * 2];
    // Warm up both pass sizes before counting; nothing below may allocate.
    renderer.render_into(&mut block);
    let mut large = vec![0.0f32; 400 * 2];
    renderer.render_into(&mut large);
    let allocations = operations(|| {
        for _ in 0..64 {
            renderer.render_into(&mut block);
        }
        // A block larger than the preallocated pass is split instead of growing.
        for _ in 0..8 {
            renderer.render_into(&mut large);
        }
    });
    assert_eq!(allocations, 0, "the send path allocated");
}
