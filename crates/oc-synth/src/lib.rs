//! Native instruments and sample playback: OpenSynth, the preset importer, the
//! sampler and the performance scheduler.
//!
//! The implementation lives in `oc-synth-core`, `oc-synth-fx` and
//! `oc-synth-import`; this crate keeps the long-standing module paths stable.

pub mod audio_region;
pub mod drumkit;
pub mod native_performance;
pub mod performance;

pub mod synth {
    //! OpenSynth engine surface.

    pub use oc_synth_core::synth::*;
    pub use oc_synth_import::{
        PROJECT_ASSET_KEYS, anchor_project_patch_assets, has_relative_project_patch_assets,
        load_project_patch,
    };

    pub mod foreign_preset {
        //! Foreign preset import.

        pub use oc_synth_import::foreign_preset::*;
    }
}

pub use oc_synth_core::midi_event::{MidiEvent, MidiMessage};
pub use oc_synth_core::{midi_event, note_pitch_curve, sampler};
