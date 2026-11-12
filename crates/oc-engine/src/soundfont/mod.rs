//! SoundFont rendering for the engine's track sources.
//!
//! One type, `SoundFontRenderer`, with the surface every render path already
//! used: construct per track with the output rate, mark the drum channel, feed
//! MIDI, pull interleaved stereo frames. The backend is the in-crate player in
//! [`crate::sf2`], so a shipping build links no SoundFont library at all.

use std::path::Path;

use crate::interpret::{MidiEvent, MidiMessage};
use crate::sf2::{DynamicRamp, Sf2Renderer};
use oc_dsl::ast::{PresetDef, Project, ProjectProperties, Track};

mod shared;

pub use shared::SharedSoundFontRenderer;

/// One track's hairpin ramps in output frames, shifted by `offset` for a
/// window render. A track with no mark returns an empty list.
pub(crate) fn dynamic_layer_ramps(
    project: &Project,
    track: &Track,
    properties: &ProjectProperties,
    sample_rate: u32,
    offset: u64,
) -> Result<Vec<DynamicRamp>, String> {
    if !oc_dsl::dynamics::track_has_hairpins(project, track) {
        return Ok(Vec::new());
    }
    let clock = oc_dsl::timing::ProjectClock::new(properties)?;
    let mut ramps = Vec::new();
    for ramp in oc_dsl::dynamics::track_layer_ramps(project, track, &clock)? {
        let start = clock.sample_at_quarter(ramp.start, sample_rate)?;
        let end = clock.sample_at_quarter(ramp.end, sample_rate)?;
        if end <= offset {
            continue;
        }
        ramps.push(DynamicRamp {
            start_frame: start.saturating_sub(offset),
            end_frame: end.saturating_sub(offset),
            start_velocity: ramp.start_velocity,
            target_velocity: ramp.target_velocity,
        });
    }
    Ok(ramps)
}

/// The gain in dB and the pan a SoundFont preset's `output` stage declares.
///
/// Only `output.gain` and `output.pan` reach the player. Any other parameter
/// has no meaning for a SoundFont and is reported once, then ignored: a
/// preset is a document, so an unknown key is a warning, not a render error.
pub(crate) fn preset_output(preset: Option<&PresetDef>) -> (f64, f64) {
    let Some(preset) = preset else {
        return (0.0, 0.0);
    };
    let mut gain_db = 0.0;
    let mut pan = 0.0;
    for (name, value) in &preset.params {
        match name.as_str() {
            "output.gain" => match value.as_f64() {
                Some(db) if db.is_finite() => gain_db = db,
                _ => warn_ignored(preset, name),
            },
            "output.pan" => match value.as_f64() {
                Some(value) if value.is_finite() => pan = value.clamp(-1.0, 1.0),
                _ => warn_ignored(preset, name),
            },
            _ => warn_ignored(preset, name),
        }
    }
    (gain_db, pan)
}

fn warn_ignored(preset: &PresetDef, name: &str) {
    crate::render_warnings::push(format!(
        "SoundFont preset '{}': parameter '{name}' has no effect",
        preset.name
    ));
}

/// A SoundFont renderer for one output rate.
pub struct SoundFontRenderer {
    player: Sf2Renderer,
    sample_rate: u32,
    cancel: Option<crate::render_cancel::CancelToken>,
}

impl SoundFontRenderer {
    /// Load a bank through the in-crate player.
    pub fn new(sample_rate: u32, soundfont_path: &Path) -> Result<Self, String> {
        Ok(Self {
            player: Sf2Renderer::new(sample_rate, soundfont_path)?,
            sample_rate,
            cancel: None,
        })
    }

    /// Prepare only the SF3 resources needed by the authored MIDI stream.
    /// Processing never decodes audio; an unprepared program switch is refused.
    pub fn new_streaming<'a>(
        sample_rate: u32,
        soundfont_path: &Path,
        events: impl IntoIterator<Item = &'a MidiMessage>,
        drum_channel: bool,
    ) -> Result<Self, String> {
        Self::new_streaming_with_options(
            sample_rate,
            soundfont_path,
            events,
            drum_channel,
            crate::render_parallel::RenderWorkerOptions::default(),
        )
    }

    /// Prepare resources using the owning request's immutable worker options.
    pub fn new_streaming_with_options<'a>(
        sample_rate: u32,
        soundfont_path: &Path,
        events: impl IntoIterator<Item = &'a MidiMessage>,
        drum_channel: bool,
        workers: crate::render_parallel::RenderWorkerOptions,
    ) -> Result<Self, String> {
        Ok(Self {
            player: Sf2Renderer::new_streaming_with_options(
                sample_rate,
                soundfont_path,
                events,
                drum_channel,
                workers,
            )?,
            sample_rate,
            cancel: None,
        })
    }

    /// Poll `token` between render blocks.
    pub fn set_cancel_token(&mut self, token: Option<crate::render_cancel::CancelToken>) {
        self.cancel = token;
    }

    /// Renderer sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Set channel 9 to the MIDI drum channel; the next note selects bank 128.
    pub fn enable_drum_channel(&self) {
        self.player.enable_drum_channel();
    }

    /// Send a MIDI message.
    pub fn send(&mut self, msg: &MidiMessage) -> Result<(), String> {
        self.player.send(msg)
    }

    /// Enable or disable the hairpin velocity-layer crossfade.
    pub fn set_dynamic_layers(&mut self, enabled: bool) {
        self.player.set_dynamic_layers(enabled);
    }

    /// Apply a preset's `output.gain` (dB) and `output.pan` to this player.
    pub fn apply_preset_output(&mut self, preset: Option<&PresetDef>) {
        let (gain_db, pan) = preset_output(preset);
        self.set_output(gain_db, pan);
    }

    /// Apply an already resolved output stage.
    pub fn set_output(&mut self, gain_db: f64, pan: f64) {
        self.player.set_output(gain_db, pan);
    }

    /// Install the track's hairpin ramps in output frames.
    pub fn set_dynamic_ramps(&mut self, ramps: Vec<DynamicRamp>) {
        self.player.set_dynamic_ramps(ramps);
    }

    /// Install a track's hairpin layer ramps and its `dynamic_layers` opt-out
    /// on a streaming lane, and place the lane at `offset` project frames.
    ///
    /// The table is stored in project frames, so a later seek only moves the
    /// clock; nothing is allocated per block.
    pub fn install_hairpin_layers(
        &mut self,
        preset: Option<&PresetDef>,
        project: &Project,
        track: &Track,
        properties: &ProjectProperties,
        offset: u64,
    ) -> Result<(), String> {
        let enabled = crate::preset::dynamic_layers_enabled(preset, track);
        self.set_dynamic_layers(enabled);
        if enabled {
            self.set_dynamic_ramps(dynamic_layer_ramps(
                project,
                track,
                properties,
                self.sample_rate,
                0,
            )?);
        }
        self.set_project_frame(offset);
        Ok(())
    }

    /// Move the lane's clock to an absolute project frame.
    pub fn set_project_frame(&mut self, frame: u64) {
        self.player.set_position(frame);
    }

    /// Return the player to its construction state without loading the bank
    /// again. The lane's drum, dynamic-layer and ramp settings survive.
    pub fn reset(&mut self) {
        self.player.reset();
    }

    /// Render `output.len() / 2` stereo frames into a caller-owned buffer,
    /// replacing its contents. Streaming calls this; it does not allocate.
    pub fn render_into(&mut self, output: &mut [f32]) {
        self.player.render_into(output);
    }

    /// Render interleaved stereo samples.
    pub fn render_samples(&mut self, num_samples: usize) -> Vec<f32> {
        let mut output = vec![0.0; num_samples.saturating_mul(2)];
        self.render_into(&mut output);
        output
    }

    /// Render a whole event list to an interleaved stereo buffer.
    pub fn render_events(&mut self, events: &[MidiEvent], duration_seconds: f64) -> Vec<f32> {
        self.try_render_events(events, duration_seconds)
            .expect("SoundFont render requires compatible events")
    }

    /// Interleave event dispatch with rendering, in blocks small enough that
    /// every event lands on its sample like the streaming path so does.
    pub fn try_render_events(
        &mut self,
        events: &[MidiEvent],
        duration_seconds: f64,
    ) -> Result<Vec<f32>, String> {
        let total_samples = (duration_seconds.max(0.0) * f64::from(self.sample_rate)) as usize;
        // Sanity cap: ten minutes, so a degenerate duration cannot exhaust RAM.
        let total_samples = total_samples.min(self.sample_rate as usize * 600);
        let chunk_frames = 64;
        let mut output = Vec::with_capacity(total_samples * 2);
        let mut block = vec![0.0f32; chunk_frames * 2];
        let mut event_index = 0;
        let mut sample_position = 0u64;
        while sample_position < total_samples as u64 {
            // A block is 64 frames, so a cancelled render stops in a few
            // milliseconds at any rate.
            if crate::render_cancel::stopped(self.cancel.as_ref()) {
                return Err(crate::render_cancel::cancelled());
            }
            while event_index < events.len() && events[event_index].time_samples <= sample_position
            {
                self.send(&events[event_index].event)?;
                event_index += 1;
            }
            let next_event = events
                .get(event_index)
                .map(|event| event.time_samples)
                .unwrap_or(total_samples as u64);
            let remaining = total_samples as u64 - sample_position;
            let frames = (chunk_frames as u64)
                .min(remaining)
                .min(next_event.saturating_sub(sample_position).max(1))
                as usize;
            if frames == 0 {
                break;
            }
            let block = &mut block[..frames * 2];
            self.render_into(block);
            output.extend_from_slice(block);
            sample_position += frames as u64;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "plugin-host")]
    use std::collections::HashMap;
    #[cfg(feature = "plugin-host")]
    use std::path::PathBuf;

    #[cfg(feature = "plugin-host")]
    const RATE: u32 = 48_000;

    #[cfg(feature = "plugin-host")]
    fn fixture() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-test.sf2")
    }

    /// One held C4 through a SoundFont preset, with the preset's `params`
    /// spliced in verbatim. An empty string declares no `params` line.
    fn project(params: &str) -> Project {
        let params = if params.is_empty() {
            String::new()
        } else {
            format!("  params: {{{params}}}\n")
        };
        let text = format!(
            r#"@ocp "0.2.0"

@project "SoundFont preset output"
  tempo: 120
  time_signature: 4/4

@preset keys
  source: soundfont
  program: 0
{params}
@track keys | preset: keys, instrument: piano/acoustic_grand
  @phrase held [1:1 - 3:1]
    C4 w mf
"#
        );
        oc_dsl::parse(&text).expect("the fixture parses")
    }

    #[cfg(feature = "plugin-host")]
    fn render_legacy(params: &str) -> Vec<f32> {
        crate::render::render_to_buffer_per_track(&project(params), &fixture(), RATE)
            .expect("the fixture renders")
    }

    #[cfg(feature = "plugin-host")]
    fn rms(samples: &[f32]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|value| f64::from(*value).powi(2)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    #[cfg(feature = "plugin-host")]
    fn channel_rms(audio: &[f32], channel: usize) -> f64 {
        let samples: Vec<f32> = audio.chunks_exact(2).map(|frame| frame[channel]).collect();
        rms(&samples)
    }

    #[cfg(feature = "plugin-host")]
    fn db(value: f64) -> f64 {
        20.0 * value.max(1e-12).log10()
    }

    #[test]
    fn preset_output_reads_gain_and_pan_and_clamps_pan() {
        let lowered = project("output.gain: -6, output.pan: -1");
        assert_eq!(preset_output(lowered.presets.first()), (-6.0, -1.0));
        let raised = project("output.gain: 3, output.pan: -4");
        assert_eq!(preset_output(raised.presets.first()), (3.0, -1.0));
        assert_eq!(preset_output(None), (0.0, 0.0));
    }

    /// The gain belongs to the source, so the measured gap is the preset's own
    /// dB value and the track fader is not involved twice.
    #[test]
    #[cfg(feature = "plugin-host")]
    fn a_preset_output_gain_moves_the_render_by_that_many_db() {
        let unity = render_legacy("output.gain: 0");
        let quiet = render_legacy("output.gain: -6");
        let gap = db(rms(&unity)) - db(rms(&quiet));
        println!("preset output gain: {gap:+.3} dB for a -6 dB parameter");
        assert!(
            (gap - 6.0).abs() <= 0.2,
            "a -6 dB preset gain measured {gap:.3} dB"
        );
    }

    /// 0 dB and centre is the player's construction state: the rendered audio
    /// is identical to a preset that declares no output parameters at all.
    #[test]
    #[cfg(feature = "plugin-host")]
    fn centre_and_zero_gain_are_bit_identical_to_no_output_params() {
        let no_params = render_legacy("");
        let explicit = render_legacy("output.gain: 0, output.pan: 0");
        assert_eq!(no_params, explicit);
    }

    /// The cache path builds its own player per track, so the pan must reach
    /// that player too. Hard left leaves the right channel silent.
    #[test]
    #[cfg(feature = "plugin-host")]
    fn a_preset_output_pan_hard_left_empties_the_right_channel() {
        let project = project("output.pan: -1");
        let mut plugins = HashMap::new();
        let audio = crate::render_cache::RenderCache::new()
            .render_full(&project, &fixture(), RATE, &mut plugins)
            .expect("the cache renders the fixture");
        let (left, right) = (channel_rms(&audio, 0), channel_rms(&audio, 1));
        println!(
            "preset output pan hard left: left {:+.2} dBFS, right {:+.2} dBFS",
            db(left),
            db(right)
        );
        assert!(left > 1e-4, "the left channel is silent");
        assert!(
            db(right) < -60.0,
            "the right channel measured {:.2} dBFS",
            db(right)
        );
    }

    #[test]
    #[cfg(feature = "plugin-host")]
    fn an_unknown_preset_parameter_warns_and_renders_unchanged() {
        let _serial = crate::render_warnings::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let baseline = render_legacy("output.gain: 0");
        crate::render_warnings::clear();
        let unknown = render_legacy("output.gain: 0, unknown.width: 0.5");
        let warnings = crate::render_warnings::take();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unknown.width")),
            "no warning named the parameter: {warnings:?}"
        );
        assert_eq!(baseline, unknown);
    }
}
