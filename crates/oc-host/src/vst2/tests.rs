//! Fixture-backed tests for the VST2 backend.

use std::path::PathBuf;

use super::*;
use crate::{PluginCategory, PluginFormat};

fn fixture(name: &str) -> Option<PathBuf> {
    let directory = PathBuf::from(option_env!("OC_VST2_FIXTURE_DIR")?);
    #[cfg(target_os = "macos")]
    let path = directory.join(format!("{name}.vst"));
    #[cfg(target_os = "windows")]
    let path = directory.join(format!("{name}.dll"));
    #[cfg(target_os = "linux")]
    let path = directory.join(format!("{name}.so"));
    path.exists().then_some(path)
}

macro_rules! fixture_or_skip {
    ($name:expr) => {
        match fixture($name) {
            Some(path) => path,
            None => {
                eprintln!("skipping: VST2 fixture '{}' was not built", $name);
                return;
            }
        }
    };
}

fn constant(frames: usize, value: f32) -> Vec<f32> {
    vec![value; frames * 2]
}

#[test]
fn abi_layout_matches_the_published_interface() {
    assert_eq!(abi::EFFECT_MAGIC, 0x5673_7450);
    assert_eq!(std::mem::size_of::<abi::AEffect>(), 192);
    #[cfg(target_pointer_width = "64")]
    {
        use std::mem::offset_of as offset;
        assert_eq!(offset!(abi::AEffect, magic), 0);
        assert_eq!(offset!(abi::AEffect, dispatcher), 8);
        assert_eq!(offset!(abi::AEffect, process), 16);
        assert_eq!(offset!(abi::AEffect, set_parameter), 24);
        assert_eq!(offset!(abi::AEffect, get_parameter), 32);
        assert_eq!(offset!(abi::AEffect, num_programs), 40);
        assert_eq!(offset!(abi::AEffect, num_params), 44);
        assert_eq!(offset!(abi::AEffect, num_inputs), 48);
        assert_eq!(offset!(abi::AEffect, num_outputs), 52);
        assert_eq!(offset!(abi::AEffect, flags), 56);
        assert_eq!(offset!(abi::AEffect, reserved1), 64);
        assert_eq!(offset!(abi::AEffect, reserved2), 72);
        assert_eq!(offset!(abi::AEffect, initial_delay), 80);
        assert_eq!(offset!(abi::AEffect, real_qualities), 84);
        assert_eq!(offset!(abi::AEffect, off_qualities), 88);
        assert_eq!(offset!(abi::AEffect, io_ratio), 92);
        assert_eq!(offset!(abi::AEffect, object), 96);
        assert_eq!(offset!(abi::AEffect, user), 104);
        assert_eq!(offset!(abi::AEffect, unique_id), 112);
        assert_eq!(offset!(abi::AEffect, version), 116);
        assert_eq!(offset!(abi::AEffect, process_replacing), 120);
        assert_eq!(offset!(abi::AEffect, process_double_replacing), 128);
        assert_eq!(offset!(abi::AEffect, future), 136);
    }
    assert_eq!(std::mem::size_of::<abi::VstMidiEvent>(), 32);
    assert_eq!(std::mem::size_of::<abi::VstTimeInfo>(), 88);
    assert_eq!(
        std::mem::offset_of!(abi::VstEvents, events),
        abi::VST_EVENTS_POINTER_OFFSET
    );
}

#[test]
fn resolves_the_bundle_executable() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let executable = resolve_executable(&bundle).expect("resolve executable");
    assert!(executable.is_file());
    #[cfg(target_os = "macos")]
    let expected_name = "OpenCompose Gain Fixture";
    #[cfg(target_os = "windows")]
    let expected_name = "OpenCompose Gain Fixture.dll";
    #[cfg(target_os = "linux")]
    let expected_name = "OpenCompose Gain Fixture.so";
    assert_eq!(
        executable.file_name().and_then(|name| name.to_str()),
        Some(expected_name)
    );
}

#[test]
fn effect_renders_exact_gain_and_offset() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    assert_eq!(instance.name(), "OpenCompose Gain Fixture");
    assert_eq!(instance.vendor(), "OpenCompose Tests");
    assert_eq!(instance.category(), PluginCategory::Effect);
    assert_eq!(instance.param_count(), 2);
    assert_eq!(instance.output_count(), 1);

    let params = instance.get_params();
    assert_eq!(params[0].name, "Gain");
    assert_eq!(params[1].name, "DC Offset");
    assert_eq!(params[0].default, 0.5);
    assert_eq!(params[1].default, 0.0);

    // gain = 2 * 0.5 = 1, offset = 0: the block passes through unchanged.
    let mut buffer = constant(128, 0.25);
    instance.process_effect_with_transport(&mut buffer, 48_000, None);
    assert!(buffer.iter().all(|sample| *sample == 0.25), "{buffer:?}");

    // gain = 2 * 0.75 = 1.5, offset = 0.25 * 0.5 = 0.125.
    assert!(instance.set_param_by_name("Gain", 0.75));
    assert!(instance.set_param_by_name("DC Offset", 0.5));
    assert_eq!(instance.get_param_by_name("gain"), Some(0.75));
    let mut buffer = constant(128, 0.25);
    instance.process_effect_with_transport(&mut buffer, 48_000, None);
    assert!(
        buffer.iter().all(|sample| *sample == 0.5),
        "expected 0.25 * 1.5 + 0.125: {buffer:?}"
    );
}

#[test]
fn chunk_roundtrip_restores_parameters() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    instance.set_param_by_name("Gain", 0.75);
    instance.set_param_by_name("DC Offset", 0.5);
    let state = instance.state_bytes().expect("capture chunk");
    assert!(state.starts_with(b"OCF2"));

    instance.set_param_by_name("Gain", 0.1);
    let mut changed = constant(32, 0.25);
    instance.process_effect_with_transport(&mut changed, 48_000, None);
    assert!(changed[0] < 0.3, "{}", changed[0]);

    instance.set_state_bytes(&state).expect("restore chunk");
    let params = instance.get_params();
    assert!((params[0].value - 0.75).abs() < 1e-6);
    assert!((params[1].value - 0.5).abs() < 1e-6);
    let mut restored = constant(32, 0.25);
    instance.process_effect_with_transport(&mut restored, 48_000, None);
    assert!(restored.iter().all(|sample| *sample == 0.5));
}

#[test]
fn instrument_receives_midi_and_uses_process_fallback() {
    let bundle = fixture_or_skip!("OpenCompose Tone Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load instrument");
    assert!(instance.is_instrument());
    assert_eq!(instance.num_inputs(), 0);
    assert_eq!(instance.num_outputs(), 2);
    assert!(instance.accepts_midi());
    // The tone fixture omits the replacing flag, so the host must use process.
    assert_eq!(instance.flags & abi::FLAG_CAN_REPLACING, 0);

    let silence = instance.process_block_with_transport(64, None);
    assert!(silence.iter().all(|sample| *sample == 0.0));

    instance.note_on(60, 1.0, 0);
    let sounding = instance.process_block_with_transport(64, None);
    assert_eq!(sounding.len(), 128);
    assert!(sounding.iter().all(|sample| *sample == 0.5), "{sounding:?}");

    instance.note_off(60, 0);
    let released = instance.process_block_with_transport(64, None);
    assert!(released.iter().all(|sample| *sample == 0.0));
}

#[test]
fn host_time_info_follows_the_transport() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    let transport = PluginTransport::new(48_000, 48_000, 137.0);
    let _ = instance.process_block_with_transport(64, Some(transport));
    let info = instance.last_time_info();
    assert!((info.tempo - 137.0).abs() < 1e-9, "{info:?}");
    assert!((info.ppq_pos - 137.0 / 60.0).abs() < 1e-6, "{info:?}");
    assert_eq!(info.sample_pos, 48_000.0);
    assert_eq!(info.flags & abi::TIME_TEMPO_VALID, abi::TIME_TEMPO_VALID);
    let display = &instance.get_params()[1].display;
    assert!(display.contains("t=137.0"), "{display}");
    assert!(display.contains("p=2.283"), "{display}");
    // The plugin reads the host version during construction.
    assert!(display.contains("v=2400"), "{display}");
}

#[test]
fn audio_master_automate_updates_the_parameter_model() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    instance.control_change(7, 64, 0);
    let _ = instance.process_block_with_transport(32, None);
    let expected = f64::from(64.0_f32 / 127.0);
    let recorded = instance.get_param_by_name("Gain").expect("gain");
    assert!(
        (recorded - expected).abs() < 1e-9,
        "{recorded} vs {expected}"
    );
}

#[test]
fn deactivated_instance_renders_silence() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let mut instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    instance.deactivate();
    let buffer = instance.process_block_with_transport(32, None);
    assert!(buffer.iter().all(|sample| *sample == 0.0));
}

#[test]
fn scan_reports_name_category_and_parameter_count() {
    let gain = fixture_or_skip!("OpenCompose Gain Fixture");
    let plugins = scan::scan_bundle(&gain).expect("scan effect");
    assert_eq!(plugins.len(), 1);
    let info = &plugins[0];
    assert_eq!(info.format, PluginFormat::Vst2);
    assert_eq!(info.id, "vst2:opencompose_gain_fixture");
    assert_eq!(info.name, "OpenCompose Gain Fixture");
    assert_eq!(info.vendor, "OpenCompose Tests");
    assert_eq!(info.category, PluginCategory::Effect);
    assert_eq!(info.num_params, 2);
    assert_eq!(info.version.as_deref(), Some("100"));

    let tone = fixture_or_skip!("OpenCompose Tone Fixture");
    let plugins = scan::scan_bundle(&tone).expect("scan instrument");
    assert_eq!(plugins[0].category, PluginCategory::Instrument);
    assert_eq!(plugins[0].id, "vst2:opencompose_tone_fixture");
}

#[test]
fn scan_does_not_invent_a_channel_layout() {
    // Four stereo output pairs and no inputs: the layout the old scanner
    // flattened to 2/2. The instance keeps the declared layout; the scan
    // result carries no channel numbers at all.
    let bundle = fixture_or_skip!("OpenCompose Multi Out Fixture");
    let instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load multi out");
    assert_eq!(instance.num_inputs(), 0);
    assert_eq!(instance.num_outputs(), 8);

    let plugins = scan::scan_bundle(&bundle).expect("scan multi out");
    assert_eq!(plugins[0].id, "vst2:opencompose_multi_out_fixture");
    let json = serde_json::to_value(&plugins[0]).expect("serialize scan result");
    assert!(json.get("num_inputs").is_none(), "{json}");
    assert!(json.get("num_outputs").is_none(), "{json}");
}

#[test]
fn broken_and_missing_bundles_are_errors_not_crashes() {
    let broken = fixture_or_skip!("OpenCompose Broken Fixture");
    let error = match Vst2Instance::load(&broken, 48_000, 512) {
        Ok(_) => panic!("broken fixture must fail"),
        Err(error) => error,
    };
    assert!(error.contains("returned no plugin"), "{error}");
    assert!(scan::scan_bundle(&broken).is_err());

    // A plugin with no audio outputs is valid (an analyzer); it renders silence.
    let no_output = fixture_or_skip!("OpenCompose No Output Fixture");
    let mut instance = Vst2Instance::load(&no_output, 48_000, 512).expect("load analyzer");
    assert_eq!(instance.num_outputs(), 0);
    let silence = instance.process_block_with_transport(32, None);
    assert!(silence.iter().all(|sample| *sample == 0.0));

    let missing = std::env::temp_dir().join("opencompose-missing-plugin.vst");
    let error = match Vst2Instance::load(&missing, 48_000, 512) {
        Ok(_) => panic!("missing path must fail"),
        Err(error) => error,
    };
    assert!(error.contains("does not exist"), "{error}");

    let garbage = std::env::temp_dir().join(format!(
        "opencode-garbage-{}-plugin.vst",
        std::process::id()
    ));
    std::fs::write(&garbage, b"this is not a plugin").expect("write garbage fixture");
    let error = match Vst2Instance::load(&garbage, 48_000, 512) {
        Ok(_) => panic!("garbage must fail"),
        Err(error) => error,
    };
    assert!(error.contains("failed to load VST2 plugin"), "{error}");
    assert!(scan::scan_bundle(&garbage).is_err());
    let _ = std::fs::remove_file(&garbage);

    let empty =
        std::env::temp_dir().join(format!("opencode-empty-{}-bundle.vst", std::process::id()));
    std::fs::create_dir_all(empty.join("Contents/MacOS")).expect("create empty bundle");
    let error = match Vst2Instance::load(&empty, 48_000, 512) {
        Ok(_) => panic!("empty bundle must fail"),
        Err(error) => error,
    };
    assert!(error.contains("contains no binary"), "{error}");
    let _ = std::fs::remove_dir_all(&empty);
}

/// A twenty-parameter plugin as the panel sees it, without loading a bundle.
fn twenty_parameters() -> Vec<Vst2Param> {
    (0..20)
        .map(|index| Vst2Param {
            index,
            name: if index == 3 {
                "Bypass".to_string()
            } else {
                format!("Param {index}")
            },
            label: if index % 2 == 0 {
                "dB".into()
            } else {
                String::new()
            },
            display: format!("{:.2}", f64::from(index) / 20.0),
            value: if index == 3 {
                1.0
            } else {
                f64::from(index) / 20.0
            },
            default: 0.0,
            can_automate: index != 7,
        })
        .collect()
}

#[test]
fn a_twenty_parameter_plugin_lists_every_parameter_for_the_panel() {
    let params = twenty_parameters();
    let surface = crate::params::surface(
        params
            .iter()
            .map(|param| {
                let steps = (param.index == 5).then_some(2);
                descriptor(param, steps)
            })
            .collect(),
    );
    let listed = surface.parameters();
    assert_eq!(listed.len(), 20);
    assert_eq!(listed[0].name, "Param 0");
    assert_eq!(listed[0].unit, "dB");
    assert_eq!(listed[1].unit, "");
    assert_eq!(listed[3].name, "Bypass");
    assert!(listed[3].by_passing, "a bypass control at 1.0 is on");
    assert!(!listed[2].by_passing);
    assert_eq!(listed[5].steps, Some(2));
    assert_eq!(listed[4].steps, None);
    assert!(!listed[7].automatable);
    assert!((listed[10].normalized - 0.5).abs() < 1e-9);
    assert_eq!(listed[10].min, 0.0);
    assert_eq!(listed[10].max, 1.0);
    assert_eq!(listed[10].default, None);
}

#[test]
fn a_bypass_control_reads_as_by_passing_only_when_it_is_on() {
    let off = Vst2Param {
        index: 0,
        name: "Bypass".into(),
        label: String::new(),
        display: "off".into(),
        value: 0.0,
        default: 0.0,
        can_automate: true,
    };
    assert!(!descriptor(&off, Some(2)).by_passing);
    let on = Vst2Param { value: 1.0, ..off };
    assert!(descriptor(&on, Some(2)).by_passing);
}

#[test]
fn a_plugin_with_no_parameters_is_not_an_empty_panel() {
    let surface = crate::params::surface(Vec::new());
    assert_eq!(
        surface,
        crate::params::ParamSurface::Unavailable {
            issue: crate::params::ParamSurfaceIssue::PluginReportsNoParameters
        }
    );
}

#[test]
fn fixture_exposes_a_panel_surface() {
    let bundle = fixture_or_skip!("OpenCompose Gain Fixture");
    let instance = Vst2Instance::load(&bundle, 48_000, 512).expect("load fixture");
    let surface = instance.parameter_surface();
    let listed = surface.parameters();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "Gain");
    assert!((listed[0].normalized - 0.5).abs() < 1e-9);
    assert_eq!(listed[1].name, "DC Offset");
    assert!((listed[1].normalized).abs() < 1e-9);
}

#[cfg(unix)]
struct CopiedModule {
    directory: PathBuf,
    path: PathBuf,
}

#[cfg(unix)]
impl CopiedModule {
    fn new(bundle: &std::path::Path, label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = oc_audio::private_fs::test_root().join(format!(
            "vst2-loader-{label}-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("fixture.so");
        std::fs::copy(resolve_executable(bundle).unwrap(), &path).unwrap();
        Self { directory, path }
    }

    fn mapped(&self) -> bool {
        let path = std::ffi::CString::new(self.path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path is NUL-terminated. RTLD_NOLOAD only obtains a reference to
        // an already loaded module and never executes a new module's entry.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: release only the temporary reference obtained by RTLD_NOLOAD.
        unsafe { libc::dlclose(handle) };
        true
    }

    fn call_entry_after_pin(&self) -> *mut abi::AEffect {
        let path = std::ffi::CString::new(self.path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: path is NUL-terminated and NOLOAD only opens an existing image.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        assert!(!handle.is_null(), "the invoked fixture is not mapped");
        // SAFETY: this test fixture exports the documented VST2 entry signature.
        let symbol = unsafe { libc::dlsym(handle, c"VSTPluginMain".as_ptr()) };
        assert!(!symbol.is_null());
        // SAFETY: release our temporary lookup reference before exercising code;
        // only the production process-pinned reference keeps it alive now.
        assert_eq!(unsafe { libc::dlclose(handle) }, 0);
        // SAFETY: the known fixture's symbol has PluginMain ABI and is pinned.
        let entry = unsafe { std::mem::transmute::<*mut c_void, abi::PluginMain>(symbol) };
        // SAFETY: the fixture accepts this host callback; a returned effect is
        // closed by the test while the pinned module still owns its code.
        unsafe { entry(audio_master) }
    }
}

#[cfg(unix)]
impl Drop for CopiedModule {
    fn drop(&mut self) {
        // Unix permits unlinking a mapped module; the process-pinned mapping
        // remains valid and the test does not leave a copied fixture on disk.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(unix)]
#[test]
fn module_lifetime_releases_uninvoked_loader_reference() {
    let bundle = fixture("OpenCompose Gain Fixture").expect("host build installs the gain fixture");
    let module = CopiedModule::new(&bundle, "uninvoked");
    assert!(!module.mapped());
    let library = DynamicLibrary::open(&module.path).unwrap();
    assert!(module.mapped());
    assert!(library.symbol(c"missing_fixture_symbol").is_null());
    let before = MODULE_RELEASE_COUNTS.with(std::cell::Cell::get);
    drop(library);
    let after = MODULE_RELEASE_COUNTS.with(std::cell::Cell::get);
    assert_eq!(
        after,
        (before.0 + 1, before.1),
        "release the owned reference exactly once"
    );
}

#[cfg(unix)]
#[test]
fn module_lifetime_pins_failed_entry() {
    let bundle =
        fixture("OpenCompose Broken Fixture").expect("host build installs the broken fixture");
    let module = CopiedModule::new(&bundle, "failed-entry");
    assert!(!module.mapped());
    let before = MODULE_RELEASE_COUNTS.with(std::cell::Cell::get);
    let error = match Vst2Instance::load(&module.path, 48_000, 512) {
        Ok(_) => panic!("the broken entry returned an instance"),
        Err(error) => error,
    };
    assert!(error.contains("returned no plugin"), "{error}");
    assert_eq!(
        MODULE_RELEASE_COUNTS.with(std::cell::Cell::get),
        before,
        "a failed entry must not release potentially live plugin code"
    );
    assert!(module.call_entry_after_pin().is_null());
}

#[cfg(unix)]
#[test]
fn module_lifetime_pins_closed_instance() {
    let bundle = fixture("OpenCompose Gain Fixture").expect("host build installs the gain fixture");
    let module = CopiedModule::new(&bundle, "closed-instance");
    assert!(!module.mapped());
    let before = MODULE_RELEASE_COUNTS.with(std::cell::Cell::get);
    let instance = Vst2Instance::load(&module.path, 48_000, 512).unwrap();
    drop(instance);
    assert_eq!(
        MODULE_RELEASE_COUNTS.with(std::cell::Cell::get),
        before,
        "effClose must not release potentially live plugin code"
    );
    let effect = module.call_entry_after_pin();
    assert!(!effect.is_null());
    let host = Arc::new(HostContext::new(48_000, 512, 2));
    // SAFETY: The known fixture returned a live, valid AEffect. Copy its
    // mandatory dispatcher while pinned code and descriptor remain owned.
    let dispatcher = unsafe { (*effect).dispatcher }.expect("fixture dispatcher");
    // SAFETY: The fixture was never activated; retained callback backing and
    // pinned code outlive synchronous CLOSE of this callable-code probe.
    unsafe { close_effect(effect, dispatcher, &host, false) };
}
