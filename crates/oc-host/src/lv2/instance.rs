//! One hosted LV2 plugin instance: ports, audio, MIDI atoms, parameters, state.

use std::ffi::c_void;
use std::path::Path;
use std::ptr;

use super::abi::*;
use super::loader::{self, Library};
use super::metadata::{self, Plugin, PortKind};
use super::urid::UridMap as HostUridMap;
use crate::PluginTransport;
use crate::ports::AudioPort;
use oc_dsp::AudioEffect;

mod atom;
mod authored_parameters;
mod bundle_path;
use atom::{
    AtomBuffer, EventHeader, read_sequence_events, write_empty_sequence, write_output_chunk,
};

const ATOM_INPUT_CAPACITY: usize = 8192;
const ATOM_OUTPUT_CAPACITY: usize = 8192;
const MAX_PENDING_EVENTS: usize = 1024;
const STATE_MAGIC: &[u8; 8] = b"OCLV2ST1";

/// One control input exposed as a host parameter.
#[derive(Clone, Debug)]
pub struct Param {
    pub id: u32,
    pub port: u32,
    pub name: String,
    pub symbol: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub current: f64,
    /// Unit symbol the bundle names, if any.
    pub unit: Option<String>,
    /// Port group label the bundle assigns, if any.
    pub group: Option<String>,
    /// Step count for a discrete control.
    pub steps: Option<u32>,
    /// `lv2:designation`, which tells a bypass port from an ordinary control.
    pub designation: Option<String>,
}

struct ParamSlot {
    slot: usize,
    param: Param,
}

#[derive(Clone, Copy)]
enum Binding {
    AudioIn(usize),
    AudioOut(usize),
    ControlIn(usize),
    ControlOut(usize),
    AtomIn(usize),
    AtomOut(usize),
}

struct PortBinding {
    index: u32,
    binding: Binding,
}

#[derive(Clone, Copy)]
struct QueuedMidi {
    offset: u32,
    data: [u8; 3],
    len: u8,
}

struct SavedEntry {
    key: String,
    type_uri: String,
    flags: u32,
    value: Vec<u8>,
}

struct SaveContext {
    map: *const HostUridMap,
    entries: Vec<SavedEntry>,
}

struct RestoreEntry {
    key: u32,
    type_: u32,
    flags: u32,
    value: Vec<u8>,
}

struct RestoreContext {
    entries: Vec<RestoreEntry>,
}

/// A loaded LV2 plugin instance with one stereo output.
pub struct Lv2Instance {
    _library: Library,
    descriptor: *const Descriptor,
    handle: Lv2Handle,
    plugin: Plugin,
    sample_rate: u32,
    max_frames: u32,
    bindings: Vec<PortBinding>,
    audio_in: Vec<Vec<f32>>,
    audio_out: Vec<Vec<f32>>,
    control_values: Vec<f32>,
    control_out_values: Vec<f32>,
    params: Vec<ParamSlot>,
    atom_in_buffers: Vec<AtomBuffer>,
    atom_out_buffers: Vec<AtomBuffer>,
    midi_input: Option<usize>,
    midi_output: Option<usize>,
    pending: Vec<QueuedMidi>,
    dropped_events: u64,
    output: Vec<f32>,
    urid_data: Box<HostUridMap>,
    _urid_map_feature: Box<super::abi::UridMap>,
    _urid_unmap_feature: Box<UridUnmap>,
    _bounded: Box<BoundedBlockLength>,
    _features: Vec<Feature>,
    feature_ptrs: Vec<*const Feature>,
    state_interface: *const StateInterface,
    latency_slot: Option<usize>,
    reported_latency: u32,
    sequence_urid: u32,
    chunk_urid: u32,
    midi_urid: u32,
    active: bool,
    cleaned_up: bool,
    name: String,
}

// SAFETY: the instance uniquely owns its handle, port allocations and retained
// module. Moving it does not move their heap storage or expose concurrent
// access; processing, parameter writes and teardown require &mut self. The
// state-save &self entrypoint cannot be shared across threads: this type is not
// Sync. Native callbacks must obey their LV2 lifetime/threading contracts.
unsafe impl Send for Lv2Instance {}

impl Lv2Instance {
    /// Load a plugin from a bundle path and URI.
    pub fn load(
        bundle: &Path,
        plugin_uri: &str,
        sample_rate: u32,
        max_frames: u32,
    ) -> Result<Self, String> {
        let plugins = metadata::scan_bundle(bundle)?;
        let plugin = plugins
            .into_iter()
            .find(|plugin| plugin.uri == plugin_uri)
            .ok_or_else(|| {
                format!(
                    "LV2 bundle {} does not declare plugin '{plugin_uri}'",
                    bundle.display()
                )
            })?;
        Self::load_plugin(plugin, sample_rate, max_frames)
    }

    /// Instantiate an already-parsed LV2 plugin.
    pub fn load_plugin(plugin: Plugin, sample_rate: u32, max_frames: u32) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("LV2 sample rate must be positive".into());
        }
        if max_frames == 0 {
            return Err("LV2 maximum block size must be positive".into());
        }
        for feature in &plugin.required_features {
            if !supported_feature(feature) {
                return Err(format!(
                    "LV2 plugin '{}' requires unsupported feature '{feature}'",
                    plugin.uri
                ));
            }
        }
        validate_ports(&plugin)?;
        let bundle_path = bundle_path::instantiate_bundle_path(&plugin.bundle_path)
            .map_err(|error| error.to_string())?;

        let library = Library::open(&plugin.binary_path)?;
        let descriptor_fn = loader::descriptor_function(&library)?;
        // SAFETY: descriptor_fn is the documented export from library, which is
        // retained locally and then in the instance through plugin cleanup.
        let descriptor = unsafe { loader::find_descriptor(descriptor_fn, &plugin.uri)? };

        let mut urid_data = Box::new(HostUridMap::new());
        let sequence_urid = urid_data.preintern(ATOM__SEQUENCE);
        let midi_urid = urid_data.preintern(MIDI__MIDI_EVENT);
        let chunk_urid = urid_data.preintern(ATOM__CHUNK);
        for uri in [
            ATOM__FLOAT,
            ATOM__INT,
            ATOM__DOUBLE,
            ATOM__BOOL,
            ATOM__ATOM,
            STATE__INTERFACE,
        ] {
            urid_data.preintern(uri);
        }
        if sequence_urid == 0 || midi_urid == 0 || chunk_urid == 0 {
            return Err("LV2 host URID map is exhausted".into());
        }

        let urid_handle = (&mut *urid_data) as *mut HostUridMap as *mut c_void;
        let urid_map_feature = Box::new(super::abi::UridMap {
            handle: urid_handle,
            map: Some(super::urid::map_callback),
        });
        let urid_unmap_feature = Box::new(UridUnmap {
            handle: urid_handle,
            unmap: Some(super::urid::unmap_callback),
        });
        let bounded = Box::new(BoundedBlockLength {
            min_block_length: 1,
            max_block_length: max_frames,
        });
        let features = vec![
            Feature {
                uri: c"http://lv2plug.in/ns/ext/urid#map".as_ptr(),
                data: (&*urid_map_feature) as *const super::abi::UridMap as *mut c_void,
            },
            Feature {
                uri: c"http://lv2plug.in/ns/ext/urid#unmap".as_ptr(),
                data: (&*urid_unmap_feature) as *const UridUnmap as *mut c_void,
            },
            Feature {
                uri: c"http://lv2plug.in/ns/ext/bounded-block-length#boundedBlockLength".as_ptr(),
                data: (&*bounded) as *const BoundedBlockLength as *mut c_void,
            },
        ];
        let mut feature_ptrs: Vec<*const Feature> = features
            .iter()
            .map(|feature| feature as *const Feature)
            .collect();
        feature_ptrs.push(ptr::null());

        // SAFETY: find_descriptor admits a non-null, aligned descriptor under
        // the LV2 export contract; library retains its backing through cleanup.
        let instantiate = unsafe { &*descriptor }
            .instantiate
            .ok_or("LV2 descriptor has no instantiate function")?;
        // SAFETY: descriptor and instantiate belong to the retained module.
        // The path is NUL-terminated, feature_ptrs is null-terminated, and every
        // pointed-to feature/map allocation remains owned through cleanup.
        let handle = unsafe {
            instantiate(
                descriptor,
                f64::from(sample_rate),
                bundle_path.as_ptr(),
                feature_ptrs.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(format!("LV2 plugin '{}' failed to instantiate", plugin.uri));
        }

        // SAFETY: find_descriptor validated this descriptor; library is still
        // owned here and then retained by the instance until after cleanup.
        let state_pointer = unsafe {
            loader::get_extension(descriptor, c"http://lv2plug.in/ns/ext/state#interface")
        };
        let state_interface = state_pointer as *const StateInterface;

        let mut instance = Self {
            _library: library,
            descriptor,
            handle,
            name: plugin.name.clone(),
            plugin,
            sample_rate,
            max_frames,
            bindings: Vec::new(),
            audio_in: Vec::new(),
            audio_out: Vec::new(),
            control_values: Vec::new(),
            control_out_values: Vec::new(),
            params: Vec::new(),
            atom_in_buffers: Vec::new(),
            atom_out_buffers: Vec::new(),
            midi_input: None,
            midi_output: None,
            pending: Vec::with_capacity(MAX_PENDING_EVENTS),
            dropped_events: 0,
            output: vec![0.0; max_frames as usize * 2],
            urid_data,
            _urid_map_feature: urid_map_feature,
            _urid_unmap_feature: urid_unmap_feature,
            _bounded: bounded,
            _features: features,
            feature_ptrs,
            state_interface,
            latency_slot: None,
            reported_latency: 0,
            sequence_urid,
            chunk_urid,
            midi_urid,
            active: false,
            cleaned_up: false,
        };
        instance.build_ports()?;
        instance.apply_designations();
        if let Err(error) = instance.connect_all() {
            instance.cleanup_now();
            return Err(error);
        }
        Ok(instance)
    }

    fn build_ports(&mut self) -> Result<(), String> {
        let mut audio_in = 0usize;
        let mut audio_out = 0usize;
        for port in &self.plugin.ports {
            let binding = match (port.kind, port.is_input()) {
                (PortKind::Audio, true) => {
                    self.audio_in.push(vec![0.0; self.max_frames as usize]);
                    let binding = Binding::AudioIn(audio_in);
                    audio_in += 1;
                    binding
                }
                (PortKind::Audio, false) => {
                    self.audio_out.push(vec![0.0; self.max_frames as usize]);
                    let binding = Binding::AudioOut(audio_out);
                    audio_out += 1;
                    binding
                }
                (PortKind::Control, true) => {
                    let default = port.default.clamp(port.minimum, port.maximum);
                    self.control_values.push(default);
                    let slot = self.control_values.len() - 1;
                    self.params.push(ParamSlot {
                        slot,
                        param: Param {
                            id: port.index,
                            port: port.index,
                            name: port.name.clone(),
                            symbol: port.symbol.clone(),
                            min: f64::from(port.minimum),
                            max: f64::from(port.maximum),
                            default: f64::from(default),
                            current: f64::from(default),
                            unit: port.unit.clone(),
                            group: port.group.clone(),
                            steps: port.steps,
                            designation: port.designation.clone(),
                        },
                    });
                    Binding::ControlIn(slot)
                }
                (PortKind::Control, false) => {
                    self.control_out_values.push(0.0);
                    Binding::ControlOut(self.control_out_values.len() - 1)
                }
                (PortKind::Atom, true) => {
                    let mut buffer = AtomBuffer::new(ATOM_INPUT_CAPACITY);
                    write_empty_sequence(&mut buffer, self.sequence_urid);
                    let is_midi = port.supports_midi;
                    self.atom_in_buffers.push(buffer);
                    let index = self.atom_in_buffers.len() - 1;
                    if is_midi && self.midi_input.is_none() {
                        self.midi_input = Some(index);
                    }
                    Binding::AtomIn(index)
                }
                (PortKind::Atom, false) => {
                    let mut buffer = AtomBuffer::new(ATOM_OUTPUT_CAPACITY);
                    write_output_chunk(&mut buffer, self.chunk_urid);
                    let is_midi = port.supports_midi;
                    self.atom_out_buffers.push(buffer);
                    let index = self.atom_out_buffers.len() - 1;
                    if is_midi && self.midi_output.is_none() {
                        self.midi_output = Some(index);
                    }
                    Binding::AtomOut(index)
                }
                (PortKind::Cv | PortKind::Other, _) => {
                    return Err(format!(
                        "LV2 plugin '{}' has unsupported port '{}' ({}); only audio, control and atom ports are hosted",
                        self.plugin.uri, port.symbol, port.index
                    ));
                }
            };
            self.bindings.push(PortBinding {
                index: port.index,
                binding,
            });
        }
        Ok(())
    }

    fn apply_designations(&mut self) {
        for (slot, port) in self
            .plugin
            .ports
            .iter()
            .filter(|port| port.is_control() && port.is_input())
            .enumerate()
        {
            let designation = port.designation.as_deref();
            if designation == Some(LV2__ENABLED) {
                self.control_values[slot] = 1.0;
            } else if designation == Some(LV2__SAMPLE_RATE) {
                self.control_values[slot] = self.sample_rate as f32;
            }
        }
        if let Some(port) = self.plugin.ports.iter().find(|port| {
            port.is_control()
                && !port.is_input()
                && port.designation.as_deref() == Some(LV2__LATENCY)
        }) {
            let slot = self
                .plugin
                .ports
                .iter()
                .filter(|candidate| candidate.is_control() && !candidate.is_input())
                .position(|candidate| candidate.index == port.index);
            self.latency_slot = slot;
        }
    }

    fn connect_all(&mut self) -> Result<(), String> {
        // SAFETY: descriptor was admitted at load and its library is retained;
        // exclusive self access prevents teardown while the table is read.
        let Some(connect) = (unsafe { &*self.descriptor }).connect_port else {
            return Err(format!(
                "LV2 plugin '{}' has no connect_port",
                self.plugin.uri
            ));
        };
        for binding in &self.bindings {
            let pointer = match binding.binding {
                Binding::AudioIn(index) => self.audio_in[index].as_mut_ptr() as *mut c_void,
                Binding::AudioOut(index) => self.audio_out[index].as_mut_ptr() as *mut c_void,
                Binding::ControlIn(slot) => {
                    // SAFETY: build_ports records this slot immediately after
                    // appending its control value; that array is not resized.
                    unsafe { self.control_values.as_mut_ptr().add(slot) as *mut c_void }
                }
                Binding::ControlOut(slot) => {
                    // SAFETY: build_ports records an existing output-control
                    // slot, and its backing array remains fixed until cleanup.
                    unsafe { self.control_out_values.as_mut_ptr().add(slot) as *mut c_void }
                }
                Binding::AtomIn(index) => self.atom_in_buffers[index].as_mut_ptr() as *mut c_void,
                Binding::AtomOut(index) => self.atom_out_buffers[index].as_mut_ptr() as *mut c_void,
            };
            // SAFETY: handle is the live instance from this descriptor, and
            // build_ports binds each declared index to retained writable port
            // storage. This call relies on the plugin respecting port bounds;
            // AtomBuffer guarantees the required eight-byte port alignment.
            unsafe { connect(self.handle, binding.index, pointer) };
        }
        Ok(())
    }

    fn ensure_active(&mut self) -> Result<(), String> {
        if self.active {
            return Ok(());
        }
        self.connect_all()?;
        // SAFETY: the retained descriptor remains readable until module drop.
        if let Some(activate) = (unsafe { &*self.descriptor }).activate {
            // SAFETY: handle is live, every port is connected, and active=false
            // prevents activation twice without an intervening deactivation.
            unsafe { activate(self.handle) };
        }
        self.active = true;
        self.read_latency();
        Ok(())
    }

    fn run_block(&mut self, frames: u32, input: Option<&[f32]>) -> Result<(), String> {
        if frames == 0 {
            return Ok(());
        }
        if frames > self.max_frames {
            return Err(format!(
                "LV2 plugin '{}' received {frames} frames; maximum is {}",
                self.plugin.uri, self.max_frames
            ));
        }
        self.ensure_active()?;
        let frame_count = frames as usize;
        for frame in 0..frame_count {
            for (channel, buffer) in self.audio_in.iter_mut().enumerate() {
                let source_channel = channel.min(1);
                buffer[frame] = input
                    .and_then(|input| input.get(frame * 2 + source_channel))
                    .copied()
                    .unwrap_or(0.0);
            }
        }
        for buffer in &mut self.audio_out {
            buffer[..frame_count].fill(0.0);
        }
        self.write_atom_inputs(frames);
        for buffer in &mut self.atom_out_buffers {
            write_output_chunk(buffer, self.chunk_urid);
        }
        self.connect_all()?;
        // SAFETY: the live instance retains this descriptor's module, and
        // exclusive self access excludes simultaneous lifecycle operations.
        let run = unsafe { &*self.descriptor }
            .run
            .ok_or_else(|| format!("LV2 plugin '{}' has no run function", self.plugin.uri))?;
        // SAFETY: ensure_active establishes activation and connection, frames
        // is within the fixed audio-buffer capacity, and exclusive self access
        // retains all connected backing. Native code must honor its port ABI;
        // AtomBuffer supplies native atom alignment; exception containment is
        // a separate foreign-call admission responsibility.
        unsafe { run(self.handle, frames) };
        self.read_latency();

        self.output.resize(frame_count * 2, 0.0);
        match self.audio_out.len() {
            0 => self.output.fill(0.0),
            1 => {
                for frame in 0..frame_count {
                    let sample = self.audio_out[0][frame];
                    self.output[frame * 2] = sample;
                    self.output[frame * 2 + 1] = sample;
                }
            }
            _ => {
                for frame in 0..frame_count {
                    self.output[frame * 2] = self.audio_out[0][frame];
                    self.output[frame * 2 + 1] = self.audio_out[1][frame];
                }
            }
        }
        Ok(())
    }

    fn write_atom_inputs(&mut self, frames: u32) {
        for buffer in &mut self.atom_in_buffers {
            write_empty_sequence(buffer, self.sequence_urid);
        }
        let Some(index) = self.midi_input else {
            self.pending.clear();
            return;
        };
        let capacity = self.atom_in_buffers[index].len();
        let mut offset = ATOM_SEQUENCE_HEADER_SIZE;
        let pending_count = self.pending.len();
        for event_index in 0..pending_count {
            let event = self.pending[event_index];
            let event_size = ATOM_EVENT_HEADER_SIZE + 8;
            if offset + event_size > capacity {
                self.dropped_events = self.dropped_events.saturating_add(1);
                continue;
            }
            let event_frames = if frames == 0 {
                0
            } else {
                event.offset.min(frames - 1) as i64
            };
            let header = EventHeader {
                frames: event_frames,
                body: Atom {
                    size: u32::from(event.len),
                    type_: self.midi_urid,
                },
            };
            let buffer = &mut self.atom_in_buffers[index];
            // SAFETY: the capacity check reserves the complete event header
            // and padded payload, QueuedMidi has exactly three initialized
            // bytes, and its local copy cannot overlap the destination buffer.
            // Unaligned writes avoid requiring Vec<u8> alignment on Rust's side.
            unsafe {
                ptr::write_unaligned(buffer.as_mut_ptr().add(offset) as *mut EventHeader, header);
                let payload = buffer.as_mut_ptr().add(offset + ATOM_EVENT_HEADER_SIZE);
                ptr::copy_nonoverlapping(event.data.as_ptr(), payload, usize::from(event.len));
            }
            offset += event_size;
        }
        self.pending.clear();
        let sequence = AtomSequence {
            atom: Atom {
                size: (offset - ATOM_SEQUENCE_HEADER_SIZE + 8) as u32,
                type_: self.sequence_urid,
            },
            body: AtomSequenceBody { unit: 0, pad: 0 },
        };
        // SAFETY: build_ports allocates ATOM_INPUT_CAPACITY bytes, which holds
        // a complete sequence header; the mutable byte buffer is retained here.
        unsafe {
            ptr::write_unaligned(
                self.atom_in_buffers[index].as_mut_ptr() as *mut AtomSequence,
                sequence,
            );
        }
    }

    fn read_latency(&mut self) {
        if let Some(slot) = self.latency_slot
            && let Some(value) = self.control_out_values.get(slot)
        {
            self.reported_latency = value.max(0.0).round() as u32;
        }
    }

    /// Current control-input parameter list.
    pub fn params(&self) -> Vec<Param> {
        self.params.iter().map(|slot| slot.param.clone()).collect()
    }

    /// The generic panel's view of every control input this bundle exposes.
    ///
    /// Host-managed ports (sample rate, reported latency) are not user controls
    /// and are left out; `lv2:enabled` is the bundle's own bypass switch, so its
    /// current state is reported as `by_passing`.
    pub fn parameter_surface(&self) -> crate::params::ParamSurface {
        let descriptors = self
            .params()
            .into_iter()
            .filter(|param| {
                !matches!(
                    param.designation.as_deref(),
                    Some(LV2__SAMPLE_RATE) | Some(LV2__LATENCY)
                )
            })
            .map(|param| crate::params::ParamDescriptor {
                index: param.port,
                name: param.name.clone(),
                unit: param.unit.clone().unwrap_or_default(),
                min: param.min,
                max: param.max,
                current: param.current,
                display: crate::params::display_text(
                    param.current,
                    param.unit.as_deref().unwrap_or_default(),
                ),
                normalized: crate::params::normalize(param.current, param.min, param.max),
                default: Some(param.default),
                steps: param.steps,
                automatable: true,
                by_passing: param.designation.as_deref() == Some(LV2__ENABLED)
                    && param.current < 0.5,
                group: param.group.clone(),
            })
            .collect();
        crate::params::surface(descriptors)
    }

    /// Set a parameter by name or symbol; returns whether it matched.
    pub fn set_param(&mut self, name: &str, value: f64) -> bool {
        let target = normalize(name);
        let Some(index) = self.params.iter().position(|slot| {
            normalize(&slot.param.name) == target
                || normalize(&slot.param.symbol) == target
                || normalize(&format!("{}{}", slot.param.symbol, slot.param.name)) == target
        }) else {
            return false;
        };
        let value = value.clamp(self.params[index].param.min, self.params[index].param.max);
        self.control_values[self.params[index].slot] = value as f32;
        self.params[index].param.current = value;
        true
    }

    /// Alias for [`Self::set_param`].
    pub fn set_param_by_name(&mut self, name: &str, value: f64) -> bool {
        self.set_param(name, value)
    }

    /// Current value of a named parameter.
    pub fn get_param(&self, name: &str) -> Option<f64> {
        let target = normalize(name);
        self.params
            .iter()
            .find(|slot| {
                normalize(&slot.param.name) == target || normalize(&slot.param.symbol) == target
            })
            .map(|slot| slot.param.current)
    }

    /// Set a parameter at a sample offset; only offset 0 is supported.
    pub fn set_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        if sample_offset != 0 {
            return false;
        }
        self.set_param(name, value)
    }

    /// Queue a parameter write at a sample offset.
    pub fn queue_param_by_name(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        self.set_param_at(name, value, sample_offset)
    }

    /// Current value of a control output by symbol.
    pub fn control_output(&self, symbol: &str) -> Option<f32> {
        let index = self
            .plugin
            .ports
            .iter()
            .filter(|port| port.is_control() && !port.is_input())
            .position(|port| port.symbol == symbol || port.name == symbol)?;
        self.control_out_values.get(index).copied()
    }

    /// Reported latency in samples.
    pub fn latency_samples(&self) -> u32 {
        self.reported_latency
    }

    /// Number of stereo output pairs; this host reads one.
    pub fn output_count(&self) -> usize {
        usize::from(!self.audio_out.is_empty())
    }

    /// Declared audio output ports; this host reads one stereo pair.
    pub fn output_ports(&self) -> Vec<AudioPort> {
        if self.audio_out.is_empty() {
            Vec::new()
        } else {
            vec![AudioPort::STEREO]
        }
    }

    /// Declared audio input ports; this host reads one stereo pair.
    pub fn input_ports(&self) -> Vec<AudioPort> {
        if self.audio_in.is_empty() {
            Vec::new()
        } else {
            vec![AudioPort::STEREO]
        }
    }

    /// Whether the plugin is an instrument.
    pub fn is_instrument(&self) -> bool {
        self.plugin.is_instrument
    }

    /// LV2 URI of the loaded plugin.
    pub fn plugin_uri(&self) -> &str {
        &self.plugin.uri
    }

    /// Bundle directory the plugin came from.
    pub fn bundle_path(&self) -> &Path {
        &self.plugin.bundle_path
    }

    /// Instance sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Largest block the plugin accepts.
    pub fn max_block_frames(&self) -> u32 {
        self.max_frames
    }

    /// MIDI events dropped because the queue was full.
    pub fn dropped_midi_events(&self) -> u64 {
        self.dropped_events
    }

    /// Render a block and return its interleaved stereo output.
    pub fn process_block_with_transport(
        &mut self,
        frames: u32,
        _transport: Option<PluginTransport>,
    ) -> &[f32] {
        let frames = frames.min(self.max_frames);
        if let Err(error) = self.run_block(frames, None) {
            log::error!("LV2 {}: {error}", self.name);
            self.output[..frames as usize * 2].fill(0.0);
        }
        &self.output[..frames as usize * 2]
    }

    /// Process an interleaved stereo buffer.
    pub fn process_interleaved(&mut self, buffer: &mut [f32], _sample_rate: u32) {
        let total_frames = buffer.len() / 2;
        if total_frames == 0 {
            buffer.fill(0.0);
            return;
        }
        let mut position = 0usize;
        while position < total_frames {
            let frames = (total_frames - position).min(self.max_frames as usize) as u32;
            let start = position * 2;
            let end = start + frames as usize * 2;
            let result = self.run_block(frames, Some(&buffer[start..end]));
            match result {
                Ok(()) => {
                    let count = frames as usize * 2;
                    buffer[start..end].copy_from_slice(&self.output[..count]);
                }
                Err(error) => {
                    log::error!("LV2 {}: {error}", self.name);
                    buffer[start..end].fill(0.0);
                }
            }
            position += frames as usize;
        }
        for sample in &mut buffer[total_frames * 2..] {
            *sample = 0.0;
        }
    }

    /// Deactivate, reset and reactivate the plugin.
    pub fn reset_state(&mut self) {
        if !self.active {
            return;
        }
        // SAFETY: descriptor remains readable while its retained module lives.
        if let Some(deactivate) = (unsafe { &*self.descriptor }).deactivate {
            // SAFETY: active=true records prior activation; exclusive access
            // excludes run or cleanup while this live handle is deactivated.
            unsafe { deactivate(self.handle) };
        }
        self.active = false;
        self.pending.clear();
        let _ = self.ensure_active();
    }

    /// Deactivate and tear down the instance.
    pub fn end_processing(&mut self) {
        if !self.active {
            return;
        }
        // SAFETY: descriptor remains readable while its retained module lives.
        if let Some(deactivate) = (unsafe { &*self.descriptor }).deactivate {
            // SAFETY: active=true records prior activation; exclusive access
            // excludes run or cleanup while this live handle is deactivated.
            unsafe { deactivate(self.handle) };
        }
        self.active = false;
    }

    fn cleanup_now(&mut self) {
        if !self.cleaned_up
            && let Some(cleanup) = (
                // SAFETY: descriptor is retained by _library until after Drop
                // completes; cleanup has not yet invalidated the handle.
                unsafe { &*self.descriptor }
            )
            .cleanup
        {
            // SAFETY: handle came from instantiate on this descriptor and is
            // cleaned at most once; active instances are deactivated first, and
            // callback/port backing survives until this synchronous call ends.
            unsafe { cleanup(self.handle) };
        }
        self.cleaned_up = true;
    }

    /// Capture the plugin state through `state:interface`.
    pub fn state_bytes(&self) -> Result<Vec<u8>, String> {
        if self.state_interface.is_null() {
            return Err(format!(
                "LV2 plugin '{}' does not implement state:interface",
                self.plugin.uri
            ));
        }
        // SAFETY: the non-null state extension pointer is the LV2 StateInterface
        // returned by this descriptor and remains valid for the plugin lifetime.
        let interface = unsafe { &*self.state_interface };
        let save = interface
            .save
            .ok_or_else(|| "LV2 state interface has no save function".to_string())?;
        let mut context = SaveContext {
            map: &*self.urid_data,
            entries: Vec::new(),
        };
        // SAFETY: handle and feature allocations remain live through save;
        // the stack context has this callback's exact type and outlives every
        // synchronous store call. The plugin must not retain the store handle
        // or overlap callbacks that require exclusive access to that context.
        let status = unsafe {
            save(
                self.handle,
                save_store,
                &mut context as *mut SaveContext as *mut c_void,
                0,
                self.feature_ptrs.as_ptr(),
            )
        };
        if status != STATE_SUCCESS {
            return Err(format!("LV2 state save failed with status {status}"));
        }
        let mut output = Vec::new();
        output.extend_from_slice(STATE_MAGIC);
        push_u32(&mut output, context.entries.len() as u32);
        for entry in &context.entries {
            push_string(&mut output, &entry.key);
            push_string(&mut output, &entry.type_uri);
            push_u32(&mut output, entry.flags);
            push_u32(&mut output, entry.value.len() as u32);
            output.extend_from_slice(&entry.value);
        }
        Ok(output)
    }

    /// Restore plugin state through `state:interface`.
    pub fn set_state_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.state_interface.is_null() {
            return Err(format!(
                "LV2 plugin '{}' does not implement state:interface",
                self.plugin.uri
            ));
        }
        // SAFETY: the non-null state extension pointer is the LV2 StateInterface
        // returned by this descriptor and remains valid for the plugin lifetime.
        let interface = unsafe { &*self.state_interface };
        let restore = interface
            .restore
            .ok_or_else(|| "LV2 state interface has no restore function".to_string())?;
        let mut reader = StateReader::new(bytes)?;
        let count = reader.u32()?;
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let key = reader.string()?;
            let type_uri = reader.string()?;
            let flags = reader.u32()?;
            let size = reader.u32()? as usize;
            let value = reader.bytes(size)?.to_vec();
            entries.push(RestoreEntry {
                key: self.urid_data.map(&key),
                type_: self.urid_data.map(&type_uri),
                flags,
                value,
            });
        }
        let mut context = RestoreContext { entries };
        // SAFETY: exclusive instance access serializes restore with processing
        // and teardown. Context entries and feature backing remain fixed until
        // restore returns; LV2 forbids retaining retrieve or its handle beyond it.
        let status = unsafe {
            restore(
                self.handle,
                restore_retrieve,
                &mut context as *mut RestoreContext as *mut c_void,
                0,
                self.feature_ptrs.as_ptr(),
            )
        };
        if status != STATE_SUCCESS {
            return Err(format!("LV2 state restore failed with status {status}"));
        }
        Ok(())
    }

    /// MIDI events produced by the last block.
    pub fn output_midi_events(&self) -> Vec<(u32, Vec<u8>)> {
        let Some(index) = self.midi_output else {
            return Vec::new();
        };
        let Some(buffer) = self.atom_out_buffers.get(index) else {
            return Vec::new();
        };
        read_sequence_events(
            buffer,
            self.sequence_urid,
            self.midi_urid,
            (self.output.len() / 2) as u32,
        )
    }

    fn queue_midi(&mut self, data: [u8; 3], offset: u32) {
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return;
        }
        self.pending.push(QueuedMidi {
            offset,
            data,
            len: 3,
        });
    }

    /// Queue a note-on with a sample offset.
    pub fn note_on_channel(&mut self, channel: u8, note: u8, velocity: f64, offset: u32) {
        let value = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
        self.queue_midi([0x90 | (channel & 0x0f), note & 0x7f, value & 0x7f], offset);
    }

    /// Queue a note-off with a sample offset.
    pub fn note_off_channel(&mut self, channel: u8, note: u8, offset: u32) {
        self.queue_midi([0x80 | (channel & 0x0f), note & 0x7f, 0], offset);
    }

    /// Queue a control change with a sample offset.
    pub fn control_change(&mut self, channel: u8, controller: u8, value: u8, offset: u32) {
        self.queue_midi(
            [0xb0 | (channel & 0x0f), controller & 0x7f, value & 0x7f],
            offset,
        );
    }

    /// Queue a program change with a sample offset.
    pub fn program_change(&mut self, channel: u8, program: u8, offset: u32) {
        self.queue_midi([0xc0 | (channel & 0x0f), program & 0x7f, 0], offset);
    }

    /// Queue a pitch bend in the MIDI -8192..8191 range.
    pub fn pitch_bend(&mut self, channel: u8, value: i16, offset: u32) {
        let value = (i32::from(value) + 8192).clamp(0, 16383) as u16;
        self.queue_midi(
            [
                0xe0 | (channel & 0x0f),
                (value & 0x7f) as u8,
                ((value >> 7) & 0x7f) as u8,
            ],
            offset,
        );
    }

    /// Queue channel pressure at a sample offset.
    pub fn channel_pressure(&mut self, channel: u8, pressure: u8, offset: u32) {
        self.queue_midi([0xd0 | (channel & 0x0f), pressure & 0x7f, 0], offset);
    }

    /// Queue polyphonic pressure at a sample offset.
    pub fn poly_pressure(&mut self, channel: u8, note: u8, pressure: u8, offset: u32) {
        self.queue_midi(
            [0xa0 | (channel & 0x0f), note & 0x7f, pressure & 0x7f],
            offset,
        );
    }

    /// No-op: LV2 core has no per-note expression event.
    pub fn note_expression(
        &mut self,
        _channel: u8,
        _note: u8,
        _expression: &str,
        _value: f64,
        _offset: u32,
    ) {
        // LV2 core has no per-note expression event; notes still reach the plugin.
    }

    /// Latency reported by the plugin.
    pub fn reported_latency(&self) -> u32 {
        self.reported_latency
    }
}

impl Drop for Lv2Instance {
    fn drop(&mut self) {
        self.end_processing();
        self.cleanup_now();
    }
}

impl AudioEffect for Lv2Instance {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        self.process_interleaved(buffer, sample_rate);
    }

    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        _transport: Option<PluginTransport>,
    ) {
        self.process_interleaved(buffer, sample_rate);
    }

    fn set_param(&mut self, name: &str, value: f64) {
        let _ = Lv2Instance::set_param(self, name, value);
    }

    fn queue_param_at(&mut self, name: &str, value: f64, sample_offset: u32) -> bool {
        Lv2Instance::set_param_at(self, name, value, sample_offset)
    }

    fn max_block_frames(&self) -> u32 {
        self.max_frames
    }

    fn get_param(&self, name: &str) -> Option<f64> {
        Lv2Instance::get_param(self, name)
    }

    fn reset(&mut self) {
        self.reset_state();
    }

    fn end_processing(&mut self) {
        Lv2Instance::end_processing(self);
    }

    fn latency_samples(&self) -> u32 {
        self.reported_latency
    }

    fn name(&self) -> &str {
        &self.name
    }
}

fn supported_feature(feature: &str) -> bool {
    matches!(
        feature,
        URID__MAP | URID__UNMAP | BOUNDED_BLOCK_LENGTH__FEATURE | LV2__HARD_RT_CAPABLE
    )
}

fn validate_ports(plugin: &Plugin) -> Result<(), String> {
    let audio_inputs = plugin.audio_inputs();
    let audio_outputs = plugin.audio_outputs();
    if audio_inputs > 2 {
        return Err(format!(
            "LV2 plugin '{}' declares {audio_inputs} audio input channels; at most stereo is supported",
            plugin.uri
        ));
    }
    if audio_outputs > 2 {
        return Err(format!(
            "LV2 plugin '{}' declares {audio_outputs} audio output channels; at most stereo is supported",
            plugin.uri
        ));
    }
    for port in &plugin.ports {
        if matches!(port.kind, PortKind::Cv | PortKind::Other) {
            return Err(format!(
                "LV2 plugin '{}' has unsupported port '{}' ({}); only audio, control and atom ports are hosted",
                plugin.uri, port.symbol, port.index
            ));
        }
        if port.is_control() && port.minimum > port.maximum {
            return Err(format!(
                "LV2 plugin '{}' port '{}' has minimum above maximum",
                plugin.uri, port.symbol
            ));
        }
    }
    Ok(())
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// # Safety
/// The non-null handle must be the live SaveContext passed to the current save
/// call, with serialized exclusive callback access. A non-null value must expose
/// size readable bytes for this call; the plugin cannot retain the host context.
unsafe extern "C" fn save_store(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> StateStatus {
    if handle.is_null() {
        return STATE_NO_SPACE;
    }
    // SAFETY: save passes an aligned live SaveContext as this exact handle;
    // exclusive callback access is required by this callback's safety contract.
    let context = unsafe { &mut *(handle as *mut SaveContext) };
    // SAFETY: state_bytes sets map to the instance-owned allocation that lives
    // through save; this read requires no overlapping map mutation.
    let map = unsafe { &*context.map };
    let key = map
        .unmap(key)
        .map(str::to_string)
        .unwrap_or_else(|| format!("urn:oc:urid:{key}"));
    let type_uri = map
        .unmap(type_)
        .map(str::to_string)
        .unwrap_or_else(|| format!("urn:oc:urid:{type_}"));
    let bytes = if size == 0 || value.is_null() {
        Vec::new()
    } else {
        // SAFETY: the callback contract requires size initialized readable
        // bytes from the non-null value. Copy them before returning to native
        // code; no borrowed pointer is retained in SavedEntry.
        unsafe { std::slice::from_raw_parts(value as *const u8, size) }.to_vec()
    };
    context.entries.push(SavedEntry {
        key,
        type_uri,
        flags,
        value: bytes,
    });
    STATE_SUCCESS
}

/// # Safety
/// The non-null handle must be the fixed RestoreContext passed to the current
/// restore call. Each non-null output pointer must be aligned and writable for
/// its scalar type; callers may use returned value bytes only during restore.
unsafe extern "C" fn restore_retrieve(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    if handle.is_null() {
        return ptr::null();
    }
    // SAFETY: restore supplies this live aligned context and never mutates
    // its entries while native code may use their returned byte pointers.
    let context = unsafe { &*(handle as *const RestoreContext) };
    let Some(entry) = context.entries.iter().find(|entry| entry.key == key) else {
        return ptr::null();
    };
    if !size.is_null() {
        // SAFETY: a non-null size output is writable/aligned by callback ABI.
        unsafe { *size = entry.value.len() };
    }
    if !type_.is_null() {
        // SAFETY: a non-null type output is writable/aligned by callback ABI.
        unsafe { *type_ = entry.type_ };
    }
    if !flags.is_null() {
        // SAFETY: a non-null flags output is writable/aligned by callback ABI.
        unsafe { *flags = entry.flags };
    }
    entry.value.as_ptr() as *const c_void
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_string(output: &mut Vec<u8>, value: &str) {
    push_u32(output, value.len() as u32);
    output.extend_from_slice(value.as_bytes());
}

struct StateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> StateReader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < STATE_MAGIC.len() || &bytes[..STATE_MAGIC.len()] != STATE_MAGIC {
            return Err("not an OpenCompose LV2 state chunk".into());
        }
        Ok(Self {
            bytes,
            position: STATE_MAGIC.len(),
        })
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> Result<String, String> {
        let length = self.u32()? as usize;
        let bytes = self.bytes(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid state string".to_string())
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "truncated LV2 state chunk".to_string())?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalizes_parameter_names() {
        assert_eq!(super::normalize("Gain (dB)"), "gaindb");
    }
}
