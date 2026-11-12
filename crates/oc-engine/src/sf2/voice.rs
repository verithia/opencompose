//! One sounding voice: sample position and loop, envelopes, filter, pan, the
//! three modulation sources and the two effect sends.

use oc_dsp::biquad::{BiquadFilter, FilterType};

use super::font::{EnvelopeShape, SampleBank, Zone, filter_q_from_centibels};
use super::modulation::{
    Lfo, MAX_FILTER_FC_CENTS, MAX_PITCH_MODULATION_CENTS, MIN_FILTER_FC_CENTS, Modulation,
    PAN_UNITS_PER_SIDE, centibels_to_gain, cents_to_hz, cents_to_ratio,
};
use super::player::DynamicRamp;

/// Voices past this count steal the oldest one, so a note-on never allocates.
pub(crate) const MAX_VOICES: usize = 128;

/// Velocity layers one note may crossfade through; the ladder is per voice.
pub(crate) const MAX_DYNAMIC_LAYERS: usize = 8;

/// Attenuation in dB that counts as silence: the format's 96 dB envelope floor.
const SILENCE_DB: f32 = 96.0;

/// Level change per envelope stage. The format defines the attack, decay and
/// release times as the time for a 96 dB change, so a decay to a shallow
/// sustain level finishes early and holds, and the release ramp's speed does
/// not depend on the level the note was released at.
const STAGE_DB: f32 = SILENCE_DB;

/// A biquad redesign costs a sin/cos pair, so a moving cutoff is refreshed at
/// most once per this many samples and only after it moved
/// [`FILTER_REFRESH_CENTS`]; a step of 4 cents is 0.23 % of the cutoff.
const FILTER_REFRESH_SAMPLES: u8 = 8;
const FILTER_REFRESH_CENTS: f32 = 4.0;

/// Constant-power pan: each side reads
/// `sqrt(2) * cos/sin` of the pan angle, so the centre is unity and a hard pan
/// is +3 dB on the live side.
const PAN_UNITY: f32 = std::f32::consts::SQRT_2;

/// Envelope stages, in specification order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Done,
}

/// Delay/attack/hold/decay/sustain/release envelope.
///
/// The attack ramps the gain linearly (in amplitude) to unity; hold and sustain
/// hold it; decay and release ramp it exponentially, 96 dB over the stage time.
/// A stage of zero length completes in one sample. The same machine serves the
/// volume and the modulation envelope; only the volume envelope decides when a
/// voice is finished.
pub(crate) struct Envelope {
    stage: Stage,
    /// Linear gain, 0 .. 1.
    gain: f32,
    /// Gain multiplier per sample inside a decay or release ramp.
    factor: f32,
    /// Gain increment per sample inside the attack.
    attack_step: f32,
    samples_left: u32,
    sustain_gain: f32,
    silence_gain: f32,
    delay_samples: u32,
    attack_samples: u32,
    hold_samples: u32,
    decay_samples: u32,
    release_samples: u32,
}

impl Envelope {
    fn new(shape: EnvelopeShape, note: u8, sample_rate: f64) -> Self {
        let seconds_to_samples = |seconds: f64| -> u32 {
            (seconds.max(0.0) * sample_rate)
                .round()
                .min(f64::from(u32::MAX)) as u32
        };
        let sustain_db = shape.sustain_db().min(SILENCE_DB);
        let mut envelope = Self {
            stage: Stage::Delay,
            gain: 0.0,
            factor: 1.0,
            attack_step: 0.0,
            samples_left: 0,
            sustain_gain: db_to_gain(sustain_db),
            silence_gain: db_to_gain(SILENCE_DB),
            delay_samples: seconds_to_samples(shape.delay),
            attack_samples: seconds_to_samples(shape.attack),
            hold_samples: seconds_to_samples(shape.hold_for(note)),
            decay_samples: seconds_to_samples(shape.decay_for(note)),
            release_samples: seconds_to_samples(shape.release),
        };
        envelope.begin();
        envelope
    }

    /// Start the current stage; zero-length stages complete immediately.
    fn begin(&mut self) {
        loop {
            match self.stage {
                Stage::Delay => {
                    self.gain = 0.0;
                    self.samples_left = self.delay_samples;
                    if self.samples_left > 0 {
                        return;
                    }
                }
                Stage::Attack => {
                    self.gain = 0.0;
                    self.samples_left = self.attack_samples;
                    self.attack_step = if self.samples_left == 0 {
                        0.0
                    } else {
                        1.0 / self.samples_left as f32
                    };
                    if self.samples_left > 0 {
                        return;
                    }
                    self.gain = 1.0;
                }
                Stage::Hold => {
                    self.gain = 1.0;
                    self.samples_left = self.hold_samples;
                    if self.samples_left > 0 {
                        return;
                    }
                }
                Stage::Decay => {
                    self.gain = 1.0;
                    self.samples_left = self.decay_samples;
                    self.factor = stage_factor(self.samples_left);
                    if self.samples_left > 0 {
                        return;
                    }
                    self.gain = self.sustain_gain;
                }
                Stage::Sustain => {
                    self.gain = self.sustain_gain;
                    self.samples_left = 0;
                    return;
                }
                Stage::Release => {
                    // The ramp starts at the level the note was released at.
                    self.samples_left = self.release_samples;
                    self.factor = stage_factor(self.samples_left);
                    if self.samples_left > 0 {
                        return;
                    }
                    self.gain = 0.0;
                }
                Stage::Done => {
                    self.gain = 0.0;
                    self.samples_left = 0;
                    return;
                }
            }
            self.stage = self.stage.next();
        }
    }

    /// Current linear gain, advancing the envelope by one sample.
    fn tick(&mut self) -> f32 {
        if self.stage == Stage::Done {
            return 0.0;
        }
        if self.samples_left > 0 {
            match self.stage {
                Stage::Attack => self.gain = (self.gain + self.attack_step).min(1.0),
                Stage::Decay => {
                    self.gain *= self.factor;
                    if self.gain <= self.sustain_gain {
                        self.gain = self.sustain_gain;
                    }
                }
                Stage::Release => {
                    self.gain *= self.factor;
                    if self.gain <= self.silence_gain {
                        self.stage = Stage::Done;
                        self.gain = 0.0;
                        return 0.0;
                    }
                }
                Stage::Delay | Stage::Hold | Stage::Sustain => {}
                Stage::Done => unreachable!(),
            }
            self.samples_left -= 1;
            if self.samples_left == 0 {
                self.stage = self.stage.next();
                self.begin();
            }
        }
        if self.stage == Stage::Done {
            0.0
        } else {
            self.gain
        }
    }

    /// Enter the release stage from the current level.
    fn release(&mut self) {
        if self.stage == Stage::Done || self.stage == Stage::Release {
            return;
        }
        self.stage = Stage::Release;
        self.begin();
    }
}

/// Gain multiplier for one sample of a 96 dB ramp over `samples`.
fn stage_factor(samples: u32) -> f32 {
    if samples == 0 {
        return 0.0;
    }
    db_to_gain(STAGE_DB / samples as f32)
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(-db / 20.0)
}

impl Stage {
    fn next(self) -> Self {
        match self {
            Self::Delay => Self::Attack,
            Self::Attack => Self::Hold,
            Self::Hold => Self::Decay,
            Self::Decay | Self::Sustain => Self::Sustain,
            Self::Release => Self::Done,
            Self::Done => Self::Done,
        }
    }
}

/// A low-pass cutoff that moves with the modulation sources.
///
/// The coefficients are redesigned only when the requested cutoff moved far
/// enough, so a sweep costs one biquad design every few dozen samples instead
/// of one per sample.
struct FilterState {
    filter: BiquadFilter,
    q: f64,
    /// Cutoff the coefficients were designed at, absolute cents.
    designed_cents: f32,
    /// Samples before another redesign is allowed.
    wait: u8,
}

impl FilterState {
    fn new(cutoff_cents: f32, q: f64, sample_rate: u32) -> Self {
        Self {
            filter: BiquadFilter::new(
                FilterType::LowPass,
                cents_to_hz(cutoff_cents),
                q,
                0.0,
                sample_rate,
            ),
            q,
            designed_cents: cutoff_cents,
            wait: 0,
        }
    }

    fn update(&mut self, cutoff_cents: f32) {
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        if (cutoff_cents - self.designed_cents).abs() < FILTER_REFRESH_CENTS {
            return;
        }
        self.filter
            .set_params(cents_to_hz(cutoff_cents), self.q, 0.0);
        self.designed_cents = cutoff_cents;
        self.wait = FILTER_REFRESH_SAMPLES - 1;
    }
}

/// How one voice follows a hairpin's velocity-layer crossfade.
pub(crate) struct DynamicLayer {
    /// The velocity the note was played at; the level law uses it.
    base_velocity: u8,
    /// The ramp this voice follows, in absolute output frames.
    ramp: DynamicRamp,
    /// Ascending layer-centre velocities.
    ladder: [u8; MAX_DYNAMIC_LAYERS],
    ladder_len: u8,
    /// This voice's own rung on the ladder.
    layer: u8,
}

impl DynamicLayer {
    /// One voice's state on a ladder of ascending layer-centre velocities.
    pub(crate) fn new(base_velocity: u8, ramp: DynamicRamp, ladder: &[u8], layer: usize) -> Self {
        let mut fixed = [0u8; MAX_DYNAMIC_LAYERS];
        let len = ladder.len().min(MAX_DYNAMIC_LAYERS);
        fixed[..len].copy_from_slice(&ladder[..len]);
        Self {
            base_velocity,
            ramp,
            ladder: fixed,
            ladder_len: len as u8,
            layer: layer.min(len.saturating_sub(1)) as u8,
        }
    }

    /// Crossfade weight for this voice at an absolute output frame.
    fn weight(&self, frame: u64) -> f32 {
        // The ramp moves the level; the layer follows the same position in the
        // velocity domain, so the two are one gesture.
        let velocity = f32::from(self.base_velocity) * self.ramp.gain_at(frame).sqrt();
        self.ladder_weight(velocity)
    }

    /// Linear crossfade between the two rungs that bracket `velocity`.
    fn ladder_weight(&self, velocity: f32) -> f32 {
        let len = usize::from(self.ladder_len).min(MAX_DYNAMIC_LAYERS);
        if len <= 1 {
            return 1.0;
        }
        let first = f32::from(self.ladder[0]);
        if velocity <= first {
            return if self.layer == 0 { 1.0 } else { 0.0 };
        }
        for index in 0..len - 1 {
            let low = f32::from(self.ladder[index]);
            let high = f32::from(self.ladder[index + 1]);
            if velocity <= high {
                let fraction = (velocity - low) / (high - low).max(f32::EPSILON);
                return match usize::from(self.layer) {
                    layer if layer == index => 1.0 - fraction,
                    layer if layer == index + 1 => fraction,
                    _ => 0.0,
                };
            }
        }
        if usize::from(self.layer) + 1 == len {
            1.0
        } else {
            0.0
        }
    }
}

/// One playing note over one zone.
pub(crate) struct Voice {
    pub channel: u8,
    pub note: u8,
    pub preset: usize,
    pub zone: usize,
    pub exclusive_class: i16,
    /// Key number the synthesis uses: the `keynum` generator or the note's own.
    pub effective_key: u8,
    /// Velocity the synthesis uses: the `velocity` generator or the note's own.
    pub effective_velocity: u8,
    pub key_down: bool,
    /// Held by the sustain pedal after its note-off.
    pedal: bool,
    pub finished: bool,
    pub age: u64,
    /// Key, root and tuning ratio, without bend or rate scale.
    tuning_ratio: f64,
    /// Pitch bend in semitones.
    bend: f64,
    /// Bank sample rate against the output rate.
    rate_scale: f64,
    /// Samples advanced per output frame.
    increment: f64,
    position: f64,
    /// Zone attenuation and velocity, before pan.
    gain: f32,
    /// The zone's own `initialAttenuation` in centibels, so a record can be
    /// clipped against it the way the format clips the summed value.
    zone_attenuation_cb: f32,
    /// Linear gain from the attenuation records; exactly 1.0 when none apply.
    mod_attenuation: f32,
    /// Zone pan, kept so a CC10 change can re-apply it.
    zone_pan: f32,
    /// CC10 pan, kept alongside the zone pan.
    channel_pan: f32,
    /// Pan from a record, in the -1..1 range.
    mod_pan: f32,
    left: f32,
    right: f32,
    envelope: Envelope,
    mod_env: Envelope,
    filter_base_cents: f32,
    filter: Option<FilterState>,
    vibrato: Lfo,
    mod_lfo: Lfo,
    modulation: Modulation,
    /// Chorus and reverb send gains, 0..1, kept in step with `modulation`.
    pub send_chorus: f32,
    pub send_reverb: f32,
    /// Level normalisation for a layer probe: the note's own velocity gain
    /// over the probe's, so the ramp is still the only level move.
    gain_scale: f32,
    /// Current crossfade weight, one per frame while the ramp moves.
    layer_gain: f32,
    /// The voice's place in a hairpin crossfade, `None` outside one.
    dynamic: Option<DynamicLayer>,
    /// Absolute output frame, so the crossfade follows the timeline.
    frame: u64,
}

impl Voice {
    #[allow(clippy::too_many_arguments)]
    /// Start a voice for a zone and note.
    pub fn start(
        channel: u8,
        preset: usize,
        zone_index: usize,
        zone: &Zone,
        sample_rate: u32,
        asset_rate: u32,
        note: u8,
        velocity: u8,
        age: u64,
        modulation: Modulation,
    ) -> Self {
        // `keynum`/`velocity` replace the note's own values after the zone has
        // been selected, so pitch, envelopes, the level law and the modulator
        // sources all read the same fixed pair.
        let effective_key = zone.effective_key(note);
        let effective_velocity = zone.effective_velocity(velocity);
        let key = f64::from(i16::from(effective_key) - i16::from(zone.root_key));
        let semitones = key * f64::from(zone.scale_tuning) / 100.0
            + f64::from(zone.coarse_tune)
            + f64::from(zone.fine_tune) / 100.0;
        let tuning_ratio = 2f64.powf(semitones / 12.0);
        let filter_base_cents = f32::from(zone.filter_cutoff_cents);
        // The zone decides whether it has a filter; a zone without one never
        // builds a biquad, however its modulation moves. The Q and the envelope
        // times are read once here: a record that reaches them behaves like a
        // generator offset, and a later controller change cannot re-aim the
        // biquad or re-arm a running stage.
        let filter_q_cb = f32::from(zone.filter_q_cb) + modulation.filter_q_cb;
        let filter = zone.uses_filter().then(|| {
            FilterState::new(
                filter_base_cents,
                filter_q_from_centibels(filter_q_cb),
                sample_rate,
            )
        });
        let volume_envelope = zone.envelope.with_time_offsets(
            modulation.attack_vol_env_tc,
            modulation.decay_vol_env_tc,
            modulation.release_vol_env_tc,
        );
        let modulation_envelope = zone
            .mod_env
            .with_time_offsets(
                modulation.attack_mod_env_tc,
                modulation.decay_mod_env_tc,
                modulation.release_mod_env_tc,
            )
            .with_sustain_offset(modulation.sustain_mod_env_cb);
        let mut voice = Self {
            channel,
            note,
            preset,
            zone: zone_index,
            exclusive_class: zone.exclusive_class,
            effective_key,
            effective_velocity,
            key_down: true,
            pedal: false,
            finished: false,
            age,
            tuning_ratio,
            bend: 0.0,
            rate_scale: f64::from(asset_rate.max(1)) / f64::from(sample_rate.max(1)),
            increment: tuning_ratio,
            position: zone.start as f64,
            gain: (zone.attenuation_gain() * velocity_gain(effective_velocity)) as f32,
            zone_attenuation_cb: zone.attenuation_cb(),
            mod_attenuation: 1.0,
            zone_pan: zone.pan,
            channel_pan: 0.0,
            mod_pan: 0.0,
            left: 1.0,
            right: 1.0,
            envelope: Envelope::new(
                volume_envelope,
                effective_key,
                f64::from(sample_rate.max(1)),
            ),
            mod_env: Envelope::new(
                modulation_envelope,
                effective_key,
                f64::from(sample_rate.max(1)),
            ),
            filter_base_cents,
            filter,
            vibrato: Lfo::new(zone.vibrato_hz, zone.vibrato_delay, sample_rate),
            mod_lfo: Lfo::new(zone.mod_lfo_hz, zone.mod_lfo_delay, sample_rate),
            modulation: Modulation::default(),
            send_chorus: 0.0,
            send_reverb: 0.0,
            gain_scale: 1.0,
            layer_gain: 1.0,
            dynamic: None,
            frame: 0,
        };
        voice.set_pan(zone.pan);
        voice.set_modulation(modulation);
        voice.refresh_increment();
        voice
    }

    /// Replace the modulation depths; a controller change reaches sounding
    /// notes this way while each voice keeps its own oscillator phase.
    ///
    /// The attenuation and pan sums are static between controller events, so
    /// they are folded into the voice's own multipliers here rather than read
    /// per sample. A zone with no such record leaves both at their neutral
    /// values, which keeps the unmodulated render bit-identical. The Q and the
    /// envelope times are not refreshed: the biquad and the envelope machines
    /// were built in [`Self::start`], and a mid-note re-aim or re-arm is a
    /// discontinuity rather than an offset.
    pub fn set_modulation(&mut self, modulation: Modulation) {
        self.modulation = modulation;
        self.send_chorus = modulation.chorus_gain();
        self.send_reverb = modulation.reverb_gain();
        self.mod_attenuation = modulation.attenuation_gain(self.zone_attenuation_cb);
        let pan = modulation.pan / PAN_UNITS_PER_SIDE;
        if pan != self.mod_pan {
            self.mod_pan = pan;
            self.refresh_pan();
        }
    }

    /// Normalise a layer probe's level to the note's own velocity.
    pub fn set_gain_scale(&mut self, scale: f32) {
        self.gain_scale = scale;
    }

    /// Attach the hairpin crossfade this voice follows.
    pub fn set_dynamic_layer(&mut self, dynamic: DynamicLayer) {
        self.dynamic = Some(dynamic);
    }

    /// Place the voice at an absolute output frame before a block.
    pub fn set_frame_base(&mut self, frame: u64) {
        self.frame = frame;
    }

    /// Pitch bend in semitones.
    pub fn set_bend(&mut self, semitones: f64) {
        self.bend = semitones;
        self.refresh_increment();
    }

    fn refresh_increment(&mut self) {
        self.increment = self.tuning_ratio * 2f64.powf(self.bend / 12.0) * self.rate_scale;
    }

    /// Set voice pan in -1..=1.
    pub fn set_pan(&mut self, pan: f32) {
        let pan = f64::from(pan.clamp(-1.0, 1.0));
        let angle = (pan + 1.0) * 0.5 * std::f64::consts::FRAC_PI_2;
        self.left = (angle.cos() * f64::from(PAN_UNITY)) as f32;
        self.right = (angle.sin() * f64::from(PAN_UNITY)) as f32;
    }

    /// Release the voice envelope.
    pub fn release(&mut self) {
        self.key_down = false;
        self.pedal = false;
        self.envelope.release();
        self.mod_env.release();
    }

    /// Keep the note sounding after its key is up; the pedal releases it later.
    pub fn hold_for_pedal(&mut self) {
        self.pedal = true;
    }

    /// Whether the sustain pedal is holding the voice.
    pub fn pedal_held(&self) -> bool {
        self.pedal
    }

    /// Channel pan from CC10, applied on top of the zone's own pan and any
    /// pan record.
    pub fn set_channel_pan(&mut self, pan: f32) {
        self.channel_pan = pan;
        self.refresh_pan();
    }

    /// Rebuild the stereo gains from the zone, channel and record pans.
    fn refresh_pan(&mut self) {
        self.set_pan(self.zone_pan + self.channel_pan + self.mod_pan);
    }

    /// Render one frame into an interleaved stereo accumulator.
    pub fn render_frame(
        &mut self,
        bank: &SampleBank,
        zone: &Zone,
        channel_gain: f32,
        output: &mut [f32],
    ) {
        self.render_frame_into(bank, zone, channel_gain, output, None);
    }

    /// Render one frame and add the same contribution, scaled by the voice's
    /// sends, into the player's chorus and reverb buses.
    pub fn render_frame_with_sends(
        &mut self,
        bank: &SampleBank,
        zone: &Zone,
        channel_gain: f32,
        output: &mut [f32],
        sends: (&mut [f32], &mut [f32]),
    ) {
        self.render_frame_into(bank, zone, channel_gain, output, Some(sends));
    }

    fn render_frame_into(
        &mut self,
        bank: &SampleBank,
        zone: &Zone,
        channel_gain: f32,
        output: &mut [f32],
        sends: Option<(&mut [f32], &mut [f32])>,
    ) {
        if self.finished || output.len() < 2 {
            return;
        }
        if let Some(dynamic) = &self.dynamic {
            self.layer_gain = dynamic.weight(self.frame);
        }
        self.frame = self.frame.wrapping_add(1);
        let level = self.envelope.tick();
        if self.stage_is_done() {
            self.finished = true;
            return;
        }
        let looping = zone.sample_mode.loops_now(self.key_down);
        if looping && self.position >= zone.loop_end as f64 {
            let loop_length = (zone.loop_end - zone.loop_start) as f64;
            let over = self.position - zone.loop_end as f64;
            self.position = zone.loop_start as f64 + over % loop_length.max(1.0);
        }
        if self.position >= zone.end as f64 {
            self.finished = true;
            return;
        }
        let mut sample = bank.read(self.position);
        // One evaluation of each modulation source per sample: a table read for
        // the two LFOs, a stage step for the envelope.
        let vibrato = self.vibrato.tick();
        let modulation_lfo = self.mod_lfo.tick();
        let modulation_env = self.mod_env.tick();
        let pitch_cents = (self.modulation.vibrato_cents * vibrato
            + self.modulation.mod_lfo_pitch_cents * modulation_lfo
            + self.modulation.mod_env_pitch_cents * modulation_env)
            .clamp(-MAX_PITCH_MODULATION_CENTS, MAX_PITCH_MODULATION_CENTS);
        if let Some(filter) = &mut self.filter {
            // Sum the cutoff's sources in cents, clamp to the legal range, then
            // let the biquad's own refresh policy decide when to redesign.
            let cutoff_cents = (self.filter_base_cents
                + self.modulation.filter_offset_cents
                + self.modulation.mod_lfo_filter_cents * modulation_lfo
                + self.modulation.mod_env_filter_cents * modulation_env)
                .clamp(MIN_FILTER_FC_CENTS, MAX_FILTER_FC_CENTS);
            filter.update(cutoff_cents);
            sample = filter.filter.process_sample(0, f64::from(sample)) as f32;
        }
        let mut gain = level * self.gain * self.gain_scale * self.layer_gain * channel_gain;
        // A record's attenuation is already in centibels and is exactly unity
        // when the zone declares none.
        gain *= self.mod_attenuation;
        let volume_cb = self.modulation.mod_lfo_volume_cb;
        if volume_cb != 0.0 {
            // The generator is centibels of attenuation; the LFO is bipolar, so
            // its peak lifts the level and its trough lowers it.
            gain *= centibels_to_gain(-volume_cb * modulation_lfo);
        }
        let left = sample * gain * self.left;
        let right = sample * gain * self.right;
        output[0] += left;
        output[1] += right;
        if let Some((chorus, reverb)) = sends {
            if self.send_chorus > 0.0 {
                chorus[0] += left * self.send_chorus;
                chorus[1] += right * self.send_chorus;
            }
            if self.send_reverb > 0.0 {
                reverb[0] += left * self.send_reverb;
                reverb[1] += right * self.send_reverb;
            }
        }
        if pitch_cents == 0.0 {
            self.position += self.increment;
        } else {
            self.position += self.increment * f64::from(cents_to_ratio(pitch_cents));
        }
    }

    /// True once the volume envelope has run out, including the release tail.
    pub fn stage_is_done(&self) -> bool {
        self.envelope.stage == Stage::Done
    }
}

/// Velocity to amplitude: the square law. Velocity 64 sits 11.9 dB below 127.
pub(crate) fn velocity_gain(velocity: u8) -> f64 {
    let normalized = f64::from(velocity.min(127)) / 127.0;
    normalized * normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::font::{EnvelopeShape, SampleMode};
    use crate::sf2::modulation::Modulation;

    fn shape(sustain_cb: f32, attack: f64, decay: f64, release: f64) -> EnvelopeShape {
        EnvelopeShape {
            delay: 0.0,
            attack,
            hold: 0.0,
            decay,
            sustain_cb,
            release,
            hold_key_scale: 0.0,
            decay_key_scale: 0.0,
        }
    }

    fn zone(sustain_cb: f32, attack: f64, decay: f64, release: f64) -> Zone {
        Zone {
            key_low: 0,
            key_high: 127,
            vel_low: 0,
            vel_high: 127,
            sample: 0,
            start: 0,
            end: 100,
            loop_start: 0,
            loop_end: 0,
            root_key: 60,
            coarse_tune: 0,
            fine_tune: 0,
            scale_tuning: 100,
            keynum: None,
            velocity: None,
            attenuation_cb: 0,
            pan: 0.0,
            sample_mode: SampleMode::NoLoop,
            envelope: shape(sustain_cb, attack, decay, release),
            mod_env: shape(0.0, 0.0, 0.0, 0.0),
            filter_cutoff_cents: 13_500,
            filter_q_cb: 0,
            exclusive_class: 0,
            vib_lfo_to_pitch: 0.0,
            vibrato_hz: 8.176,
            vibrato_delay: 0.0,
            mod_lfo_to_pitch: 0.0,
            mod_lfo_hz: 8.176,
            mod_lfo_delay: 0.0,
            mod_lfo_to_filter_fc: 0.0,
            mod_lfo_to_volume: 0.0,
            mod_env_to_pitch: 0.0,
            mod_env_to_filter_fc: 0.0,
            chorus_send: 0,
            reverb_send: 0,
            modulators: Vec::new(),
        }
    }

    fn db(gain: f32) -> f32 {
        20.0 * gain.max(1e-9).log10()
    }

    fn start(zone: &Zone, note: u8) -> Voice {
        Voice::start(
            0,
            0,
            0,
            zone,
            44_100,
            44_100,
            note,
            100,
            0,
            Modulation::default(),
        )
    }

    #[test]
    fn attack_reaches_unity_and_decay_stops_at_sustain() {
        let zone = zone(200.0, 0.01, 0.01, 0.01);
        let mut envelope = Envelope::new(zone.envelope, 60, 44_100.0);
        // 441 samples of attack, then a decay to -20 dB.
        let mut peak = 0.0f32;
        for _ in 0..441 {
            peak = peak.max(envelope.tick());
        }
        assert!(db(peak) > -0.1, "attack peak {:.2} dB", db(peak));
        for _ in 0..441 {
            envelope.tick();
        }
        let sustain = (0..8).map(|_| envelope.tick()).next_back().unwrap();
        // 200 units is 19.2 dB on the measured sustain scale.
        assert!(
            (db(sustain) + 19.2).abs() < 0.5,
            "sustain {:.2} dB",
            db(sustain)
        );
    }

    #[test]
    fn release_falls_to_silence_and_finishes() {
        let zone = zone(0.0, 0.001, 0.0, 0.02);
        let mut envelope = Envelope::new(zone.envelope, 60, 44_100.0);
        for _ in 0..200 {
            envelope.tick();
        }
        envelope.release();
        let mut last = 1.0;
        for _ in 0..(0.02 * 44_100.0) as usize + 2 {
            last = envelope.tick();
        }
        assert_eq!(envelope.stage, Stage::Done);
        assert_eq!(last, 0.0);
    }

    #[test]
    fn zero_length_stages_do_not_hang() {
        let zone = zone(0.0, 0.0, 0.0, 0.0);
        let mut envelope = Envelope::new(zone.envelope, 60, 44_100.0);
        assert_eq!(envelope.tick(), 1.0);
        envelope.release();
        assert_eq!(envelope.tick(), 0.0);
        assert_eq!(envelope.stage, Stage::Done);
    }

    #[test]
    fn pan_law_is_constant_power() {
        let zone = zone(0.0, 0.001, 0.0, 0.001);
        let voice = start(&zone, 60);
        assert!((voice.left - 1.0).abs() < 1e-6);
        assert!((voice.right - 1.0).abs() < 1e-6);
        let mut hard_right = start(&zone, 60);
        hard_right.set_pan(1.0);
        assert!(hard_right.left < 1e-6);
        assert!((hard_right.right - std::f32::consts::SQRT_2).abs() < 1e-6);
    }

    #[test]
    fn bend_and_bank_rate_compose_in_the_increment() {
        let zone = zone(0.0, 0.001, 0.0, 0.001);
        // One octave above root at the bank's own rate.
        let mut voice = start(&zone, 72);
        assert!((voice.increment - 2.0).abs() < 1e-9);
        voice.set_bend(12.0);
        assert!((voice.increment - 4.0).abs() < 1e-9);
        // A 48 kHz bank read at 24 kHz output plays an octave lower in frames.
        let at_half_rate = Voice::start(
            0,
            0,
            0,
            &zone,
            24_000,
            48_000,
            72,
            100,
            0,
            Modulation::default(),
        );
        assert!((at_half_rate.increment - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_filter_stays_off_until_something_modulates_it() {
        let mut zone = zone(0.0, 0.001, 0.0, 0.001);
        let voice = start(&zone, 60);
        assert!(voice.filter.is_none(), "an open cutoff has no biquad");
        // A cutoff that a record can move brings the filter in.
        zone.filter_cutoff_cents = 8000;
        let voice = start(&zone, 60);
        assert!(voice.filter.is_some());
        // A static sample at the open default stays unfiltered even when a
        // filter depth is declared: the zone asks for no filter, so no biquad
        // is ever built and a controller move cannot create one.
        zone.filter_cutoff_cents = 13_500;
        let mut voice = start(&zone, 60);
        assert!(voice.filter.is_none());
        voice.set_modulation(Modulation::from_generators(
            0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 0.0,
        ));
        assert!(voice.filter.is_none(), "a filterless zone stays filterless");
        // A zone that declares a filter depth engages the filter from the
        // start, even with an otherwise open cutoff.
        zone.mod_lfo_to_filter_fc = 100.0;
        assert!(start(&zone, 60).filter.is_some());
    }

    #[test]
    fn the_fixed_key_and_velocity_generators_replace_the_note() {
        let mut zone = zone(0.0, 0.001, 0.0, 0.001);
        // Without the generators the note and its velocity pass through.
        let voice = start(&zone, 60);
        assert_eq!((voice.effective_key, voice.effective_velocity), (60, 100));
        // `keynum` 72 an octave above the root: the increment doubles, and the
        // envelope's key scaling sees 72 rather than the key played.
        zone.keynum = Some(72);
        zone.velocity = Some(20);
        let fixed = start(&zone, 48);
        assert_eq!(fixed.effective_key, 72);
        assert_eq!(fixed.effective_velocity, 20);
        assert!((fixed.increment - 2.0).abs() < 1e-9);
        // The level law follows the fixed velocity, not the played one.
        let played_quiet = start(&zone, 48);
        let mut loud_zone = zone;
        loud_zone.velocity = Some(127);
        let played_loud = start(&loud_zone, 48);
        assert!(played_loud.gain > played_quiet.gain * 30.0);
    }

    #[test]
    fn a_dynamic_layer_crossfades_across_the_ramp() {
        let ramp = DynamicRamp {
            start_frame: 0,
            end_frame: 100,
            start_velocity: 33,
            target_velocity: 96,
        };
        // The bank splits at velocity 64; the centres are the run midpoints.
        let ladder = [32u8, 95];
        let low = DynamicLayer::new(33, ramp, &ladder, 0);
        let high = DynamicLayer::new(33, ramp, &ladder, 1);
        // The low layer carries the start, the high layer the end.
        assert!(low.weight(0) > 0.98);
        assert!(high.weight(100) > 0.98);
        // A hairpin is linear in decibels, so halfway is 8.46^0.25 in velocity
        // terms, not the arithmetic midpoint.
        let halfway = high.weight(50);
        assert!(
            (halfway - 0.386).abs() < 0.01,
            "halfway weight was {halfway}"
        );
        let mut previous = high.weight(0);
        for frame in (0..=100).step_by(5) {
            let value = high.weight(frame);
            assert!(value + 1e-5 >= previous, "the mix moves one way");
            assert!((low.weight(frame) + value - 1.0).abs() < 1e-5);
            previous = value;
        }
    }

    #[test]
    fn the_cutoff_refresh_is_rate_limited() {
        let mut state = FilterState::new(8000.0, 1.0, 48_000);
        // Less than 4 cents does not redesign.
        state.update(8002.0);
        assert_eq!(state.designed_cents, 8000.0);
        // A larger step redesigns once, then waits a few samples.
        state.update(8100.0);
        assert_eq!(state.designed_cents, 8100.0);
        state.update(8200.0);
        assert_eq!(state.designed_cents, 8100.0, "refreshed too soon");
        for _ in 0..FILTER_REFRESH_SAMPLES {
            state.update(8200.0);
        }
        assert_eq!(state.designed_cents, 8200.0);
    }
}
