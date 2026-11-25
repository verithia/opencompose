//! VST3 IPC over the shared Intel plugin worker.
mod insert;
use super::*;
use crate::ports::{AudioPort, ports_from_unsigned};
use crate::vst3_instance::{
    Vst3AuthoredInput, Vst3NormalizedEvent, Vst3OrderedEvent, Vst3OwnedAuthoredPlan,
    Vst3OwnedInstance, Vst3OwnedInstrumentBlock, Vst3ProcessingBudget,
};
pub use insert::{BridgedVst3Insert, Vst3InsertSetup};
use std::borrow::Cow;
use std::collections::HashMap;

const MAX_EVENTS: usize = 1024;

pub fn needs_vst3_translation(path: &Path) -> bool {
    cfg!(target_arch = "aarch64")
        && crate::bundle::binary_path(path).is_some_and(|path| intel_executable(&path))
}

#[derive(Serialize, Deserialize)]
pub(super) struct Settings {
    class_name: Option<String>,
    parameters: Vec<Vst3AuthoredInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidechain_input: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct Description {
    output_channels: Vec<u32>,
    latency: u32,
    tail: u32,
}

#[derive(Serialize, Deserialize)]
enum Command<'a> {
    Bind(Vec<String>),
    CaptureState,
    Process {
        frames: u32,
        timing: Timing,
        pairs: Cow<'a, [usize]>,
        events: Cow<'a, [Vst3OrderedEvent]>,
    },
}

fn pair_count(channels: &[u32]) -> Option<usize> {
    if channels.len() > MAX_OUTPUT_PAIRS * 2 {
        return None;
    }
    let total = channels
        .iter()
        .try_fold(0u32, |sum, &count| sum.checked_add(count))?;
    (1..=(MAX_OUTPUT_PAIRS * 2) as u32)
        .contains(&total)
        .then(|| total.div_ceil(2) as usize)
}

fn valid_block(frames: u32, pairs: &[usize], capacity: usize, event_count: usize) -> bool {
    if frames == 0
        || frames as usize > FRAMES
        || pairs.is_empty()
        || pairs.len() > capacity
        || event_count > MAX_EVENTS
    {
        return false;
    }
    let mut selected = 0u32;
    for &pair in pairs {
        if pair >= capacity || selected & (1 << pair) != 0 {
            return false;
        }
        selected |= 1 << pair;
    }
    true
}

/// Translated VST3 instrument. The device callback consumes the producer ring,
/// so only the background playback or export worker touches this socket.
pub struct BridgedVst3 {
    _worker: Worker,
    stream: UnixStream,
    description: Description,
    pairs: usize,
    scratch: [u8; FRAMES * 8],
    events: Vec<Vst3OrderedEvent>,
    bindings: HashMap<String, u32>,
    fault: Option<String>,
    reported: Cell<bool>,
}

impl BridgedVst3 {
    pub fn load(plan: Vst3OwnedAuthoredPlan, mode: BridgeMode) -> Result<Self, String> {
        let state = plan.component_state.as_deref().unwrap_or_default();
        let init = Init {
            version: VST3_VERSION,
            role: Role::Instrument,
            plugin: plan
                .path
                .canonicalize()
                .map_err(|error| error.to_string())?,
            sample_rate: plan.sample_rate,
            state_bytes: state.len(),
            parameters: Vec::new(),
            insert_transport: None,
            vst3: Some(Settings {
                class_name: plan.class_name,
                parameters: plan.parameters,
                sidechain_input: None,
            }),
        };
        let (worker, stream, description): (_, _, Description) = connect(init, state, mode)?;
        let pairs = pair_count(&description.output_channels)
            .ok_or("Intel VST3 worker reported an unsupported output layout")?;
        Ok(Self {
            _worker: worker,
            stream,
            description,
            pairs,
            scratch: [0; FRAMES * 8],
            events: Vec::with_capacity(MAX_EVENTS),
            bindings: HashMap::new(),
            fault: None,
            reported: Cell::new(false),
        })
    }

    pub fn output_ports(&self) -> Vec<AudioPort> {
        ports_from_unsigned(&self.description.output_channels)
    }

    pub fn output_count(&self) -> usize {
        self.pairs
    }

    pub fn latency_samples(&self) -> u32 {
        self.description.latency
    }

    pub fn tail_samples(&self) -> u32 {
        self.description.tail
    }

    pub fn take_processing_failure(&self) -> ProcessingFailureReport {
        match &self.fault {
            None => ProcessingFailureReport::Unavailable,
            Some(_) if self.reported.replace(true) => ProcessingFailureReport::Reported,
            Some(error) => ProcessingFailureReport::Ready(error.clone()),
        }
    }

    pub fn failed(&self) -> bool {
        self.fault.is_some()
    }

    pub fn bind_names(&mut self, names: Vec<String>) -> Result<Vec<u32>, String> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let count = names.len();
        write_header(&mut self.stream, &Command::Bind(names)).map_err(|error| error.to_string())?;
        let ids = read_header::<Result<Vec<u32>, String>>(&mut self.stream)
            .map_err(|error| error.to_string())??;
        if ids.len() != count {
            return Err("Intel VST3 worker returned an invalid parameter binding".into());
        }
        Ok(ids)
    }

    pub fn state_bytes(&mut self) -> Result<Vec<u8>, String> {
        write_header(&mut self.stream, &Command::CaptureState)
            .map_err(|error| error.to_string())?;
        let length = read_header::<Result<usize, String>>(&mut self.stream)
            .map_err(|error| error.to_string())??;
        if length > crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES {
            return Err("Intel VST3 worker returned oversized component state".into());
        }
        let mut state = vec![0; length];
        self.stream
            .read_exact(&mut state)
            .map_err(|error| error.to_string())?;
        Ok(state)
    }

    pub fn queue_event(&mut self, event: Vst3OrderedEvent) -> Result<(), String> {
        if self.events.len() == MAX_EVENTS {
            return Err("Intel VST3 block exceeds the event limit".into());
        }
        self.events.push(event);
        Ok(())
    }

    pub fn queue_parameter(&mut self, name: &str, value: f64, offset: u32) -> Result<(), String> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!(
                "VST3 normalized parameter '{name}' is outside 0..=1"
            ));
        }
        let id = if let Some(&id) = self.bindings.get(name) {
            id
        } else {
            if self.bindings.len() == crate::vst3_instance::MAX_VST3_AUTHORED_PARAMETERS {
                return Err("Intel VST3 parameter map exceeds its preparation budget".into());
            }
            let id = self.bind_names(vec![name.to_owned()])?[0];
            self.bindings.insert(name.to_owned(), id);
            id
        };
        self.queue_event(Vst3OrderedEvent::Parameter(Vst3NormalizedEvent {
            id,
            value,
            sample_offset: offset,
        }))
    }

    pub fn process_queued(&mut self, block: Vst3OwnedInstrumentBlock<'_>) -> Result<(), String> {
        let mut events = std::mem::take(&mut self.events);
        let result = self.process(block, &events);
        events.clear();
        self.events = events;
        result
    }

    pub fn process(
        &mut self,
        block: Vst3OwnedInstrumentBlock<'_>,
        events: &[Vst3OrderedEvent],
    ) -> Result<(), String> {
        if let Some(error) = &self.fault {
            return Err(error.clone());
        }
        let Vst3OwnedInstrumentBlock {
            frames,
            transport,
            parameters,
            output_pairs,
            outputs,
        } = block;
        let Some(transport) = transport else {
            return Err("Intel VST3 block requires a transport".into());
        };
        let samples = frames as usize * 2;
        if !parameters.is_empty()
            || !valid_block(frames, output_pairs, self.pairs, events.len())
            || outputs.len() != output_pairs.len()
            || outputs.iter().any(|output| output.len() != samples)
        {
            return Err("Invalid Intel VST3 process block".into());
        }
        let result = (|| {
            write_header(
                &mut self.stream,
                &Command::Process {
                    frames,
                    timing: Timing::from_transport(transport),
                    pairs: Cow::Borrowed(output_pairs),
                    events: Cow::Borrowed(events),
                },
            )
            .map_err(|error| error.to_string())?;
            read_header::<Result<(), String>>(&mut self.stream)
                .map_err(|error| error.to_string())??;
            for output in outputs.iter_mut() {
                let bytes = &mut self.scratch[..samples * 4];
                self.stream
                    .read_exact(bytes)
                    .and_then(|_| audio_samples(bytes, output))
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        if let Err(error) = &result {
            for output in outputs {
                output.fill(0.0);
            }
            self.fault = Some(format!("Intel VST3 worker: {error}"));
        }
        result
    }
}

fn read_initial_state(init: &Init, stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    if init.version != VST3_VERSION
        || !(8_000..=192_000).contains(&init.sample_rate)
        || init.state_bytes > crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES
        || !init.plugin.is_absolute()
    {
        return Err("Invalid Intel VST3 worker setup".into());
    }
    let mut state = vec![0; init.state_bytes];
    stream
        .read_exact(&mut state)
        .map_err(|error| error.to_string())?;
    Ok(state)
}

fn prepare(
    init: Init,
    settings: Settings,
    stream: &mut UnixStream,
) -> Result<Vst3OwnedInstance, String> {
    if !init.parameters.is_empty() || settings.sidechain_input.is_some() {
        return Err("Invalid Intel VST3 instrument setup".into());
    }
    let state = read_initial_state(&init, stream)?;
    Vst3OwnedAuthoredPlan {
        path: init.plugin,
        class_name: settings.class_name,
        sample_rate: init.sample_rate,
        max_frames: FRAMES as u32,
        component_state: (!state.is_empty()).then_some(state),
        parameters: settings.parameters,
        budget: Vst3ProcessingBudget {
            events: MAX_EVENTS,
            points_per_parameter: 128,
        },
    }
    .execute_on_ui()
    .map_err(|error| error.to_string())
}

pub(super) fn serve(mut stream: UnixStream, init: Init, settings: Settings) -> io::Result<()> {
    use crate::application_main::{ApplicationMainQueue, ApplicationOfflineMainScope, MainRuntime};
    // SAFETY: The Intel helper entry point calls this on its process main thread.
    let main = unsafe { ApplicationMainQueue::register_runtime_current(MainRuntime::CommandLine) }
        .map_err(io::Error::other)?;
    let _native_events = ApplicationOfflineMainScope::enter().map_err(io::Error::other)?;
    if init.role == Role::Insert {
        return insert::serve(stream, init, settings, &main);
    }
    let rate = init.sample_rate;
    let mut plugin = match prepare(init, settings, &mut stream) {
        Ok(plugin) => plugin,
        Err(error) => return write_header(&mut stream, &Err::<Description, _>(error)),
    };
    let description = Description {
        output_channels: plugin
            .snapshot
            .outputs
            .iter()
            .map(|port| port.channels)
            .collect(),
        latency: plugin.snapshot.latency_samples,
        tail: plugin.snapshot.tail_samples,
    };
    let Some(capacity) = pair_count(&description.output_channels) else {
        return write_header(
            &mut stream,
            &Err::<Description, _>("Unsupported Intel VST3 output layout"),
        );
    };
    write_header(&mut stream, &Ok::<_, String>(description))?;
    let mut outputs = vec![vec![0.0; FRAMES * 2]; capacity];
    let mut scratch = [0; FRAMES * 8];
    loop {
        main.pump().map_err(io::Error::other)?;
        crate::application_main::service_offline_main().map_err(io::Error::other)?;
        let request = match read_header::<Command<'static>>(&mut stream) {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            result => result?,
        };
        match request {
            Command::Bind(names) => {
                let result = plugin
                    .control
                    .normalized_bindings_plan(names)
                    .execute_on_ui()
                    .map_err(|error| error.to_string());
                write_header(&mut stream, &result)?;
            }
            Command::CaptureState => {
                let state = plugin.control.capture_state_plan().execute_on_ui();
                match state {
                    Ok(bytes) if bytes.len() <= crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES => {
                        write_header(&mut stream, &Ok::<_, String>(bytes.len()))?;
                        stream.write_all(&bytes)?;
                    }
                    Ok(_) => write_header(
                        &mut stream,
                        &Err::<usize, _>("Oversized VST3 component state"),
                    )?,
                    Err(error) => write_header(&mut stream, &Err::<usize, _>(error.to_string()))?,
                }
            }
            Command::Process {
                frames,
                timing,
                pairs,
                events,
            } => {
                if !valid_block(frames, &pairs, capacity, events.len()) {
                    return write_header(&mut stream, &Err::<(), _>("Invalid Intel VST3 block"));
                }
                let count = frames as usize * 2;
                let outputs = &mut outputs[..pairs.len()];
                for output in outputs.iter_mut() {
                    output.resize(count, 0.0);
                }
                let result = plugin
                    .processor
                    .process_outputs_with_ordered_events_into(
                        Vst3OwnedInstrumentBlock {
                            frames,
                            transport: Some(&timing.to_transport(rate)),
                            parameters: &[],
                            output_pairs: &pairs,
                            outputs,
                        },
                        &events,
                    )
                    .map_err(|error| format!("VST3 processing: {error:?}"));
                write_header(&mut stream, &result)?;
                if result.is_err() {
                    break;
                }
                let bytes = &mut scratch[..count * 4];
                for output in outputs {
                    audio_bytes(output, bytes);
                    stream.write_all(bytes)?;
                }
            }
        }
    }
    drop(plugin);
    main.pump().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_rejects_duplicate_outputs_and_bounded_block_overflow() {
        assert_eq!(pair_count(&[1, 2, 3]), Some(3));
        assert_eq!(pair_count(&[64, 1]), None);
        assert!(valid_block(128, &[2, 0], 3, 4));
        assert!(!valid_block(128, &[2, 2], 3, 4));
        assert!(!valid_block(128, &[3], 3, 4));
        assert!(!valid_block(513, &[0], 3, 4));
        assert!(!valid_block(128, &[0], 3, MAX_EVENTS + 1));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires Rosetta, the Intel helper and a VST3 fixture with a level parameter"]
    fn translated_state_roundtrip_preserves_audible_parameter_change() {
        use crate::vst3_instance::{Vst3MidiEvent, Vst3MidiMessage};

        let path =
            PathBuf::from(std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST3_INSTRUMENT").unwrap());
        let level = std::env::var("OPENCOMPOSE_TEST_INTEL_VST3_VOLUME_PARAMETER").unwrap();
        let open = |component_state| {
            BridgedVst3::load(
                Vst3OwnedAuthoredPlan {
                    path: path.clone(),
                    class_name: None,
                    sample_rate: 44_100,
                    max_frames: FRAMES as u32,
                    component_state,
                    parameters: Vec::new(),
                    budget: Vst3ProcessingBudget::default(),
                },
                BridgeMode::Offline,
            )
            .unwrap()
        };
        let peak = |plugin: &mut BridgedVst3, quiet: bool| {
            if quiet {
                plugin.queue_parameter(&level, 0.15, 0).unwrap();
            }
            plugin
                .queue_event(Vst3OrderedEvent::Midi(Vst3MidiEvent {
                    channel: 0,
                    sample_offset: 8,
                    message: Vst3MidiMessage::NoteOn {
                        key: 60,
                        velocity: 100,
                    },
                }))
                .unwrap();
            let mut output = [vec![0.0; 256]];
            let clock = PluginTransport::new(0, 44_100, 120.0);
            plugin
                .process_queued(Vst3OwnedInstrumentBlock {
                    frames: 128,
                    transport: Some(&clock),
                    parameters: &[],
                    output_pairs: &[0],
                    outputs: &mut output,
                })
                .unwrap();
            assert!(output[0][..16].iter().all(|sample| *sample == 0.0));
            output[0].iter().fold(0.0f32, |peak, x| peak.max(x.abs()))
        };
        let default_peak = peak(&mut open(None), false);
        let mut edited = open(None);
        let edited_peak = peak(&mut edited, true);
        let state = edited.state_bytes().unwrap();
        assert!(!state.is_empty());
        let restored_peak = peak(&mut open(Some(state)), false);
        assert!(edited_peak > 0.0 && edited_peak < default_peak);
        assert!(
            (edited_peak - restored_peak).abs() < 1e-6,
            "edited {edited_peak}, restored {restored_peak}"
        );
    }
}
