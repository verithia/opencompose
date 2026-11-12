use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

use super::*;
pub(super) mod editor_handler;
use vst3::Steinberg::{
    FIDString, FUnknown, TBool, TUID,
    Vst::{
        BusDirection, BusInfo_::BusFlags_, BusTypes_, IComponentHandlerTrait, IProgramListData,
        IProgramListDataTrait, IUnitInfo, IUnitInfoTrait, IoMode, MediaType, ProgramListID,
        ProgramListInfo, RoutingInfo, SpeakerArr, SpeakerArrangement, String128, TChar, UnitID,
        UnitInfo,
    },
    int32, kResultFalse, tresult, uint32,
};

#[cfg(target_os = "windows")]
const PLAYING_FLAG: u32 = StatesAndFlags_::kPlaying as u32;
#[cfg(not(target_os = "windows"))]
const PLAYING_FLAG: u32 = StatesAndFlags_::kPlaying;

/// Program list the fake plugin advertises through `IUnitInfo`.
pub const FAKE_PROGRAM_LIST_ID: i32 = 1;
/// Program names, in list order. The first renders silence and the second
/// renders a positive signal, so a test can prove which one was selected.
pub const FAKE_PROGRAM_NAMES: [&str; 2] = ["Init", "Bright"];
/// Normalized parameter the fake stores for each program.
pub const FAKE_PROGRAM_VALUES: [f64; 2] = [0.0, 0.75];

/// Shared observation points for the in-process fake plugin.
pub struct FakeControl {
    pub bus_count: AtomicUsize,
    pub state_bus_count: AtomicUsize,
    /// Declared stereo input buses; instruments declare none.
    pub input_bus_count: AtomicUsize,
    pub last_note_channel: AtomicI32,
    pub bus_queries: AtomicUsize,
    pub process_calls: AtomicUsize,
    /// Encode the process ordinal as quiet PCM for subprocess pipeline tests.
    pub encode_process_ordinal: AtomicBool,
    /// Fixed output attenuation for guarded CLI export fixtures.
    pub output_scale_bits: AtomicU32,
    pub input_events_null: AtomicBool,
    /// Peak of every declared input bus in the last process call.
    pub input_bus_peaks: Mutex<Vec<f32>>,
    /// Record `input_bus_peaks`; off lets an allocation test use a
    /// process call that itself does not allocate.
    pub track_input_peaks: AtomicBool,
    /// Sine frequency for the main output; 0 keeps the fixed test values.
    pub tone_hz: AtomicU32,
    pub param_changes: Mutex<Vec<CapturedParamChange>>,
    /// Process context of every process call, in call order.
    pub contexts: Mutex<Vec<CapturedContext>>,
    /// Normalized value the fake controller and processor share.
    pub param_value: Mutex<f64>,
    /// `IComponentHandler` the fake controller was given.
    pub last_handler: AtomicUsize,
    editor_handler: Mutex<Option<Arc<editor_handler::Registration>>>,
    /// Copy input bus 0 to the main output, the shape of an insert effect
    /// that generates nothing of its own.
    pub copy_input: AtomicBool,
    /// Render only while at least one note is held, the shape of a hosted
    /// instrument with no release tail.
    pub note_gated: AtomicBool,
    /// Notes currently held, counted from the event list of each block.
    pub active_notes: AtomicUsize,
    /// Tail the fake reports through `getTailSamples`.
    pub tail_samples: AtomicU32,
    /// Keyswitches this controller reports; 0 means it exposes none.
    pub keyswitch_count: AtomicUsize,
    /// Note expressions this controller reports.
    pub note_expression_count: AtomicUsize,
    /// Result `IEditController::setComponentState` returns.
    pub controller_state_result: AtomicI32,
    /// `getParameterInfo` calls, so a test can prove name lookups are cached.
    pub param_info_queries: AtomicUsize,
}

/// One `ProcessContext` a fake plugin observed, reduced to timing fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapturedContext {
    pub process_call: usize,
    pub num_samples: u32,
    pub project_time_samples: i64,
    pub project_time_music: f64,
    pub tempo: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub playing: bool,
}

/// One parameter point a fake plugin observed in `ProcessData`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapturedParamChange {
    pub process_call: usize,
    pub param_id: ParamID,
    pub sample_offset: i32,
    pub value: f64,
}

impl Default for FakeControl {
    fn default() -> Self {
        Self {
            bus_count: AtomicUsize::new(1),
            state_bus_count: AtomicUsize::new(1),
            input_bus_count: AtomicUsize::new(0),
            last_note_channel: AtomicI32::new(-1),
            bus_queries: AtomicUsize::new(0),
            process_calls: AtomicUsize::new(0),
            encode_process_ordinal: AtomicBool::new(false),
            output_scale_bits: AtomicU32::new(1.0f32.to_bits()),
            input_events_null: AtomicBool::new(true),
            input_bus_peaks: Mutex::new(Vec::new()),
            track_input_peaks: AtomicBool::new(true),
            tone_hz: AtomicU32::new(0),
            param_changes: Mutex::new(Vec::new()),
            contexts: Mutex::new(Vec::new()),
            param_value: Mutex::new(0.5),
            last_handler: AtomicUsize::new(0),
            editor_handler: Mutex::new(None),
            copy_input: AtomicBool::new(false),
            note_gated: AtomicBool::new(false),
            active_notes: AtomicUsize::new(0),
            tail_samples: AtomicU32::new(0),
            keyswitch_count: AtomicUsize::new(0),
            note_expression_count: AtomicUsize::new(0),
            controller_state_result: AtomicI32::new(0),
            param_info_queries: AtomicUsize::new(0),
        }
    }
}

struct FakePlugin {
    control: Arc<FakeControl>,
    channels: i32,
}

impl Class for FakePlugin {
    type Interfaces = (IComponent, IAudioProcessor, IUnitInfo, IProgramListData);
}

impl IUnitInfoTrait for FakePlugin {
    unsafe fn getUnitCount(&self) -> int32 {
        0
    }

    unsafe fn getUnitInfo(&self, _unit_index: int32, _info: *mut UnitInfo) -> tresult {
        kResultFalse
    }

    unsafe fn getProgramListCount(&self) -> int32 {
        1
    }

    unsafe fn getProgramListInfo(&self, list_index: int32, info: *mut ProgramListInfo) -> tresult {
        if list_index != 0 || info.is_null() {
            return kResultFalse;
        }
        // SAFETY: The unsafe COM caller supplies writable ProgramListInfo storage; its pointer and supported index were checked.
        let info = unsafe { &mut *info };
        info.id = FAKE_PROGRAM_LIST_ID;
        write_char16(&mut info.name, "Factory");
        info.programCount = FAKE_PROGRAM_NAMES.len() as int32;
        kResultOk
    }

    unsafe fn getProgramName(
        &self,
        list_id: ProgramListID,
        program_index: int32,
        name: *mut String128,
    ) -> tresult {
        if program_index < 0 {
            return kResultFalse;
        }
        let Some(program) = FAKE_PROGRAM_NAMES.get(program_index as usize) else {
            return kResultFalse;
        };
        if list_id != FAKE_PROGRAM_LIST_ID || name.is_null() {
            return kResultFalse;
        }
        // SAFETY: The unsafe COM caller supplies a full String128 output; the pointer and program-list index were checked.
        write_char16(unsafe { &mut *name }, program);
        kResultOk
    }

    unsafe fn getProgramInfo(
        &self,
        _list_id: ProgramListID,
        _program_index: int32,
        _attribute_id: FIDString,
        _attribute_value: *mut String128,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn hasProgramPitchNames(
        &self,
        _list_id: ProgramListID,
        _program_index: int32,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getProgramPitchName(
        &self,
        _list_id: ProgramListID,
        _program_index: int32,
        _midi_pitch: i16,
        _name: *mut String128,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getSelectedUnit(&self) -> UnitID {
        0
    }

    unsafe fn selectUnit(&self, _unit_id: UnitID) -> tresult {
        kResultOk
    }

    unsafe fn getUnitByBus(
        &self,
        _media_type: MediaType,
        _direction: BusDirection,
        _bus_index: int32,
        _channel: int32,
        _unit_id: *mut UnitID,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn setUnitProgramData(
        &self,
        _list_or_unit_id: int32,
        _program_index: int32,
        _data: *mut IBStream,
    ) -> tresult {
        kResultFalse
    }
}

impl IProgramListDataTrait for FakePlugin {
    unsafe fn programDataSupported(&self, list_id: ProgramListID) -> tresult {
        if list_id == FAKE_PROGRAM_LIST_ID {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getProgramData(
        &self,
        list_id: ProgramListID,
        program_index: int32,
        data: *mut IBStream,
    ) -> tresult {
        if list_id != FAKE_PROGRAM_LIST_ID {
            return kResultFalse;
        }
        if program_index < 0 {
            return kResultFalse;
        }
        let Some(value) = FAKE_PROGRAM_VALUES.get(program_index as usize) else {
            return kResultFalse;
        };
        write_state_value(data, *value)
    }

    unsafe fn setProgramData(
        &self,
        list_id: ProgramListID,
        program_index: int32,
        data: *mut IBStream,
    ) -> tresult {
        if list_id != FAKE_PROGRAM_LIST_ID {
            return kResultFalse;
        }
        if program_index < 0 || FAKE_PROGRAM_VALUES.get(program_index as usize).is_none() {
            return kResultFalse;
        }
        match read_state_value(data) {
            Some(value) => {
                *self.control.param_value.lock().unwrap() = value;
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

impl IPluginBaseTrait for FakePlugin {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for FakePlugin {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        -1
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, direction: BusDirection) -> int32 {
        self.control.bus_queries.fetch_add(1, Ordering::Relaxed);
        if media_type == MediaTypes_::kEvent as int32 {
            return i32::from(
                direction == BusDirections_::kInput as int32
                    && self.control.input_bus_count.load(Ordering::Relaxed) == 0,
            );
        }
        if media_type != MediaTypes_::kAudio as int32 {
            return 0;
        }
        if direction == BusDirections_::kOutput as int32 {
            self.control.bus_count.load(Ordering::Relaxed) as int32
        } else if direction == BusDirections_::kInput as int32 {
            self.control.input_bus_count.load(Ordering::Relaxed) as int32
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        direction: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type == MediaTypes_::kEvent as int32
            && direction == BusDirections_::kInput as int32
            && index == 0
            && !bus.is_null()
            && self.control.input_bus_count.load(Ordering::Relaxed) == 0
        {
            // SAFETY: the native caller provides one writable BusInfo and the
            // null check above precedes this bounded output write.
            let mut info: BusInfo = unsafe { std::mem::zeroed() };
            info.mediaType = media_type;
            info.direction = direction;
            info.channelCount = 16;
            // SAFETY: this nonnull native output points to one writable BusInfo.
            unsafe {
                *bus = info;
            }
            return kResultOk;
        }
        let count = if direction == BusDirections_::kOutput as int32 {
            self.control.bus_count.load(Ordering::Relaxed)
        } else if direction == BusDirections_::kInput as int32 {
            self.control.input_bus_count.load(Ordering::Relaxed)
        } else {
            0
        };
        if bus.is_null()
            || media_type != MediaTypes_::kAudio as int32
            || index < 0
            || index as usize >= count
        {
            return -1;
        }
        // SAFETY: BusInfo contains only scalar, character-array and union-free ABI fields for which an all-zero initial representation is valid.
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        info.mediaType = media_type;
        info.direction = direction;
        info.channelCount = self.channels;
        info.busType = if index == 0 {
            BusTypes_::kMain as int32
        } else {
            BusTypes_::kAux as int32
        };
        let flags = BusFlags_::kDefaultActive;
        // The Windows bindings represent this enum as signed.
        #[cfg(windows)]
        let flags = flags as u32;
        info.flags = flags;
        // SAFETY: The unsafe COM caller supplies writable BusInfo storage; the bus index and non-null pointer were checked above.
        unsafe { ptr::write(bus, info) };
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _in_info: *mut RoutingInfo,
        _out_info: *mut RoutingInfo,
    ) -> tresult {
        -1
    }

    unsafe fn activateBus(
        &self,
        _media_type: MediaType,
        _direction: BusDirection,
        _index: int32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        self.control.bus_count.store(
            self.control.state_bus_count.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        read_param_value(state, &self.control);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_param_value(state, &self.control)
    }
}

impl IAudioProcessorTrait for FakePlugin {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        _num_ins: int32,
        _outputs: *mut SpeakerArrangement,
        _num_outs: int32,
    ) -> tresult {
        kResultOk
    }

    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: int32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        let count = if direction == BusDirections_::kOutput as int32 {
            self.control.bus_count.load(Ordering::Relaxed)
        } else if direction == BusDirections_::kInput as int32 {
            self.control.input_bus_count.load(Ordering::Relaxed)
        } else {
            0
        };
        if arrangement.is_null() || index < 0 || index as usize >= count {
            return -1;
        }
        // SAFETY: The unsafe COM caller supplies writable SpeakerArrangement storage; the index and non-null pointer were checked.
        unsafe { ptr::write(arrangement, SpeakerArr::kStereo) };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, _size: int32) -> tresult {
        kResultOk
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return -1;
        }
        // SAFETY: The process caller retains an initialized ProcessData and all referenced planes for this synchronous call; null was rejected.
        let data = unsafe { &*data };
        let process_call = self.control.process_calls.fetch_add(1, Ordering::Relaxed);
        if self
            .control
            .track_input_peaks
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let mut input_bus_peaks = Vec::with_capacity(data.numInputs.max(0) as usize);
            for bus_index in 0..data.numInputs.max(0) as usize {
                // SAFETY: ProcessData.inputs contains numInputs descriptors for this call; bus_index is within that supplied array.
                let bus = unsafe { &*data.inputs.add(bus_index) };
                let mut peak = 0.0f32;
                for channel in 0..bus.numChannels as usize {
                    // SAFETY: This fixture processes 32-bit samples; the supplied bus owns numChannels channel-pointer entries for the call.
                    let buffer = unsafe { *bus.__field0.channelBuffers32.add(channel) };
                    if buffer.is_null() {
                        continue;
                    }
                    for frame in 0..data.numSamples as usize {
                        // SAFETY: The non-null input plane contains numSamples readable f32 samples supplied by the process caller.
                        peak = peak.max(unsafe { *buffer.add(frame) }.abs());
                    }
                }
                input_bus_peaks.push(peak);
            }
            *self.control.input_bus_peaks.lock().unwrap() = input_bus_peaks;
        }
        if !data.processContext.is_null() {
            // SAFETY: The process caller retains the non-null ProcessContext until this synchronous callback returns.
            let context = unsafe { &*data.processContext };
            self.control.contexts.lock().unwrap().push(CapturedContext {
                process_call,
                num_samples: data.numSamples.max(0) as u32,
                project_time_samples: context.projectTimeSamples,
                project_time_music: context.projectTimeMusic,
                tempo: context.tempo,
                time_sig_numerator: context.timeSigNumerator,
                time_sig_denominator: context.timeSigDenominator,
                playing: context.state & PLAYING_FLAG != 0,
            });
        }
        self.control
            .input_events_null
            .store(data.inputEvents.is_null(), Ordering::Relaxed);
        if !data.inputEvents.is_null() {
            let events = data.inputEvents;
            // SAFETY: The process caller retains this non-null IEventList and its matching vtable for the complete processing call.
            let count = unsafe { ((*(*events).vtbl).getEventCount)(events) };
            for index in 0..count {
                // SAFETY: Event is a C ABI scalar/union record with a valid zero representation; getEvent initializes the active payload before use.
                let mut event: Event = unsafe { std::mem::zeroed() };
                // SAFETY: The process caller retains the same live event-list interface and vtable throughout this callback.
                let get_event = unsafe { (*(*events).vtbl).getEvent };
                // SAFETY: The event index is below the supplied count and event is writable storage for the host list to initialize synchronously.
                if unsafe { get_event(events, index, &mut event) } != kResultOk {
                    continue;
                }
                if event.r#type as i32 == EventTypes_::kNoteOnEvent as i32 {
                    self.control.last_note_channel.store(
                        // SAFETY: The event type was checked as kNoteOnEvent, so noteOn is the active union payload.
                        unsafe { event.__field0.noteOn.channel } as i32,
                        Ordering::Relaxed,
                    );
                    self.control.active_notes.fetch_add(1, Ordering::Relaxed);
                } else if event.r#type as i32 == EventTypes_::kNoteOffEvent as i32
                    && self.control.active_notes.load(Ordering::Relaxed) > 0
                {
                    self.control.active_notes.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        let mut gain = *self.control.param_value.lock().unwrap() * 2.0;
        if !data.inputParameterChanges.is_null() {
            let changes = data.inputParameterChanges;
            // SAFETY: The process caller retains this non-null IParameterChanges and its matching vtable for the processing call.
            let count = unsafe { ((*(*changes).vtbl).getParameterCount)(changes) };
            for queue_index in 0..count {
                // SAFETY: queue_index is below this live changes interface count; returned queues stay borrowed for the process call.
                let queue = unsafe { ((*(*changes).vtbl).getParameterData)(changes, queue_index) };
                if queue.is_null() {
                    continue;
                }
                // SAFETY: The returned queue is non-null and remains retained by the process caller for this synchronous ID query.
                let param_id = unsafe { ((*(*queue).vtbl).getParameterId)(queue) };
                // SAFETY: The same non-null queue and its matching vtable remain live for this point-count query.
                let points = unsafe { ((*(*queue).vtbl).getPointCount)(queue) };
                for point_index in 0..points {
                    let mut sample_offset = 0;
                    let mut value = 0.0;
                    // SAFETY: The point index is below the queue count, and both output scalars are writable until this synchronous query returns.
                    if unsafe {
                        ((*(*queue).vtbl).getPoint)(
                            queue,
                            point_index,
                            &mut sample_offset,
                            &mut value,
                        )
                    } == kResultOk
                    {
                        if param_id == FAKE_PARAM_ID {
                            gain = value * 2.0;
                        }
                        self.control
                            .param_changes
                            .lock()
                            .unwrap()
                            .push(CapturedParamChange {
                                process_call,
                                param_id,
                                sample_offset,
                                value,
                            });
                    }
                }
            }
        }
        *self.control.param_value.lock().unwrap() = gain / 2.0;
        let tone_hz = self.control.tone_hz.load(Ordering::Relaxed);
        let copy_input = self.control.copy_input.load(Ordering::Relaxed);
        let note_gated = self.control.note_gated.load(Ordering::Relaxed)
            && self.control.active_notes.load(Ordering::Relaxed) == 0;
        for bus_index in 0..data.numOutputs as usize {
            // SAFETY: ProcessData.outputs supplies numOutputs descriptors for this call; bus_index stays within that array.
            let bus = unsafe { &mut *data.outputs.add(bus_index) };
            for channel in 0..bus.numChannels as usize {
                // SAFETY: This 32-bit process bus supplies numChannels channel pointers retained for the complete call.
                let buffer = unsafe { *bus.__field0.channelBuffers32.add(channel) };
                if buffer.is_null() {
                    continue;
                }
                let fixed = ((bus_index + 1) as f32 + channel as f32 * 0.5) * gain as f32;
                for frame in 0..data.numSamples as usize {
                    let value = if self.control.encode_process_ordinal.load(Ordering::Relaxed) {
                        (process_call + 1) as f32 / 65536.0
                    } else if copy_input {
                        // SAFETY: The current process input descriptors remain live; input_sample checks bus/channel/frame bounds before reading.
                        unsafe { input_sample(data, 0, channel, frame) }
                    } else if note_gated {
                        0.0
                    } else if tone_hz > 0 && bus_index == 0 {
                        let phase =
                            frame as f32 * tone_hz as f32 / 48_000.0 * std::f32::consts::TAU;
                        phase.sin() * gain as f32
                    } else {
                        fixed
                    };
                    // SAFETY: The non-null output plane supplies numSamples writable f32 slots; frame is within that caller-owned span.
                    unsafe {
                        *buffer.add(frame) = value
                            * f32::from_bits(self.control.output_scale_bits.load(Ordering::Relaxed))
                    };
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        self.control.tail_samples.load(Ordering::Relaxed)
    }
}

/// One sample of a declared input bus, or silence when it has none.
unsafe fn input_sample(data: &ProcessData, bus_index: usize, channel: usize, frame: usize) -> f32 {
    if bus_index >= data.numInputs.max(0) as usize {
        return 0.0;
    }
    // SAFETY: The process caller supplies its numInputs descriptors; bus_index was checked against that count.
    let bus = unsafe { &*data.inputs.add(bus_index) };
    if channel >= bus.numChannels as usize {
        return 0.0;
    }
    // SAFETY: The caller supplies numChannels f32 plane pointers for this input bus; channel was checked against that count.
    let buffer = unsafe { *bus.__field0.channelBuffers32.add(channel) };
    if buffer.is_null() || frame >= data.numSamples as usize {
        return 0.0;
    }
    // SAFETY: The non-null input plane supplies numSamples readable f32 slots; the frame bound was checked.
    unsafe { *buffer.add(frame) }
}

impl FakePlugin {
    fn new(control: Arc<FakeControl>, channels: i32) -> Self {
        Self { control, channels }
    }
}

/// Build a fake multi-output VST3 instance; every bus/channel has a fixed
/// distinct sample value so routing tests can identify each destination.
pub fn fake_instance(control: &Arc<FakeControl>, channels: i32, max_frames: u32) -> Vst3Instance {
    let wrapper = ComWrapper::new(FakePlugin::new(Arc::clone(control), channels));
    let component: ComPtr<IComponent> = wrapper.to_com_ptr().unwrap();
    let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
    drop(wrapper);
    Vst3Instance::from_test_parts(component, processor, 48_000, max_frames)
}

/// Preset id for a fake component that declares one stereo output bus.
pub const FAKE_ONE_BUS_ID: &str = "test.fake.one_bus";
/// The one-bus fake without the CLI clipping guard's 0.125 attenuation.
pub const FAKE_UNITY_ONE_BUS_ID: &str = "test.fake.one_bus_unity";
/// The two-bus fake without the CLI clipping guard's 0.125 attenuation.
pub const FAKE_UNITY_TWO_BUS_ID: &str = "test.fake.two_bus_unity";
/// Preset id for a fake component that declares two stereo output buses.
pub const FAKE_TWO_BUS_ID: &str = "test.fake.two_bus";
/// Preset id for a fake component that declares four stereo output buses.
pub const FAKE_FOUR_BUS_ID: &str = "test.fake.four_bus";
/// Preset id for a fake component that declares one stereo input bus and
/// one stereo output bus, the shape of an insert effect.
pub const FAKE_EFFECT_ID: &str = "test.fake.effect";

/// Declared stereo output buses for a fake preset id; `None` for real ids.
pub const FAKE_BLOCK_ORDINAL_ID: &str = "test.fake.block_ordinal";
/// Declared output bus count for a fake preset.
pub fn fake_plugin_bus_count(id: &str) -> Option<usize> {
    match id {
        FAKE_ONE_BUS_ID | FAKE_BLOCK_ORDINAL_ID | FAKE_UNITY_ONE_BUS_ID => Some(1),
        FAKE_TWO_BUS_ID | FAKE_UNITY_TWO_BUS_ID => Some(2),
        FAKE_FOUR_BUS_ID => Some(4),
        FAKE_EFFECT_ID => Some(1),
        _ => None,
    }
}

/// Build the fake instance a test preset id names. Offline and streaming
/// tests share the id so one project can run through both renderers.
pub fn fake_instance_for_id(id: &str, max_frames: u32) -> Option<Vst3Instance> {
    let buses = fake_plugin_bus_count(id)?;
    let control = Arc::new(FakeControl::default());
    control.bus_count.store(buses, Ordering::Relaxed);
    control.state_bus_count.store(buses, Ordering::Relaxed);
    control
        .input_bus_count
        .store(usize::from(id == FAKE_EFFECT_ID), Ordering::Relaxed);
    Some(fake_instance(&control, 2, max_frames))
}

/// Parameter-capable acquisition for complete owned preparation. Its declared
/// buses match the routing-only fake IDs; the controller exposes the existing
/// Gain parameter without changing routing tests' controller-free instances.
pub fn fake_parameter_instance_for_id(id: &str, max_frames: u32) -> Option<Vst3Instance> {
    let buses = fake_plugin_bus_count(id)?;
    let control = Arc::new(FakeControl::default());
    control
        .encode_process_ordinal
        .store(id == FAKE_BLOCK_ORDINAL_ID, Ordering::Relaxed);
    // The CLI clipping guard remains active even with float output. This exact
    // power-of-two scale leaves headroom for the four-output sum; pipeline
    // assertions undo it only after reading the successfully guarded export.
    control.output_scale_bits.store(
        if matches!(id, FAKE_UNITY_ONE_BUS_ID | FAKE_UNITY_TWO_BUS_ID) {
            1.0f32
        } else {
            0.125
        }
        .to_bits(),
        Ordering::Relaxed,
    );
    control.bus_count.store(buses, Ordering::Relaxed);
    control.state_bus_count.store(buses, Ordering::Relaxed);
    control
        .input_bus_count
        .store(usize::from(id == FAKE_EFFECT_ID), Ordering::Relaxed);
    Some(fake_effect_instance(&control, 2, max_frames))
}

/// Build a fake instance with an explicit output and input bus count; the
/// shared control lets a test inspect which input bus received audio.
pub fn fake_instance_with_buses(
    output_buses: usize,
    input_buses: usize,
    max_frames: u32,
) -> (Vst3Instance, Arc<FakeControl>) {
    let control = Arc::new(FakeControl::default());
    control.bus_count.store(output_buses, Ordering::Relaxed);
    control
        .state_bus_count
        .store(output_buses, Ordering::Relaxed);
    control
        .input_bus_count
        .store(input_buses, Ordering::Relaxed);
    let instance = fake_instance(&control, 2, max_frames);
    (instance, control)
}

/// Parameter the fake edit controller exposes for automation tests.
pub const FAKE_PARAM_ID: ParamID = 42;
/// Title the fake controller reports for that parameter.
pub const FAKE_PARAM_NAME: &str = "Gain";

struct FakeEditController {
    control: Arc<FakeControl>,
    handler: editor_handler::Slot,
}

impl IPluginBaseTrait for FakeEditController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.handler.clear();
        kResultOk
    }
}

impl IEditControllerTrait for FakeEditController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        let result = self
            .control
            .controller_state_result
            .load(std::sync::atomic::Ordering::Relaxed);
        if result != kResultOk {
            // A processor-only controller answers kNotImplemented, and a
            // controller that cannot read a component state fails without
            // touching its own values.
            return result;
        }
        read_param_value(state, &self.control);
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        read_param_value(state, &self.control);
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_param_value(state, &self.control)
    }

    unsafe fn getParameterCount(&self) -> int32 {
        1
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        self.control
            .param_info_queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if index != 0 || info.is_null() {
            return kResultFalse;
        }
        // SAFETY: The unsafe COM caller supplies writable ParameterInfo storage; index zero and the non-null pointer were checked.
        let info = unsafe { &mut *info };
        info.id = FAKE_PARAM_ID;
        write_char16(&mut info.title, FAKE_PARAM_NAME);
        write_char16(&mut info.shortTitle, "Gain");
        write_char16(&mut info.units, "");
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.5;
        info.unitId = -1;
        info.flags = ParameterFlags_::kCanAutomate;
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        _id: ParamID,
        _value: ParamValue,
        _string: *mut String128,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getParamValueByString(
        &self,
        _id: ParamID,
        _string: *mut TChar,
        _value: *mut ParamValue,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn normalizedParamToPlain(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: ParamID, value: ParamValue) -> ParamValue {
        value.clamp(0.0, 1.0)
    }

    unsafe fn getParamNormalized(&self, _id: ParamID) -> ParamValue {
        *self.control.param_value.lock().unwrap()
    }

    unsafe fn setParamNormalized(&self, _id: ParamID, value: ParamValue) -> tresult {
        *self.control.param_value.lock().unwrap() = value;
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: The COM caller supplies a live optional handler for this call;
        // the fixture slot retains its own reference before returning.
        unsafe { self.handler.set(&self.control, handler) }
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

impl Class for FakeEditController {
    type Interfaces = (
        IEditController,
        IKeyswitchController,
        INoteExpressionController,
    );
}

/// Keyswitches the fake controller reports, in list order.
const FAKE_KEYSWITCHES: [(&str, i32, i32); 2] = [("Kick", 36, 36), ("Snare", 38, 38)];

impl IKeyswitchControllerTrait for FakeEditController {
    unsafe fn getKeyswitchCount(&self, _bus_index: int32, _channel: i16) -> int32 {
        self.control.keyswitch_count.load(Ordering::Relaxed) as int32
    }

    unsafe fn getKeyswitchInfo(
        &self,
        _bus_index: int32,
        _channel: i16,
        index: int32,
        info: *mut KeyswitchInfo,
    ) -> tresult {
        if info.is_null() || index < 0 {
            return kResultFalse;
        }
        let Some((name, min, max)) = FAKE_KEYSWITCHES.get(index as usize) else {
            return kResultFalse;
        };
        // SAFETY: The unsafe COM caller supplies writable KeyswitchInfo storage; null and the keyswitch index were checked.
        let info = unsafe { &mut *info };
        info.typeId = index as u32;
        write_char16(&mut info.title, name);
        write_char16(&mut info.shortTitle, name);
        info.keyswitchMin = *min;
        info.keyswitchMax = *max;
        info.keyRemapped = *min;
        info.unitId = -1;
        kResultOk
    }
}

impl INoteExpressionControllerTrait for FakeEditController {
    unsafe fn getNoteExpressionCount(&self, _bus_index: int32, _channel: i16) -> int32 {
        self.control.note_expression_count.load(Ordering::Relaxed) as int32
    }

    unsafe fn getNoteExpressionInfo(
        &self,
        _bus_index: int32,
        _channel: i16,
        index: int32,
        info: *mut NoteExpressionTypeInfo,
    ) -> tresult {
        if info.is_null() {
            return kResultFalse;
        }
        // SAFETY: The unsafe COM caller supplies writable NoteExpressionTypeInfo storage; the null pointer was rejected.
        let info = unsafe { &mut *info };
        match index {
            0 => {
                info.typeId = 1;
                write_char16(&mut info.title, "Pan");
                write_char16(&mut info.shortTitle, "Pan");
                info.valueDesc = vst3::Steinberg::Vst::NoteExpressionValueDescription {
                    defaultValue: 0.5,
                    minimum: 0.0,
                    maximum: 1.0,
                    stepCount: 0,
                };
                info.associatedParameterId = FAKE_PARAM_ID;
                info.flags = NoteExpressionTypeFlags_::kIsBipolar as i32;
                kResultOk
            }
            1 => {
                info.typeId = 2;
                write_char16(&mut info.title, "Tuning");
                write_char16(&mut info.shortTitle, "Tune");
                write_char16(&mut info.units, "ct");
                info.valueDesc = vst3::Steinberg::Vst::NoteExpressionValueDescription {
                    defaultValue: 0.5,
                    minimum: 0.0,
                    maximum: 1.0,
                    stepCount: 0,
                };
                info.flags = NoteExpressionTypeFlags_::kIsAbsolute as i32;
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn getNoteExpressionStringByValue(
        &self,
        _bus_index: int32,
        _channel: i16,
        _id: NoteExpressionTypeID,
        _value_normalized: f64,
        _string: *mut String128,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getNoteExpressionValueByString(
        &self,
        _bus_index: int32,
        _channel: i16,
        _id: NoteExpressionTypeID,
        _string: *const vst3::Steinberg::Vst::TChar,
        _value_normalized: *mut f64,
    ) -> tresult {
        kResultFalse
    }
}

fn write_char16(target: &mut [char16], text: &str) {
    for (index, unit) in text.encode_utf16().take(target.len() - 1).enumerate() {
        target[index] = unit as char16;
    }
    target[text.encode_utf16().count().min(target.len() - 1)] = 0;
}

/// Build a fake effect instance with an edit controller so parameter names
/// resolve and queued changes reach `ProcessData`.
pub fn fake_effect_instance(
    control: &Arc<FakeControl>,
    channels: i32,
    max_frames: u32,
) -> Vst3Instance {
    let scope = editor_handler::Scope::new();
    let plugin = ComWrapper::new(FakePlugin::new(Arc::clone(control), channels));
    let component: ComPtr<IComponent> = plugin.to_com_ptr().unwrap();
    let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
    let controller = ComWrapper::new(FakeEditController {
        control: Arc::clone(control),
        handler: editor_handler::Slot::new(&scope),
    });
    let pointer: ComPtr<IEditController> = controller.to_com_ptr().unwrap();
    controller.handler.attach_controller(pointer.as_ptr());
    let controller = pointer;
    let mut instance = Vst3Instance::from_test_parts_with_controller(
        component, processor, controller, 48_000, max_frames,
    );
    instance.fixture_handler_guard = Some(scope);
    instance
}

/// Give a fake instance the host plug frame `editor_info` needs, so a test
/// can exercise `createView` without a window.
#[cfg(target_os = "macos")]
pub fn install_plug_frame(instance: &mut Vst3Instance) {
    let handles = Arc::new(Vst3EditorHandles::new(Arc::clone(&instance.failure)));
    *instance.plug_frame = Some(
        ComWrapper::new(Vst3PlugFrame::new(handles))
            .to_com_ptr::<IPlugFrame>()
            .expect("IPlugFrame wrapper exposes its interface"),
    );
}

/// Build a fake instance that owns host services, so it can play the editor
/// side of an edit: the fake controller keeps the handler it is given.
pub fn fake_editor_instance(
    control: &Arc<FakeControl>,
    channels: i32,
    max_frames: u32,
) -> Vst3Instance {
    let scope = editor_handler::Scope::new();
    let host = Vst3Host::new();
    let handler = host.to_com_ptr::<IComponentHandler>().unwrap();
    let plugin = ComWrapper::new(FakePlugin::new(Arc::clone(control), channels));
    let component: ComPtr<IComponent> = plugin.to_com_ptr().unwrap();
    let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
    let controller = ComWrapper::new(FakeEditController {
        control: Arc::clone(control),
        handler: editor_handler::Slot::new(&scope),
    });
    let pointer: ComPtr<IEditController> = controller.to_com_ptr().unwrap();
    controller.handler.attach_controller(pointer.as_ptr());
    let controller = pointer;
    // SAFETY: the instance keeps the handler for as long as it lives.
    unsafe {
        let _ = controller.setComponentHandler(handler.as_ptr());
    }
    let mut instance = Vst3Instance::from_test_parts_with_host(
        component, processor, controller, host, 48_000, max_frames,
    );
    instance.fixture_handler_guard = Some(scope);
    instance
}

fn write_state_value(state: *mut IBStream, value: f64) -> tresult {
    if state.is_null() {
        return kResultFalse;
    }
    let bytes = value.to_le_bytes();
    let mut written = 0;
    // SAFETY: VST3 passes a live stream for program data.
    let stream = unsafe { ComRef::<IBStream>::from_raw_unchecked(state) };
    // SAFETY: The retained in-process stream receives a live byte buffer and writable count; the byte length matches the supplied buffer.
    unsafe {
        let _ = stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut());
        stream.write(
            bytes.as_ptr() as *mut std::ffi::c_void,
            bytes.len() as int32,
            &mut written,
        );
    }
    kResultOk
}

fn read_state_value(state: *mut IBStream) -> Option<f64> {
    if state.is_null() {
        return None;
    }
    let mut bytes = [0_u8; 8];
    let mut read = 0;
    // SAFETY: VST3 passes a live stream for program data.
    let stream = unsafe { ComRef::<IBStream>::from_raw_unchecked(state) };
    // SAFETY: The retained in-process stream receives writable byte/count buffers of the exact requested capacity.
    let result = unsafe {
        let _ = stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut());
        stream.read(
            bytes.as_mut_ptr() as *mut std::ffi::c_void,
            bytes.len() as int32,
            &mut read,
        )
    };
    (result == kResultOk && read == bytes.len() as int32).then_some(f64::from_le_bytes(bytes))
}

fn write_param_value(state: *mut IBStream, control: &FakeControl) -> tresult {
    if state.is_null() {
        return kResultOk;
    }
    let value = *control.param_value.lock().unwrap();
    let bytes = value.to_le_bytes();
    let mut written = 0;
    // SAFETY: VST3 passes a live stream for getState/setState.
    let stream = unsafe { ComRef::<IBStream>::from_raw_unchecked(state) };
    // SAFETY: The retained in-process stream receives live serialized-value bytes and writable count for this synchronous write.
    unsafe {
        let _ = stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut());
        stream.write(
            bytes.as_ptr() as *mut std::ffi::c_void,
            bytes.len() as int32,
            &mut written,
        );
    }
    kResultOk
}

fn read_param_value(state: *mut IBStream, control: &FakeControl) -> bool {
    if state.is_null() {
        return false;
    }
    let mut bytes = [0_u8; 8];
    let mut read = 0;
    // SAFETY: VST3 passes a live stream for getState/setState.
    let stream = unsafe { ComRef::<IBStream>::from_raw_unchecked(state) };
    // SAFETY: The retained in-process stream receives writable serialized-value/count buffers sized for this synchronous read.
    let result = unsafe {
        let _ = stream.seek(0, IStreamSeekMode_::kIBSeekSet as int32, ptr::null_mut());
        stream.read(
            bytes.as_mut_ptr() as *mut std::ffi::c_void,
            bytes.len() as int32,
            &mut read,
        )
    };
    if result != kResultOk || read != bytes.len() as int32 {
        return false;
    }
    *control.param_value.lock().unwrap() = f64::from_le_bytes(bytes);
    true
}

/// Retire inert fixture COM owners on the thread that registered them.
pub fn drain_editor_handlers() {
    editor_handler::drain();
}

/// What a plug-in editor does when a knob moves.
pub fn editor_perform_edit(control: &FakeControl, id: ParamID, value: f64) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    *control.param_value.lock().unwrap() = value;
    // SAFETY: The thread-affine loan pins both the registered handler and fake
    // controller until this synchronous callback, including any reentry, returns.
    unsafe { loan.handler().performEdit(id, value) }
}
/// Begin an editor parameter edit.
pub fn editor_begin_edit(control: &FakeControl, id: ParamID) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    // SAFETY: The loan pins the live handler/controller on their registration
    // thread, with no registry borrow or fixture lock across this callback.
    unsafe { loan.handler().beginEdit(id) }
}
/// End an editor parameter edit.
pub fn editor_end_edit(control: &FakeControl, id: ParamID) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    // SAFETY: The thread-affine loan retains the controller used by this gesture
    // boundary as well as the handler throughout the synchronous callback.
    unsafe { loan.handler().endEdit(id) }
}
/// What a plugin does after a program change: it tells the host the
/// parameter values moved.
pub fn editor_restart_component(control: &FakeControl, flags: i32) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    // SAFETY: The current-thread loan retains the live handler and controller;
    // the scalar flags do not borrow external storage.
    unsafe { loan.handler().restartComponent(flags) }
}
/// What a plugin does when it changes the document on its own.
pub fn editor_set_dirty(control: &FakeControl, dirty: bool) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    use vst3::Steinberg::Vst::IComponentHandler2Trait;
    let Some(handler) = loan
        .handler()
        .cast::<vst3::Steinberg::Vst::IComponentHandler2>()
    else {
        return kResultFalse;
    };
    // SAFETY: The owned queried interface and original controller/handler loan
    // remain on the registration thread through this synchronous call.
    unsafe { handler.setDirty(u8::from(dirty)) }
}
/// What a plugin does when it wants its editor on screen.
pub fn editor_request_open_editor(control: &FakeControl, name: &str) -> tresult {
    let Some(loan) = editor_handler::loan(control) else {
        return kResultFalse;
    };
    use vst3::Steinberg::Vst::IComponentHandler2Trait;
    let Some(handler) = loan
        .handler()
        .cast::<vst3::Steinberg::Vst::IComponentHandler2>()
    else {
        return kResultFalse;
    };
    let name = std::ffi::CString::new(name).unwrap_or_default();
    // SAFETY: The owned queried interface and scoped controller loan remain
    // live while borrowing this NUL-terminated name on the registration thread.
    unsafe { handler.requestOpenEditor(name.as_ptr()) }
}
