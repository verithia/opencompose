//! Effect and transport contracts shared by the DSP graph and plugin hosts.

mod transport_range;
pub use transport_range::{TransportBlockError, TransportRangeError};

/// Host timeline information supplied to plugins during offline render blocks.
#[derive(Debug, Clone)]
pub struct PluginTransport {
    pub sample_pos: u64,
    pub sample_rate: u32,
    pub tempo: f64,
    pub time_sig_numerator: u16,
    pub time_sig_denominator: u16,
    pub playing: bool,
    clock: Option<std::sync::Arc<oc_dsl::timing::ProjectClock>>,
}

impl PluginTransport {
    /// Start a transport at an absolute sample position and tempo.
    pub fn new(sample_pos: u64, sample_rate: u32, tempo: f64) -> Self {
        Self {
            sample_pos,
            sample_rate,
            tempo,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            playing: true,
            clock: None,
        }
    }

    /// Attach a fixed meter when no project clock is present.
    pub fn with_time_signature(mut self, numerator: u16, denominator: u16) -> Self {
        self.time_sig_numerator = numerator.max(1);
        self.time_sig_denominator = denominator.max(1);
        self
    }

    /// Absolute transport time in seconds.
    pub fn seconds(&self) -> f64 {
        self.sample_pos as f64 / self.sample_rate as f64
    }

    /// Absolute musical position in quarter notes.
    pub fn quarter_notes(&self) -> f64 {
        self.clock_position()
            .map(|position| position.quarter)
            .unwrap_or_else(|| self.seconds() * self.tempo / 60.0)
    }

    /// Attach the authored project clock used for musical positions.
    pub fn with_clock(mut self, clock: std::sync::Arc<oc_dsl::timing::ProjectClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// The authored project clock, when this transport carries one.
    pub fn clock(&self) -> Option<&std::sync::Arc<oc_dsl::timing::ProjectClock>> {
        self.clock.as_ref()
    }

    fn clock_position(&self) -> Option<oc_dsl::timing::ClockPosition> {
        self.clock.as_ref().map(|clock| {
            clock
                .position_at_sample(self.sample_pos, self.sample_rate)
                .expect("validated render clock and sample rate")
        })
    }

    /// The tempo at this transport position.
    pub fn current_tempo(&self) -> f64 {
        self.clock_position()
            .map(|position| position.bpm)
            .unwrap_or(self.tempo)
    }

    /// First sample after this position where the authored clock changes.
    pub fn next_clock_change_sample(&self) -> Option<u64> {
        self.clock.as_ref().and_then(|clock| {
            clock
                .next_change_sample(self.sample_pos, self.sample_rate)
                .expect("validated render clock")
        })
    }

    /// (numerator, denominator, bar start in quarters, zero-indexed bar number).
    pub fn musical_position(&self) -> (u16, u16, f64, u32) {
        if let Some(position) = self.clock_position() {
            return (
                u16::from(position.signature.numerator),
                u16::from(position.signature.denominator),
                position.bar_start_quarter,
                position.bar_number - 1,
            );
        }
        let length = f64::from(self.time_sig_numerator.max(1)) * 4.0
            / f64::from(self.time_sig_denominator.max(1));
        let bar = (self.quarter_notes() / length).floor();
        (
            self.time_sig_numerator,
            self.time_sig_denominator,
            bar * length,
            bar as u32,
        )
    }

    /// Clone this transport at another absolute sample position.
    pub fn at_sample(&self, sample_pos: u64) -> Self {
        let mut result = self.clone();
        result.sample_pos = sample_pos;
        result
    }

    /// Frames from this position to the next notated beat boundary.
    ///
    /// The beat is the meter's denominator note, so a 6/8 bar has six beat
    /// boundaries and each is an eighth note. This is a host-callback hint; it
    /// never changes how audio is rendered.
    pub fn frames_to_next_beat(&self) -> u32 {
        let (_, denominator, bar_start, _) = self.musical_position();
        let beat_quarters = 4.0 / f64::from(denominator.max(1));
        let beat = self.quarter_notes();
        let index = ((beat - bar_start) / beat_quarters).floor().max(0.0);
        let next = bar_start + (index + 1.0) * beat_quarters;
        let frames = self
            .clock
            .as_ref()
            .and_then(|clock| {
                clock
                    .sample_at_quarter(next, self.sample_rate)
                    .ok()
                    .map(|sample| sample.saturating_sub(self.sample_pos))
            })
            .unwrap_or_else(|| {
                ((next - beat).max(0.0) * 60.0 / self.current_tempo() * f64::from(self.sample_rate))
                    .round() as u64
            });
        u32::try_from(frames).unwrap_or(u32::MAX)
    }

    /// True when this transport carries an authored tempo/meter map.
    /// True when this transport carries an authored tempo/meter map.
    pub fn has_clock(&self) -> bool {
        self.clock.is_some()
    }

    /// Largest block which does not cross an authored tempo/meter change.
    /// Largest block that does not cross an authored tempo or meter change.
    pub fn block_frames(&self, maximum: usize) -> usize {
        self.clock
            .as_ref()
            .and_then(|clock| {
                clock
                    .next_change_sample(self.sample_pos, self.sample_rate)
                    .expect("validated render clock and sample rate")
            })
            .map(|next| maximum.min((next - self.sample_pos) as usize))
            .unwrap_or(maximum)
    }
}

/// Scratch preparation failed before audio processing began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockPreparationError {
    CapacityExceeded,
}

/// Control-plane delivery state for a processor's immutable first failure.
/// `Pending` means publication is in progress; it must not replace the original
/// cause with a fallback. `Reported` means another owner already consumed it.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessingFailureReport {
    Pending,
    Ready(String),
    Reported,
    Unavailable,
}

impl ProcessingFailureReport {
    /// Select the most useful report. Callers stop querying after `Ready` so
    /// additional processors retain their independently reportable failures.
    pub fn merge(self, next: Self) -> Self {
        match (self, next) {
            (ready @ Self::Ready(_), _) | (_, ready @ Self::Ready(_)) => ready,
            (Self::Pending, _) | (_, Self::Pending) => Self::Pending,
            (Self::Reported, _) | (_, Self::Reported) => Self::Reported,
            _ => Self::Unavailable,
        }
    }
}

/// The additional signal an effect receives for this call. Control values are
/// normalized values per frame, not a second audio stream.
#[derive(Debug, Clone, Copy)]
pub enum AudioProcessInput<'a> {
    Main,
    Sidechain(&'a [f32]),
    Control(&'a [f32]),
}

/// Borrowed processing context. The caller keeps the transport and its project
/// clock alive until processing returns; this value owns no shared allocation.
#[derive(Debug, Clone, Copy)]
pub struct AudioProcessContext<'a> {
    pub sample_rate: u32,
    pub transport: Option<&'a PluginTransport>,
    pub input: AudioProcessInput<'a>,
}

/// A prepared stereo processor that renders one bounded block at a time.
pub trait AudioEffect: Send {
    /// Reserve scratch for `max_frames` stereo frames (two interleaved samples
    /// per frame), outside processing. Implementations must
    /// preserve parameters, latency, and signal history without consuming audio.
    fn prepare_block(&mut self, _max_frames: usize) -> Result<(), BlockPreparationError> {
        Ok(())
    }

    /// Bounded status check: true when this processor can no longer produce
    /// valid audio. Implementations must not allocate, lock, or call a plugin.
    fn processing_failed(&self) -> bool {
        false
    }

    /// Describe and consume the first failure once, outside audio processing.
    /// This control-plane method may allocate and must never run on a callback.
    fn take_processing_failure(&self) -> ProcessingFailureReport {
        ProcessingFailureReport::Unavailable
    }

    /// Process a buffer of stereo interleaved samples in-place.
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32);

    /// Process with caller-retained timeline and signal context. The default
    /// bridge preserves each existing Main/Sidechain/Control override, including
    /// its own validation and empty-buffer behavior. Its temporary transport
    /// clone cannot release the current clock's last owner during this call.
    ///
    /// This adapter does not certify a legacy implementation as realtime-safe:
    /// implementations still own their allocation, locking and retained-asset
    /// retirement policies. Prepared implementations can override this method
    /// to consume the borrow directly. By-value compatibility methods below
    /// retain their existing ownership semantics.
    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        let transport = context.transport.cloned();
        match context.input {
            AudioProcessInput::Main => {
                self.process_with_transport(buffer, context.sample_rate, transport);
            }
            AudioProcessInput::Sidechain(sidechain) => {
                self.process_with_sidechain(buffer, sidechain, context.sample_rate, transport);
            }
            AudioProcessInput::Control(control) => {
                self.process_with_control(buffer, control, context.sample_rate, transport);
            }
        }
    }

    /// Process with optional host timeline context. Built-in effects ignore this
    /// by default; plugin effects can use it for tempo-synced internals.
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        _transport: Option<PluginTransport>,
    ) {
        self.process(buffer, sample_rate);
    }

    /// Process with a detector/key input. Plugins with an auxiliary audio bus
    /// override this; ordinary effects retain their one-input behavior.
    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        _sidechain: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_transport(buffer, sample_rate, transport);
    }

    fn supports_sidechain(&self) -> bool {
        false
    }

    fn sidechain_source(&self) -> Option<&str> {
        None
    }

    /// Process with a control input: one normalized `0..1` value per frame,
    /// read from the graph's control edge. A device that declares a control
    /// port overrides this; the control signal is never audio and never enters
    /// the output sum.
    fn process_with_control(
        &mut self,
        buffer: &mut [f32],
        _control: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_transport(buffer, sample_rate, transport);
    }

    /// True when the device declares a control input port.
    fn supports_control_input(&self) -> bool {
        false
    }

    /// The control source this device reads, when a control edge feeds it.
    fn control_source(&self) -> Option<&str> {
        None
    }

    /// Configure timeline alignment between the main and key inputs. Returns
    /// additional main-path latency introduced by the alignment.
    fn configure_sidechain_alignment(
        &mut self,
        _main_latency: u32,
        _sidechain_latency: u32,
    ) -> u32 {
        0
    }

    /// Set a named parameter (e.g., "ratio", "threshold", "decay").
    fn set_param(&mut self, name: &str, value: f64);

    /// Queue a named parameter change at a block-relative sample offset. The
    /// value is in the device's plain domain, like `set_param`. Returns false
    /// when the device cannot schedule changes inside a block.
    fn queue_param_at(&mut self, _name: &str, _value: f64, _sample_offset: u32) -> bool {
        false
    }

    /// True when `queue_param_at` lands inside the block instead of at its
    /// boundary. Callers apply `set_param` per segment otherwise.
    fn supports_param_offsets(&self) -> bool {
        false
    }

    /// Largest block this device accepts in one processing call; `u32::MAX`
    /// when it has no fixed limit.
    fn max_block_frames(&self) -> u32 {
        u32::MAX
    }

    /// Get current parameter value.
    fn get_param(&self, name: &str) -> Option<f64>;

    /// Reset internal state (e.g., delay lines, filter history).
    fn reset(&mut self);

    /// Select the instance seed for a device whose modulation is seeded.
    ///
    /// Renderers seed from the authored device location, never from the clock
    /// or an address, so the same project always renders the same audio.
    /// Devices whose output does not depend on a seed ignore this.
    fn set_seed(&mut self, _seed: u32) {}

    /// Release audio-thread processing state. Plugin hosts call this on the
    /// thread that processed the chain before the effect is deactivated; a
    /// host that stops processing on a different thread violates CLAP's
    /// audio-thread contract.
    fn end_processing(&mut self) {}

    /// Algorithmic latency introduced by this effect, in stereo sample frames.
    /// Hosts use this to align parallel paths; zero is correct for built-ins.
    fn latency_samples(&self) -> u32 {
        0
    }

    /// Reported release/tail duration in sample frames. Hosts render silence
    /// through this interval so delayed/reverberant output is not truncated.
    fn tail_samples(&self) -> u32 {
        0
    }

    /// Ring-out this device needs when a bounce renders past its range end, in
    /// sample frames.
    ///
    /// A bounce reads this so the media keeps a reverb's decay; the full-mix
    /// length policy never does, so a device that reports here does not move a
    /// render's length. Devices whose [`Self::tail_samples`] already covers
    /// their ring-out leave this at zero.
    fn bounce_tail_samples(&self) -> u32 {
        0
    }

    /// Hosted-plugin release tail for the streaming idle gate.
    ///
    /// `None` when this device is not a hosted plugin or its format provides
    /// no tail signal; such a plugin is never paused.
    fn plugin_tail_samples(&self) -> Option<u32> {
        None
    }

    /// Enable the streaming idle gate. Offline renders never enable it.
    fn set_streaming_idle_gate(&mut self, _enabled: bool) {}

    /// Current gain reduction in dB (positive means attenuation) for meters.
    fn gain_reduction_db(&self) -> f64 {
        0.0
    }

    /// Number of bands this device meters individually (0 when it has none).
    fn band_count(&self) -> usize {
        0
    }

    /// Signed dynamic gain of band `index` in dB (negative = reduction).
    fn band_gain_db(&self, _index: usize) -> Option<f64> {
        None
    }

    /// Peak gain reduction since reset, in dB, for meters.
    fn max_gain_reduction_db(&self) -> f64 {
        self.gain_reduction_db()
    }

    /// Headroom to the device's ceiling in dB when it has one.
    fn ceiling_margin_db(&self) -> Option<f64> {
        None
    }

    /// Human-readable name.
    fn name(&self) -> &str;
}
