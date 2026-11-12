//! The player's two effect sends: one chorus and one reverb bus shared by every
//! voice of a player.
//!
//! The format gives each voice a chorus and a reverb send in tenths of a
//! percent (`chorusEffectsSend`/`reverbEffectsSend`, moved at run time by CC93
//! and CC91 through the default modulators). A voice's dry contribution is also
//! summed into the two buses, scaled by its send; the buses run one device
//! instance for the whole player, and their wet return is added to the output.
//!
//! The level law is linear in the send value: half the send is half the wet
//! amplitude (6 dB), and a bank that declares no send and moves no controller
//! gets no effect at all. The return constant turns the bus output into the
//! level a full send reaches on the same fixture.
//!
//! Buffers are sized once per player and a call is split into fixed-size
//! chunks, so the render path allocates nothing whatever block size a caller
//! hands in.

use oc_dsp::AudioEffect;
use oc_dsp::chorus::Chorus;
use oc_dsp::reverb::Reverb;

/// Frames accumulated per pass. A streaming block is at most 128 frames and an
/// offline block is 64, so one pass covers a whole block.
pub const SEND_BLOCK_FRAMES: usize = 128;

/// Wet return applied to the bus output. The two buses are fed at unity for a
/// full send, so this is the level a full send reaches.
pub const CHORUS_RETURN: f32 = 1.854;
/// Reverb return level at a full send.
pub const REVERB_RETURN: f32 = 0.328;

/// Default reverb tail.
const REVERB_DECAY_SECONDS: f64 = 2.2;

/// The player's shared effect buses.
pub struct SendBuses {
    chorus: Chorus,
    reverb: Reverb,
    chorus_bus: Vec<f32>,
    reverb_bus: Vec<f32>,
    sample_rate: u32,
    active: bool,
}

impl SendBuses {
    /// Create the shared chorus and reverb send buses.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let mut chorus = Chorus::new(sample_rate);
        // A send bus is wet only; the dry signal reaches the output on its own.
        chorus.set_param("mix", 1.0);
        let mut reverb = Reverb::new(sample_rate);
        reverb.set_param("mix", 1.0);
        reverb.set_param("damping", 0.0);
        reverb.set_decay_seconds(REVERB_DECAY_SECONDS);
        Self {
            chorus,
            reverb,
            chorus_bus: vec![0.0; SEND_BLOCK_FRAMES * 2],
            reverb_bus: vec![0.0; SEND_BLOCK_FRAMES * 2],
            sample_rate,
            active: false,
        }
    }

    /// True once a voice has a send or a send controller moved; from then on
    /// the buses run every block, so a reverb tail outlives its voices.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Mark the buses active.
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Zero the buses for a pass and hand them to the voice loop.
    pub fn begin(&mut self, len: usize) -> (&mut [f32], &mut [f32]) {
        let len = len.min(self.chorus_bus.len());
        self.chorus_bus[..len].fill(0.0);
        self.reverb_bus[..len].fill(0.0);
        (&mut self.chorus_bus[..len], &mut self.reverb_bus[..len])
    }

    /// Run both buses over the pass and add their wet return to `dry`.
    pub fn finish(&mut self, dry: &mut [f32]) {
        let len = dry.len().min(self.chorus_bus.len()) & !1;
        if len == 0 {
            return;
        }
        self.chorus
            .process(&mut self.chorus_bus[..len], self.sample_rate);
        self.reverb
            .process(&mut self.reverb_bus[..len], self.sample_rate);
        for (frame, (chorus, reverb)) in dry[..len].chunks_exact_mut(2).zip(
            self.chorus_bus[..len]
                .chunks_exact(2)
                .zip(self.reverb_bus[..len].chunks_exact(2)),
        ) {
            frame[0] += chorus[0] * CHORUS_RETURN + reverb[0] * REVERB_RETURN;
            frame[1] += chorus[1] * CHORUS_RETURN + reverb[1] * REVERB_RETURN;
        }
    }
}
