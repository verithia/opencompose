//! Gain ramps for the mixer's fader, pan, send, return and mute stages.
//!
//! A gain that holds its value for the whole chunk turns a change written while
//! audio runs into one step at the next chunk boundary. These ramps move the
//! gain over [`RAMP_MS`] at the point it reaches the mix, so the same change
//! arrives as a short slope.
//!
//! The move is linear, not one-pole: the value lands on its target exactly, so a
//! lane whose gain does not change renders bit-identical samples to a lane with
//! no ramp, and the tail of a ride sits at the written level instead of
//! approaching it.

use std::collections::HashMap;

use oc_dsp::simd;

use super::{BusNode, SendRoute, TrackRouting};

/// Duration a fader, pan, send, return or master change travels over.
pub(super) const RAMP_MS: f64 = 10.0;

/// Frames a change travels over at `sample_rate`.
pub(super) fn ramp_frames(sample_rate: u32) -> u32 {
    crate::smoothing::frames_for_ms(RAMP_MS, sample_rate)
}

/// Linear ramp for one gain.
#[derive(Clone, Copy, Debug)]
pub(super) struct GainRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
    /// False until the first target arrives: a graph that carries an authored
    /// value snaps to it instead of fading in from the default.
    started: bool,
}

impl Default for GainRamp {
    fn default() -> Self {
        Self {
            current: 1.0,
            target: 1.0,
            step: 0.0,
            remaining: 0,
            started: false,
        }
    }
}

impl GainRamp {
    /// A ramp already sitting on `value`.
    pub(super) fn settled(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            started: true,
        }
    }

    /// The gain this ramp applies now.
    pub(super) fn value(&self) -> f32 {
        self.current
    }

    /// True while the gain holds its target for the rest of the block.
    pub(super) fn is_settled(&self) -> bool {
        self.remaining == 0
    }

    /// Point the ramp at `target`; `frames` is the length of the move. A target
    /// that did not change leaves the ramp where it is.
    pub(super) fn set_target(&mut self, target: f32, frames: u32) {
        if !self.started {
            *self = Self::settled(target);
            return;
        }
        if target == self.target {
            return;
        }
        self.target = target;
        if frames == 0 || self.current == target {
            self.current = target;
            self.remaining = 0;
            self.step = 0.0;
            return;
        }
        self.remaining = frames;
        self.step = (target - self.current) / frames as f32;
    }

    /// Next sample of the ramp; the target holds once the move is spent.
    pub(super) fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            } else {
                self.current += self.step;
            }
        }
        self.current
    }

    /// Multiply every sample by the moving gain.
    pub(super) fn apply(&mut self, buffer: &mut [f32]) {
        for sample in buffer {
            *sample *= self.next();
        }
    }
}

/// One ramp per channel, for a fader and pan that move together.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct StereoGainRamp {
    left: GainRamp,
    right: GainRamp,
}

impl StereoGainRamp {
    /// Point both channels at their target.
    pub(super) fn set_target(&mut self, left: f32, right: f32, frames: u32) {
        self.left.set_target(left, frames);
        self.right.set_target(right, frames);
    }

    /// True while both channels hold their target.
    pub(super) fn is_settled(&self) -> bool {
        self.left.is_settled() && self.right.is_settled()
    }

    /// Multiply the interleaved buffer by the per-channel gain. A settled ramp
    /// is one constant per channel, which keeps a static render bit-identical.
    /// The odd tail sample, if any, keeps its value as before.
    pub(super) fn apply(&mut self, buffer: &mut [f32]) {
        let even = buffer.len() & !1;
        if self.is_settled() {
            simd::channel_gains(&mut buffer[..even], self.left.value(), self.right.value());
            return;
        }
        for (index, sample) in buffer[..even].iter_mut().enumerate() {
            *sample *= if index % 2 == 0 {
                self.left.next()
            } else {
                self.right.next()
            };
        }
    }
}

/// Ramp state for one track's gain stages.
#[derive(Clone, Debug, Default)]
pub(super) struct TrackRamps {
    fader: StereoGainRamp,
    gate: GainRamp,
    sends: Vec<GainRamp>,
}

impl TrackRamps {
    /// Apply the mute and solo gate. Returns true when the channel stays silent
    /// for this block, so the caller can skip it entirely.
    pub(super) fn gate_block(&mut self, audible: bool, buffer: &mut [f32], frames: u32) -> bool {
        self.gate
            .set_target(if audible { 1.0 } else { 0.0 }, frames);
        if self.gate.is_settled() {
            return !audible;
        }
        self.gate.apply(buffer);
        false
    }

    /// Apply the fader and pan; both channels move on their own ramp.
    pub(super) fn fader_block(&mut self, gains: (f32, f32), buffer: &mut [f32], frames: u32) {
        self.fader.set_target(gains.0, gains.1, frames);
        self.fader.apply(buffer);
    }

    /// Point one send amount at its written value; `None` when this route has no
    /// ramp state yet, so the caller applies the amount directly.
    pub(super) fn send(&mut self, index: usize, amount: f32, frames: u32) -> Option<&mut GainRamp> {
        let ramp = self.sends.get_mut(index)?;
        ramp.set_target(amount, frames);
        Some(ramp)
    }

    /// Keep one send ramp per route. A route whose amount moved keeps its ramp;
    /// a route added since the last edit starts at its amount.
    pub(super) fn resize_sends(&mut self, sends: &[SendRoute]) {
        self.sends.truncate(sends.len());
        while self.sends.len() < sends.len() {
            self.sends
                .push(GainRamp::settled(sends[self.sends.len()].amount as f32));
        }
    }
}

/// Ramp state for every gain the mixer applies, beside the values it patches.
#[derive(Clone, Debug, Default)]
pub(super) struct MixerRamps {
    /// Parallel to `AudioGraph::routing` for a graph built from a project.
    pub(super) tracks: Vec<TrackRamps>,
    /// Return fader per bus name.
    pub(super) buses: HashMap<String, GainRamp>,
    pub(super) master: GainRamp,
}

impl MixerRamps {
    /// Ramp state for a freshly built graph. A ramp with no authored value yet
    /// snaps on its first block, so neither build order nor a channel width
    /// detected from the first buffer can fade anything in.
    pub(super) fn for_project(routing: &[TrackRouting], buses: &HashMap<String, BusNode>) -> Self {
        Self {
            tracks: routing
                .iter()
                .map(|route| TrackRamps {
                    sends: route
                        .sends
                        .iter()
                        .map(|send| GainRamp::settled(send.amount as f32))
                        .collect(),
                    ..TrackRamps::default()
                })
                .collect(),
            buses: buses
                .keys()
                .map(|name| (name.clone(), GainRamp::default()))
                .collect(),
            master: GainRamp::default(),
        }
    }

    /// Drop every move in flight; the next block snaps to the current values.
    pub(super) fn snap(&mut self) {
        for track in &mut self.tracks {
            track.fader = StereoGainRamp::default();
            track.gate = GainRamp::default();
            for send in &mut track.sends {
                *send = GainRamp::default();
            }
        }
        for bus in self.buses.values_mut() {
            *bus = GainRamp::default();
        }
        self.master = GainRamp::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AudioGraph;

    const SAMPLE_RATE: u32 = 48_000;
    const CHUNK: usize = 4096;
    const TONE_HZ: f64 = 1_000.0;

    /// A sine carried on both channels, 1 s is 48 000 samples at the rate above.
    fn tone(frames: usize, phase: &mut f64) -> Vec<f32> {
        let step = std::f64::consts::TAU * TONE_HZ / f64::from(SAMPLE_RATE);
        let mut buffer = vec![0.0f32; frames * 2];
        for frame in 0..frames {
            let value = (*phase).sin() as f32;
            *phase += step;
            buffer[frame * 2] = value;
            buffer[frame * 2 + 1] = value;
        }
        buffer
    }

    fn largest_step(samples: &[f32]) -> f32 {
        let mut largest = 0.0f32;
        for frame in 1..samples.len() / 2 {
            for channel in 0..2 {
                let delta =
                    (samples[frame * 2 + channel] - samples[(frame - 1) * 2 + channel]).abs();
                largest = largest.max(delta);
            }
        }
        largest
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// Left and right mean square summed; constant-power panning keeps it fixed.
    fn power(samples: &[f32]) -> f64 {
        let frames = (samples.len() / 2) as f64;
        let mut squares = 0.0;
        for frame in samples.chunks_exact(2) {
            squares += f64::from(frame[0]).powi(2) + f64::from(frame[1]).powi(2);
        }
        squares / frames
    }

    fn project() -> oc_dsl::ast::Project {
        oc_dsl::parse("@project\n@track a | instrument: keys/piano\n").unwrap()
    }

    /// A track feeding a bus return, with `send` as its only link to that bus.
    fn send_project(send: &str) -> oc_dsl::ast::Project {
        oc_dsl::parse(&format!(
            r#"@project
@track a | instrument: keys/piano
  mix: send(space={send})
@bus space
  routes: [master]
@master
"#
        ))
        .unwrap()
    }

    /// A track whose only path to the master is its output bus.
    fn bus_project() -> oc_dsl::ast::Project {
        oc_dsl::parse(
            r#"@project
@track a | instrument: keys/piano
  mix: output=group
@bus group
  routes: [master]
@master
"#,
        )
        .unwrap()
    }

    /// A send level is the third place the mixer writes a gain; it scales the
    /// tap its route already carries, so the write is the only thing that moves.
    #[test]
    fn a_send_level_ride_ramps_instead_of_stepping() {
        let mut graph = AudioGraph::from_project(&send_project("1"), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        graph.apply_send_state(&send_project("0.25"));
        for _ in 0..3 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let own = largest_step(&out[..CHUNK * 2]);
        let stepped = largest_step(&out);
        assert!(
            stepped < own * 1.1,
            "largest sample step was {stepped}, the tone's own was {own}"
        );
        // Master is the direct path plus the send through a unity return.
        let quiet: f64 = crate::graph::db_to_linear(-12.0);
        let expected = 20.0 * ((1.0 + quiet) / 2.0).log10();
        let reached = 20.0 * (rms(&out[out.len() - 4800 * 2..]) / rms(&out[..CHUNK * 2])).log10();
        assert!(
            (reached - expected).abs() < 0.1,
            "tail landed at {reached:.3} dB"
        );
    }

    /// The bus return fader is the fourth; with the bus as the only path to the
    /// master, its ramp is the whole output.
    #[test]
    fn a_bus_return_ride_ramps_instead_of_stepping() {
        let mut graph = AudioGraph::from_project(&bus_project(), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        let mut quiet = bus_project();
        quiet.buses[0].volume_db = -12.0;
        graph.apply_mixer_state(&quiet);
        for _ in 0..3 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let own = largest_step(&out[..CHUNK * 2]);
        let stepped = largest_step(&out);
        assert!(
            stepped < own * 1.1,
            "largest sample step was {stepped}, the tone's own was {own}"
        );
        let reached = 20.0 * (rms(&out[out.len() - 4800 * 2..]) / rms(&out[..CHUNK * 2])).log10();
        assert!(
            (reached + 12.0).abs() < 0.1,
            "tail landed at {reached:.3} dB"
        );
    }

    /// A 0 dB to -12 dB master write between two chunks would step the output
    /// by 0.4995 at the boundary without a ramp. A 10 ms ramp keeps the largest
    /// move close to the tone's own 0.093.
    #[test]
    fn a_master_ride_ramps_instead_of_stepping() {
        let mut graph = AudioGraph::from_project(&project(), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        graph.set_master_gain_db(-12.0);
        for _ in 0..11 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let own = largest_step(&out[..CHUNK * 2]);
        let stepped = largest_step(&out);
        assert!(
            stepped < own * 1.1,
            "largest sample step was {stepped}, the tone's own was {own}"
        );
        let reached = 20.0 * (rms(&out[out.len() - 4800 * 2..]) / rms(&out[..CHUNK * 2])).log10();
        assert!(
            (reached + 12.0).abs() < 0.1,
            "tail landed at {reached:.3} dB"
        );
    }

    /// The same write on the track fader, which also scales every post-fader
    /// send and the master input.
    #[test]
    fn a_track_fader_ride_ramps_instead_of_stepping() {
        let mut graph = AudioGraph::from_project(&project(), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        let mut quiet = project();
        quiet.tracks[0].mix.volume_db = -12.0;
        graph.apply_mixer_state(&quiet);
        for _ in 0..11 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let own = largest_step(&out[..CHUNK * 2]);
        let stepped = largest_step(&out);
        assert!(
            stepped < own * 1.1,
            "largest sample step was {stepped}, the tone's own was {own}"
        );
        let reached = 20.0 * (rms(&out[out.len() - 4800 * 2..]) / rms(&out[..CHUNK * 2])).log10();
        assert!(
            (reached + 12.0).abs() < 0.1,
            "tail landed at {reached:.3} dB"
        );
    }

    /// Fader, pan, return and master gains all hold one value in a project that
    /// never changes, so two graphs fed the same input must agree bit for bit.
    #[test]
    fn a_static_project_renders_the_same_samples_twice() {
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
@track a | instrument: keys/piano
  mix: vol=-6dB, pan=-0.25, output=group
@bus group
  mix: vol=-3dB
  routes: [master]
@master
  volume: -2dB
"#,
        )
        .unwrap();
        let mut first = AudioGraph::from_project(&project, SAMPLE_RATE);
        let mut second = AudioGraph::from_project(&project, SAMPLE_RATE);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for _ in 0..3 {
            let mut phase = 0.0;
            left.extend(first.mix(&mut [tone(CHUNK, &mut phase)]));
            let mut phase = 0.0;
            right.extend(second.mix(&mut [tone(CHUNK, &mut phase)]));
        }
        assert_eq!(left.len(), right.len());
        for (index, (left, right)) in left.iter().zip(&right).enumerate() {
            assert_eq!(
                left.to_bits(),
                right.to_bits(),
                "sample {index} differed: {left} against {right}"
            );
        }
    }

    /// An equal-power pan law keeps total power fixed; both channels ramp, and
    /// the ramp lands on the law's own gains.
    #[test]
    fn a_pan_jump_keeps_the_total_power() {
        let mut graph = AudioGraph::from_project(&project(), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        let mut panned = project();
        panned.tracks[0].mix.pan = -1.0;
        graph.apply_mixer_state(&panned);
        for _ in 0..3 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let tail = &out[out.len() - 4800 * 2..];
        let moved = 10.0 * (power(tail) / power(&out[..CHUNK * 2])).log10();
        assert!(moved.abs() < 0.1, "total power moved {moved:.3} dB");
        assert!(
            tail.chunks_exact(2).all(|frame| frame[1] == 0.0),
            "the right channel never reached the hard-left gain"
        );
    }

    /// A mute is a gain step, so it moves on the same ramp; a channel that has
    /// finished fading out is skipped instead of ramping silence.
    #[test]
    fn mute_and_unmute_ramp_without_a_click() {
        let mut graph = AudioGraph::from_project(&project(), SAMPLE_RATE);
        let mut phase = 0.0;
        let mut out = Vec::new();
        out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        let mut muted = project();
        muted.tracks[0].mix.mute = true;
        graph.apply_mixer_state(&muted);
        for _ in 0..3 {
            out.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }

        let own = largest_step(&out[..CHUNK * 2]);
        let stepping = largest_step(&out);
        assert!(
            stepping < own * 1.1,
            "mute stepped by {stepping}, the tone's own was {own}"
        );
        assert!(
            out[out.len() - 4800 * 2..]
                .iter()
                .all(|sample| *sample == 0.0),
            "the channel never reached silence"
        );

        graph.apply_mixer_state(&project());
        let mut back = Vec::new();
        for _ in 0..3 {
            back.extend(graph.mix(&mut [tone(CHUNK, &mut phase)]));
        }
        let settled = largest_step(&back[back.len() - 4800 * 2..]);
        let stepping = largest_step(&back);
        assert!(
            stepping < settled * 1.1,
            "unmute stepped by {stepping}, the settled tone's own was {settled}"
        );
        assert!(rms(&back[back.len() - 4800 * 2..]) > 0.1);
    }

    /// A moving gain lane owns its own per-sample curve; the manual fader ramp
    /// must not engage, or the curve would be smoothed twice.
    #[test]
    fn a_moving_automation_lane_is_not_smoothed_by_the_manual_ramp() {
        let project = oc_dsl::parse(
            r#"@project
  tempo: 120
@track a | instrument: keys/piano
  gain_automation: linear [1:1 - 2:1] {1:1=1, 2:1=0}
@master
"#,
        )
        .unwrap();
        // At 8 Hz a bar is sixteen frames, so the lane runs over frames 0..16.
        let mut graph = AudioGraph::from_project(&project, 8);
        let mut out = Vec::new();
        for _ in 0..2 {
            out.extend(graph.mix(&mut [vec![1.0; 16]]));
            assert!(graph.ramps.tracks[0].fader.is_settled());
            assert!(graph.ramps.tracks[0].gate.is_settled());
        }
        let pan = std::f32::consts::FRAC_1_SQRT_2;
        for frame in 0..16 {
            let expected = pan * (1.0 - frame as f32 / 16.0);
            assert!(
                (out[frame * 2] - expected).abs() < 1e-6,
                "frame {frame} was {}, expected {expected}",
                out[frame * 2]
            );
        }
    }

    /// The ramp itself: one linear move, then the target holds exactly.
    #[test]
    fn a_ramp_moves_linearly_and_lands_on_its_target() {
        let mut ramp = GainRamp::settled(0.0);
        ramp.set_target(1.0, 4);
        let mut buffer = [1.0f32; 8];
        ramp.apply(&mut buffer[..4]);
        assert_eq!(buffer[..4], [0.25, 0.5, 0.75, 1.0]);
        assert!(ramp.is_settled());
        assert_eq!(ramp.value(), 1.0);
        // A settled ramp holds the target and a repeated target starts nothing.
        ramp.apply(&mut buffer[4..]);
        assert_eq!(buffer[4..], [1.0; 4]);
        ramp.set_target(1.0, 4);
        assert!(ramp.is_settled());
    }

    /// A value that never moves is not a ramp at all, and a ramp with no window
    /// snaps: nothing between the two can fade in a graph that carries values.
    #[test]
    fn an_unstarted_or_zero_length_move_snaps() {
        let mut ramp = GainRamp::default();
        assert!(ramp.is_settled());
        assert_eq!(ramp.value(), 1.0);
        ramp.set_target(0.5, 4);
        assert!(ramp.is_settled());
        assert_eq!(ramp.value(), 0.5);

        let mut ramp = GainRamp::settled(0.5);
        ramp.set_target(0.25, 0);
        assert!(ramp.is_settled());
        assert_eq!(ramp.value(), 0.25);
    }
}
