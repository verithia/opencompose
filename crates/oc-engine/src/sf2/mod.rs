//! SoundFont 2 and SF3 bank reading and sample playback, implemented in-crate.
//!
//! Layers:
//!
//! - `riff`: the `RIFF`/`sfbk` chunk tree, tolerant of the padding and size
//!   defects real banks ship with.
//! - `font`: `INFO`, `sdta` (`smpl`) and `pdta` (`phdr`, `pbag`, `pgen`,
//!   `pmod`, `inst`, `ibag`, `igen`, `imod`, `shdr`) into presets whose zones
//!   are already merged and resolved against a shared waveform bank.
//! - `vorbis`: one sample's Ogg Vorbis stream to 16-bit points, for banks that
//!   store their sample data compressed (SF3).
//! - `player`: channel state, program/bank selection, drum channel, voice
//!   allocation, and interleaved stereo rendering.
//! - `voice`: one note over one zone — pitch, loop modes, volume envelope,
//!   low-pass filter, pan.
//!
//! ## Generator subset
//!
//! Audibility only: key/velocity ranges, `sampleModes` (off, continuous,
//! until-release), `overridingRootKey` with the sample header's key as
//! fallback, `coarseTune`/`fineTune`/`scaleTuning`, `initialAttenuation`, `pan`,
//! the volume envelope (`delay`, `attack`, `hold`, `decay`, `sustain`,
//! `release`, keynum-to-hold/decay), `initialFilterFc`/`initialFilterQ`, the
//! sample address offsets and `exclusiveClass` choking.
//!
//! `keynum` and `velocity` are honoured as the format defines them: an
//! instrument zone that declares one synthesizes as that fixed key or velocity,
//! whatever the note-on carried. Range selection still uses the played note;
//! pitch, the envelope's key scaling, the level law, the velocity-to-cutoff
//! default and both modulator sources read the fixed value.
//!
//! ## Modulation
//!
//! Three per-voice sources move the pitch, cutoff and volume destinations,
//! and bank records add level and stereo image. The vibrato LFO
//! (`vibLfoToPitch`) is opened by the format's default CC1 and
//! channel-pressure modulators; the modulation LFO drives `modLfoToPitch`,
//! `modLfoToFilterFc` and `modLfoToVolume`; the modulation envelope drives
//! `modEnvToPitch` and `modEnvToFilterFc` with its own
//! `modEnv*`/`keynumToModEnv*` generators. The filter cutoff is the zone's
//! `initialFilterFc` plus the format's velocity-to-cutoff default (a concave
//! 2400-cent swing: full velocity leaves the generator's cutoff, a soft note
//! is darker), the static records, the LFO and envelope swings, clamped to the
//! format's legal range; pitch modulation is clamped to ±12000 cents. A zone
//! that asks for no filter keeps no biquad at any velocity.
//!
//! `pmod`/`imod` records are applied when they target a modelled destination
//! with a source this engine reads (no controller, velocity, key, polyphonic
//! pressure, channel pressure, pitch wheel, pitch-wheel sensitivity or a MIDI
//! controller), any of the four transforms, and an optional secondary amount
//! source of the same kind. Beyond the pitch, cutoff, volume and send
//! destinations, a record may also drive `initialAttenuation` (its amount is
//! centibels, summed with the generator, and the total clips at 144 dB) and
//! `pan` (generator units, summed with the zone's and CC10's pan). Polyphonic
//! pressure is the voice's own note pressure: a `PolyPressure` message
//! refreshes the voices holding that note and leaves every other note alone.
//! The rest is counted in a warning.
//!
//! ## Effect sends
//!
//! `chorusEffectsSend` and `reverbEffectsSend` (tenths of a percent, moved by
//! CC93/CC91) feed two buses owned by the player: one chorus and one reverb
//! instance shared by every voice, wet only, with the return calibrated to the
//! send level. A bank whose sends stay zero never enters that path, so it
//! renders bit-identically. The bus is per player, the send is taken from the
//! voice's panned stereo output, and the two devices' parameters are fixed:
//! the format declares none.
//!
//! ## SF3 banks
//!
//! A bank whose sample data starts with an Ogg page header is read as SF3:
//! every `shdr` offset is a byte offset into `smpl`, a sample is either an Ogg
//! Vorbis stream or raw 16-bit PCM, and its loop points are frame offsets from
//! the sample's start. Decoding rebuilds one flat waveform and rewrites the
//! headers into frame offsets, so presets, zones, generators and modulators
//! resolve exactly as they do for a raw bank; only the bytes on disk change.
//! Detection is by content, not by file name.
//!
//! ## Deliberately absent in this phase
//!
//! - Modulator records whose source is an unassigned enumerator, whose
//!   transform is outside the format's four, whose destination is outside the
//!   modelled set, or past the 16-per-zone cap. The destinations a real bank
//!   reaches and this engine does not are `initialFilterQ` (9), the envelope
//!   time generators (delay/attack/hold/decay/release, 21-40), the sample start
//!   and loop offsets (0-3, 45, 50), `freqModLFO` (22) and `fineTune` (52); the
//!   design decision for each is in `docs/sf2-player.md`.
//! - Stereo sample pairs (`sample_type` 2/4) play as mono; a bank's 24-bit
//!   `sm24` low bytes are honoured, but a 24-bit sample is not dithered.
//! - Crossfade loops.
//! - Compressed 24-bit samples, and a loop range that leaves its sample (the
//!   loader clamps it to the sample's decoded length).

pub mod font;
pub mod generator;
pub mod modulation;
mod player;
pub mod progress;
pub mod riff;
mod sends;
mod voice;
mod vorbis;

pub use font::{
    EnvelopeShape, Error, Instrument, Preset, SampleBank, SampleHeader, SampleMode, SoundFont,
    Zone, attenuation_to_gain,
};
pub use player::{BEND_RANGE_SEMITONES, DynamicRamp, MASTER_GAIN, Sf2Renderer};
pub use progress::{Report, set_report_sink};
