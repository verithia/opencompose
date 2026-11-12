//! Shared-access SoundFont renderer.
//!
//! The render paths take `&mut`: one owner, one thread, no locking. The
//! audition figure only has `&`, so it gets this wrapper, which borrows the
//! underlying `SoundFontRenderer` for the duration of each call.

use std::cell::RefCell;
use std::path::Path;

use super::SoundFontRenderer;
use crate::interpret::MidiMessage;

/// SoundFont renderer with shared (`&self`) access to one voice at a time.
pub struct SharedSoundFontRenderer {
    inner: RefCell<SoundFontRenderer>,
}

impl SharedSoundFontRenderer {
    /// Create a renderer for one SoundFont.
    pub fn new(sample_rate: u32, soundfont_path: &Path) -> Result<Self, String> {
        Ok(Self {
            inner: RefCell::new(SoundFontRenderer::new(sample_rate, soundfont_path)?),
        })
    }

    /// Enable the drum channel.
    pub fn enable_drum_channel(&self) {
        self.inner.borrow().enable_drum_channel();
    }

    /// Send a MIDI message.
    pub fn send(&self, msg: &MidiMessage) -> Result<(), String> {
        self.inner.borrow_mut().send(msg)
    }

    /// Render interleaved stereo samples.
    pub fn render_samples(&self, num_samples: usize) -> Vec<f32> {
        self.inner.borrow_mut().render_samples(num_samples)
    }

    /// Renderer sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.inner.borrow().sample_rate()
    }
}
