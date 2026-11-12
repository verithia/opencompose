//! Owned native control plans, captured without running a plugin under app locks.
use super::*;
use crate::clap_main::ClapMainExecutor;

/// Owned plan to refresh a CLAP parameter list on the main loop.
pub struct ClapParameterReadPlan {
    source: Arc<ClapCore>,
}
impl ClapParameterReadPlan {
    /// Execute the read on the main executor, or inline without one.
    pub fn execute_on_main(&self) -> Result<Vec<PluginParam>, String> {
        let source = Arc::clone(&self.source);
        let work = move || {
            if source.retired.load(std::sync::atomic::Ordering::Acquire) {
                return Err("The CLAP parameter owner was replaced".into());
            }
            source.apply_control_changes()?;
            let parameters = source.refresh_parameters()?;
            source
                .host
                .params_changed
                .store(false, std::sync::atomic::Ordering::Release);
            Ok(parameters
                .into_iter()
                .filter(|param| param.flags & clap_sys::ext::params::CLAP_PARAM_IS_HIDDEN == 0)
                .collect())
        };
        match ClapMainExecutor::registered() {
            Ok(executor) => executor.dispatch_control(work)?.wait(),
            Err(_) => work(),
        }
    }
    /// Whether the plan belongs to this instance's parameter owner.
    pub fn is_for(&self, instance: &ClapInstance) -> bool {
        Arc::ptr_eq(&self.source, &instance.core)
    }
}

/// An owned state capture, safe to queue after releasing the document lock.
/// Owned plan to capture plugin state after releasing the document lock.
pub struct ClapStateCapturePlan {
    source: Arc<ClapCore>,
}
impl ClapStateCapturePlan {
    /// Execute the capture on the main executor, or inline without one.
    pub fn execute_on_main(self) -> Result<Vec<u8>, String> {
        let work = move || {
            if self
                .source
                .retired
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("The CLAP source was retired before state capture".into());
            }
            self.source.save_state()
        };
        match ClapMainExecutor::registered() {
            Ok(executor) => executor.dispatch_control(work)?.wait(),
            Err(_) => work(),
        }
    }
}

/// Capture this value under the document lock; execute it only after releasing
/// that lock. Its strong source lease keeps callbacks alive during dispatch.
pub struct ClapOfflineForkPlan {
    source: Arc<ClapCore>,
    path: std::path::PathBuf,
    id: String,
    sample_rate: u32,
    max_frames: u32,
}
impl ClapOfflineForkPlan {
    /// Build the offline fork on the main executor, or inline without one.
    pub fn execute_on_main(self) -> Result<ClapInstance, String> {
        let work = move || {
            if self
                .source
                .retired
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("The CLAP source was retired before offline preparation".into());
            }
            let state = self.source.save_state()?;
            let mut fresh =
                ClapInstance::load(&self.path, &self.id, self.sample_rate, self.max_frames)?;
            fresh.set_state_bytes(&state)?;
            fresh.refresh_control_metadata()?;
            Ok(fresh)
        };
        match ClapMainExecutor::registered() {
            Ok(executor) => executor.dispatch_control(work)?.wait(),
            // A standalone same-control-thread caller needs no event dispatch.
            // Application adapters register before allowing worker preparation.
            Err(_) => work(),
        }
    }
}

impl ClapInstance {
    /// Capture an owned plan to refresh parameters later.
    pub fn parameter_read_plan(&self) -> ClapParameterReadPlan {
        ClapParameterReadPlan {
            source: Arc::clone(&self.core),
        }
    }
    /// Relinquish a document's owner without running foreign code under its
    /// mutex. Retired GUI requests are cancelled before main-loop cleanup.
    pub fn retire_deferred(self) {
        self.core
            .retired
            .store(true, std::sync::atomic::Ordering::Release);
        match ClapMainExecutor::registered() {
            Ok(executor) => {
                if let Err((error, task)) = executor.defer_retirement(Box::new(move || drop(self)))
                {
                    log::error!("CLAP retirement retained: {error}");
                    std::mem::forget(task);
                }
            }
            Err(_) => {
                // No main loop can prove safe foreign destruction here.
                log::error!("CLAP retirement retained because no main executor is registered");
                std::mem::forget(self);
            }
        }
    }

    /// Capture an owned plan to save state later.
    pub fn state_capture_plan(&self) -> ClapStateCapturePlan {
        ClapStateCapturePlan {
            source: Arc::clone(&self.core),
        }
    }

    /// Capture an owned plan to fork this instance for offline rendering.
    pub fn offline_fork_plan(&self) -> ClapOfflineForkPlan {
        ClapOfflineForkPlan {
            source: Arc::clone(&self.core),
            path: self.bundle_path.clone(),
            id: self.plugin_id.clone(),
            sample_rate: self.sample_rate,
            max_frames: self.max_frames,
        }
    }

    /// Prepare the entire native owner on main, including the caller's preset
    /// restore and metadata work. Call only from a control worker after releasing
    /// document locks, or from the main thread itself. Never use from audio.
    pub fn load_prepared_on_main(
        path: &Path,
        plugin_id: &str,
        sample_rate: u32,
        max_frames: u32,
        prepare: impl FnOnce(&mut Self) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let path = path.to_path_buf();
        let id = plugin_id.to_owned();
        let work = move || {
            let mut instance = Self::load(&path, &id, sample_rate, max_frames)?;
            prepare(&mut instance)?;
            instance.refresh_control_metadata()?;
            Ok(instance)
        };
        ClapMainExecutor::registered()?
            .dispatch_control(work)?
            .wait()
    }
}

impl ClapCore {
    fn control_call<T>(
        &self,
        operation: ClapOperation,
        call: impl FnOnce() -> Result<T, Fault>,
    ) -> Result<T, String> {
        self.check_control_thread()?;
        let _main = self
            .main_call()
            .ok_or("The CLAP owner is busy on the main thread; retry control preparation")?;
        if self.failure.failed() {
            return Err(self
                .failure
                .first_fault()
                .map(|record| record.fault)
                .unwrap_or_else(Fault::quarantined)
                .describe_for("CLAP"));
        }
        call().map_err(|fault| {
            self.failure.record(operation, fault);
            fault.describe_for("CLAP")
        })
    }

    fn state_extension(&self) -> Result<*const clap_sys::ext::state::clap_plugin_state, String> {
        // SAFETY: this strong core owns the immutable descriptor until teardown.
        let get_extension = unsafe { (*self.plugin).get_extension }
            .ok_or("The CLAP plugin has no extension query")?;
        let extension = self.control_call(ClapOperation::GetExtension, || {
            // SAFETY: the typed callback belongs to this retained plugin and the
            // static CLAP extension ID is valid for the complete invocation.
            unsafe {
                clap_guard::extension(
                    get_extension,
                    self.plugin,
                    clap_sys::ext::state::CLAP_EXT_STATE.as_ptr(),
                )
            }
        })?;
        if extension.is_null() {
            return Err("CLAP plugin does not expose state extension".into());
        }
        Ok(extension.cast())
    }

    pub(super) fn save_state(&self) -> Result<Vec<u8>, String> {
        self.apply_control_changes()?;
        let extension = self.state_extension()?;
        // SAFETY: the extension belongs to this live strong core until destroy.
        let save = unsafe { (*extension).save }
            .ok_or("CLAP plugin state extension has no save callback")?;
        let mut output = ClapStateOutputStream::new();
        output.failure = Some(Arc::clone(&self.failure));
        let stream = output.as_stream();
        let saved = self.control_call(ClapOperation::StateSave, || {
            // SAFETY: the stream and its context remain borrowed for this guarded
            // call; the plugin may not retain a CLAP state stream afterwards.
            unsafe { clap_guard::save(save, self.plugin, &stream) }
        })?;
        if !saved {
            return Err("CLAP plugin failed to save state bytes".into());
        }
        Ok(output.data)
    }

    pub(super) fn restore_state(&self, bytes: &[u8]) -> Result<(), String> {
        let extension = self.state_extension()?;
        // SAFETY: the extension belongs to this live strong core until destroy.
        let load = unsafe { (*extension).load }
            .ok_or("CLAP plugin state extension has no load callback")?;
        let mut input = ClapStateInputStream::new(bytes);
        input.failure = Some(Arc::clone(&self.failure));
        let stream = input.as_stream();
        let loaded = self.control_call(ClapOperation::StateLoad, || {
            // SAFETY: bytes, stream, and context remain borrowed for this exact
            // state callback. The plugin may not retain the state stream.
            unsafe { clap_guard::load(load, self.plugin, &stream) }
        })?;
        if !loaded {
            return Err("CLAP plugin rejected state bytes".into());
        }
        Ok(())
    }
}

fn bounded_parameter_text<const N: usize>(text: &[std::ffi::c_char; N]) -> String {
    let bytes: Vec<_> = text
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
impl ClapCore {
    fn parameter_extension(&self) -> Option<*const clap_sys::ext::params::clap_plugin_params> {
        self.can_flush
            .store(false, std::sync::atomic::Ordering::Release);
        // SAFETY: a strong core retains its initialized immutable descriptor.
        let query = unsafe { (*self.plugin).get_extension }?;
        self.control_call(ClapOperation::GetExtension, || {
            // SAFETY: the static ID and retained descriptor satisfy the exact query ABI.
            unsafe {
                clap_guard::extension(
                    query,
                    self.plugin,
                    clap_sys::ext::params::CLAP_EXT_PARAMS.as_ptr(),
                )
            }
        })
        .ok()
        .filter(|extension| !extension.is_null())
        .map(|extension| {
            let extension = extension as *const clap_sys::ext::params::clap_plugin_params;
            // SAFETY: the live strong core retains this returned immutable extension.
            self.can_flush.store(
                // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
                unsafe { (*extension).flush.is_some() },
                std::sync::atomic::Ordering::Release,
            );
            extension
        })
    }
    fn read_parameters(&self) -> Result<Vec<PluginParam>, String> {
        let mut params = Vec::new();

        let Some(params_ext) = self.parameter_extension() else {
            return Ok(params);
        };

        // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
        let count_fn = match unsafe { (*params_ext).count } {
            Some(f) => f,
            None => return Ok(params),
        };
        // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
        let get_info_fn = match unsafe { (*params_ext).get_info } {
            Some(f) => f,
            None => return Ok(params),
        };
        // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
        let get_value_fn = unsafe { (*params_ext).get_value };
        // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
        let value_to_text_fn = unsafe { (*params_ext).value_to_text };

        let count = self
            // SAFETY: healthy-instance admission retains callback storage; borrowed arguments live through this exact-signature call.
            .control_call(ClapOperation::ParameterQuery, || unsafe {
                clap_guard::plugin_u32(count_fn, self.plugin)
            })
            .unwrap_or(0);
        if count > 16384 {
            return Err("The CLAP parameter inventory exceeds the supported capacity".into());
        }
        for i in 0..count {
            // SAFETY: clap_param_info contains integers, raw pointers, and inline character arrays; zero creates valid writable storage for the synchronous get_info callback.
            let mut info: clap_sys::ext::params::clap_param_info = unsafe { std::mem::zeroed() };
            let ok = self
                // SAFETY: The Core/finalizer retains the native descriptor and module; its main admission and exact-signature shim keep borrowed metadata/stream arguments live through the call.
                .control_call(ClapOperation::ParameterQuery, || unsafe {
                    clap_guard::param_info(get_info_fn, self.plugin, i, &mut info)
                })
                .unwrap_or(false);
            if !ok {
                continue;
            }

            if !info.min_value.is_finite()
                || !info.max_value.is_finite()
                || info.min_value > info.max_value
                || !info.default_value.is_finite()
            {
                return Err("CLAP reported an invalid parameter range".into());
            }
            let name = bounded_parameter_text(&info.name);

            let module = bounded_parameter_text(&info.module);

            // Get current value
            let current = if let Some(f) = get_value_fn {
                let mut val = info.default_value;
                // SAFETY: The Core/finalizer retains the native descriptor and module; its main admission and exact-signature shim keep borrowed metadata/stream arguments live through the call.
                let _ = self.control_call(ClapOperation::ParameterQuery, || unsafe {
                    clap_guard::param_value(f, self.plugin, info.id, &mut val)
                });
                val
            } else {
                info.default_value
            };

            // Get display text for current value
            if !current.is_finite() {
                return Err("CLAP reported a non-finite parameter value".into());
            }
            let display = if let Some(f) = value_to_text_fn {
                let mut buf = [0 as std::ffi::c_char; 256];
                let ok = self
                    // SAFETY: The Core/finalizer retains the native descriptor and module; its main admission and exact-signature shim keep borrowed metadata/stream arguments live through the call.
                    .control_call(ClapOperation::ParameterQuery, || unsafe {
                        clap_guard::param_text(
                            f,
                            self.plugin,
                            info.id,
                            current,
                            buf.as_mut_ptr(),
                            256,
                        )
                    })
                    .unwrap_or(false);
                if ok {
                    bounded_parameter_text(&buf)
                } else {
                    format!("{:.2}", current)
                }
            } else {
                format!("{:.2}", current)
            };

            params.push(PluginParam {
                id: info.id,
                name,
                module,
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                current,
                display,
                flags: info.flags,
            });
        }

        if let Some(record) = self.failure.first_fault() {
            return Err(record.fault.describe_for("CLAP"));
        }
        Ok(params)
    }
    pub(super) fn refresh_parameters(&self) -> Result<Vec<PluginParam>, String> {
        self.check_control_thread()?;
        let parameters = self.read_parameters()?;
        *self
            .parameters
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = parameters.clone();
        Ok(parameters)
    }
    pub(super) fn apply_control_changes(&self) -> Result<(), String> {
        if self.processing.load(std::sync::atomic::Ordering::Acquire) {
            // The audio owner flushes on its next block; main must not race it.
            return Ok(());
        }
        if !self
            .control_pending
            .load(std::sync::atomic::Ordering::Acquire)
            && !self
                .host
                .flush_requested
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        let extension = self
            .parameter_extension()
            .ok_or("CLAP control has no parameter extension")?;
        // SAFETY: the strong core retains this extension throughout the guarded call.
        let flush =
            // SAFETY: The strong Core retains the initialized plugin and its SDK params-extension table until destruction; metadata preparation reads its immutable callback slots on main.
            unsafe { (*extension).flush }.ok_or("CLAP control has no parameter flush callback")?;
        let _main = self.main_call().ok_or("The CLAP main owner is busy")?;
        let _audio = if self.active.load(std::sync::atomic::Ordering::Acquire) {
            match self.audio_call() {
                Ok(permit) => Some(permit),
                Err(_) => return Ok(()),
            }
        } else {
            None
        };
        let changes = {
            std::mem::take(
                &mut *self
                    .controls
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            )
        };
        let requested = self
            .host
            .flush_requested
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        if changes.is_empty() && !requested {
            self.control_pending
                .store(false, std::sync::atomic::Ordering::Release);
            return Ok(());
        }
        let events: Vec<_> = changes
            .into_iter()
            .map(|(param_id, value)| clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            })
            .collect();
        let headers: Vec<_> = events
            .iter()
            .map(|event| &event.header as *const clap_event_header)
            .collect();
        let list = EventList {
            events: &headers,
            failure: &self.failure,
        };
        let input = clap_input_events {
            ctx: &list as *const EventList<'_> as *mut c_void,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };
        let output = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(output_events_push),
        };
        // SAFETY: event/stream storage is borrowed only for this exact flush;
        // main is temporarily the symbolic audio thread while the owner is active.
        let result = unsafe { clap_guard::flush(flush, self.plugin, &input, &output) };
        if let Err(fault) = result {
            self.failure.record(ClapOperation::ParameterFlush, fault);
            return Err(fault.describe_for("CLAP"));
        }
        drop(_audio);
        drop(_main);
        self.refresh_parameters()?;
        let queue = self
            .controls
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.control_pending
            .store(!queue.is_empty(), std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// An exact cached-control binding, prepared without enqueuing or native calls.
/// The retained core identity and descriptor are checked again at consumption.
/// A validated control write awaiting consumption.
pub struct PreparedClapControlParameter {
    source: Arc<ClapCore>,
    parameter: PluginParam,
    actual: f64,
    normalized: f64,
}
impl PreparedClapControlParameter {
    /// Normalized 0..1 value computed at preparation.
    pub fn normalized_value(&self) -> f64 {
        self.normalized
    }
}
impl ClapInstance {
    fn admit_editor_control(&self) -> Result<(), String> {
        if !self
            .core
            .can_flush
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(
                "This CLAP plugin cannot apply editor controls through parameter flushing".into(),
            );
        }
        if self.processing || self.core.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err("CLAP editor controls require a live idle editor owner".into());
        }
        if self
            .core
            .host
            .params_changed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(
                "The CLAP parameter inventory changed; refresh the parameters before editing"
                    .into(),
            );
        }
        Ok(())
    }
    /// Validate and clamp a control write without queueing it.
    pub fn prepare_control_parameter_value(
        &self,
        name: &str,
        value: f64,
    ) -> Result<PreparedClapControlParameter, String> {
        if !value.is_finite() {
            return Err("CLAP parameter values must be finite".into());
        }
        self.admit_editor_control()?;
        let parameters = self
            .core
            .parameters
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let parameter = parameters
            .iter()
            .find(|param| param.name == name)
            .ok_or_else(|| format!("Unknown CLAP parameter '{name}'"))?;
        if parameter.flags & clap_sys::ext::params::CLAP_PARAM_IS_READONLY != 0 {
            return Err(format!("CLAP parameter '{name}' is read-only"));
        }
        if !parameter.min.is_finite() || !parameter.max.is_finite() || parameter.min > parameter.max
        {
            return Err(format!(
                "CLAP parameter '{name}' has invalid range metadata"
            ));
        }
        let actual = value.clamp(parameter.min, parameter.max);
        let normalized = crate::params::normalize(actual, parameter.min, parameter.max);
        Ok(PreparedClapControlParameter {
            source: Arc::clone(&self.core),
            parameter: parameter.clone(),
            actual,
            normalized,
        })
    }
    /// Queue a prepared write after rechecking its owner and inventory.
    pub fn queue_prepared_control_parameter(
        &self,
        prepared: PreparedClapControlParameter,
    ) -> Result<(), String> {
        if !Arc::ptr_eq(&self.core, &prepared.source) {
            return Err("The CLAP parameter owner was replaced".into());
        }
        self.admit_editor_control()?;
        let parameters = self
            .core
            .parameters
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let parameter = parameters
            .iter()
            .find(|param| param.name == prepared.parameter.name)
            .ok_or_else(|| {
                "The CLAP parameter inventory changed; refresh the parameters before editing"
                    .to_string()
            })?;
        let bound = &prepared.parameter;
        if parameter.id != bound.id
            || parameter.name != bound.name
            || parameter.module != bound.module
            || parameter.min.to_bits() != bound.min.to_bits()
            || parameter.max.to_bits() != bound.max.to_bits()
            || parameter.default.to_bits() != bound.default.to_bits()
            || parameter.flags != bound.flags
        {
            return Err(
                "The CLAP parameter inventory changed; refresh the parameters before editing"
                    .into(),
            );
        }
        // Keep the authoritative metadata lock through queue admission, matching
        // the original parameters→controls lock order. Values are not recomputed.
        self.admit_editor_control()?;
        let mut controls = self
            .core
            .controls
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = controls.iter_mut().find(|(id, _)| *id == parameter.id) {
            existing.1 = prepared.actual;
        } else {
            if controls.len() >= 256 {
                return Err(
                    "The CLAP control queue is full; retry after pending edits are applied".into(),
                );
            }
            controls.push((parameter.id, prepared.actual));
        }
        self.core
            .control_pending
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}
