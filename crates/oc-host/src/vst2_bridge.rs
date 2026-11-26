//! Out-of-process bridge for Intel plugins on Apple Silicon.
mod startup;
mod vst3;
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
pub use vst3::{BridgedVst3, BridgedVst3Insert, Vst3InsertSetup, needs_vst3_translation};

use oc_audio::{
    AudioEffect, MidiInputError, MidiInputEvent, PluginTransport, ProcessingFailureReport,
};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 3;
const VST3_VERSION: u32 = 4;
const FRAMES: usize = 512;
const MAX_OUTPUT_PAIRS: usize = crate::vst2::MAX_OUTPUT_PAIRS;
const MAX_HEADER: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_TIMEOUT: Duration = Duration::from_millis(40);
const MAX_PRE_ROLL_MS: u64 = 600_000;
const STREAMING_STATE_PRE_ROLL_SECONDS: f64 = 0.25;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BridgeMode {
    Offline,
    Streaming,
}

thread_local! {
    static MODE: Cell<Option<BridgeMode>> = const { Cell::new(None) };
}

/// Authorize synchronous IPC on an export or background render worker.
/// The audio device callback must never enter this scope.
#[must_use]
pub struct BridgeScope {
    previous: Option<BridgeMode>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl BridgeScope {
    pub fn enter(mode: BridgeMode) -> Self {
        Self {
            previous: MODE.replace(Some(mode)),
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for BridgeScope {
    fn drop(&mut self) {
        MODE.set(self.previous);
    }
}

pub fn bridge_mode() -> Option<BridgeMode> {
    MODE.get()
}

pub fn needs_translation(path: &Path) -> bool {
    if !cfg!(target_arch = "aarch64") {
        return false;
    }
    let Ok(executable) = crate::vst2::resolve_executable(path) else {
        return false;
    };
    intel_executable(&executable)
}

fn intel_executable(executable: &Path) -> bool {
    std::fs::File::open(executable).is_ok_and(|binary| intel_only_macho(binary).unwrap_or(false))
}

fn intel_only_macho(mut binary: impl Read) -> io::Result<bool> {
    const INTEL: u32 = 0x0100_0007;
    const ARM: u32 = 0x0100_000c;
    let mut header = [0; 8];
    binary.read_exact(&mut header)?;
    let magic = u32::from_be_bytes(header[..4].try_into().unwrap());
    let (word, stride): (fn([u8; 4]) -> u32, usize) = match magic {
        0xcafe_babe => (u32::from_be_bytes, 20),
        0xcafe_babf => (u32::from_be_bytes, 32),
        0xbeba_feca => (u32::from_le_bytes, 20),
        0xbfba_feca => (u32::from_le_bytes, 32),
        0xcffa_edfe => return Ok(u32::from_le_bytes(header[4..].try_into().unwrap()) == INTEL),
        0xfeed_facf => return Ok(u32::from_be_bytes(header[4..].try_into().unwrap()) == INTEL),
        _ => return Ok(false),
    };
    let slices = word(header[4..].try_into().unwrap());
    if slices > 64 {
        return Ok(false);
    }
    let mut has_intel = false;
    let mut entry = [0; 32];
    for _ in 0..slices {
        binary.read_exact(&mut entry[..stride])?;
        match word(entry[..4].try_into().unwrap()) {
            ARM => return Ok(false),
            INTEL => has_intel = true,
            _ => {}
        }
    }
    Ok(has_intel)
}

pub(crate) fn helper_path() -> Option<PathBuf> {
    let path = match std::env::var_os("OPENCOMPOSE_VST2_BRIDGE_HELPER") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()
            .ok()?
            .with_file_name("opencompose-vst2-bridge-x86_64"),
    };
    (path.is_absolute() && path.is_file()).then_some(path)
}

#[derive(Serialize, Deserialize)]
struct Init {
    version: u32,
    role: Role,
    plugin: PathBuf,
    sample_rate: u32,
    state_bytes: usize,
    parameters: Vec<(String, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    insert_transport: Option<Timing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vst3: Option<vst3::Settings>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Role {
    Insert,
    Instrument,
}

#[derive(Serialize, Deserialize)]
struct MidiPacket {
    bytes: [u8; 3],
    offset: u32,
}

impl MidiPacket {
    fn validate(&self, frames: usize) -> Result<(), MidiInputError> {
        if self.offset as usize >= frames {
            return Err(MidiInputError::InvalidOffset);
        }
        if !matches!(self.bytes[0], 0x80..=0xef) || self.bytes[1] > 127 || self.bytes[2] > 127 {
            return Err(MidiInputError::InvalidMessage);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Timing {
    sample: u64,
    tempo: f64,
    quarter: f64,
    bar_start: f64,
    bar: u32,
    numerator: u16,
    denominator: u16,
    playing: bool,
}

impl Timing {
    fn from_transport(transport: &PluginTransport) -> Self {
        let (numerator, denominator, bar_start, bar) = transport.musical_position();
        Self {
            sample: transport.sample_pos,
            tempo: transport.current_tempo(),
            quarter: transport.quarter_notes(),
            bar_start,
            bar,
            numerator,
            denominator,
            playing: transport.playing,
        }
    }

    fn to_transport(&self, rate: u32) -> PluginTransport {
        let mut stamp = PluginTransport::new(self.sample, rate, self.tempo)
            .with_time_signature(self.numerator, self.denominator)
            .with_musical_position(self.quarter, self.bar_start, self.bar);
        stamp.playing = self.playing;
        stamp
    }
}

#[derive(Serialize, Deserialize)]
enum Request {
    Process {
        frames: usize,
        timing: Option<Timing>,
        midi: Vec<MidiPacket>,
        has_key: bool,
        outputs: u32,
    },
    Parameter {
        name: String,
        value: f64,
    },
    PreRoll {
        milliseconds: u64,
    },
    Reset,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    name: String,
    latency: u32,
    tail: Option<u32>,
    sidechain: bool,
    #[serde(default)]
    accepts_midi: bool,
    output_channels: u32,
    parameters: Vec<(String, f64)>,
}

fn encode_header(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let data = serde_json::to_vec(value)?;
    if data.len() > MAX_HEADER {
        return Err(io::Error::other(
            "VST2 bridge header exceeds its byte limit",
        ));
    }
    Ok(data)
}

fn write_header(stream: &mut UnixStream, value: &impl Serialize) -> io::Result<()> {
    write_header_bytes(stream, &encode_header(value)?)
}

fn write_header_bytes(stream: &mut impl Write, data: &[u8]) -> io::Result<()> {
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(data)
}

fn read_header<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> io::Result<T> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_HEADER {
        return Err(io::Error::other(
            "VST2 bridge header exceeds its byte limit",
        ));
    }
    let mut data = vec![0; length];
    stream.read_exact(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}

fn audio_bytes(buffer: &[f32], scratch: &mut [u8]) {
    for (value, out) in buffer.iter().zip(scratch.chunks_exact_mut(4)) {
        out.copy_from_slice(&value.to_le_bytes());
    }
}

fn audio_samples(scratch: &[u8], buffer: &mut [f32]) -> io::Result<()> {
    let values = scratch
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four bytes")));
    if !values.clone().all(f32::is_finite) {
        return Err(io::Error::other("VST2 bridge transmitted non-finite PCM"));
    }
    for (sample, value) in buffer.iter_mut().zip(values) {
        *sample = value;
    }
    Ok(())
}

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn connect<T: serde::de::DeserializeOwned>(
    request: Init,
    state: &[u8],
    mode: BridgeMode,
) -> Result<(Worker, UnixStream, T), String> {
    if state.len() > crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES {
        return Err("Intel plugin state exceeds its byte limit".into());
    }
    let helper = helper_path().ok_or("The OpenCompose Intel plugin worker is missing")?;
    let header = encode_header(&request).map_err(|error| error.to_string())?;
    let key = startup::SetupKey::new(&request.plugin, &helper, &header, state);
    startup::admit(key, || {
        let mut command = Command::new("/usr/bin/arch");
        command.arg("-x86_64").arg(helper);
        connect_worker(command, &header, state, mode, TIMEOUT)
    })
    .map_err(|error| error.to_string())
}

pub(super) fn allow_setup_retry() {
    startup::allow_retry();
}

fn connect_worker<T: serde::de::DeserializeOwned>(
    mut command: Command,
    header: &[u8],
    state: &[u8],
    mode: BridgeMode,
    timeout: Duration,
) -> Result<(Worker, UnixStream, T), startup::StartupError> {
    use startup::StartupError;
    let expires = Instant::now() + timeout;
    let (mut stream, child_socket) =
        UnixStream::pair().map_err(|error| StartupError::Io("creating its socket", error))?;
    let fd: OwnedFd = child_socket.into();
    let worker = Worker(
        command
            .stdin(Stdio::from(fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| StartupError::Io("launching the helper", error))?,
    );
    let metadata = startup::exchange(&mut stream, header, state, expires)?;
    let processing_timeout = match mode {
        BridgeMode::Offline => TIMEOUT,
        BridgeMode::Streaming => STREAM_TIMEOUT,
    };
    stream
        .set_read_timeout(Some(processing_timeout))
        .and_then(|_| stream.set_write_timeout(Some(processing_timeout)))
        .map_err(|error| StartupError::Io("configuring processing", error))?;
    Ok((worker, stream, metadata))
}

pub struct BridgedVst2 {
    _worker: Worker,
    stream: UnixStream,
    metadata: Metadata,
    rate: u32,
    role: Role,
    read_timeout: Duration,
    pre_roll_until: Option<Instant>,
    midi: Vec<MidiPacket>,
    fault: Option<String>,
    reported: Cell<bool>,
    scratch: [u8; FRAMES * 8],
}

impl BridgedVst2 {
    pub fn load(
        plugin: &Path,
        sample_rate: u32,
        state: Option<&[u8]>,
        parameters: Vec<(String, f64)>,
    ) -> Result<Self, String> {
        Self::load_insert(plugin, sample_rate, state, parameters, None)
    }

    pub fn load_insert(
        plugin: &Path,
        sample_rate: u32,
        state: Option<&[u8]>,
        parameters: Vec<(String, f64)>,
        transport: Option<&PluginTransport>,
    ) -> Result<Self, String> {
        let mode = bridge_mode().ok_or("Intel VST2 inserts require a background render worker")?;
        Self::start(
            plugin,
            sample_rate,
            state,
            parameters,
            mode,
            Role::Insert,
            transport,
        )
    }

    pub fn load_instrument(
        plugin: &Path,
        sample_rate: u32,
        state: Option<&[u8]>,
        parameters: Vec<(String, f64)>,
        mode: BridgeMode,
    ) -> Result<Self, String> {
        let mut instrument = Self::start(
            plugin,
            sample_rate,
            state,
            parameters,
            mode,
            Role::Instrument,
            None,
        )?;
        if mode == BridgeMode::Streaming && state.is_some_and(|bytes| !bytes.is_empty()) {
            instrument.begin_pre_roll(STREAMING_STATE_PRE_ROLL_SECONDS)?;
        }
        Ok(instrument)
    }

    fn start(
        plugin: &Path,
        sample_rate: u32,
        state: Option<&[u8]>,
        parameters: Vec<(String, f64)>,
        mode: BridgeMode,
        role: Role,
        transport: Option<&PluginTransport>,
    ) -> Result<Self, String> {
        let state = state.unwrap_or_default();
        let request = Init {
            version: VERSION,
            role,
            plugin: plugin.canonicalize().map_err(|error| error.to_string())?,
            sample_rate,
            state_bytes: state.len(),
            parameters,
            insert_transport: transport.map(Timing::from_transport),
            vst3: None,
        };
        let (worker, stream, mut metadata): (_, _, Metadata) = connect(request, state, mode)?;
        if !(1..=MAX_OUTPUT_PAIRS as u32 * 2).contains(&metadata.output_channels) {
            return Err("Intel VST2 bridge reported an invalid output layout".into());
        }
        // An instrument already has to accept MIDI to pass load admission.
        // The default also covers instrument metadata from an older worker.
        metadata.accepts_midi |= role == Role::Instrument;
        let capacity = if metadata.accepts_midi {
            crate::vst2::MAX_PENDING_MIDI
        } else {
            0
        };
        Ok(Self {
            _worker: worker,
            stream,
            metadata,
            rate: sample_rate,
            role,
            read_timeout: match mode {
                BridgeMode::Offline => TIMEOUT,
                BridgeMode::Streaming => STREAM_TIMEOUT,
            },
            pre_roll_until: None,
            midi: Vec::with_capacity(capacity),
            fault: None,
            reported: Cell::new(false),
            scratch: [0; FRAMES * 8],
        })
    }

    fn control(&mut self, request: Request) -> Result<(), String> {
        self.await_pre_roll()?;
        write_header(&mut self.stream, &request).map_err(|e| e.to_string())?;
        read_header::<Result<(), String>>(&mut self.stream).map_err(|e| e.to_string())?
    }

    fn with_read_timeout(
        &mut self,
        timeout: Duration,
        exchange: impl FnOnce(&mut UnixStream) -> io::Result<Result<(), String>>,
    ) -> Result<(), String> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        let result = exchange(&mut self.stream);
        let restored = self.stream.set_read_timeout(Some(self.read_timeout));
        result.map_err(|error| error.to_string())??;
        restored.map_err(|error| error.to_string())
    }

    fn await_pre_roll(&mut self) -> Result<(), String> {
        if let Some(deadline) = self.pre_roll_until.take() {
            let timeout = self.read_timeout + deadline.saturating_duration_since(Instant::now());
            self.with_read_timeout(timeout, read_header::<Result<(), String>>)
                .map_err(|error| format!("Intel VST2 pre-roll: {error}"))?;
        }
        Ok(())
    }

    /// Start loading on the plugin's process main thread. The caller can
    /// prepare other sources until the instrument is first used.
    pub fn begin_pre_roll(&mut self, seconds: f64) -> Result<(), String> {
        let milliseconds = pre_roll_millis(seconds)?;
        if milliseconds == 0 {
            return Ok(());
        }
        self.await_pre_roll()?;
        self.with_read_timeout(TIMEOUT, |stream| {
            write_header(stream, &Request::PreRoll { milliseconds })?;
            read_header(stream)
        })?;
        self.pre_roll_until = Some(Instant::now() + Duration::from_millis(milliseconds));
        Ok(())
    }

    pub fn queue_midi(&mut self, bytes: [u8; 3], offset: u32) -> Result<(), String> {
        self.queue_packet(MidiPacket { bytes, offset })
            .map_err(|error| format!("Intel VST2 MIDI: {error:?}"))
    }

    fn queue_packet(&mut self, packet: MidiPacket) -> Result<(), MidiInputError> {
        if !self.metadata.accepts_midi {
            return Err(MidiInputError::Unsupported);
        }
        if self.fault.is_some() {
            return Err(MidiInputError::ProcessorFailed);
        }
        packet.validate(FRAMES)?;
        if self.midi.len() == crate::vst2::MAX_PENDING_MIDI {
            return Err(MidiInputError::CapacityExceeded);
        }
        self.midi.push(packet);
        Ok(())
    }

    pub fn change_parameter(&mut self, name: &str, value: f64) -> Result<(), String> {
        if let Some(error) = &self.fault {
            return Err(error.clone());
        }
        self.control(Request::Parameter {
            name: name.into(),
            value,
        })
    }

    pub fn plugin_tail(&self) -> Option<u32> {
        self.metadata.tail
    }

    pub fn output_count(&self) -> usize {
        self.metadata.output_channels.div_ceil(2) as usize
    }

    pub fn output_ports(&self) -> Vec<crate::ports::AudioPort> {
        crate::ports::ports_from_unsigned(&[self.metadata.output_channels])
    }

    /// Process once and append the requested pairs to pre-reserved storage.
    pub fn render_instrument_outputs_into(
        &mut self,
        frames: usize,
        transport: &PluginTransport,
        pairs: &[usize],
        outputs: &mut [Vec<f32>],
    ) -> Result<(), String> {
        if let Some(error) = &self.fault {
            return Err(error.clone());
        }
        if self.role != Role::Instrument
            || frames > FRAMES
            || pairs.is_empty()
            || pairs.len() != outputs.len()
            || outputs
                .iter()
                .any(|out| out.capacity() - out.len() < frames * 2)
        {
            return Err("Intel VST2 bridge received an invalid output block".into());
        }
        let mut mask = 0;
        let mut slots = [0; MAX_OUTPUT_PAIRS];
        let mut starts = [0; MAX_OUTPUT_PAIRS];
        for (slot, (&pair, output)) in pairs.iter().zip(outputs.iter()).enumerate() {
            if pair >= self.output_count() || mask & (1 << pair) != 0 {
                return Err("Intel VST2 bridge output pair is unavailable or duplicated".into());
            }
            mask |= 1 << pair;
            slots[pair] = slot;
            starts[slot] = output.len();
        }
        if frames == 0 {
            return Ok(());
        }
        let result = self
            .exchange(frames, None, None, Some(transport), mask)
            .and_then(|()| {
                let mut pending = mask;
                while pending != 0 {
                    let slot = slots[pending.trailing_zeros() as usize];
                    pending &= pending - 1;
                    let out = &mut outputs[slot];
                    let start = starts[slot];
                    out.resize(start + frames * 2, 0.0);
                    self.read_audio(&mut out[start..])?;
                }
                Ok(())
            });
        if let Err(error) = &result {
            for (output, start) in outputs.iter_mut().zip(starts) {
                output.truncate(start);
            }
            self.fault = Some(error.clone());
        }
        result
    }

    pub fn render_instrument(
        &mut self,
        buffer: &mut [f32],
        transport: PluginTransport,
    ) -> Result<(), String> {
        self.process_with_transport(buffer, self.rate, Some(transport));
        self.fault.clone().map_or(Ok(()), Err)
    }

    fn process_block(
        &mut self,
        buffer: &mut [f32],
        key: Option<&[f32]>,
        transport: Option<PluginTransport>,
    ) -> Result<(), String> {
        if buffer.len() > FRAMES * 2 || !buffer.len().is_multiple_of(2) {
            return Err("Intel VST2 bridge received an invalid stereo block".into());
        }
        if key.is_some_and(|key| !self.metadata.sidechain || key.len() != buffer.len()) {
            return Err(
                "Intel VST2 bridge requires an available key input and matching stereo frames"
                    .into(),
            );
        }
        self.exchange(
            buffer.len() / 2,
            (self.role == Role::Insert).then_some(&*buffer),
            key,
            transport.as_ref(),
            1,
        )?;
        self.read_audio(buffer)
    }

    fn exchange(
        &mut self,
        frames: usize,
        input: Option<&[f32]>,
        key: Option<&[f32]>,
        transport: Option<&PluginTransport>,
        outputs: u32,
    ) -> Result<(), String> {
        self.await_pre_roll()?;
        let timing = transport.map(Timing::from_transport);
        let bytes = &mut self.scratch[..frames * 8];
        let request = Request::Process {
            frames,
            timing,
            midi: std::mem::take(&mut self.midi),
            has_key: key.is_some(),
            outputs,
        };
        let sent = write_header(&mut self.stream, &request);
        if let Request::Process { mut midi, .. } = request {
            midi.clear();
            self.midi = midi;
        }
        sent.map_err(|e| e.to_string())?;
        if let Some(input) = input {
            audio_bytes(input, bytes);
            self.stream.write_all(bytes).map_err(|e| e.to_string())?;
        }
        if let Some(key) = key {
            audio_bytes(key, bytes);
            self.stream.write_all(bytes).map_err(|e| e.to_string())?;
        }
        read_header::<Result<(), String>>(&mut self.stream).map_err(|e| e.to_string())?
    }

    fn read_audio(&mut self, buffer: &mut [f32]) -> Result<(), String> {
        let bytes = &mut self.scratch[..buffer.len() * 4];
        self.stream
            .read_exact(bytes)
            .and_then(|_| audio_samples(bytes, buffer))
            .map_err(|e| e.to_string())
    }
}

impl AudioEffect for BridgedVst2 {
    fn midi_input_capacity(&self) -> usize {
        if self.metadata.accepts_midi {
            crate::vst2::MAX_PENDING_MIDI
        } else {
            0
        }
    }

    fn queue_midi_input(&mut self, event: MidiInputEvent) -> Result<(), MidiInputError> {
        self.queue_packet(MidiPacket {
            bytes: event.message.bytes()?,
            offset: event.sample_offset,
        })
    }

    fn name(&self) -> &str {
        &self.metadata.name
    }

    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_with_transport(buffer, sample_rate, None);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_input(buffer, None, rate, transport);
    }

    fn supports_sidechain(&self) -> bool {
        self.metadata.sidechain
    }

    fn process_with_sidechain(
        &mut self,
        buffer: &mut [f32],
        key: &[f32],
        rate: u32,
        transport: Option<PluginTransport>,
    ) {
        self.process_with_input(buffer, Some(key), rate, transport);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        if self.fault.is_none() {
            self.fault = self.change_parameter(name, value).err();
            if self.fault.is_none()
                && let Some((_, v)) = self
                    .metadata
                    .parameters
                    .iter_mut()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
            {
                *v = value;
            }
        }
    }
    fn get_param(&self, name: &str) -> Option<f64> {
        self.metadata
            .parameters
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
    fn reset(&mut self) {
        self.midi.clear();
        if self.fault.is_none() {
            self.fault = self.control(Request::Reset).err();
        }
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
    fn processing_failed(&self) -> bool {
        self.fault.is_some()
    }
    fn take_processing_failure(&self) -> ProcessingFailureReport {
        match &self.fault {
            None => ProcessingFailureReport::Unavailable,
            Some(_) if self.reported.replace(true) => ProcessingFailureReport::Reported,
            Some(detail) => ProcessingFailureReport::Ready(format!("Intel VST2 bridge: {detail}")),
        }
    }
}

impl BridgedVst2 {
    fn process_with_input(
        &mut self,
        buffer: &mut [f32],
        key: Option<&[f32]>,
        rate: u32,
        transport: Option<PluginTransport>,
    ) {
        if self.fault.is_none() && !buffer.is_empty() {
            self.fault = if rate == self.rate {
                self.process_block(buffer, key, transport).err()
            } else {
                Some("Bridge sample rate changed after preparation".into())
            };
        }
    }
}

fn load_plugin(
    init: Init,
    stream: &mut UnixStream,
) -> Result<(crate::vst2::Vst2Instance, Metadata), String> {
    if init.version != VERSION
        || !(8_000..=192_000).contains(&init.sample_rate)
        || init.state_bytes > crate::plugin_state_io::MAX_PLUGIN_STATE_BYTES
        || !init.plugin.is_absolute()
    {
        return Err("Invalid Intel VST2 bridge setup".into());
    }
    let mut state = vec![0; init.state_bytes];
    stream
        .read_exact(&mut state)
        .map_err(|error| error.to_string())?;
    let mut plugin =
        crate::vst2::Vst2Instance::load(&init.plugin, init.sample_rate, FRAMES as u32)?;
    if !state.is_empty() {
        plugin.set_state_bytes(&state)?;
    }
    let prepare_insert =
        init.role == Role::Insert && (!state.is_empty() || !init.parameters.is_empty());
    for (name, value) in init.parameters {
        if !value.is_finite() || !plugin.set_param_by_name(&name, value) {
            return Err(format!("Invalid VST2 bridge control: {name}"));
        }
    }
    if prepare_insert {
        let transport = init
            .insert_transport
            .as_ref()
            .map(|stamp| stamp.to_transport(init.sample_rate));
        plugin.prepare_restored_insert(transport.as_ref())?;
    } else {
        plugin
            .prepare_block_buffers(FRAMES)
            .map_err(|_| "Intel VST2 plugin reported an unsupported audio layout")?;
    }
    match init.role {
        Role::Insert if !plugin.supports_insert_layout() => {
            return Err(
                "The Intel VST2 bridge requires a mono or stereo insert with up to one stereo key input"
                    .into(),
            );
        }
        Role::Instrument if !plugin.is_bridge_instrument() => {
            return Err(
                "The Intel VST2 instrument must accept MIDI and provide audio output".into(),
            );
        }
        _ => {}
    }
    let metadata = Metadata {
        name: plugin.name().to_string(),
        latency: plugin.latency_samples(),
        tail: plugin.plugin_tail_samples(),
        sidechain: init.role == Role::Insert && plugin.supports_sidechain(),
        accepts_midi: plugin.midi_input_capacity() > 0,
        output_channels: plugin.num_outputs() as u32,
        parameters: plugin
            .get_params()
            .into_iter()
            .map(|p| (p.name, p.value))
            .collect(),
    };
    Ok((plugin, metadata))
}

fn pre_roll_millis(seconds: f64) -> Result<u64, String> {
    if !(0.0..=MAX_PRE_ROLL_MS as f64 / 1000.0).contains(&seconds) {
        return Err("Intel VST2 pre-roll must be finite and within 0..600 seconds".into());
    }
    Ok((seconds * 1000.0).ceil() as u64)
}

fn finish_pre_roll(
    plugin: &mut crate::vst2::Vst2Instance,
    duration: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + duration;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        crate::application_main::service_offline_main()?;
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
    plugin.reset();
    if plugin.processing_failed() {
        return Err("Intel VST2 instrument failed to reset after pre-roll".into());
    }
    Ok(())
}

/// Run on the main thread of the translated worker. The inherited socket is
/// separate from stdout and stderr, which native plugins may write to.
pub fn run_helper() -> io::Result<()> {
    if !cfg!(target_arch = "x86_64") {
        return Err(io::Error::other("VST2 bridge helper must be Intel"));
    }
    let mut stream = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut init: Init = read_header(&mut stream)?;
    if let Some(settings) = init.vst3.take() {
        return vst3::serve(stream, init, settings);
    }
    use crate::application_main::{ApplicationMainQueue, ApplicationOfflineMainScope, MainRuntime};
    // SAFETY: The Intel helper entry point calls this on its process main thread.
    let _main = unsafe { ApplicationMainQueue::register_runtime_current(MainRuntime::CommandLine) }
        .map_err(io::Error::other)?;
    let _native_events = ApplicationOfflineMainScope::enter().map_err(io::Error::other)?;
    let rate = init.sample_rate;
    let role = init.role;
    let (mut plugin, metadata) = match load_plugin(init, &mut stream) {
        Ok(loaded) => loaded,
        Err(error) => return write_header(&mut stream, &Err::<Metadata, _>(error)),
    };
    let accepts_key = metadata.sidechain;
    let accepts_midi = metadata.accepts_midi;
    let output_count = metadata.output_channels.div_ceil(2);
    write_header(&mut stream, &Ok::<_, String>(metadata))?;
    let mut scratch = [0; FRAMES * 8];
    let mut samples = [0.0; FRAMES * 2];
    let mut key_samples = [0.0; FRAMES * 2];
    let mut has_started = false;
    loop {
        let request = match read_header::<Request>(&mut stream) {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            result => result?,
        };
        let result = match request {
            Request::Process {
                frames,
                timing,
                midi,
                has_key,
                outputs,
            } if (1..=FRAMES).contains(&frames) => {
                has_started = true;
                if outputs == 0
                    || outputs.leading_zeros() < u32::BITS - output_count
                    || (role == Role::Insert && outputs != 1)
                {
                    return write_header(
                        &mut stream,
                        &Err::<(), _>("Invalid VST2 bridge output selection"),
                    );
                }
                let bytes = &mut scratch[..frames * 8];
                let data = &mut samples[..frames * 2];
                if role == Role::Insert {
                    stream.read_exact(bytes)?;
                    audio_samples(bytes, data)?;
                }
                let key = if has_key {
                    if !accepts_key {
                        return write_header(
                            &mut stream,
                            &Err::<(), _>("VST2 bridge key input is unavailable"),
                        );
                    }
                    let key = &mut key_samples[..frames * 2];
                    stream.read_exact(bytes)?;
                    audio_samples(bytes, key)?;
                    Some(&*key)
                } else {
                    None
                };
                if (!accepts_midi && !midi.is_empty())
                    || midi.len() > crate::vst2::MAX_PENDING_MIDI
                    || midi.iter().any(|packet| packet.validate(frames).is_err())
                {
                    return write_header(
                        &mut stream,
                        &Err::<(), _>("Invalid VST2 bridge MIDI block"),
                    );
                }
                for packet in midi {
                    let [status, data1, data2] = packet.bytes;
                    plugin.queue_midi(status, data1, data2, packet.offset as i32);
                }
                let transport = timing.map(|stamp| stamp.to_transport(rate));
                let result = match (role, key) {
                    (Role::Insert, Some(key)) => {
                        plugin.process_effect_with_key_into(data, key, rate, transport)
                    }
                    (Role::Insert, None) => plugin.process_effect_into(data, rate, transport),
                    (Role::Instrument, _) => plugin.process_block_into(data, transport),
                }
                .map_err(|error| error.to_string());
                write_header(&mut stream, &result)?;
                if result.is_err() {
                    break;
                }
                let mut pending = outputs;
                while pending != 0 {
                    let pair = pending.trailing_zeros();
                    pending &= pending - 1;
                    if pair != 0 {
                        plugin.read_output_pair(pair as usize, data);
                    }
                    audio_bytes(data, bytes);
                    stream.write_all(bytes)?;
                }
                continue;
            }
            Request::Process { .. } => Err("Invalid VST2 bridge frame count".to_string()),
            Request::Parameter { name, value } => {
                if value.is_finite() && plugin.set_param_by_name(&name, value) {
                    Ok(())
                } else {
                    Err(format!("Invalid VST2 bridge control: {name}"))
                }
            }
            Request::PreRoll { milliseconds }
                if role == Role::Instrument
                    && !has_started
                    && (1..=MAX_PRE_ROLL_MS).contains(&milliseconds) =>
            {
                has_started = true;
                // The first audio call can start an asynchronous sample load.
                // Discard its PCM and send no authored transport or MIDI.
                let primed = plugin
                    .process_block_into(&mut samples[..FRAMES], None)
                    .map_err(|error| error.to_string());
                write_header(&mut stream, &primed)?;
                if primed.is_err() {
                    break;
                }
                finish_pre_roll(&mut plugin, Duration::from_millis(milliseconds))
            }
            Request::PreRoll { .. } => Err("Invalid Intel VST2 pre-roll request".into()),
            Request::Reset => {
                plugin.reset();
                if plugin.processing_failed() {
                    Err("VST2 bridge reset failed".to_string())
                } else {
                    Ok(())
                }
            }
        };
        write_header(&mut stream, &result)?;
        if result.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires Rosetta, the Intel helper and a cross-built four-input fixture"]
    fn translated_vst2_key_pair_reaches_the_plugin_without_changing_the_main_pair() {
        let fixture = std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_KEY").unwrap();
        let _scope = BridgeScope::enter(BridgeMode::Offline);
        let mut effect = BridgedVst2::load(Path::new(&fixture), 48_000, None, Vec::new()).unwrap();
        assert!(effect.supports_sidechain());
        let input = [0.125, -0.25, 0.25, 0.125];
        let key = [0.5, 0.25, -0.125, -0.5];
        let mut with_key = input;
        effect.process_with_sidechain(&mut with_key, &key, 48_000, None);
        assert_eq!(with_key, [0.625, 0.0, 0.125, -0.375]);
        let mut main_only = input;
        effect.process(&mut main_only, 48_000);
        assert_eq!(main_only, input);
        assert!(!effect.processing_failed());
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires Rosetta, the Intel helper and a cross-built multi-output instrument"]
    fn translated_vst2_returns_requested_pairs_from_one_processing_call() {
        let fixture = std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_MULTI").unwrap();
        let mut instrument = BridgedVst2::load_instrument(
            Path::new(&fixture),
            48_000,
            None,
            Vec::new(),
            BridgeMode::Offline,
        )
        .unwrap();
        assert_eq!(instrument.output_count(), 4);
        assert_eq!(instrument.output_ports()[0].channels, 8);
        let transport = PluginTransport::new(0, 48_000, 120.0);
        let mut outputs = [Vec::with_capacity(8), Vec::with_capacity(8)];
        instrument.queue_midi([0x90, 60, 127], 0).unwrap();
        instrument
            .render_instrument_outputs_into(2, &transport, &[3, 0], &mut outputs)
            .unwrap();
        assert_eq!(outputs[0], [98.0 / 256.0, 114.0 / 256.0].repeat(2));
        assert_eq!(outputs[1], [2.0 / 256.0, 18.0 / 256.0].repeat(2));
        assert!(
            instrument
                .render_instrument_outputs_into(2, &transport, &[3, 3], &mut outputs)
                .is_err()
        );
        assert!(
            instrument
                .render_instrument_outputs_into(2, &transport, &[4, 0], &mut outputs)
                .is_err()
        );
        assert_eq!(outputs.each_ref().map(Vec::len), [4, 4]);
        assert!(!instrument.processing_failed());
        instrument
            .render_instrument_outputs_into(2, &transport.at_sample(2), &[3, 0], &mut outputs)
            .unwrap();
        assert_eq!(&outputs[0][4..], [100.0 / 256.0, 116.0 / 256.0].repeat(2));
        assert_eq!(&outputs[1][4..], [4.0 / 256.0, 20.0 / 256.0].repeat(2));

        let before = outputs.clone();
        for output in &mut outputs {
            output.reserve(4);
        }
        instrument.change_parameter("DC Offset", 1.0).unwrap();
        assert!(
            instrument
                .render_instrument_outputs_into(2, &transport.at_sample(4), &[3, 0], &mut outputs)
                .is_err()
        );
        assert_eq!(outputs, before);
        assert!(instrument.processing_failed());
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires Rosetta, the Intel helper and a cross-built four-input fixture"]
    fn translated_vst2_insert_routes_midi_and_sidechain_in_the_same_block() {
        use oc_audio::MidiInputMessage;

        let fixture = std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_KEY").unwrap();
        let _scope = BridgeScope::enter(BridgeMode::Streaming);
        let mut effect = BridgedVst2::load(Path::new(&fixture), 48_000, None, Vec::new()).unwrap();
        assert_eq!(effect.midi_input_capacity(), crate::vst2::MAX_PENDING_MIDI);
        let cc = MidiInputEvent {
            sample_offset: 0,
            message: MidiInputMessage::ControlChange {
                channel: 2,
                controller: 7,
                value: 0,
            },
        };
        assert_eq!(
            effect.queue_midi_input(MidiInputEvent {
                sample_offset: FRAMES as u32,
                ..cc
            }),
            Err(MidiInputError::InvalidOffset)
        );
        assert_eq!(
            oc_test_support::operations(|| effect.queue_midi_input(cc).unwrap()),
            0
        );
        let mut audio = [0.125, -0.25, 0.25, 0.125];
        let key = [0.5, 0.25, -0.125, -0.5];
        effect.process_with_sidechain(&mut audio, &key, 48_000, None);
        assert_eq!(audio, [0.0; 4]);
        assert!(!effect.processing_failed());

        effect
            .queue_midi_input(MidiInputEvent {
                message: MidiInputMessage::ControlChange {
                    channel: 2,
                    controller: 7,
                    value: 127,
                },
                ..cc
            })
            .unwrap();
        let mut audio = [0.125, -0.25, 0.25, 0.125];
        effect.process_with_sidechain(&mut audio, &key, 48_000, None);
        assert_eq!(audio, [1.25, 0.0, 0.25, -0.75]);
        assert!(!effect.processing_failed());
    }

    #[test]
    fn midi_packet_admits_only_channel_messages_within_the_block() {
        let mut packet = MidiPacket {
            bytes: [0x92, 60, 127],
            offset: 3,
        };
        assert_eq!(packet.validate(4), Ok(()));
        assert_eq!(packet.validate(3), Err(MidiInputError::InvalidOffset));
        packet.bytes[0] = 0xf0;
        assert_eq!(packet.validate(4), Err(MidiInputError::InvalidMessage));
        packet.bytes = [0x92, 128, 127];
        assert_eq!(packet.validate(4), Err(MidiInputError::InvalidMessage));
    }

    #[test]
    fn pre_roll_rejects_unbounded_time_and_does_not_shorten_the_requested_interval() {
        for seconds in [-0.0001, f64::NAN, f64::INFINITY, 601.0] {
            assert!(pre_roll_millis(seconds).is_err());
        }
        assert_eq!(pre_roll_millis(0.0), Ok(0));
        assert_eq!(pre_roll_millis(0.0001), Ok(1));
        assert_eq!(pre_roll_millis(600.0), Ok(MAX_PRE_ROLL_MS));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    #[ignore = "requires Rosetta, the Intel helper and a sample instrument with its saved state"]
    fn translated_pre_roll_loads_the_saved_samples_before_the_first_note() {
        let plugin = std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_SAMPLER").unwrap();
        let state_path = std::env::var_os("OPENCOMPOSE_TEST_INTEL_VST2_SAMPLER_STATE").unwrap();
        let state = std::fs::read(state_path).unwrap();
        let mut instrument = BridgedVst2::load_instrument(
            Path::new(&plugin),
            44_100,
            Some(&state),
            Vec::new(),
            BridgeMode::Offline,
        )
        .unwrap();
        instrument.begin_pre_roll(4.0).unwrap();

        let transport = PluginTransport::new(0, 44_100, 156.0);
        let mut output = [Vec::with_capacity(FRAMES * 2)];
        instrument.queue_midi([0x90, 36, 108], 64).unwrap();
        instrument
            .render_instrument_outputs_into(FRAMES, &transport, &[0], &mut output)
            .unwrap();
        assert!(output[0][..128].iter().all(|sample| sample.abs() < 1e-5));
        assert!(output[0][128..].iter().any(|sample| sample.abs() > 0.001));
    }

    #[test]
    fn metadata_from_an_older_worker_does_not_advertise_insert_midi() {
        let metadata: Metadata = serde_json::from_str(
            r#"{"name":"VST2","latency":0,"tail":null,"sidechain":true,"output_channels":2,"parameters":[]}"#,
        )
        .unwrap();
        assert!(!metadata.accepts_midi);
    }

    #[test]
    fn stereo_pcm_rejects_non_finite_data() {
        let mut bytes = [0; 8];
        let mut out = [0.0; 2];
        audio_bytes(&[0.5, -0.125], &mut bytes);
        audio_samples(&bytes, &mut out).unwrap();
        assert_eq!(out, [0.5, -0.125]);
        audio_bytes(&[0.5, f32::NAN], &mut bytes);
        assert!(audio_samples(&bytes, &mut out).is_err());
    }

    #[test]
    fn protocol_rejects_oversized_header_before_allocation() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&((MAX_HEADER + 1) as u32).to_le_bytes())
            .unwrap();
        assert!(read_header::<Request>(&mut receiver).is_err());
    }

    #[test]
    fn musical_position_keeps_the_remote_bar_and_ppq() {
        let block = PluginTransport::new(192_000, 48_000, 120.0)
            .with_time_signature(7, 8)
            .with_musical_position(13.25, 10.0, 3);
        assert_eq!(block.quarter_notes(), 13.25);
        assert_eq!(block.musical_position(), (7, 8, 10.0, 3));
        let next_bar = block.at_sample(198_000);
        assert_eq!(next_bar.quarter_notes(), 13.5);
        assert_eq!(next_bar.musical_position(), (7, 8, 13.5, 4));
    }

    #[test]
    fn arm_slice_takes_precedence_in_a_universal_macho() {
        let mut fat = vec![0xca, 0xfe, 0xba, 0xbf, 0, 0, 0, 2];
        let push_slice = |table: &mut Vec<u8>, cpu: u32| {
            let start = table.len();
            table.extend_from_slice(&cpu.to_be_bytes());
            table.resize(start + 32, 0);
        };
        push_slice(&mut fat, 0x0100_0007);
        assert!(intel_only_macho(fat.as_slice()).is_err());
        push_slice(&mut fat, 7);
        assert!(intel_only_macho(fat.as_slice()).unwrap());
        fat[40..44].copy_from_slice(&0x0100_000c_u32.to_be_bytes());
        assert!(!intel_only_macho(fat.as_slice()).unwrap());
    }

    #[test]
    fn bridge_admission_is_scoped_and_thread_local() {
        assert_eq!(bridge_mode(), None);
        {
            let _scope = BridgeScope::enter(BridgeMode::Offline);
            assert_eq!(bridge_mode(), Some(BridgeMode::Offline));
            assert_eq!(std::thread::spawn(bridge_mode).join().unwrap(), None);
            {
                let _scope = BridgeScope::enter(BridgeMode::Streaming);
                assert_eq!(bridge_mode(), Some(BridgeMode::Streaming));
            }
            assert_eq!(bridge_mode(), Some(BridgeMode::Offline));
        }
        assert_eq!(bridge_mode(), None);
    }
}
