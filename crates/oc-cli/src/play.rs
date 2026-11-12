//! `ocp play`: project audio plus a live controller instrument and MIDI output.
//!
//! Audio is rendered offline into a buffer, then streamed through the audio-IO
//! owner, so the CLI opens the same device and format the DAW persisted. The
//! MIDI thread owns all controller I/O and pushes parsed events into the SPSC
//! queue consumed by the audio callback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use oc_audio::audio_io::{AudioIo, CpalBackend, RenderSink, RenderSource, StreamSpec};
use oc_dsl::ast::Project;
use oc_engine::interpret::{self, InterpretSettings};
use oc_engine::midi_io::{
    LiveEvent, LiveEventConsumer, MidiInputEvent, MidiSession, MidirBackend, OutputScheduler,
    event_queue,
};
use oc_synth::synth::{OpenSynth, SynthPatch, load_project_patch};

/// What the live play command should open and how.
pub struct PlayOptions {
    pub midi_in: Option<String>,
    pub midi_out: Option<String>,
    pub midi_clock: bool,
    pub midi_thru: bool,
    pub soundfont: Option<PathBuf>,
    pub sample_rate: u32,
}

/// Play the project live on the selected audio and MIDI devices.
pub fn run(project: &Project, root: Option<&Path>, options: PlayOptions) -> Result<()> {
    // The persisted device and period apply here too; the explicit rate wins,
    // and an unsupported one is negotiated down before anything is rendered.
    let io = Arc::new(AudioIo::new(
        Arc::new(CpalBackend::new()),
        AudioIo::preferences_path(),
    ));
    let mut selection = io.selection();
    selection.sample_rate = Some(options.sample_rate);
    // The CLI borrows the DAW's device choice; it must not overwrite the file
    // with the rate it was told to render.
    let status = io.apply_transient(&selection);
    if let Some(failure) = &status.failure {
        eprintln!("audio device: {}", describe_failure(failure));
    }
    let sample_rate = io.sample_rate();
    if let Some(opened) = &status.opened {
        println!(
            "Output: {} at {} Hz ({} frames)",
            opened.name,
            opened.sample_rate,
            opened
                .buffer_frames
                .map(|frames| frames.to_string())
                .unwrap_or_else(|| "device default".to_string())
        );
    }
    let soundfont = options.soundfont.unwrap_or_default();
    let audio = oc_engine::render::render_to_buffer_per_track(project, &soundfont, sample_rate)
        .map_err(anyhow::Error::msg)
        .context("render project for playback")?;

    let patch = live_patch(project, root).context("prepare live instrument")?;
    let synth = OpenSynth::new_with_tempo(patch, project.properties.tempo);
    let (producer, consumer) = event_queue(1024);
    let position = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let project_audio = Arc::new(audio);
    let mixer = Mixer {
        consumer,
        synth,
        audio: Arc::clone(&project_audio),
        position: Arc::clone(&position),
        sample_rate,
        scratch: vec![0.0f32; 2048],
        cursor: 0,
    };
    let source = Arc::new(MixerSource(std::sync::Mutex::new(Some(mixer))));
    let stream = io
        .open_stream(source)
        .map_err(anyhow::Error::msg)
        .context("open audio output stream")?;

    let midi_thread = if options.midi_in.is_some() || options.midi_out.is_some() {
        let mut session = MidiSession::open(
            MidirBackend::new().map_err(anyhow::Error::msg)?,
            options.midi_in.as_deref(),
            options.midi_out.as_deref(),
        )
        .map_err(anyhow::Error::msg)
        .context("open MIDI ports")?;
        session.set_thru(options.midi_thru);
        if let Some(name) = session.input_name() {
            println!("MIDI input:  {name}");
        }
        if let Some(name) = session.output_name() {
            println!(
                "MIDI output: {name} (clock {})",
                if options.midi_clock { "on" } else { "off" }
            );
        }
        let events = if options.midi_out.is_some() {
            interpret::try_interpret(
                project,
                &InterpretSettings {
                    sample_rate,
                    ..Default::default()
                },
            )
            .map_err(anyhow::Error::msg)
            .context("schedule MIDI output")?
        } else {
            Vec::new()
        };
        let mut scheduler = OutputScheduler::new(events, options.midi_clock);
        let position = Arc::clone(&position);
        let stop = Arc::clone(&stop);
        let tempo = project.properties.tempo;
        let mut producer = producer;
        Some(std::thread::spawn(move || {
            let mut input = [MidiInputEvent::default(); 256];
            let mut output = [LiveEvent::default(); 256];
            let result = 'midi: loop {
                if stop.load(Ordering::Acquire) {
                    break Ok(());
                }
                let count = match session.poll(&mut input) {
                    Ok(count) => count,
                    Err(error) => break Err(format!("MIDI input stopped. {error}")),
                };
                for event in &input[..count] {
                    if event.event.to_engine_message().is_some()
                        && producer.push(event.event).is_err()
                    {
                        break 'midi Err("Live MIDI synth queue overflowed".to_string());
                    }
                }
                if let Err(error) = session.forward(&input[..count]) {
                    break Err(format!("MIDI thru stopped. {error}"));
                }
                let due = scheduler.advance(
                    position.load(Ordering::Acquire),
                    sample_rate,
                    tempo,
                    &mut output,
                );
                for event in &output[..due] {
                    if let Err(error) = session.send_live(*event) {
                        break 'midi Err(format!("MIDI output stopped. {error}"));
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            stop.store(true, Ordering::Release);
            // Release sustain before asking external instruments to end notes.
            for channel in 0..16 {
                for controller in [64, 123] {
                    let _ = session.send_engine_message(
                        &oc_engine::interpret::MidiMessage::ControlChange {
                            channel,
                            controller,
                            value: 0,
                        },
                    );
                }
            }
            result
        }))
    } else {
        None
    };

    println!(
        "Playing {:.2}s at {} Hz. Ctrl+C to stop.",
        project_audio.len() as f64 / f64::from(sample_rate) / 2.0,
        sample_rate
    );
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(stream);
    if let Some(thread) = midi_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("MIDI worker panicked"))?
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

/// Mixes the rendered project with the live instrument into the device callback.
struct Mixer {
    consumer: LiveEventConsumer,
    synth: OpenSynth,
    audio: Arc<Vec<f32>>,
    position: Arc<AtomicU64>,
    sample_rate: u32,
    scratch: Vec<f32>,
    cursor: usize,
}

impl Mixer {
    fn render(&mut self, output: &mut [f32]) {
        let sample_rate = self.sample_rate;
        self.consumer.drain_into(&mut self.synth, 256);
        let mut offset = 0;
        while offset < output.len() {
            let length = (output.len() - offset).min(self.scratch.len());
            {
                let block = &mut self.scratch[..length];
                self.synth.render_interleaved(block, sample_rate);
                for (sample, live) in output[offset..offset + length].iter_mut().zip(block.iter()) {
                    *sample += *live * 0.7;
                }
            }
            offset += length;
        }
        for sample in output.iter_mut() {
            if self.cursor < self.audio.len() {
                *sample += self.audio[self.cursor];
            }
            self.cursor += 1;
        }
        self.position
            .store((self.cursor / 2) as u64, Ordering::Release);
    }
}

/// Builds the callback for one stream open. The CLI opens once and never
/// sweeps, so the mixer is moved into the first callback.
struct MixerSource(std::sync::Mutex<Option<Mixer>>);

impl RenderSource for MixerSource {
    fn create(&self, _spec: &StreamSpec) -> Arc<dyn RenderSink> {
        let mixer = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        match mixer {
            Some(mixer) => Arc::new(MixerSink(std::cell::UnsafeCell::new(mixer))),
            // A device change during `ocp play` would rebuild the stream; there
            // is no second mixer, so the new stream stays silent.
            None => Arc::new(SilentSink),
        }
    }
}

/// One device callback thread drives this sink.
struct MixerSink(std::cell::UnsafeCell<Mixer>);

// SAFETY: cpal calls one sink from one device callback thread.
unsafe impl Sync for MixerSink {}

impl RenderSink for MixerSink {
    fn fill(&self, output: &mut [f32]) {
        // SAFETY: the backend owns this sink for one callback thread only.
        let mixer = unsafe { &mut *self.0.get() };
        mixer.render(output);
    }

    fn failed(&self, error: String) {
        eprintln!("audio output error: {error}");
    }
}

struct SilentSink;

impl RenderSink for SilentSink {
    fn fill(&self, output: &mut [f32]) {
        output.fill(0.0);
    }
}

/// One line for a device that could not be opened or kept.
fn describe_failure(failure: &oc_audio::audio_io::AudioFailure) -> String {
    use oc_audio::audio_io::AudioFailure;
    match failure {
        AudioFailure::RecordingActive => "stop recording before changing audio devices".to_string(),
        AudioFailure::NoDevices => "no audio output device is available".to_string(),
        AudioFailure::ListFailed { error } => format!("could not list devices: {error}"),
        AudioFailure::OpenFailed {
            device,
            error,
            fallback,
        } => match fallback {
            Some(fallback) => format!("could not open {device}: {error}; using {fallback}"),
            None => format!("could not open {device}: {error}"),
        },
        AudioFailure::StreamLost { device, error } => {
            format!("could not move the open streams to {device}: {error}")
        }
        AudioFailure::NoInputDevices => "no audio input device is available".to_string(),
        AudioFailure::InputOpenFailed {
            device,
            error,
            fallback,
        } => match fallback {
            Some(fallback) => format!("could not open input {device}: {error}; using {fallback}"),
            None => format!("could not open input {device}: {error}"),
        },
    }
}

/// Prefer the first OpenSynth preset so the controller plays the project's patch.
fn live_patch(project: &Project, root: Option<&Path>) -> Result<SynthPatch> {
    let resolved = oc_dsl::preset::resolve(project).map_err(anyhow::Error::msg)?;
    let found = resolved.tracks.iter().find_map(|track| {
        let name = track.preset.as_deref()?;
        let preset = resolved.presets.iter().find(|preset| preset.name == name)?;
        (preset.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)).then_some((track, preset))
    });
    let mut patch = match found {
        Some((track, preset)) => {
            let params =
                oc_engine::preset::builtin_track_params_at(&resolved, Some(preset), track, root);
            load_project_patch(&params, root).map_err(anyhow::Error::msg)?
        }
        None => SynthPatch::default(),
    };
    patch.apply_params(Some(&oc_engine::mpe::patch_params(&resolved)));
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::describe_failure;
    use oc_audio::audio_io::AudioFailure;

    #[test]
    fn recording_blocks_audio_device_changes() {
        assert_eq!(
            describe_failure(&AudioFailure::RecordingActive),
            "stop recording before changing audio devices"
        );
    }
}
