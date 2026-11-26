use super::{MAX_EVENTS, Settings, read_initial_state};
use crate::application_main::ApplicationMainQueue;
use crate::ports::BusTopology;
use crate::vst2_bridge::*;
use crate::vst3_instance::{Vst3Instance, normalize_param_name};
use oc_audio::{AudioProcessContext, AudioProcessInput};
use std::borrow::Cow;
use std::collections::BTreeMap;

pub struct Vst3InsertSetup<'a> {
    pub path: &'a Path,
    pub class_name: &'a str,
    pub sample_rate: u32,
    pub state: Option<&'a [u8]>,
    pub parameters: Vec<(String, f64)>,
    pub sidechain_input: Option<usize>,
    pub transport: Option<&'a PluginTransport>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Point {
    name: String,
    value: f64,
    offset: u32,
}

#[derive(Serialize, Deserialize)]
enum Request<'a> {
    Process {
        frames: usize,
        timing: Option<Timing>,
        has_key: bool,
        parameters: Cow<'a, [Point]>,
    },
    Set {
        name: Cow<'a, str>,
        value: f64,
    },
    Reset,
}

pub struct BridgedVst3Insert {
    _worker: Worker,
    stream: UnixStream,
    rate: u32,
    metadata: Metadata,
    pending: Vec<Point>,
    scratch: [u8; FRAMES * 8],
    fault: Option<String>,
    reported: Cell<bool>,
}

impl BridgedVst3Insert {
    pub fn load(setup: Vst3InsertSetup<'_>, mode: BridgeMode) -> Result<Self, String> {
        let state = setup.state.unwrap_or_default();
        let init = Init {
            version: VST3_VERSION,
            role: Role::Insert,
            plugin: setup
                .path
                .canonicalize()
                .map_err(|error| error.to_string())?,
            sample_rate: setup.sample_rate,
            state_bytes: state.len(),
            parameters: setup.parameters,
            insert_transport: setup.transport.map(Timing::from_transport),
            vst3: Some(Settings {
                class_name: Some(setup.class_name.into()),
                parameters: Vec::new(),
                sidechain_input: setup.sidechain_input,
            }),
        };
        let (worker, stream, metadata) = connect(init, state, mode)?;
        Ok(Self {
            _worker: worker,
            stream,
            metadata,
            rate: setup.sample_rate,
            pending: Vec::with_capacity(MAX_EVENTS),
            scratch: [0; FRAMES * 8],
            fault: None,
            reported: Cell::new(false),
        })
    }

    fn reset_worker(&mut self) -> Result<(), String> {
        let playback_timeout = self
            .stream
            .read_timeout()
            .map_err(|error| error.to_string())?;
        self.stream
            .set_read_timeout(Some(TIMEOUT))
            .map_err(|error| error.to_string())?;
        let response = write_header(&mut self.stream, &Request::Reset)
            .and_then(|()| read_header::<Result<(), String>>(&mut self.stream))
            .map_err(|error| error.to_string());
        let restored = self.stream.set_read_timeout(playback_timeout);
        response??;
        restored.map_err(|error| error.to_string())
    }

    fn exchange(
        &mut self,
        input: &mut [f32],
        key: Option<&[f32]>,
        timing: Option<&PluginTransport>,
    ) -> Result<(), String> {
        let frames = input.len() / 2;
        if input.len() != frames * 2
            || frames > FRAMES
            || key.is_some_and(|key| !self.metadata.sidechain || key.len() != input.len())
        {
            return Err("Invalid Intel VST3 insert stereo block".into());
        }
        write_header(
            &mut self.stream,
            &Request::Process {
                frames,
                timing: timing.map(Timing::from_transport),
                has_key: key.is_some(),
                parameters: Cow::Borrowed(&self.pending),
            },
        )
        .map_err(|error| error.to_string())?;
        let bytes = &mut self.scratch[..frames * 8];
        for pcm in std::iter::once(&*input).chain(key) {
            audio_bytes(pcm, bytes);
            self.stream
                .write_all(bytes)
                .map_err(|error| error.to_string())?;
        }
        read_header::<Result<(), String>>(&mut self.stream)
            .map_err(|error| error.to_string())??;
        self.stream
            .read_exact(bytes)
            .and_then(|_| audio_samples(bytes, input))
            .map_err(|error| error.to_string())
    }
}

impl AudioEffect for BridgedVst3Insert {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_with_transport(buffer, sample_rate, None);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_context(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Main,
            },
        );
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_context(
            buffer,
            AudioProcessContext {
                sample_rate,
                transport: transport.as_ref(),
                input: AudioProcessInput::Sidechain(key),
            },
        );
    }

    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        if buffer.is_empty() {
            return;
        }
        if self.fault.is_none() {
            let key = match context.input {
                AudioProcessInput::Sidechain(key) => Some(key),
                _ => None,
            };
            self.fault = if context.sample_rate == self.rate {
                self.exchange(buffer, key, context.transport).err()
            } else {
                Some("Sample rate changed after Intel VST3 insert preparation".into())
            };
        }
        self.pending.clear();
        if self.fault.is_some() {
            buffer.fill(0.0);
        }
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if self.fault.is_some() {
            return;
        }
        let request = Request::Set {
            name: Cow::Borrowed(name),
            value,
        };
        match write_header(&mut self.stream, &request)
            .and_then(|_| read_header::<Result<Option<f64>, String>>(&mut self.stream))
        {
            Ok(Ok(value)) => {
                let name = normalize_param_name(name);
                if let Some(value) = value
                    && let Some((_, current)) = self
                        .metadata
                        .parameters
                        .iter_mut()
                        .find(|(key, _)| key == &name)
                {
                    *current = value;
                }
            }
            Err(error) => self.fault = Some(error.to_string()),
            Ok(Err(error)) => self.fault = Some(error),
        }
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        let name = normalize_param_name(name);
        self.metadata
            .parameters
            .iter()
            .find(|(key, _)| key == &name)
            .map(|(_, value)| *value)
    }

    fn queue_param_at(&mut self, name: &str, value: f64, offset: u32) -> bool {
        if self.fault.is_some() {
            return true; // The caller must not retry a failed processor's write.
        }
        if self.pending.len() >= MAX_EVENTS {
            self.fault = Some("Intel VST3 insert block exceeds the parameter event limit".into());
            return true;
        }
        self.pending.push(Point {
            name: name.into(),
            value,
            offset,
        });
        true
    }

    fn supports_param_offsets(&self) -> bool {
        true
    }

    fn reset(&mut self) {
        self.pending.clear();
        if self.fault.is_none() {
            self.fault = self.reset_worker().err();
        }
    }

    fn name(&self) -> &str {
        &self.metadata.name
    }

    fn supports_sidechain(&self) -> bool {
        self.metadata.sidechain
    }

    fn max_block_frames(&self) -> u32 {
        FRAMES as u32
    }

    fn latency_samples(&self) -> u32 {
        self.metadata.latency
    }

    fn tail_samples(&self) -> u32 {
        self.metadata.tail.unwrap_or(0)
    }

    fn plugin_tail_samples(&self) -> Option<u32> {
        self.metadata.tail
    }

    fn processing_failed(&self) -> bool {
        self.fault.is_some()
    }

    fn take_processing_failure(&self) -> ProcessingFailureReport {
        match &self.fault {
            None => ProcessingFailureReport::Unavailable,
            Some(_) if self.reported.replace(true) => ProcessingFailureReport::Reported,
            Some(error) => ProcessingFailureReport::Ready(format!("Intel VST3 insert: {error}")),
        }
    }
}

fn open_effect(init: &Init, settings: &Settings, state: &[u8]) -> Result<Vst3Instance, String> {
    let name = settings
        .class_name
        .as_deref()
        .ok_or("Intel VST3 insert requires a class name")?;
    let mut plugin = Vst3Instance::load_class(&init.plugin, name, init.sample_rate, FRAMES as u32)?;
    if !state.is_empty() {
        plugin.set_state_bytes(state)?;
    }
    let topology = BusTopology::new(plugin.input_ports(), plugin.output_ports());
    if topology.is_instrument() {
        return Err(format!("{name}: an instrument cannot be used as an insert"));
    }
    if let Some(index) = settings.sidechain_input {
        if plugin.sidechain_input_bus(index).is_none() {
            return Err(format!(
                "sidechain_input={index} is unavailable; the plugin declares {} sidechain inputs",
                plugin.sidechain_input_count()
            ));
        }
        plugin.set_sidechain_input_index(index);
    }
    for (name, value) in &init.parameters {
        plugin.set_param_by_name(name, *value);
    }
    let transport = init
        .insert_transport
        .as_ref()
        .map(|stamp| stamp.to_transport(init.sample_rate));
    plugin.prepare_restored_insert(transport.as_ref())?;
    Ok(plugin)
}

fn describe(plugin: &Vst3Instance, name: String) -> Metadata {
    let mut parameters = BTreeMap::new();
    for (param, value) in plugin.get_params_with_values() {
        if value.is_finite() {
            parameters
                .entry(normalize_param_name(&param.name))
                .or_insert(value);
        }
    }
    Metadata {
        name,
        latency: plugin.latency_samples(),
        tail: plugin.plugin_tail_samples(),
        sidechain: plugin.has_sidechain_input(),
        accepts_midi: false,
        output_channels: plugin.output_ports().iter().map(|port| port.channels).sum(),
        parameters: parameters.into_iter().collect(),
    }
}

fn read_pcm(stream: &mut UnixStream, bytes: &mut [u8], pcm: &mut [f32]) -> io::Result<()> {
    stream.read_exact(bytes)?;
    audio_samples(bytes, pcm)
}

pub(super) fn serve(
    mut stream: UnixStream,
    mut init: Init,
    settings: Settings,
    main: &ApplicationMainQueue,
) -> io::Result<()> {
    let rate = init.sample_rate;
    let state = match read_initial_state(&init, &mut stream) {
        Ok(state) => state,
        Err(error) => return write_header(&mut stream, &Err::<Metadata, _>(error)),
    };
    if !settings.parameters.is_empty() || init.parameters.len() > MAX_EVENTS {
        return write_header(
            &mut stream,
            &Err::<Metadata, _>("Invalid Intel VST3 insert parameters"),
        );
    }
    let mut plugin = match open_effect(&init, &settings, &state) {
        Ok(loaded) => loaded,
        Err(error) => return write_header(&mut stream, &Err::<Metadata, _>(error)),
    };
    let metadata = describe(&plugin, settings.class_name.clone().unwrap_or_default());
    let accepts_key = metadata.sidechain;
    write_header(&mut stream, &Ok::<_, String>(metadata))?;
    let mut scratch = [0; FRAMES * 8];
    let mut input = [0.0; FRAMES * 2];
    let mut key = [0.0; FRAMES * 2];
    loop {
        main.pump().map_err(io::Error::other)?;
        crate::application_main::service_offline_main().map_err(io::Error::other)?;
        let request = match read_header::<Request<'static>>(&mut stream) {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            result => result?,
        };
        match request {
            Request::Process {
                frames,
                timing,
                has_key,
                parameters,
            } => {
                if !(1..=FRAMES).contains(&frames)
                    || (has_key && !accepts_key)
                    || parameters.len() > MAX_EVENTS
                    || parameters
                        .iter()
                        .any(|point| point.offset as usize >= frames || !point.value.is_finite())
                {
                    return write_header(
                        &mut stream,
                        &Err::<(), _>("Invalid Intel VST3 insert block"),
                    );
                }
                let bytes = &mut scratch[..frames * 8];
                let input = &mut input[..frames * 2];
                read_pcm(&mut stream, bytes, input)?;
                let key = if has_key {
                    let key = &mut key[..frames * 2];
                    read_pcm(&mut stream, bytes, key)?;
                    Some(&*key)
                } else {
                    None
                };
                for point in parameters.iter() {
                    plugin.queue_param_plain_by_name(&point.name, point.value, point.offset as i32);
                }
                plugin.process_effect_with_sidechain(
                    input,
                    key,
                    rate,
                    timing.map(|timing| timing.to_transport(rate)),
                );
                let result = plugin.failure().message().map_or(Ok(()), Err);
                write_header(&mut stream, &result)?;
                if result.is_err() {
                    break;
                }
                audio_bytes(input, bytes);
                stream.write_all(bytes)?;
            }
            Request::Set { name, value } => {
                let current = plugin
                    .set_param_by_name(&name, value)
                    .then(|| plugin.get_param_by_name(&name))
                    .flatten()
                    .filter(|value| value.is_finite());
                if current.is_some() {
                    let key = normalize_param_name(&name);
                    if let Some((_, stored)) = init
                        .parameters
                        .iter_mut()
                        .find(|(existing, _)| normalize_param_name(existing) == key)
                    {
                        *stored = value;
                    } else if init.parameters.len() < MAX_EVENTS {
                        init.parameters.push((name.into_owned(), value));
                    } else {
                        return write_header(
                            &mut stream,
                            &Err::<Option<f64>, _>(
                                "Intel VST3 insert exceeds its live parameter limit",
                            ),
                        );
                    }
                }
                write_header(
                    &mut stream,
                    &plugin.failure().message().map_or(Ok(current), Err),
                )?;
            }
            Request::Reset => {
                // A processor can retain smoothing and delay history after a
                // stop/start. Reopen only this insert in the same worker.
                let result = open_effect(&init, &settings, &state).map(|fresh| plugin = fresh);
                write_header(&mut stream, &result)?;
                if result.is_err() {
                    break;
                }
            }
        }
    }
    drop(plugin);
    main.pump().map_err(io::Error::other)
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;

    fn tone(block: usize) -> Vec<f32> {
        (0..FRAMES)
            .flat_map(|frame| {
                let time = (block * FRAMES + frame) as f32 / 44_100.0;
                let value = 0.13 * (time * 580.0 * std::f32::consts::TAU).sin();
                [value, value * 0.75]
            })
            .collect()
    }

    fn assert_matches(native: &[f32], translated: &[f32]) {
        assert!(native.iter().any(|sample| sample.abs() > 0.001));
        assert_eq!(native.len(), translated.len());
        let max_delta = native
            .iter()
            .zip(translated)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_delta < 1e-4,
            "translation changed the audio by {max_delta}"
        );
    }

    #[test]
    #[ignore = "requires Rosetta, the Intel helper and dual-architecture kHs Gain"]
    fn translated_insert_matches_native_state_and_parameter_points() {
        let path = PathBuf::from(std::env::var_os("OPENCOMPOSE_TEST_VST3_GAIN").unwrap());
        let open_native =
            || Vst3Instance::load_class(&path, "kHs Gain", 44_100, FRAMES as u32).unwrap();
        let mut author = open_native();
        assert!(author.set_param_by_name("Gain", 0.25));
        let mut warmup = tone(0);
        author.process(&mut warmup, 44_100);
        let state = author.state_bytes().unwrap();

        let mut native = open_native();
        native.set_state_bytes(&state).unwrap();
        native.prepare_restored_insert(None).unwrap();
        let mut translated = BridgedVst3Insert::load(
            Vst3InsertSetup {
                path: &path,
                class_name: "kHs Gain",
                sample_rate: 44_100,
                state: Some(&state),
                parameters: Vec::new(),
                sidechain_input: None,
                transport: None,
            },
            BridgeMode::Streaming,
        )
        .unwrap();
        let mut ordinary = tone(0);
        let mut reference = ordinary.clone();
        let mut result = ordinary.clone();
        open_native().process(&mut ordinary, 44_100);
        native.process(&mut reference, 44_100);
        translated.process(&mut result, 44_100);
        assert_matches(&reference, &result);
        assert!(
            ordinary
                .iter()
                .zip(&result)
                .any(|(a, b)| (a - b).abs() > 0.01),
            "the restored state must alter the gain",
        );
        assert!(native.queue_param_at("Gain", 1.7, 256));
        assert!(translated.queue_param_at("Gain", 1.7, 256));
        let mut reference = tone(1);
        let mut result = reference.clone();
        native.process(&mut reference, 44_100);
        translated.process(&mut result, 44_100);
        assert_matches(&reference, &result);

        translated.set_param("Gain", 0.65);
        assert!((translated.get_param("Gain").unwrap() - 0.65).abs() < 1e-5);
        translated.reset();
        let mut fresh = open_native();
        fresh.set_state_bytes(&state).unwrap();
        assert!(fresh.set_param_by_name("Gain", 0.65));
        fresh.prepare_restored_insert(None).unwrap();
        let mut reference = tone(0);
        let mut result = reference.clone();
        fresh.process(&mut reference, 44_100);
        translated.process(&mut result, 44_100);
        assert_matches(&reference, &result);

        let mut invalid = vec![0.1; (FRAMES + 1) * 2];
        translated.process(&mut invalid, 44_100);
        assert!(invalid.iter().all(|&sample| sample == 0.0));
        assert!(matches!(
            translated.take_processing_failure(),
            ProcessingFailureReport::Ready(_)
        ));
        assert!(matches!(
            translated.take_processing_failure(),
            ProcessingFailureReport::Reported
        ));
    }

    #[test]
    #[ignore = "requires Rosetta, the Intel helper and dual-architecture FabFilter Pro-C 2"]
    fn translated_insert_matches_native_sidechain_and_refuses_invalid_bus() {
        let path = PathBuf::from(std::env::var_os("OPENCOMPOSE_TEST_VST3_COMPRESSOR").unwrap());
        let parameters = [
            ("Threshold", 0.4),
            ("Ratio", 0.65),
            ("Side Chain Expert Mode", 1.0),
            ("Side Chain Input", 1.0),
            ("Auto Gain", 0.0),
        ];
        let open = |index| {
            BridgedVst3Insert::load(
                Vst3InsertSetup {
                    path: &path,
                    class_name: "Pro-C 2",
                    sample_rate: 44_100,
                    state: None,
                    parameters: parameters
                        .into_iter()
                        .map(|(name, value)| (name.to_owned(), value))
                        .collect(),
                    sidechain_input: Some(index),
                    transport: None,
                },
                BridgeMode::Offline,
            )
        };
        assert!(open(1).err().unwrap().contains("sidechain_input=1"));
        let mut translated = open(0).unwrap();
        let mut native = Vst3Instance::load_class(&path, "Pro-C 2", 44_100, FRAMES as u32).unwrap();
        for (name, value) in parameters {
            assert!(native.set_param_by_name(name, value));
        }
        native.prepare_restored_insert(None).unwrap();
        let mut without_key = open(0).unwrap();
        let mut energy_with_key = 0.0;
        let mut energy_without_key = 0.0;
        for block in 0..20 {
            let mut main = tone(block);
            let mut result = main.clone();
            let mut control = main.clone();
            let key: Vec<_> = main.iter().map(|sample| *sample * 7.0).collect();
            let clock = PluginTransport::new((block * FRAMES) as u64, 44_100, 128.0);
            native.process_with_sidechain(&mut main, &key, 44_100, Some(clock.clone()));
            translated.process_with_sidechain(&mut result, &key, 44_100, Some(clock.clone()));
            without_key.process_with_transport(&mut control, 44_100, Some(clock));
            assert_matches(&main, &result);
            energy_with_key += result.iter().map(|sample| sample * sample).sum::<f32>();
            energy_without_key += control.iter().map(|sample| sample * sample).sum::<f32>();
        }
        assert!(
            energy_with_key < energy_without_key * 0.75,
            "keyed energy {energy_with_key}, unkeyed energy {energy_without_key}",
        );
    }
}
