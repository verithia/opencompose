//! Streaming BS.1770 short-term loudness over a sliding window.
//!
//! This is the live form of the file meter's 3 s window: the same K-weighting
//! pair (`super::kweight`), the same LU offset and the same 3 s span, updated
//! one hop at a time so a render callback can publish it per chunk.

use super::kweight::KWeightChannel;

/// Span of the short-term window, in seconds.
pub const SHORT_TERM_SECONDS: f64 = 3.0;
/// Offset that turns K-weighted mean power into LUFS (BS.1770-4).
pub const LOUDNESS_OFFSET_LU: f64 = -0.691;
/// Readings below the absolute gate are absent rather than very small numbers.
pub const FLOOR_LUFS: f64 = -70.0;
/// Display ceiling; a louder reading clamps here.
pub const CEILING_LUFS: f64 = 12.0;

const HOP_SECONDS: f64 = 0.1;
/// Power of a digital-silent window; below it the reading is absent.
const MIN_POWER: f64 = 1e-12;

/// K-weighted power of the last window, kept as one mean per hop.
pub struct ShortTermLoudness {
    filters: [KWeightChannel; 2],
    hop_frames: usize,
    frames_in_hop: usize,
    hop_energy: f64,
    hops: Vec<f64>,
    head: usize,
    filled: usize,
}

impl ShortTermLoudness {
    /// A meter for interleaved stereo at `sample_rate`; the window is
    /// [`SHORT_TERM_SECONDS`] and the hop is one tenth of a second.
    pub fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let hop_frames = ((f64::from(sample_rate) * HOP_SECONDS).round() as usize).max(1);
        let capacity = ((SHORT_TERM_SECONDS / HOP_SECONDS).round() as usize).max(1);
        Self {
            filters: [KWeightChannel::new(f64::from(sample_rate)); 2],
            hop_frames,
            frames_in_hop: 0,
            hop_energy: 0.0,
            hops: vec![0.0; capacity],
            head: 0,
            filled: 0,
        }
    }

    /// Span of the window this meter reads, in seconds.
    pub fn window_seconds(&self) -> f64 {
        SHORT_TERM_SECONDS
    }

    /// Drop the window and the filter state; a seek or a stop calls this so a
    /// reading never mixes audio from two places in the timeline.
    pub fn reset(&mut self) {
        self.filters[0].reset();
        self.filters[1].reset();
        self.frames_in_hop = 0;
        self.hop_energy = 0.0;
        self.hops.fill(0.0);
        self.head = 0;
        self.filled = 0;
    }

    /// Feed interleaved stereo frames; a trailing odd sample is ignored.
    pub fn push_interleaved(&mut self, frames: &[f32]) {
        for frame in frames.as_chunks::<2>().0 {
            let left = self.filters[0].process(f64::from(frame[0]));
            let right = self.filters[1].process(f64::from(frame[1]));
            // BS.1770 sums the channels; a dual-mono source reads 3 LU louder
            // than the same signal in one channel.
            self.hop_energy += left * left + right * right;
            self.frames_in_hop += 1;
            if self.frames_in_hop >= self.hop_frames {
                self.close_hop();
            }
        }
    }

    /// Current short-term loudness in LUFS.
    ///
    /// `None` until the window holds a full [`SHORT_TERM_SECONDS`], and `None`
    /// for a window below the [`FLOOR_LUFS`] absolute gate. A reading above
    /// [`CEILING_LUFS`] clamps to the ceiling.
    pub fn lufs(&self) -> Option<f64> {
        if self.filled < self.hops.len() {
            return None;
        }
        let power = self.hops.iter().sum::<f64>() / self.hops.len() as f64;
        if power <= MIN_POWER {
            return None;
        }
        reading(power)
    }

    fn close_hop(&mut self) {
        let mean = self.hop_energy / self.frames_in_hop.max(1) as f64;
        self.hops[self.head] = mean;
        self.head = (self.head + 1) % self.hops.len();
        self.filled = (self.filled + 1).min(self.hops.len());
        self.hop_energy = 0.0;
        self.frames_in_hop = 0;
    }
}

/// K-weighted mean power as a loudness reading, gated and clamped.
///
/// A window at or below [`FLOOR_LUFS`] is absent; a window above
/// [`CEILING_LUFS`] clamps to the ceiling.
fn reading(power: f64) -> Option<f64> {
    let value = LOUDNESS_OFFSET_LU + 10.0 * power.log10();
    (value >= FLOOR_LUFS).then(|| value.min(CEILING_LUFS))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(amplitude: f64, seconds: f64) -> Vec<f32> {
        let frames = (f64::from(RATE) * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let sample = (amplitude
                * (std::f64::consts::TAU * 1_000.0 * index as f64 / f64::from(RATE)).sin())
                as f32;
            buffer.push(sample);
            buffer.push(sample);
        }
        buffer
    }

    fn feed(meter: &mut ShortTermLoudness, input: &[f32], chunk_frames: usize) {
        for chunk in input.chunks(chunk_frames * 2) {
            meter.push_interleaved(chunk);
        }
    }

    #[test]
    fn a_full_window_of_silence_reads_nothing() {
        let mut meter = ShortTermLoudness::new(RATE);
        meter.push_interleaved(&vec![0.0; RATE as usize * 4 * 2]);
        assert_eq!(meter.lufs(), None);
    }

    #[test]
    fn a_readout_waits_for_a_full_window() {
        let mut meter = ShortTermLoudness::new(RATE);
        assert_eq!(meter.lufs(), None);
        meter.push_interleaved(&tone(0.5, 2.9));
        assert_eq!(meter.lufs(), None, "the window is not full yet");
        meter.push_interleaved(&tone(0.5, 0.2));
        assert!(meter.lufs().is_some());
    }

    /// Halving the amplitude of a steady tone lowers short-term loudness by
    /// 6.02 dB, which is the only calibration a meter needs to be honest.
    #[test]
    fn six_db_of_level_is_six_db_of_loudness() {
        let mut loud = ShortTermLoudness::new(RATE);
        feed(&mut loud, &tone(0.5, 4.0), 4_096);
        let mut quiet = ShortTermLoudness::new(RATE);
        feed(&mut quiet, &tone(0.25, 4.0), 4_096);
        let loud = loud.lufs().expect("a steady tone reads");
        let quiet = quiet.lufs().expect("a steady tone reads");
        assert!(
            (loud - quiet - 6.0206).abs() < 0.05,
            "{loud} vs {quiet} LUFS"
        );
    }

    #[test]
    fn chunk_size_does_not_change_the_reading() {
        let input = tone(0.3, 4.0);
        let mut small = ShortTermLoudness::new(RATE);
        feed(&mut small, &input, 97);
        let mut large = ShortTermLoudness::new(RATE);
        feed(&mut large, &input, 4_096);
        let difference = (small.lufs().unwrap() - large.lufs().unwrap()).abs();
        assert!(
            difference < 1e-9,
            "block size moved the reading by {difference}"
        );
    }

    /// After a stop the window keeps its last 3 s; reset clears it so a resume
    /// never averages audio from before the stop.
    #[test]
    fn reset_drops_the_window() {
        let mut meter = ShortTermLoudness::new(RATE);
        feed(&mut meter, &tone(0.5, 4.0), 4_096);
        assert!(meter.lufs().is_some());
        meter.reset();
        assert_eq!(meter.lufs(), None);
    }

    #[test]
    fn a_silent_tail_empties_the_window() {
        let mut meter = ShortTermLoudness::new(RATE);
        feed(&mut meter, &tone(0.5, 3.5), 4_096);
        let loud = meter.lufs().expect("the tone reads");
        feed(&mut meter, &vec![0.0; RATE as usize * 4 * 2], 4_096);
        assert_eq!(meter.lufs(), None, "the window emptied; loud was {loud}");
    }

    #[test]
    fn a_reading_above_the_ceiling_clamps_and_below_the_floor_is_absent() {
        assert_eq!(reading(1e3), Some(CEILING_LUFS));
        assert_eq!(reading(1e-9), None);
    }
}
