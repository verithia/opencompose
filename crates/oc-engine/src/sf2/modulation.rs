//! SoundFont 2 modulation: the specification's default modulator set, the
//! modulation and vibrato LFOs and the modulation envelope.
//!
//! Every destination is summed in its own unit before the voice clamps it:
//! cents for pitch, absolute cents above 8.176 Hz for the filter cutoff and
//! centibels of attenuation for volume. A destination reads its generator
//! value first, then the format's default modulators, then the bank's
//! `pmod`/`imod` records of the zone, so the order is fixed and deterministic.
//!
//! LFOs are a phase accumulator into a 1024-point compile-time sine table with
//! linear interpolation; a pitch offset is a table read on a ratio table. Both
//! are evaluated once per sample per voice. The only coarser update is the
//! biquad's coefficient set, which costs a sin/cos pair and is refreshed when
//! the cutoff moved by 4 cents and at most once every 8 samples.
//!
//! Two groups of destinations are read once at note-on instead of being
//! refreshed with the controllers: `initialFilterQ` and the seven envelope-stage
//! destinations. A moving Q would redesign the biquad on every controller event,
//! and a moving envelope time would re-arm a stage that is already running;
//! neither is an offset on a value the voice holds. Both groups therefore behave
//! like a generator offset fixed at note-on, and [`Modulation::add`] still sums
//! them so the destinations are modelled rather than counted.

/// Vibrato depth in cents at full modulation, from the format's default CC1
/// and channel-pressure modulators.
pub const DEFAULT_VIBRATO_CENTS: f32 = 50.0;

/// LFO rate at 0 absolute cents: the format's origin for every LFO.
pub const DEFAULT_LFO_HZ: f32 = 8.176;

/// Widest pitch modulation the voice applies, in either direction.
pub const MAX_PITCH_MODULATION_CENTS: f32 = 12_000.0;

/// The format's legal range for an absolute filter cutoff, in cents above
/// 8.176 Hz.
pub const MIN_FILTER_FC_CENTS: f32 = 1_500.0;
/// Highest absolute filter cutoff, in cents.
pub const MAX_FILTER_FC_CENTS: f32 = 13_500.0;

/// `modLfoToVolume` is a centibel scale; this is the deepest value it carries.
pub const MAX_MOD_LFO_VOLUME_CB: f32 = 960.0;

/// The format's ceiling for the summed `initialAttenuation` value, in
/// centibels: the generator plus every record clips at 144 dB.
pub const MAX_ATTENUATION_CB: f32 = 1440.0;

/// One `pan` generator unit: the format stores pan as -500 (left) to 500
/// (right), while a voice works in the -1..1 range.
pub const PAN_UNITS_PER_SIDE: f32 = 500.0;

/// Full send in tenths of a percent, the format's `chorusEffectsSend` and
/// `reverbEffectsSend` range.
pub const MAX_SEND_TENTHS: f32 = 1000.0;

/// The format's default CC91/CC93 send modulators: 200 tenths of a percent at
/// full controller travel, i.e. a fifth of the send.
pub const DEFAULT_SEND_CENTS: f32 = 200.0;

/// The format's default velocity-to-cutoff modulator: the concave curve below
/// full velocity, 2400 cents deep at zero velocity and zero at full velocity,
/// so a soft note is darker than a hard one by up to two octaves of cutoff.
pub const DEFAULT_VELOCITY_FILTER_CENTS: f32 = 2_400.0;

/// Longest modulator list read for one zone. Real lists hold a handful of
/// records; the cap keeps a malformed chunk from costing per-voice work.
pub const MAX_ZONE_MODULATORS: usize = 16;

/// Size of one `pmod`/`imod` record in bytes.
pub const MODULATOR_RECORD_SIZE: usize = 10;

/// Where a modulator sends its contribution, named as the generator it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// `modLfoToPitch`, cents.
    ModLfoToPitch,
    /// `vibLfoToPitch`, cents.
    VibLfoToPitch,
    /// `modEnvToPitch`, cents.
    ModEnvToPitch,
    /// `initialFilterFc`, absolute cents.
    InitialFilterFc,
    /// `initialFilterQ`, centibels of resonance. Read once at note-on.
    InitialFilterQ,
    /// `modLfoToFilterFc`, cents.
    ModLfoToFilterFc,
    /// `modEnvToFilterFc`, cents.
    ModEnvToFilterFc,
    /// `modLfoToVolume`, centibels of attenuation.
    ModLfoToVolume,
    /// `initialAttenuation`, centibels of attenuation. The zone's own generator
    /// is scaled by the format's EMU factor on load, so a record's amount is
    /// the specification's centibel directly.
    InitialAttenuation,
    /// `pan`, in generator units (-500 left .. 500 right).
    Pan,
    /// `chorusEffectsSend`, tenths of a percent of a full send.
    ChorusSend,
    /// `reverbEffectsSend`, tenths of a percent of a full send.
    ReverbSend,
    /// `attackModEnv`, timecents. Read once at note-on.
    AttackModEnv,
    /// `decayModEnv`, timecents. Read once at note-on.
    DecayModEnv,
    /// `sustainModEnv`, in the generator's 0.1 % units. Read once at note-on.
    SustainModEnv,
    /// `releaseModEnv`, timecents. Read once at note-on.
    ReleaseModEnv,
    /// `attackVolEnv`, timecents. Read once at note-on.
    AttackVolEnv,
    /// `decayVolEnv`, timecents. Read once at note-on.
    DecayVolEnv,
    /// `releaseVolEnv`, timecents. Read once at note-on.
    ReleaseVolEnv,
}

impl Destination {
    /// Modulator destination opcode to a modelled destination.
    fn from_opcode(opcode: u16) -> Option<Self> {
        Some(match opcode {
            5 => Self::ModLfoToPitch,
            6 => Self::VibLfoToPitch,
            7 => Self::ModEnvToPitch,
            8 => Self::InitialFilterFc,
            9 => Self::InitialFilterQ,
            10 => Self::ModLfoToFilterFc,
            11 => Self::ModEnvToFilterFc,
            13 => Self::ModLfoToVolume,
            15 => Self::ChorusSend,
            16 => Self::ReverbSend,
            17 => Self::Pan,
            26 => Self::AttackModEnv,
            28 => Self::DecayModEnv,
            29 => Self::SustainModEnv,
            30 => Self::ReleaseModEnv,
            34 => Self::AttackVolEnv,
            36 => Self::DecayVolEnv,
            38 => Self::ReleaseVolEnv,
            48 => Self::InitialAttenuation,
            _ => return None,
        })
    }
}

/// Where a modulator reads its input from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModSource {
    /// The format's "no controller" source: a constant at full scale.
    None,
    MidiController(u8),
    ChannelPressure,
    /// Polyphonic key pressure: the pressure of the note that owns the voice.
    PolyPressure,
    Velocity,
    Keynum,
    PitchWheel,
    /// The pitch-bend range in semitones, read as the format's sensitivity
    /// source. The player's range is fixed, so this is a constant.
    PitchWheelSensitivity,
}

/// Which way the source moves the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Positive,
    Negative,
}

/// Whether the source reads 0..1 or -1..1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Unipolar,
    Bipolar,
}

/// Curve applied to the source value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Linear,
    /// The format's concave curve, `-(20/96) log10(x^2)`, clamped to 0..1:
    /// full at zero source and zero at full scale, so 960 units of concave
    /// attenuation is exactly `x^2` of level and the format's
    /// velocity-to-attenuation default modulator reproduces the engine's
    /// measured `(v/127)^2` law.
    Concave,
    /// The complement of [`Self::Concave`]: zero at zero source, full at full
    /// scale.
    Convex,
    /// 1 when the source is at or past halfway, else 0.
    Switch,
}

/// Floor inside the concave curve's logarithm; it keeps zero at a finite
/// negative value instead of an infinity.
const CURVE_FLOOR: f32 = 1e-9;

impl Transform {
    fn apply(self, source: f32) -> f32 {
        match self {
            Self::Linear => source,
            Self::Concave => concave(source),
            Self::Convex => 1.0 - concave(source),
            Self::Switch => {
                if source >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

/// `-(20/96) log10(x^2)`: 0 at full scale, 1 at `10^(-96/20)` of it and at
/// every smaller source.
fn concave(source: f32) -> f32 {
    let squared = source.clamp(0.0, 1.0) * source.clamp(0.0, 1.0);
    let value = -(20.0 / 96.0) * ((squared + CURVE_FLOOR) / (1.0 + CURVE_FLOOR)).log10();
    value.clamp(0.0, 1.0)
}

/// One modulator: source, curve, destination and amount in the destination's
/// unit at full scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Modulator {
    pub source: ModSource,
    pub destination: Destination,
    pub direction: Direction,
    pub polarity: Polarity,
    pub transform: Transform,
    pub amount: f32,
    /// An optional second source that scales the amount, as the format's
    /// record carries one.
    pub amount_source: Option<AmountSource>,
}

/// A modulator record's secondary source: it scales the amount by its own
/// mapped value, with its own direction and polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmountSource {
    pub source: ModSource,
    pub direction: Direction,
    pub polarity: Polarity,
}

/// The per-channel inputs a modulator reads while a note sounds.
#[derive(Debug, Clone, Copy)]
pub struct ModInput<'a> {
    pub controllers: &'a [u8; 128],
    /// Channel pressure, 0 .. 127.
    pub pressure: u8,
    /// Polyphonic pressure of this voice's own note, 0 .. 127.
    pub poly_pressure: u8,
    pub key: u8,
    pub velocity: u8,
    /// Pitch wheel position, -1.0 .. 1.0.
    pub bend: f64,
}

impl<'a> ModInput<'a> {
    /// Controller, pressure, key, velocity and bend inputs.
    pub fn new(controllers: &'a [u8; 128], pressure: u8, key: u8, velocity: u8, bend: f64) -> Self {
        Self {
            controllers,
            pressure,
            poly_pressure: 0,
            key,
            velocity,
            bend,
        }
    }

    /// Attach the voice's note pressure; a fresh input starts at zero.
    pub fn with_poly_pressure(mut self, pressure: u8) -> Self {
        self.poly_pressure = pressure;
        self
    }
}

impl Modulator {
    /// Contribution in the destination's units for the current inputs.
    pub fn value(&self, input: &ModInput<'_>) -> f32 {
        let directed = map(
            self.polarity,
            self.direction,
            self.transform.apply(self.raw(input)),
        );
        let scale = match &self.amount_source {
            None => 1.0,
            Some(amount_source) => map(
                amount_source.polarity,
                amount_source.direction,
                amount_source.source.value(input),
            ),
        };
        self.amount * directed * scale
    }

    /// The source's raw value, before the transform and the mapping: 0..1 for
    /// every source except the pitch wheel, whose sensitivity is a positive
    /// count of semitones.
    fn raw(&self, input: &ModInput<'_>) -> f32 {
        self.source.value(input)
    }
}

impl ModSource {
    fn value(self, input: &ModInput<'_>) -> f32 {
        match self {
            Self::None => 1.0,
            Self::MidiController(controller) => {
                f32::from(input.controllers[usize::from(controller.min(127))]) / 127.0
            }
            Self::ChannelPressure => f32::from(input.pressure.min(127)) / 127.0,
            Self::PolyPressure => f32::from(input.poly_pressure.min(127)) / 127.0,
            Self::Velocity => f32::from(input.velocity.min(127)) / 127.0,
            Self::Keynum => f32::from(input.key.min(127)) / 127.0,
            Self::PitchWheel => (input.bend.clamp(-1.0, 1.0) as f32 + 1.0) * 0.5,
            Self::PitchWheelSensitivity => super::player::BEND_RANGE_SEMITONES as f32 / 127.0,
        }
    }
}

/// The four mappings, linear in the source's raw value: unipolar `x` / `1-x`,
/// bipolar `2x-1` / `1-2x`. A negative unipolar source is the complement of
/// the positive one, not its negation, which is what the format's
/// velocity-to-attenuation default needs.
fn map(polarity: Polarity, direction: Direction, shaped: f32) -> f32 {
    match (polarity, direction) {
        (Polarity::Unipolar, Direction::Positive) => shaped,
        (Polarity::Unipolar, Direction::Negative) => 1.0 - shaped,
        (Polarity::Bipolar, Direction::Positive) => shaped * 2.0 - 1.0,
        (Polarity::Bipolar, Direction::Negative) => 1.0 - shaped * 2.0,
    }
}

/// The format's default modulators that this engine models, in the order they
/// are added.
///
/// The specification's full default list, and what happens to each entry:
///
/// | Source | Destination | Amount | Here |
/// |---|---|---|---|
/// | velocity | `initialAttenuation` | 960 cB, concave | the measured `(v/127)^2` level law in the SoundFont voice; a bank record targeting the same destination adds its own centibels |
/// | velocity | `initialFilterFc` | -2400 cents, concave | applied to a zone that engages its low-pass: full velocity is the generator's cutoff, and the cutoff closes by up to 2400 cents at zero velocity |
/// | channel pressure | `vibLfoToPitch` | 50 cents | applied here |
/// | CC1 | `vibLfoToPitch` | 50 cents | applied here |
/// | CC7 | `initialAttenuation` | 960 cB, concave | the CC7 `(v/127)^2` level law |
/// | CC10 | `pan` | 1000 | the constant-power pan law; a bank record targeting `pan` adds generator units to it |
/// | CC11 | `initialAttenuation` | 960 cB, concave | the CC11 `(v/127)^2` level law |
/// | CC91 | `reverbEffectsSend` | 200 | applied, summed with the zone's generator |
/// | CC93 | `chorusEffectsSend` | 200 | applied, summed with the zone's generator |
/// | pitch wheel | `initialFilterFc` | 0, amount source pitch wheel | inert (amount 0) |
///
/// No default modulator routes the modulation LFO or the modulation envelope:
/// a bank reaches them through the `modLfoTo*`/`modEnvTo*` generators and its
/// own records.
pub const DEFAULT_MODULATORS: [Modulator; 5] = [
    Modulator {
        source: ModSource::MidiController(1),
        destination: Destination::VibLfoToPitch,
        direction: Direction::Positive,
        polarity: Polarity::Unipolar,
        transform: Transform::Linear,
        amount: DEFAULT_VIBRATO_CENTS,
        amount_source: None,
    },
    Modulator {
        source: ModSource::ChannelPressure,
        destination: Destination::VibLfoToPitch,
        direction: Direction::Positive,
        polarity: Polarity::Unipolar,
        transform: Transform::Linear,
        amount: DEFAULT_VIBRATO_CENTS,
        amount_source: None,
    },
    Modulator {
        source: ModSource::Velocity,
        destination: Destination::InitialFilterFc,
        direction: Direction::Positive,
        polarity: Polarity::Unipolar,
        transform: Transform::Concave,
        amount: -DEFAULT_VELOCITY_FILTER_CENTS,
        amount_source: None,
    },
    Modulator {
        source: ModSource::MidiController(91),
        destination: Destination::ReverbSend,
        direction: Direction::Positive,
        polarity: Polarity::Unipolar,
        transform: Transform::Linear,
        amount: DEFAULT_SEND_CENTS,
        amount_source: None,
    },
    Modulator {
        source: ModSource::MidiController(93),
        destination: Destination::ChorusSend,
        direction: Direction::Positive,
        polarity: Polarity::Unipolar,
        transform: Transform::Linear,
        amount: DEFAULT_SEND_CENTS,
        amount_source: None,
    },
];

/// The summed modulation depths of one zone for one controller state.
///
/// Pitch and cutoff depths are in cents, the volume depth in centibels of
/// attenuation. The voice evaluates these once per sample against the LFO and
/// envelope values and clamps the sum per destination. The attenuation and pan
/// sums are static between controller events, so the voice applies them when
/// the depths are refreshed instead of per sample.
///
/// `filter_q_cb` and the seven envelope offsets are different: the voice reads
/// them once in `Voice::start`, and a later refresh keeps them in the struct but
/// cannot re-aim the biquad or re-arm an envelope stage.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Modulation {
    /// Vibrato LFO to pitch, cents.
    pub vibrato_cents: f32,
    /// Modulation LFO to pitch, cents.
    pub mod_lfo_pitch_cents: f32,
    /// Modulation envelope to pitch, cents.
    pub mod_env_pitch_cents: f32,
    /// Modulation LFO to filter cutoff, cents.
    pub mod_lfo_filter_cents: f32,
    /// Modulation envelope to filter cutoff, cents.
    pub mod_env_filter_cents: f32,
    /// Modulation LFO to volume, centibels of attenuation.
    pub mod_lfo_volume_cb: f32,
    /// Modulator records that offset the cutoff directly, cents.
    pub filter_offset_cents: f32,
    /// Modulator records that sharpen the filter, centibels of resonance.
    pub filter_q_cb: f32,
    /// Modulator records that offset the modulation envelope's attack,
    /// timecents.
    pub attack_mod_env_tc: f32,
    /// Modulation envelope decay offset, timecents.
    pub decay_mod_env_tc: f32,
    /// Modulation envelope sustain offset, in the generator's 0.1 % units.
    pub sustain_mod_env_cb: f32,
    /// Modulation envelope release offset, timecents.
    pub release_mod_env_tc: f32,
    /// Volume envelope attack offset, timecents.
    pub attack_vol_env_tc: f32,
    /// Volume envelope decay offset, timecents.
    pub decay_vol_env_tc: f32,
    /// Volume envelope release offset, timecents.
    pub release_vol_env_tc: f32,
    /// Modulator records that attenuate the voice, centibels. The zone's own
    /// `initialAttenuation` generator is a separate value in generator units.
    pub attenuation_cb: f32,
    /// Modulator records that pan the voice, generator units (-500..500).
    pub pan: f32,
    /// Chorus send in tenths of a percent: the zone's generator plus every
    /// modulator that reaches `chorusEffectsSend`.
    pub chorus_send: f32,
    /// Reverb send in tenths of a percent, same summation.
    pub reverb_send: f32,
}

impl Modulation {
    /// One zone's generator values, before the modulators are added.
    #[allow(clippy::too_many_arguments)]
    pub fn from_generators(
        vibrato_cents: f32,
        mod_lfo_pitch_cents: f32,
        mod_env_pitch_cents: f32,
        mod_lfo_filter_cents: f32,
        mod_env_filter_cents: f32,
        mod_lfo_volume_cb: f32,
        chorus_send: f32,
        reverb_send: f32,
    ) -> Self {
        Self {
            vibrato_cents,
            mod_lfo_pitch_cents,
            mod_env_pitch_cents,
            mod_lfo_filter_cents,
            mod_env_filter_cents,
            mod_lfo_volume_cb,
            filter_offset_cents: 0.0,
            filter_q_cb: 0.0,
            attack_mod_env_tc: 0.0,
            decay_mod_env_tc: 0.0,
            sustain_mod_env_cb: 0.0,
            release_mod_env_tc: 0.0,
            attack_vol_env_tc: 0.0,
            decay_vol_env_tc: 0.0,
            release_vol_env_tc: 0.0,
            attenuation_cb: 0.0,
            pan: 0.0,
            chorus_send,
            reverb_send,
        }
    }

    /// Add one modulator's contribution to its destination.
    pub fn add(&mut self, destination: Destination, amount: f32) {
        match destination {
            Destination::ModLfoToPitch => self.mod_lfo_pitch_cents += amount,
            Destination::VibLfoToPitch => self.vibrato_cents += amount,
            Destination::ModEnvToPitch => self.mod_env_pitch_cents += amount,
            Destination::InitialFilterFc => self.filter_offset_cents += amount,
            Destination::InitialFilterQ => self.filter_q_cb += amount,
            Destination::ModLfoToFilterFc => self.mod_lfo_filter_cents += amount,
            Destination::ModEnvToFilterFc => self.mod_env_filter_cents += amount,
            Destination::ModLfoToVolume => self.mod_lfo_volume_cb += amount,
            Destination::AttackModEnv => self.attack_mod_env_tc += amount,
            Destination::DecayModEnv => self.decay_mod_env_tc += amount,
            Destination::SustainModEnv => self.sustain_mod_env_cb += amount,
            Destination::ReleaseModEnv => self.release_mod_env_tc += amount,
            Destination::AttackVolEnv => self.attack_vol_env_tc += amount,
            Destination::DecayVolEnv => self.decay_vol_env_tc += amount,
            Destination::ReleaseVolEnv => self.release_vol_env_tc += amount,
            Destination::InitialAttenuation => self.attenuation_cb += amount,
            Destination::Pan => self.pan += amount,
            Destination::ChorusSend => self.chorus_send += amount,
            Destination::ReverbSend => self.reverb_send += amount,
        }
    }

    /// The chorus send as a linear gain, 0..1.
    pub fn chorus_gain(&self) -> f32 {
        (self.chorus_send / MAX_SEND_TENTHS).clamp(0.0, 1.0)
    }

    /// The reverb send as a linear gain, 0..1.
    pub fn reverb_gain(&self) -> f32 {
        (self.reverb_send / MAX_SEND_TENTHS).clamp(0.0, 1.0)
    }

    /// The attenuation a record adds, as a linear gain, given the zone's own
    /// attenuation in centibels.
    ///
    /// The format clips the generator plus every record at 144 dB and does not
    /// allow the sum to go below none, so the record's own contribution is
    /// bounded by the room the generator leaves. A zone with no record gets
    /// exactly 1.0, which keeps the unmodulated render bit-identical.
    pub fn attenuation_gain(&self, zone_cb: f32) -> f32 {
        let extra = (zone_cb + self.attenuation_cb).clamp(0.0, MAX_ATTENUATION_CB) - zone_cb;
        centibels_to_gain(extra)
    }

    /// True when the filter cutoff moves with time or with a controller.
    pub fn modulates_filter(&self) -> bool {
        self.mod_lfo_filter_cents != 0.0
            || self.mod_env_filter_cents != 0.0
            || self.filter_offset_cents != 0.0
    }
}

/// A decoded `pmod`/`imod` record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoded {
    /// Read and modelled.
    Modulator(Modulator),
    /// A terminal record: the end of a zone's list.
    Terminal,
    /// Structurally a modulator, outside the modelled subset.
    Unsupported,
}

/// Decode one modulator record (10 bytes).
pub fn decode(record: &[u8]) -> Decoded {
    if record.len() < MODULATOR_RECORD_SIZE {
        return Decoded::Terminal;
    }
    let source_word = u16::from_le_bytes([record[0], record[1]]);
    let destination = u16::from_le_bytes([record[2], record[3]]);
    let amount = i16::from_le_bytes([record[4], record[5]]);
    let amount_source = u16::from_le_bytes([record[6], record[7]]);
    let transform = u16::from_le_bytes([record[8], record[9]]);
    if source_word == 0 && destination == 0 && amount == 0 && amount_source == 0 && transform == 0 {
        return Decoded::Terminal;
    }
    let Some(destination) = Destination::from_opcode(destination) else {
        return Decoded::Unsupported;
    };
    let Some(source) = decode_source(source_word) else {
        return Decoded::Unsupported;
    };
    let amount_source = match amount_source {
        0 => None,
        word => {
            let Some(source) = decode_source(word) else {
                return Decoded::Unsupported;
            };
            Some(AmountSource {
                source,
                direction: if word & 0x0100 != 0 {
                    Direction::Negative
                } else {
                    Direction::Positive
                },
                polarity: if word & 0x0200 != 0 {
                    Polarity::Bipolar
                } else {
                    Polarity::Unipolar
                },
            })
        }
    };
    let transform = match transform {
        0 => Transform::Linear,
        1 => Transform::Concave,
        2 => Transform::Convex,
        3 => Transform::Switch,
        _ => return Decoded::Unsupported,
    };
    Decoded::Modulator(Modulator {
        source,
        destination,
        direction: if source_word & 0x0100 != 0 {
            Direction::Negative
        } else {
            Direction::Positive
        },
        polarity: if source_word & 0x0200 != 0 {
            Polarity::Bipolar
        } else {
            Polarity::Unipolar
        },
        transform,
        amount: f32::from(amount),
        amount_source,
    })
}

/// Source word to a modelled source; `None` for an unassigned enumerator (some
/// banks use those for internal modulators).
///
/// The general-controller indices are the format's, not MIDI status bytes:
/// 0x00 no controller, 0x02 velocity, 0x03 key number, 0x0A poly pressure,
/// 0x0D channel pressure, 0x0E pitch wheel, 0x10 pitch-wheel sensitivity. Bit 7
/// turns the low seven bits into a MIDI controller number.
fn decode_source(word: u16) -> Option<ModSource> {
    if word & 0x80 != 0 {
        return Some(ModSource::MidiController((word & 0x7f) as u8));
    }
    Some(match word & 0x7f {
        0x00 => ModSource::None,
        0x02 => ModSource::Velocity,
        0x03 => ModSource::Keynum,
        0x0a => ModSource::PolyPressure,
        0x0d => ModSource::ChannelPressure,
        0x0e => ModSource::PitchWheel,
        0x10 => ModSource::PitchWheelSensitivity,
        _ => return None,
    })
}

// ============================================================================
// LFO
// ============================================================================

/// Phase bits used to index the sine table.
const SINE_BITS: u32 = 10;
const SINE_SIZE: usize = 1 << SINE_BITS;
const SINE_TABLE: [f32; SINE_SIZE + 1] = build_sine_table();

/// Cents spanned by the pitch-ratio table (one octave each way).
const RATIO_SPAN_CENTS: f32 = 1200.0;
const RATIO_SIZE: usize = 1024;
const RATIO_TABLE: [f32; RATIO_SIZE + 1] = build_ratio_table();

/// 2^n for n in -10..=10, the octave fold of a pitch offset up to 12000 cents.
const OCTAVE_RATIOS: [f32; 21] = build_octave_ratios();

/// The volume-LFO gain table: one entry per centibel, -960..=960.
const GAIN_TABLE_LOW: i32 = -960;
const GAIN_TABLE_SIZE: usize = 1921;
const GAIN_TABLE: [f32; GAIN_TABLE_SIZE] = build_gain_table();

const TAU: f64 = std::f64::consts::TAU;
const PI: f64 = std::f64::consts::PI;

/// Sine of an angle, folded into the first quadrant before the series.
const fn const_sin(angle: f64) -> f64 {
    let mut x = angle;
    while x >= TAU {
        x -= TAU;
    }
    if x < 0.0 {
        x = -x;
    }
    let mut sign = 1.0;
    if x > PI {
        x -= PI;
        sign = -1.0;
    }
    if x > PI * 0.5 {
        x = PI - x;
    }
    let mut sum = 0.0f64;
    let mut term = x;
    let mut k = 1.0f64;
    while k < 16.0 {
        sum += term;
        term = -term * x * x / ((k + 1.0) * (k + 2.0));
        k += 2.0;
    }
    sign * sum
}

const fn build_sine_table() -> [f32; SINE_SIZE + 1] {
    let mut table = [0.0f32; SINE_SIZE + 1];
    let mut index = 0;
    while index <= SINE_SIZE {
        table[index] = const_sin(TAU * index as f64 / SINE_SIZE as f64) as f32;
        index += 1;
    }
    table
}

/// `2^exponent` for |exponent| up to 16, as a power of two times a series.
const fn const_exp2(exponent: f64) -> f64 {
    let mut remainder = exponent;
    let mut scale = 1.0f64;
    while remainder > 0.5 {
        remainder -= 1.0;
        scale *= 2.0;
    }
    while remainder < -0.5 {
        remainder += 1.0;
        scale *= 0.5;
    }
    let x = remainder * std::f64::consts::LN_2;
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    let mut k = 1.0f64;
    while k < 18.0 {
        term = term * x / k;
        sum += term;
        k += 1.0;
    }
    sum * scale
}

/// 2^(cents / 1200) across [-RATIO_SPAN_CENTS, RATIO_SPAN_CENTS].
const fn build_ratio_table() -> [f32; RATIO_SIZE + 1] {
    let mut table = [1.0f32; RATIO_SIZE + 1];
    let span = RATIO_SPAN_CENTS as f64;
    let mut index = 0;
    while index <= RATIO_SIZE {
        let cents = -span + 2.0 * span * index as f64 / RATIO_SIZE as f64;
        table[index] = const_exp2(cents / 1200.0) as f32;
        index += 1;
    }
    table
}

const fn build_octave_ratios() -> [f32; 21] {
    let mut table = [1.0f32; 21];
    let mut index = 0;
    while index <= 20 {
        let exponent = index as i32 - 10;
        let mut value = 1.0f64;
        let mut step = 0;
        while step < exponent.unsigned_abs() {
            if exponent >= 0 {
                value *= 2.0;
            } else {
                value *= 0.5;
            }
            step += 1;
        }
        table[index] = value as f32;
        index += 1;
    }
    table
}

/// 10^(-centibels / 200) — the volume LFO's gain, one entry per centibel.
const fn build_gain_table() -> [f32; GAIN_TABLE_SIZE] {
    let mut table = [1.0f32; GAIN_TABLE_SIZE];
    let mut index = 0;
    while index < GAIN_TABLE_SIZE {
        let centibels = (index as i32 + GAIN_TABLE_LOW) as f64;
        // log2(10) / 200, so 2^(-cb * ln(10) / (200 * ln(2))) = 10^(-cb/200).
        table[index] = const_exp2(-centibels * 0.016_609_640_170_968_12) as f32;
        index += 1;
    }
    table
}

/// Interpolated sine of a 32-bit phase.
fn sine(phase: u32) -> f32 {
    let index = (phase >> (32 - SINE_BITS)) as usize;
    let fraction = (phase & ((1 << SINE_BITS) - 1)) as f32 * (1.0 / SINE_SIZE as f32);
    let low = SINE_TABLE[index];
    low + (SINE_TABLE[index + 1] - low) * fraction
}

/// Frequency multiplier for a pitch offset in cents, clamped to ±12000.
pub fn cents_to_ratio(cents: f32) -> f32 {
    let cents = cents.clamp(-MAX_PITCH_MODULATION_CENTS, MAX_PITCH_MODULATION_CENTS);
    // Fold whole octaves away so the table keeps its accuracy over two decades.
    let octaves = (cents / 1200.0).round() as i32;
    let remainder = cents - octaves as f32 * 1200.0;
    let position = (remainder + RATIO_SPAN_CENTS) * (RATIO_SIZE as f32 / (2.0 * RATIO_SPAN_CENTS));
    let index = (position as usize).min(RATIO_SIZE - 1);
    let fraction = position - index as f32;
    let ratio = RATIO_TABLE[index] + (RATIO_TABLE[index + 1] - RATIO_TABLE[index]) * fraction;
    ratio * OCTAVE_RATIOS[(octaves + 10) as usize]
}

/// Linear gain for an attenuation in centibels, from the volume-LFO table.
pub fn centibels_to_gain(centibels: f32) -> f32 {
    let highest = (GAIN_TABLE_LOW + GAIN_TABLE_SIZE as i32 - 1) as f32;
    let position = centibels.clamp(GAIN_TABLE_LOW as f32, highest) - GAIN_TABLE_LOW as f32;
    let index = (position as usize).min(GAIN_TABLE_SIZE - 2);
    let fraction = position - index as f32;
    GAIN_TABLE[index] + (GAIN_TABLE[index + 1] - GAIN_TABLE[index]) * fraction
}

/// Cutoff in hertz for an absolute-cent cutoff, clamped to the legal range.
///
/// The formula matches the one `Zone::filter_cutoff_hz` has always used, so an
/// unmodulated filter opens on exactly the same coefficients as before.
pub fn cents_to_hz(cents: f32) -> f64 {
    let cents = cents.clamp(MIN_FILTER_FC_CENTS, MAX_FILTER_FC_CENTS);
    8.176 * 2f64.powf(f64::from(cents) / 1200.0)
}

/// One voice's LFO: phase, rate and delay.
///
/// State is per voice. The LFO runs from note-on (after its delay) whether or
/// not a depth is nonzero, which is what automation expects; the depth is a
/// separate per-voice value refreshed from the channel's controller state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lfo {
    phase: u32,
    /// Phase increment per sample.
    step: u32,
    delay_left: u32,
}

impl Lfo {
    /// LFO from a frequency and a delay in seconds.
    pub fn new(frequency_hz: f32, delay_seconds: f64, sample_rate: u32) -> Self {
        let rate = f64::from(frequency_hz.clamp(0.1, 100.0)) / f64::from(sample_rate.max(1));
        let step = (rate * 4_294_967_296.0) as u32;
        let delay_left = (delay_seconds.max(0.0) * f64::from(sample_rate.max(1))) as u32;
        Self {
            phase: 0,
            step,
            delay_left,
        }
    }

    /// Sine in [-1, 1] for the current sample, advancing the LFO.
    pub fn tick(&mut self) -> f32 {
        if self.delay_left > 0 {
            self.delay_left -= 1;
            return 0.0;
        }
        let value = sine(self.phase);
        self.phase = self.phase.wrapping_add(self.step);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(controllers: &[u8; 128]) -> ModInput<'_> {
        ModInput::new(controllers, 0, 60, 64, 0.0)
    }

    #[test]
    fn the_default_modulators_open_the_vibrato_with_cc1_and_pressure() {
        let mut controllers = [0u8; 128];
        assert_eq!(DEFAULT_MODULATORS[0].value(&input(&controllers)), 0.0);
        controllers[1] = 127;
        assert!((DEFAULT_MODULATORS[0].value(&input(&controllers)) - 50.0).abs() < 1e-6);
        controllers[1] = 64;
        assert!(
            (DEFAULT_MODULATORS[0].value(&input(&controllers)) - 50.0 * 64.0 / 127.0).abs() < 1e-6
        );
        // Channel pressure is the second default modulator.
        let pressure = ModInput::new(&[0u8; 128], 127, 60, 64, 0.0);
        assert!((DEFAULT_MODULATORS[1].value(&pressure) - 50.0).abs() < 1e-6);
        // Both drive the vibrato's pitch depth.
        assert_eq!(
            DEFAULT_MODULATORS[0].destination,
            Destination::VibLfoToPitch
        );
    }

    #[test]
    fn direction_polarity_and_switch_scale_the_contribution() {
        let mut controllers = [0u8; 128];
        let base = Modulator {
            source: ModSource::MidiController(2),
            destination: Destination::ModLfoToPitch,
            direction: Direction::Positive,
            polarity: Polarity::Unipolar,
            transform: Transform::Linear,
            amount: 150.0,
            amount_source: None,
        };
        let value =
            |modulator: &Modulator, controllers: &[u8; 128]| modulator.value(&input(controllers));

        controllers[2] = 127;
        assert!((value(&base, &controllers) - 150.0).abs() < 1e-6);
        // A negative unipolar source is the complement: full source is zero.
        let mut negative = base;
        negative.direction = Direction::Negative;
        assert!(value(&negative, &controllers).abs() < 1e-6);
        controllers[2] = 0;
        assert!((value(&negative, &controllers) - 150.0).abs() < 1e-6);
        // The switch ignores the amount until the source passes halfway.
        let mut switch = negative;
        switch.transform = Transform::Switch;
        assert!((value(&switch, &controllers) - 150.0).abs() < 1e-6);
        controllers[2] = 127;
        assert_eq!(value(&switch, &controllers), 0.0);
        // Bipolar reads the whole -1..1 range from the same 0..1 source.
        let mut bipolar = base;
        bipolar.polarity = Polarity::Bipolar;
        controllers[2] = 64;
        assert!(value(&bipolar, &controllers).abs() < 2.0);
        controllers[2] = 127;
        assert!((value(&bipolar, &controllers) - 150.0).abs() < 1e-6);
        controllers[2] = 0;
        assert!((value(&bipolar, &controllers) + 150.0).abs() < 1e-6);
        // A negative bipolar source mirrors it: 1 - 2x.
        let mut mirrored = bipolar;
        mirrored.direction = Direction::Negative;
        controllers[2] = 127;
        assert!((value(&mirrored, &controllers) + 150.0).abs() < 1e-6);
        controllers[2] = 0;
        assert!((value(&mirrored, &controllers) - 150.0).abs() < 1e-6);
    }

    #[test]
    fn records_outside_the_subset_are_reported_not_applied() {
        let mut record = [0u8; MODULATOR_RECORD_SIZE];
        // CC1 -> vibLfoToPitch, 50 cents, linear.
        record[..2].copy_from_slice(&(0x80u16 | 1).to_le_bytes());
        record[2..4].copy_from_slice(&6u16.to_le_bytes());
        record[4..6].copy_from_slice(&50i16.to_le_bytes());
        assert!(matches!(decode(&record), Decoded::Modulator(m)
                if m.amount == 50.0 && m.destination == Destination::VibLfoToPitch));
        // Every modelled destination decodes.
        for (opcode, destination) in [
            (5u16, Destination::ModLfoToPitch),
            (6, Destination::VibLfoToPitch),
            (7, Destination::ModEnvToPitch),
            (8, Destination::InitialFilterFc),
            (9, Destination::InitialFilterQ),
            (10, Destination::ModLfoToFilterFc),
            (11, Destination::ModEnvToFilterFc),
            (13, Destination::ModLfoToVolume),
            (15, Destination::ChorusSend),
            (16, Destination::ReverbSend),
            (17, Destination::Pan),
            (26, Destination::AttackModEnv),
            (28, Destination::DecayModEnv),
            (29, Destination::SustainModEnv),
            (30, Destination::ReleaseModEnv),
            (34, Destination::AttackVolEnv),
            (36, Destination::DecayVolEnv),
            (38, Destination::ReleaseVolEnv),
            (48, Destination::InitialAttenuation),
        ] {
            record[2..4].copy_from_slice(&opcode.to_le_bytes());
            assert!(
                matches!(decode(&record), Decoded::Modulator(m) if m.destination == destination),
                "opcode {opcode}"
            );
        }
        // A terminal record ends a list.
        assert_eq!(decode(&[0u8; MODULATOR_RECORD_SIZE]), Decoded::Terminal);
        // Back to the base record's destination before reusing it.
        record[2..4].copy_from_slice(&16u16.to_le_bytes());
        // The four transforms decode; a fifth is outside the format.
        for (value, expected) in [
            (0u16, Transform::Linear),
            (1, Transform::Concave),
            (2, Transform::Convex),
            (3, Transform::Switch),
        ] {
            let mut transformed = record;
            transformed[8..10].copy_from_slice(&value.to_le_bytes());
            assert!(
                matches!(decode(&transformed), Decoded::Modulator(m) if m.transform == expected),
                "transform {value}"
            );
        }
        let mut fifth = record;
        fifth[8] = 4;
        assert_eq!(decode(&fifth), Decoded::Unsupported);
        // A secondary amount source decodes with its own direction and polarity.
        let mut secondary = record;
        secondary[6..8].copy_from_slice(&(0x80u16 | 3 | 0x0100).to_le_bytes());
        let decoded = decode(&secondary);
        assert_eq!(
            decoded,
            Decoded::Modulator(Modulator {
                source: ModSource::MidiController(1),
                destination: Destination::ReverbSend,
                direction: Direction::Positive,
                polarity: Polarity::Unipolar,
                transform: Transform::Linear,
                amount: 50.0,
                amount_source: Some(AmountSource {
                    source: ModSource::MidiController(3),
                    direction: Direction::Negative,
                    polarity: Polarity::Unipolar,
                }),
            })
        );
        // An unassigned amount source is outside the subset.
        let mut bad_secondary = record;
        bad_secondary[6..8].copy_from_slice(&0x05u16.to_le_bytes());
        assert_eq!(decode(&bad_secondary), Decoded::Unsupported);
        let mut other = record;
        other[2..4].copy_from_slice(&22u16.to_le_bytes());
        assert_eq!(decode(&other), Decoded::Unsupported);
        // Poly pressure is a modelled source (index 0x0A).
        let mut poly = record;
        poly[..2].copy_from_slice(&0x0Au16.to_le_bytes());
        assert!(
            matches!(decode(&poly), Decoded::Modulator(m) if m.source == ModSource::PolyPressure)
        );
        // The format's general-controller indices decode as their sources.
        for (word, expected) in [
            (0x02u16, ModSource::Velocity),
            (0x03, ModSource::Keynum),
            (0x0A, ModSource::PolyPressure),
            (0x0D, ModSource::ChannelPressure),
            (0x0E, ModSource::PitchWheel),
            (0x10, ModSource::PitchWheelSensitivity),
            (0x00, ModSource::None),
            (0x80 | 64, ModSource::MidiController(64)),
        ] {
            let mut source = record;
            source[..2].copy_from_slice(&word.to_le_bytes());
            assert!(
                matches!(decode(&source), Decoded::Modulator(m) if m.source == expected),
                "source word {word:#06x}"
            );
        }
    }

    #[test]
    fn destination_sums_keep_their_unit() {
        let mut modulation =
            Modulation::from_generators(50.0, 100.0, 200.0, 300.0, 400.0, -600.0, 0.0, 0.0);
        modulation.add(Destination::VibLfoToPitch, 10.0);
        modulation.add(Destination::ModLfoToPitch, 20.0);
        modulation.add(Destination::ModEnvToPitch, 30.0);
        modulation.add(Destination::InitialFilterFc, 40.0);
        modulation.add(Destination::InitialFilterQ, 110.0);
        modulation.add(Destination::ModLfoToFilterFc, 50.0);
        modulation.add(Destination::ModEnvToFilterFc, 60.0);
        modulation.add(Destination::ModLfoToVolume, 70.0);
        modulation.add(Destination::AttackModEnv, 120.0);
        modulation.add(Destination::DecayModEnv, 130.0);
        modulation.add(Destination::SustainModEnv, 140.0);
        modulation.add(Destination::ReleaseModEnv, 150.0);
        modulation.add(Destination::AttackVolEnv, 160.0);
        modulation.add(Destination::DecayVolEnv, 170.0);
        modulation.add(Destination::ReleaseVolEnv, 180.0);
        modulation.add(Destination::InitialAttenuation, 100.0);
        modulation.add(Destination::Pan, 250.0);
        modulation.add(Destination::ChorusSend, 80.0);
        modulation.add(Destination::ReverbSend, 90.0);
        assert_eq!(modulation.vibrato_cents, 60.0);
        assert_eq!(modulation.mod_lfo_pitch_cents, 120.0);
        assert_eq!(modulation.mod_env_pitch_cents, 230.0);
        assert_eq!(modulation.filter_offset_cents, 40.0);
        assert_eq!(modulation.filter_q_cb, 110.0);
        assert_eq!(modulation.mod_lfo_filter_cents, 350.0);
        assert_eq!(modulation.mod_env_filter_cents, 460.0);
        assert_eq!(modulation.mod_lfo_volume_cb, -530.0);
        assert_eq!(modulation.attack_mod_env_tc, 120.0);
        assert_eq!(modulation.decay_mod_env_tc, 130.0);
        assert_eq!(modulation.sustain_mod_env_cb, 140.0);
        assert_eq!(modulation.release_mod_env_tc, 150.0);
        assert_eq!(modulation.attack_vol_env_tc, 160.0);
        assert_eq!(modulation.decay_vol_env_tc, 170.0);
        assert_eq!(modulation.release_vol_env_tc, 180.0);
        assert_eq!(modulation.attenuation_cb, 100.0);
        assert_eq!(modulation.pan, 250.0);
        assert_eq!(modulation.chorus_send, 80.0);
        assert_eq!(modulation.reverb_send, 90.0);
        assert_eq!(modulation.chorus_gain(), 0.08);
        assert_eq!(modulation.reverb_gain(), 0.09);
        // The gain stops at a full send and never goes negative.
        let over = Modulation {
            chorus_send: 1500.0,
            reverb_send: -100.0,
            ..Default::default()
        };
        assert_eq!(over.chorus_gain(), 1.0);
        assert_eq!(over.reverb_gain(), 0.0);
        assert!(modulation.modulates_filter());
        assert!(!Modulation::default().modulates_filter());
    }

    #[test]
    fn an_attenuation_record_is_centibels_on_top_of_the_zone_generator() {
        // No record is exactly unity, so the unmodulated render cannot move.
        let none = Modulation::default();
        assert_eq!(none.attenuation_gain(0.0), 1.0);
        assert_eq!(none.attenuation_gain(200.0), 1.0);
        // A 200 cB record is 20 dB of extra attenuation.
        let modulation = Modulation {
            attenuation_cb: 200.0,
            ..Default::default()
        };
        let gain = f64::from(modulation.attenuation_gain(0.0));
        assert!((gain - 0.1).abs() < 1e-5, "{gain}");
        // The format clips the generator plus the records at 144 dB, so a
        // generator that already sits at 130 dB leaves room for 14.
        let near_ceiling = modulation.attenuation_gain(1300.0);
        let expected = f64::from(centibels_to_gain(140.0));
        assert!((f64::from(near_ceiling) - expected).abs() < 1e-5);
        // A negative record may only undo what the generator applied: with a
        // 100 cB generator, -400 cB leaves the voice at unity.
        let boost = Modulation {
            attenuation_cb: -400.0,
            ..Default::default()
        };
        let gain = f64::from(boost.attenuation_gain(100.0));
        assert!((gain - f64::from(centibels_to_gain(-100.0))).abs() < 1e-5);
        assert!(
            (f64::from(boost.attenuation_gain(100.0)) * f64::from(centibels_to_gain(100.0)) - 1.0)
                .abs()
                < 1e-3
        );
        let gain = f64::from(boost.attenuation_gain(600.0));
        assert!((gain - f64::from(centibels_to_gain(-400.0))).abs() < 1e-5);
    }

    #[test]
    fn the_concave_curve_turns_velocity_into_the_squared_level_law() {
        // The format's velocity-to-attenuation default modulator carries 960
        // units with the concave transform; 960 units of attenuation is 96 dB,
        // so the transform has to make that exactly the measured (v/127)^2 law.
        for velocity in [1u8, 16, 32, 64, 96, 127] {
            let x = f32::from(velocity) / 127.0;
            let attenuation_cb = 960.0 * Transform::Concave.apply(x);
            let gain = 10f32.powf(-attenuation_cb / 200.0);
            assert!(
                (gain / (x * x) - 1.0).abs() < 0.01,
                "velocity {velocity}: gain {gain} vs {}",
                x * x
            );
        }
    }

    #[test]
    fn the_velocity_filter_default_is_the_formats_curve() {
        // 2400 cents of concave curve: zero at full velocity, two octaves down
        // at zero velocity, and the format's gentle -298 cents at velocity 64.
        let value = |velocity: u8| {
            let input = ModInput::new(&[0u8; 128], 0, 60, velocity, 0.0);
            DEFAULT_MODULATORS
                .iter()
                .filter(|modulator| modulator.destination == Destination::InitialFilterFc)
                .map(|modulator| modulator.value(&input))
                .sum::<f32>()
        };
        assert!(value(127).abs() < 1e-4, "full velocity: {}", value(127));
        assert!(
            (value(0) + 2400.0).abs() < 1e-3,
            "zero velocity: {}",
            value(0)
        );
        assert!(
            (value(64) + 297.7).abs() < 0.5,
            "velocity 64: {}",
            value(64)
        );
        // Monotone: every step up opens the cutoff.
        let mut previous = value(0);
        for velocity in 1..=127u8 {
            let current = value(velocity);
            assert!(current >= previous - 1e-4);
            previous = current;
        }
    }

    #[test]
    fn poly_pressure_reads_the_voice_own_note() {
        let mut modulator = Modulator {
            source: ModSource::PolyPressure,
            destination: Destination::InitialFilterFc,
            direction: Direction::Positive,
            polarity: Polarity::Unipolar,
            transform: Transform::Linear,
            amount: 2400.0,
            amount_source: None,
        };
        let mut input = ModInput::new(&[0u8; 128], 0, 60, 100, 0.0);
        assert_eq!(modulator.value(&input), 0.0);
        input.poly_pressure = 64;
        assert!((modulator.value(&input) - 2400.0 * 64.0 / 127.0).abs() < 1e-3);
        input.poly_pressure = 127;
        assert!((modulator.value(&input) - 2400.0).abs() < 1e-3);
        // Channel pressure is a different source and does not feed it.
        let channel = ModInput::new(&[0u8; 128], 127, 60, 100, 0.0);
        assert_eq!(modulator.value(&channel), 0.0);
        // The other direction closes the cutoff.
        modulator.direction = Direction::Negative;
        assert!(modulator.value(&input).abs() < 1e-3);
    }

    #[test]
    fn the_transforms_hit_their_endpoints_and_stay_monotone() {
        // Concave reads as the attenuation a source asks for: full scale is
        // none, and silence is all of it. Convex is its complement.
        for (transform, starts, ends) in [
            (Transform::Linear, 0.0, 1.0),
            (Transform::Concave, 1.0, 0.0),
            (Transform::Convex, 0.0, 1.0),
        ] {
            assert!(
                (transform.apply(0.0) - starts).abs() < 1e-6,
                "{transform:?}"
            );
            assert!((transform.apply(1.0) - ends).abs() < 1e-6, "{transform:?}");
            let mut previous = transform.apply(0.0);
            for step in 1..=100 {
                let value = transform.apply(step as f32 / 100.0);
                let rising = if ends > starts {
                    value >= previous - 1e-6
                } else {
                    value <= previous + 1e-6
                };
                assert!(rising, "{transform:?} at {step}");
                assert!((0.0..=1.0).contains(&value));
                previous = value;
            }
        }

        // The two curves are complements: a quarter of the way up one is three
        // quarters of the way up the other (0.2509 is the exact concave value).
        assert!((Transform::Concave.apply(0.25) - 0.2509).abs() < 1e-3);
        assert!((Transform::Convex.apply(0.25) - 0.7491).abs() < 1e-3);
        assert_eq!(Transform::Switch.apply(0.49), 0.0);
        assert_eq!(Transform::Switch.apply(0.5), 1.0);
    }

    #[test]
    fn the_secondary_amount_source_scales_the_amount() {
        let mut controllers = [0u8; 128];
        let mut modulator = Modulator {
            source: ModSource::MidiController(2),
            destination: Destination::ModEnvToPitch,
            direction: Direction::Positive,
            polarity: Polarity::Unipolar,
            transform: Transform::Linear,
            amount: 1200.0,
            amount_source: Some(AmountSource {
                source: ModSource::MidiController(3),
                direction: Direction::Positive,
                polarity: Polarity::Unipolar,
            }),
        };
        controllers[2] = 127;
        controllers[3] = 64;
        let scaled = modulator.value(&input(&controllers));
        assert!((scaled - 1200.0 * 64.0 / 127.0).abs() < 1e-3, "{scaled}");
        // A second source at zero silences the contribution.
        controllers[3] = 0;
        assert_eq!(modulator.value(&input(&controllers)), 0.0);
        // A negative unipolar second source is its complement.
        modulator.amount_source = Some(AmountSource {
            source: ModSource::MidiController(3),
            direction: Direction::Negative,
            polarity: Polarity::Unipolar,
        });
        assert!((modulator.value(&input(&controllers)) - 1200.0).abs() < 1e-3);
        controllers[3] = 127;
        assert!(modulator.value(&input(&controllers)).abs() < 1e-3);
        // A bipolar second source reads the whole range, so half is zero.
        modulator.amount_source = Some(AmountSource {
            source: ModSource::MidiController(3),
            direction: Direction::Positive,
            polarity: Polarity::Bipolar,
        });
        controllers[3] = 64;
        assert!(modulator.value(&input(&controllers)).abs() < 1200.0 * 2.0 / 127.0);
        controllers[3] = 0;
        assert!((modulator.value(&input(&controllers)) + 1200.0).abs() < 1e-3);
    }

    #[test]
    fn the_pitch_wheel_and_its_sensitivity_read_their_own_ranges() {
        // The wheel's raw value is its position, 0.5 at centre; the sensitivity
        // is the player's fixed bend range in semitones over 127.
        let centre = ModInput::new(&[0u8; 128], 0, 60, 64, 0.0);
        let mut wheel = Modulator {
            source: ModSource::PitchWheel,
            destination: Destination::ModEnvToPitch,
            direction: Direction::Positive,
            polarity: Polarity::Unipolar,
            transform: Transform::Linear,
            amount: 1200.0,
            amount_source: None,
        };
        assert!((wheel.value(&centre) - 600.0).abs() < 1e-3);
        let up = ModInput::new(&[0u8; 128], 0, 60, 64, 1.0);
        assert!((wheel.value(&up) - 1200.0).abs() < 1e-3);
        wheel.polarity = Polarity::Bipolar;
        assert!(wheel.value(&centre).abs() < 1e-3);
        assert!((wheel.value(&up) - 1200.0).abs() < 1e-3);
        wheel.source = ModSource::PitchWheelSensitivity;
        wheel.polarity = Polarity::Unipolar;
        let expected = 1200.0 * super::super::player::BEND_RANGE_SEMITONES as f32 / 127.0;
        assert!((wheel.value(&centre) - expected).abs() < 1e-3);
        // The format's "no controller" source is a constant at full scale.
        wheel.source = ModSource::None;
        assert!((wheel.value(&centre) - 1200.0).abs() < 1e-3);
    }

    #[test]
    fn the_send_controllers_are_the_formats_default_modulators() {
        let mut controllers = [0u8; 128];
        let send = |controllers: &[u8; 128], destination: Destination| -> f32 {
            DEFAULT_MODULATORS
                .iter()
                .filter(|modulator| modulator.destination == destination)
                .map(|modulator| modulator.value(&input(controllers)))
                .sum()
        };
        assert_eq!(send(&controllers, Destination::ReverbSend), 0.0);
        assert_eq!(send(&controllers, Destination::ChorusSend), 0.0);
        controllers[91] = 127;
        controllers[93] = 64;
        assert!((send(&controllers, Destination::ReverbSend) - 200.0).abs() < 1e-6);
        assert!((send(&controllers, Destination::ChorusSend) - 200.0 * 64.0 / 127.0).abs() < 1e-6);
        // They add to a zone's own generator rather than replacing it.
        let mut modulation =
            Modulation::from_generators(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 100.0, 100.0);
        for modulator in DEFAULT_MODULATORS.iter() {
            modulation.add(modulator.destination, modulator.value(&input(&controllers)));
        }
        assert!((modulation.reverb_send - 300.0).abs() < 1e-6);
        assert!((modulation.chorus_gain() - 0.2008).abs() < 1e-3);
    }

    #[test]
    fn the_sine_and_ratio_tables_are_accurate() {
        for quarter in 0..8 {
            let phase = quarter * (u32::MAX / 8) + 1;
            let expected = const_sin(f64::from(phase) / 4_294_967_296.0 * TAU);
            assert!(
                (f64::from(sine(phase)) - expected).abs() < 1e-4,
                "phase {phase}"
            );
        }
        for cents in [
            -12_000.0f32,
            -3600.0,
            -1200.0,
            -700.0,
            -50.0,
            0.0,
            50.0,
            700.0,
            1200.0,
            3600.0,
            12_000.0,
        ] {
            let expected = 2.0f64.powf(f64::from(cents) / 1200.0);
            let actual = f64::from(cents_to_ratio(cents));
            assert!(
                (actual / expected - 1.0).abs() < 1e-4,
                "{cents} cents: {actual} vs {expected}"
            );
        }
        // Beyond the format's clamp the ratio stops at ten octaves.
        assert!((cents_to_ratio(50_000.0) - 1024.0).abs() < 1e-3);
        assert!((cents_to_ratio(-50_000.0) - 1.0 / 1024.0).abs() < 1e-6);
    }

    #[test]
    fn centibels_map_to_ten_to_the_minus_cb_over_two_hundred() {
        for centibels in [
            -960.0f32, -600.0, -100.0, -10.0, 0.0, 10.0, 100.0, 600.0, 960.0,
        ] {
            let expected = 10.0f64.powf(-f64::from(centibels) / 200.0);
            let actual = f64::from(centibels_to_gain(centibels));
            assert!(
                (actual / expected - 1.0).abs() < 1e-4,
                "{centibels} cB: {actual} vs {expected}"
            );
        }
        // Between table entries the interpolation stays within the same bound.
        let expected = 10.0f64.powf(-0.5 / 200.0);
        assert!((f64::from(centibels_to_gain(0.5)) / expected - 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_cutoff_law_keeps_the_legal_range() {
        assert!((cents_to_hz(13_500.0) - 19_913.0).abs() < 5.0);
        assert!((cents_to_hz(6900.0) - 440.0).abs() < 0.5);
        // Outside the range the cutoff clamps instead of running away.
        assert_eq!(cents_to_hz(0.0), cents_to_hz(MIN_FILTER_FC_CENTS));
        assert_eq!(cents_to_hz(20_000.0), cents_to_hz(MAX_FILTER_FC_CENTS));
        assert!((cents_to_hz(MIN_FILTER_FC_CENTS) - 19.4).abs() < 0.2);
    }

    #[test]
    fn the_lfo_runs_at_its_rate_after_its_delay() {
        let sample_rate = 48_000;
        let mut lfo = Lfo::new(8.0, 0.1, sample_rate);
        for _ in 0..4_800 {
            assert_eq!(lfo.tick(), 0.0);
        }
        // After the delay the sine rises from zero: no jump on the first sample.
        assert!(lfo.tick().abs() < 0.02);
        let mut peak: f32 = 0.0;
        let mut crossings = 0;
        let mut previous = 0.0f32;
        for _ in 0..(sample_rate * 2) {
            let value = lfo.tick();
            if previous <= 0.0 && value > 0.0 {
                crossings += 1;
            }
            previous = value;
            peak = peak.max(value);
        }
        assert!((peak - 1.0).abs() < 0.01, "peak {peak}");
        // Two seconds at 8 Hz is sixteen cycles.
        assert!((15..=17).contains(&crossings), "crossings {crossings}");
    }

    #[test]
    fn a_zero_depth_lfo_still_runs() {
        // The oscillator phase is not gated on the depth, so a wheel move that
        // arrives mid-note joins a running LFO instead of restarting it.
        let mut lfo = Lfo::new(8.0, 0.0, 48_000);
        let mut first_quarter = 0.0f32;
        for _ in 0..1_500 {
            first_quarter = lfo.tick();
        }
        assert!(first_quarter.abs() > 0.5, "{first_quarter}");
    }
}
