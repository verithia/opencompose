//! SoundFont 2 generator opcodes and the per-zone generator set.
//!
//! Only the generators that decide audibility and modulation are modelled.
//! Unknown opcodes are dropped at parse time, which is why [`GenSet`] is
//! sparse.

/// Number of opcode slots; opcodes above the last modelled one are ignored.
pub const OPCODE_COUNT: usize = 61;

/// Generator opcodes, named as in the specification's generator table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generator {
    StartAddrsOffset,
    EndAddrsOffset,
    StartLoopAddrsOffset,
    EndLoopAddrsOffset,
    StartAddrsCoarseOffset,
    ModLfoToPitch,
    VibLfoToPitch,
    ModEnvToPitch,
    InitialFilterFc,
    InitialFilterQ,
    ModLfoToFilterFc,
    ModEnvToFilterFc,
    EndAddrsCoarseOffset,
    ModLfoToVolume,
    ChorusSend,
    ReverbSend,
    DelayModLfo,
    FreqModLfo,
    DelayVibLfo,
    FreqVibLfo,
    DelayModEnv,
    AttackModEnv,
    HoldModEnv,
    DecayModEnv,
    SustainModEnv,
    ReleaseModEnv,
    KeynumToModEnvHold,
    KeynumToModEnvDecay,
    Pan,
    DelayVolEnv,
    AttackVolEnv,
    HoldVolEnv,
    DecayVolEnv,
    SustainVolEnv,
    ReleaseVolEnv,
    KeynumToVolEnvHold,
    KeynumToVolEnvDecay,
    Instrument,
    KeyRange,
    VelRange,
    StartLoopAddrsCoarseOffset,
    InitialAttenuation,
    EndLoopAddrsCoarseOffset,
    CoarseTune,
    FineTune,
    /// A fixed key number for the zone, whatever key triggered it.
    Keynum,
    /// A fixed velocity for the zone, whatever velocity triggered it.
    Velocity,
    SampleId,
    SampleModes,
    ScaleTuning,
    ExclusiveClass,
    OverridingRootKey,
}

impl Generator {
    /// Map a wire opcode to a modelled generator; `None` means "not used here".
    pub fn from_opcode(opcode: u16) -> Option<Self> {
        Some(match opcode {
            0 => Self::StartAddrsOffset,
            1 => Self::EndAddrsOffset,
            2 => Self::StartLoopAddrsOffset,
            3 => Self::EndLoopAddrsOffset,
            4 => Self::StartAddrsCoarseOffset,
            5 => Self::ModLfoToPitch,
            6 => Self::VibLfoToPitch,
            7 => Self::ModEnvToPitch,
            8 => Self::InitialFilterFc,
            9 => Self::InitialFilterQ,
            10 => Self::ModLfoToFilterFc,
            11 => Self::ModEnvToFilterFc,
            12 => Self::EndAddrsCoarseOffset,
            13 => Self::ModLfoToVolume,
            15 => Self::ChorusSend,
            16 => Self::ReverbSend,
            17 => Self::Pan,
            21 => Self::DelayModLfo,
            22 => Self::FreqModLfo,
            23 => Self::DelayVibLfo,
            24 => Self::FreqVibLfo,
            25 => Self::DelayModEnv,
            26 => Self::AttackModEnv,
            27 => Self::HoldModEnv,
            28 => Self::DecayModEnv,
            29 => Self::SustainModEnv,
            30 => Self::ReleaseModEnv,
            31 => Self::KeynumToModEnvHold,
            32 => Self::KeynumToModEnvDecay,
            33 => Self::DelayVolEnv,
            34 => Self::AttackVolEnv,
            35 => Self::HoldVolEnv,
            36 => Self::DecayVolEnv,
            37 => Self::SustainVolEnv,
            38 => Self::ReleaseVolEnv,
            39 => Self::KeynumToVolEnvHold,
            40 => Self::KeynumToVolEnvDecay,
            41 => Self::Instrument,
            43 => Self::KeyRange,
            44 => Self::VelRange,
            45 => Self::StartLoopAddrsCoarseOffset,
            48 => Self::InitialAttenuation,
            50 => Self::EndLoopAddrsCoarseOffset,
            46 => Self::Keynum,
            47 => Self::Velocity,
            51 => Self::CoarseTune,
            52 => Self::FineTune,
            53 => Self::SampleId,
            54 => Self::SampleModes,
            56 => Self::ScaleTuning,
            57 => Self::ExclusiveClass,
            58 => Self::OverridingRootKey,
            _ => return None,
        })
    }

    /// Specification opcode number.
    pub fn opcode(self) -> u16 {
        match self {
            Self::StartAddrsOffset => 0,
            Self::EndAddrsOffset => 1,
            Self::StartLoopAddrsOffset => 2,
            Self::EndLoopAddrsOffset => 3,
            Self::StartAddrsCoarseOffset => 4,
            Self::ModLfoToPitch => 5,
            Self::VibLfoToPitch => 6,
            Self::ModEnvToPitch => 7,
            Self::InitialFilterFc => 8,
            Self::InitialFilterQ => 9,
            Self::ModLfoToFilterFc => 10,
            Self::ModEnvToFilterFc => 11,
            Self::EndAddrsCoarseOffset => 12,
            Self::ModLfoToVolume => 13,
            Self::ChorusSend => 15,
            Self::ReverbSend => 16,
            Self::Pan => 17,
            Self::DelayModLfo => 21,
            Self::FreqModLfo => 22,
            Self::DelayVibLfo => 23,
            Self::FreqVibLfo => 24,
            Self::DelayModEnv => 25,
            Self::AttackModEnv => 26,
            Self::HoldModEnv => 27,
            Self::DecayModEnv => 28,
            Self::SustainModEnv => 29,
            Self::ReleaseModEnv => 30,
            Self::KeynumToModEnvHold => 31,
            Self::KeynumToModEnvDecay => 32,
            Self::DelayVolEnv => 33,
            Self::AttackVolEnv => 34,
            Self::HoldVolEnv => 35,
            Self::DecayVolEnv => 36,
            Self::SustainVolEnv => 37,
            Self::ReleaseVolEnv => 38,
            Self::KeynumToVolEnvHold => 39,
            Self::KeynumToVolEnvDecay => 40,
            Self::Instrument => 41,
            Self::KeyRange => 43,
            Self::VelRange => 44,
            Self::StartLoopAddrsCoarseOffset => 45,
            Self::InitialAttenuation => 48,
            Self::EndLoopAddrsCoarseOffset => 50,
            Self::Keynum => 46,
            Self::Velocity => 47,
            Self::CoarseTune => 51,
            Self::FineTune => 52,
            Self::SampleId => 53,
            Self::SampleModes => 54,
            Self::ScaleTuning => 56,
            Self::ExclusiveClass => 57,
            Self::OverridingRootKey => 58,
        }
    }

    /// Specification default, used when neither zone supplies the generator.
    pub fn default_value(self) -> i16 {
        match self {
            // -1 is the format's "use the note's own value" sentinel.
            Self::Keynum | Self::Velocity => -1,
            Self::StartAddrsOffset
            | Self::EndAddrsOffset
            | Self::StartLoopAddrsOffset
            | Self::EndLoopAddrsOffset
            | Self::StartAddrsCoarseOffset
            | Self::EndAddrsCoarseOffset
            | Self::StartLoopAddrsCoarseOffset
            | Self::EndLoopAddrsCoarseOffset
            | Self::CoarseTune
            | Self::FineTune
            | Self::Pan
            | Self::InitialAttenuation
            | Self::InitialFilterQ
            | Self::ModLfoToPitch
            | Self::VibLfoToPitch
            | Self::ModEnvToPitch
            | Self::ModLfoToFilterFc
            | Self::ModEnvToFilterFc
            | Self::ModLfoToVolume
            | Self::ChorusSend
            | Self::ReverbSend
            | Self::SustainVolEnv
            | Self::SustainModEnv
            | Self::KeynumToVolEnvHold
            | Self::KeynumToVolEnvDecay
            | Self::KeynumToModEnvHold
            | Self::KeynumToModEnvDecay
            | Self::SampleModes
            | Self::ExclusiveClass => 0,
            Self::DelayVolEnv
            | Self::DelayVibLfo
            | Self::HoldVolEnv
            | Self::KeyRange
            | Self::VelRange
            | Self::Instrument
            | Self::SampleId
            | Self::OverridingRootKey
            | Self::DelayModLfo
            | Self::DelayModEnv
            | Self::HoldModEnv => -32768,
            Self::AttackVolEnv
            | Self::DecayVolEnv
            | Self::ReleaseVolEnv
            | Self::AttackModEnv
            | Self::DecayModEnv
            | Self::ReleaseModEnv => -12000,
            Self::FreqVibLfo | Self::FreqModLfo => 0,
            Self::InitialFilterFc => 13500,
            Self::ScaleTuning => 100,
        }
    }

    /// Generators whose preset-level value is intersected, not added.
    pub fn is_range(self) -> bool {
        matches!(self, Self::KeyRange | Self::VelRange)
    }
}

/// A sparse generator set: only the opcodes a zone actually declares.
#[derive(Debug, Clone, Copy)]
pub struct GenSet {
    values: [Option<i16>; OPCODE_COUNT],
}

impl Default for GenSet {
    fn default() -> Self {
        Self {
            values: [None; OPCODE_COUNT],
        }
    }
}

impl GenSet {
    /// Insert an opcode; the first declaration in a zone wins.
    pub fn insert(&mut self, generator: Generator, amount: i16) {
        let slot = &mut self.values[generator.opcode() as usize];
        // The first declaration of an opcode in a zone wins (spec 8.5).
        slot.get_or_insert(amount);
    }

    /// Declared amount for an opcode.
    pub fn get(&self, generator: Generator) -> Option<i16> {
        self.values[generator.opcode() as usize]
    }

    /// Value with the specification default applied.
    pub fn value(&self, generator: Generator) -> i16 {
        self.get(generator)
            .unwrap_or_else(|| generator.default_value())
    }

    /// True when neither zone declared this generator.
    pub fn is_absent(&self, generator: Generator) -> bool {
        self.get(generator).is_none()
    }

    /// Copy every declared opcode over the receiver; the local zone wins.
    pub fn overlay(&mut self, local: &GenSet) {
        for (slot, value) in self.values.iter_mut().zip(local.values.iter()) {
            if value.is_some() {
                *slot = *value;
            }
        }
    }

    /// Add another zone's declared values, the preset-zone rule for numeric
    /// generators (spec 8.5: preset values are offsets from the instrument's).
    pub fn add_offsets(&mut self, offset: &GenSet) {
        for (slot, value) in self.values.iter_mut().zip(offset.values.iter()) {
            let Some(value) = value else { continue };
            *slot = Some(slot.unwrap_or(0).saturating_add(*value));
        }
    }

    /// Intersect the key/velocity range this set declares with a preset range.
    pub fn intersect_range(&mut self, generator: Generator, low: u8, high: u8) {
        let (existing_low, existing_high) = self.key_range(generator);
        let low = existing_low.max(low);
        let high = existing_high.min(high);
        self.values[generator.opcode() as usize] = Some(pack_range(low, high));
    }

    /// Declared key or velocity range as `(low, high)`.
    pub fn key_range(&self, generator: Generator) -> (u8, u8) {
        match self.get(generator) {
            Some(packed) => unpack_range(packed),
            None => (0, 127),
        }
    }
}

/// Ranges are stored as `low << 8 | high` in the file's two amount bytes.
pub fn unpack_range(packed: i16) -> (u8, u8) {
    let bytes = (packed as u16).to_le_bytes();
    (bytes[0], bytes[1])
}

/// Pack low and high bytes into a generator amount.
pub fn pack_range(low: u8, high: u8) -> i16 {
    u16::from_le_bytes([low, high]) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_round_trip() {
        for opcode in 0..=60u16 {
            if let Some(generator) = Generator::from_opcode(opcode) {
                assert_eq!(generator.opcode(), opcode);
            }
        }
        assert!(Generator::from_opcode(99).is_none());
        assert!(
            Generator::from_opcode(6).is_some(),
            "vibrato depth is modelled"
        );
        assert!(
            Generator::from_opcode(5).is_some(),
            "mod LFO to pitch is modelled"
        );
        for (opcode, generator, default) in [
            (46u16, Generator::Keynum, -1i16),
            (47, Generator::Velocity, -1),
        ] {
            assert_eq!(Generator::from_opcode(opcode), Some(generator));
            assert_eq!(generator.default_value(), default);
        }
    }

    #[test]
    fn local_zone_overlays_global_zone() {
        let mut global = GenSet::default();
        global.insert(Generator::AttackVolEnv, -4000);
        global.insert(Generator::Pan, 100);
        let mut local = GenSet::default();
        local.insert(Generator::AttackVolEnv, -1200);
        global.overlay(&local);
        assert_eq!(global.value(Generator::AttackVolEnv), -1200);
        assert_eq!(global.value(Generator::Pan), 100);
    }

    #[test]
    fn preset_values_add_to_instrument_values() {
        let mut instrument = GenSet::default();
        instrument.insert(Generator::InitialAttenuation, 100);
        instrument.insert(Generator::CoarseTune, 12);
        let mut preset = GenSet::default();
        preset.insert(Generator::InitialAttenuation, 50);
        instrument.add_offsets(&preset);
        assert_eq!(instrument.value(Generator::InitialAttenuation), 150);
        assert_eq!(instrument.value(Generator::CoarseTune), 12);
        // Absent generators keep their defaults, they are not added twice.
        assert_eq!(instrument.value(Generator::ScaleTuning), 100);
    }

    #[test]
    fn ranges_pack_low_byte_first() {
        let mut set = GenSet::default();
        set.insert(Generator::KeyRange, pack_range(36, 96));
        assert_eq!(set.key_range(Generator::KeyRange), (36, 96));
        set.intersect_range(Generator::KeyRange, 48, 127);
        assert_eq!(set.key_range(Generator::KeyRange), (48, 96));
        set.intersect_range(Generator::KeyRange, 100, 110);
        assert_eq!(set.key_range(Generator::KeyRange), (100, 96));
    }
}
