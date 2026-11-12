use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

fn device(
    key: &str,
    name: &str,
    rates: &[u32],
    buffer: Option<(u32, u32)>,
    default: bool,
) -> OutputDevice {
    OutputDevice {
        key: key.to_string(),
        name: name.to_string(),
        is_default: default,
        channels: 2,
        sample_rates: rates.to_vec(),
        default_rate: rates.get(1).copied().unwrap_or(48_000),
        buffer_frames: buffer,
    }
}

fn three_devices() -> Vec<OutputDevice> {
    vec![
        device(
            "coreaudio:speakers",
            "Speakers",
            &[44_100, 48_000],
            Some((64, 2_048)),
            true,
        ),
        device(
            "coreaudio:usb",
            "USB Interface",
            &[44_100, 48_000, 96_000],
            Some((32, 1_024)),
            false,
        ),
        device("coreaudio:hdmi", "HDMI", &[48_000], None, false),
    ]
}

fn backend() -> Arc<FakeBackend> {
    Arc::new(FakeBackend::new(
        three_devices(),
        Some("coreaudio:speakers".to_string()),
    ))
}

/// A managed stream double: records every reopen and can refuse to come back.
struct TestStream {
    role: &'static str,
    open: AtomicBool,
    reopened: Mutex<Vec<StreamSpec>>,
    reopens: AtomicUsize,
}

impl TestStream {
    fn new(role: &'static str, open: bool) -> Arc<Self> {
        Arc::new(Self {
            role,
            open: AtomicBool::new(open),
            reopened: Mutex::new(Vec::new()),
            reopens: AtomicUsize::new(0),
        })
    }

    fn reopens(&self) -> usize {
        self.reopens.load(Ordering::Acquire)
    }

    fn specs(&self) -> Vec<StreamSpec> {
        self.reopened.lock().unwrap().clone()
    }
}

impl ManagedStream for TestStream {
    fn role(&self) -> &'static str {
        self.role
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    fn reopen(&self, spec: &StreamSpec) -> Result<(), String> {
        self.reopens.fetch_add(1, Ordering::AcqRel);
        self.reopened.lock().unwrap().push(spec.clone());
        self.open.store(true, Ordering::Release);
        Ok(())
    }
}

/// A fresh directory with an `audio.json` path inside it. The settings file
/// always lives in its own directory, exactly like `~/.opencompose`.
fn temp_path(name: &str) -> (PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let dir =
        std::env::temp_dir().join(format!("oc-audio-io-{}-{nanos}-{name}", std::process::id()));
    let path = dir.join("audio.json");
    (dir, path)
}

#[test]
fn selection_persists_and_restores_across_a_restart() {
    let (dir, path) = temp_path("restart");
    let io = AudioIo::new(backend(), Some(path.clone()));
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        output_device_name: None,
        sample_rate: Some(48_000),
        buffer_frames: Some(256),
    });
    assert_eq!(
        status.opened.as_ref().map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:usb"))
    );
    assert!(status.notices.is_empty(), "{:?}", status.notices);
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(json.contains("coreaudio:usb"));
    assert!(json.contains("USB Interface"));

    let restarted = AudioIo::new(backend(), Some(path.clone()));
    let restored = restarted.status();
    assert_eq!(
        restored.selection.output_device.as_deref(),
        Some("coreaudio:usb")
    );
    let opened = restored.opened.expect("restored device");
    assert_eq!(opened.name, "USB Interface");
    assert_eq!(opened.sample_rate, 48_000);
    assert_eq!(opened.buffer_frames, Some(256));
    assert!(!restored.missing_device);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_missing_stored_device_falls_back_to_the_default_and_reports_it() {
    let (dir, path) = temp_path("missing");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &path,
        r#"{"output_device":"coreaudio:gone","output_device_name":"Studio Box","sample_rate":48000}"#,
    )
    .unwrap();
    let backend = backend();
    let io = AudioIo::new(backend.clone(), Some(path.clone()));
    let main = TestStream::new("main", true);
    io.register_stream(main.clone());
    let status = io.status();
    assert!(status.missing_device);
    assert_eq!(
        status.opened.as_ref().map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:speakers")),
        "the system default opens instead"
    );
    assert!(
        status.notices.contains(&AudioNotice::MissingDevice {
            key: "coreaudio:gone".to_string(),
            name: "Studio Box".to_string()
        }),
        "{:?}",
        status.notices
    );
    assert_eq!(
        status.selection.output_device.as_deref(),
        Some("coreaudio:gone"),
        "the stored key survives the fallback"
    );

    // The device comes back: a refresh plus a reconnect restores it.
    let mut devices = three_devices();
    devices.push(device(
        "coreaudio:gone",
        "Studio Box",
        &[44_100, 48_000],
        Some((64, 1_024)),
        false,
    ));
    backend.set_devices(devices);
    io.refresh().unwrap();
    assert!(!io.status().missing_device, "the device is present again");
    assert!(io.reconnect(), "a reconnect moves the open streams back");
    assert_eq!(main.reopens(), 1);
    assert_eq!(
        main.specs().pop().unwrap().key.as_deref(),
        Some("coreaudio:gone")
    );
    let reconnected = io.status();
    assert!(!reconnected.missing_device);
    assert_eq!(
        reconnected
            .opened
            .as_ref()
            .map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:gone"))
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_unsupported_rate_opens_the_nearest_one_and_says_so() {
    let io = AudioIo::new(backend(), None);
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:speakers".to_string()),
        sample_rate: Some(96_000),
        ..AudioSelection::default()
    });
    let opened = status.opened.expect("device open");
    assert_eq!(opened.sample_rate, 48_000);
    assert!(
        status.notices.contains(&AudioNotice::RateSubstituted {
            requested: 96_000,
            chosen: 48_000
        }),
        "{:?}",
        status.notices
    );
    assert_eq!(
        status.selection.sample_rate,
        Some(96_000),
        "the requested rate is what gets stored"
    );
}

#[test]
fn a_failed_open_snaps_back_to_the_previous_device_with_the_reason() {
    let backend = backend();
    let io = AudioIo::new(backend.clone(), None);
    let first = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        ..AudioSelection::default()
    });
    assert!(first.failure.is_none());

    backend.fail_open("HDMI", "the device is in use");
    let failed = io.apply(&AudioSelection {
        output_device: Some("coreaudio:hdmi".to_string()),
        ..AudioSelection::default()
    });
    assert_eq!(
        failed.selection.output_device.as_deref(),
        Some("coreaudio:usb"),
        "the picker shows the device that is still open"
    );
    assert_eq!(
        failed.opened.as_ref().map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:usb"))
    );
    match failed.failure.expect("failure") {
        AudioFailure::OpenFailed {
            device,
            error,
            fallback,
        } => {
            assert_eq!(device, "HDMI");
            assert_eq!(error, "the device is in use");
            assert_eq!(fallback.as_deref(), Some("USB Interface"));
        }
        other => panic!("unexpected failure {other:?}"),
    }
    assert!(!failed.stream_open);
}

#[test]
fn a_switch_rebuilds_only_the_streams_that_were_open() {
    let io = AudioIo::new(backend(), None);
    let main = TestStream::new("main", true);
    let audition = TestStream::new("audition", true);
    let editor = TestStream::new("editor", false);
    io.register_stream(main.clone());
    io.register_stream(audition.clone());
    io.register_stream(editor.clone());

    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        sample_rate: Some(48_000),
        ..AudioSelection::default()
    });
    assert!(status.failure.is_none(), "{:?}", status.failure);
    assert!(status.stream_open);

    assert_eq!(main.reopens(), 1);
    assert_eq!(audition.reopens(), 1);
    assert_eq!(editor.reopens(), 0, "a closed stream stays closed");
    let spec = main.specs().pop().unwrap();
    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.key.as_deref(), Some("coreaudio:usb"));
    assert_eq!(audition.specs().pop().unwrap(), spec);

    let report = io.sweep(&io.spec());
    assert_eq!(report.attempted, vec!["main", "audition"]);
    assert!(report.failed.is_empty());
}

#[test]
fn a_stream_that_cannot_come_back_cancels_the_switch() {
    let backend = backend();
    let io = AudioIo::new(backend.clone(), None);
    let first = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        ..AudioSelection::default()
    });
    assert!(first.failure.is_none());

    struct Broken(Arc<AtomicUsize>);
    impl ManagedStream for Broken {
        fn role(&self) -> &'static str {
            "main"
        }
        fn is_open(&self) -> bool {
            true
        }
        fn reopen(&self, _spec: &StreamSpec) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err("the device refused a second stream".to_string())
        }
    }
    let reopens = Arc::new(AtomicUsize::new(0));
    let broken: Arc<dyn ManagedStream> = Arc::new(Broken(Arc::clone(&reopens)));
    io.register_stream(Arc::clone(&broken));

    let failed = io.apply(&AudioSelection {
        output_device: Some("coreaudio:hdmi".to_string()),
        ..AudioSelection::default()
    });
    assert_eq!(
        failed.selection.output_device.as_deref(),
        Some("coreaudio:usb"),
        "nothing moved, so the picker keeps the working device"
    );
    assert!(matches!(
        failed.failure,
        Some(AudioFailure::StreamLost { .. })
    ));
    assert_eq!(
        reopens.load(Ordering::Acquire),
        1,
        "failure retains the old stream"
    );
}

#[test]
fn a_buffer_outside_the_device_range_is_clamped() {
    let io = AudioIo::new(backend(), None);
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        buffer_frames: Some(4_096),
        ..AudioSelection::default()
    });
    assert_eq!(status.opened.unwrap().buffer_frames, Some(1_024));
    assert!(
        status.notices.contains(&AudioNotice::BufferClamped {
            requested: 4_096,
            chosen: 1_024,
            min: 32,
            max: 1_024
        }),
        "{:?}",
        status.notices
    );
}

#[test]
fn a_device_that_controls_its_period_keeps_the_default_with_a_note() {
    let io = AudioIo::new(backend(), None);
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:hdmi".to_string()),
        buffer_frames: Some(512),
        ..AudioSelection::default()
    });
    assert_eq!(status.opened.unwrap().buffer_frames, None);
    assert!(
        status
            .notices
            .contains(&AudioNotice::BufferUnsupported { requested: 512 }),
        "{:?}",
        status.notices
    );
}

#[test]
fn a_refused_fixed_period_falls_back_and_reports_the_backend_error() {
    let backend = backend();
    backend.refuse_fixed_buffer("USB Interface");
    let io = AudioIo::new(backend, None);
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        buffer_frames: Some(256),
        ..AudioSelection::default()
    });
    let opened = status.opened.expect("device open");
    assert_eq!(opened.buffer_frames, None);
    assert!(!opened.buffer_fixed);
    assert!(
        status
            .notices
            .iter()
            .any(|notice| matches!(notice, AudioNotice::BufferRejected { requested: 256, .. })),
        "{:?}",
        status.notices
    );
}

#[test]
fn a_corrupt_settings_file_uses_defaults_and_says_so() {
    let (dir, path) = temp_path("corrupt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "{ this is not json").unwrap();
    let io = AudioIo::new(backend(), Some(path.clone()));
    let status = io.status();
    assert!(status.selection.output_device.is_none());
    assert!(
        status
            .notices
            .iter()
            .any(|notice| matches!(notice, AudioNotice::SettingsUnreadable { .. })),
        "{:?}",
        status.notices
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_devices_is_a_failure_not_a_silent_success() {
    let io = AudioIo::new(Arc::new(NullBackend), None);
    let status = io.apply(&AudioSelection::default());
    assert_eq!(status.failure, Some(AudioFailure::NoDevices));
    assert!(status.opened.is_none());
    assert!(status.devices.is_empty());
}

#[test]
fn a_dead_stream_is_reported_in_the_status_and_clears_the_opened_stream() {
    let io = AudioIo::new(backend(), None);
    let opened = io.apply(&AudioSelection::default());
    assert!(opened.opened.is_some());
    io.note_stream_failure("main", "the device was removed");
    let status = io.status();
    assert!(
        status.notices.iter().any(|notice| matches!(
            notice,
            AudioNotice::StreamFailed { role, error }
                if role == "main" && error == "the device was removed"
        )),
        "{:?}",
        status.notices
    );
}

#[test]
fn the_default_device_follows_the_os_rather_than_a_pinned_key() {
    let backend = backend();
    let io = AudioIo::new(backend.clone(), None);
    let status = io.apply(&AudioSelection::default());
    assert_eq!(status.selection.output_device, None);
    assert_eq!(io.spec().key, None, "system default is not pinned");
    assert_eq!(
        status.opened.as_ref().map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:speakers"))
    );

    // The OS default moves; the next open follows it.
    backend.set_devices(vec![
        device("coreaudio:usb", "USB Interface", &[48_000], None, true),
        device(
            "coreaudio:speakers",
            "Speakers",
            &[44_100, 48_000],
            Some((64, 2_048)),
            false,
        ),
    ]);
    io.refresh().unwrap();
    assert_eq!(
        io.status()
            .opened
            .as_ref()
            .map(|opened| opened.key.as_deref()),
        Some(Some("coreaudio:usb"))
    );
    assert_eq!(io.spec().sample_rate, 48_000);
}

#[test]
fn the_nearest_rate_prefers_the_closer_one_and_the_lower_on_a_tie() {
    assert_eq!(nearest_rate(&[44_100, 48_000], 96_000), 48_000);
    assert_eq!(nearest_rate(&[44_100, 48_000], 46_500), 48_000);
    assert_eq!(nearest_rate(&[44_100, 48_000], 46_050), 44_100);
    assert_eq!(nearest_rate(&[], 48_000), 48_000);
}

#[test]
fn a_selection_that_only_changes_the_device_keeps_the_rate_and_period() {
    let io = AudioIo::new(backend(), None);
    let status = io.apply(&AudioSelection {
        output_device: Some("coreaudio:usb".to_string()),
        sample_rate: Some(48_000),
        buffer_frames: Some(512),
        output_device_name: None,
    });
    let opened = status.opened.expect("device open");
    assert_eq!(opened.sample_rate, 48_000);
    assert_eq!(opened.buffer_frames, Some(512));
    assert!(status.notices.is_empty(), "{:?}", status.notices);
}

#[test]
#[ignore = "enumerates the real devices; run manually on each platform"]
fn native_device_keys_and_formats() {
    let backend = CpalBackend::new();
    let devices = backend.list_outputs().expect("device list");
    println!("default = {:?}", backend.default_output());
    for device in &devices {
        println!(
            "{} | {} | default={} | channels={} | rates={:?} | default_rate={} | buffer={:?}",
            device.key,
            device.name,
            device.is_default,
            device.channels,
            device.sample_rates,
            device.default_rate,
            device.buffer_frames
        );
        assert!(
            !device.key.starts_with("name:"),
            "no stable key for {}",
            device.name
        );
    }
}

// ============================================================================
// Capture
// ============================================================================

fn input(key: &str, name: &str, rates: &[u32], channels: u16, default: bool) -> InputDevice {
    InputDevice {
        key: key.to_string(),
        name: name.to_string(),
        is_default: default,
        channels,
        sample_rates: rates.to_vec(),
        default_rate: rates.get(1).copied().unwrap_or(48_000),
    }
}

/// A capture double that keeps every frame it is given.
#[derive(Default)]
struct CaptureSink {
    frames: Mutex<Vec<f32>>,
    errors: Mutex<Vec<String>>,
}

impl CaptureSink {
    fn frames(&self) -> Vec<f32> {
        self.frames.lock().unwrap().clone()
    }
}

impl InputSink for CaptureSink {
    fn capture(&self, samples: &[f32], _channels: u16) {
        self.frames.lock().unwrap().extend_from_slice(samples);
    }

    fn failed(&self, error: String) {
        self.errors.lock().unwrap().push(error);
    }
}

fn input_backend() -> Arc<FakeBackend> {
    Arc::new(
        FakeBackend::new(three_devices(), Some("coreaudio:speakers".to_string())).with_inputs(
            vec![
                input(
                    "coreaudio:mic",
                    "Built-in Microphone",
                    &[44_100, 48_000],
                    1,
                    true,
                ),
                input(
                    "coreaudio:usb-in",
                    "USB Interface Input",
                    &[44_100, 48_000, 96_000],
                    2,
                    false,
                ),
            ],
            Some("coreaudio:mic".to_string()),
        ),
    )
}

#[test]
fn input_devices_are_listed_with_a_default_and_stable_keys() {
    let io = AudioIo::new(input_backend(), None);
    io.refresh_inputs().expect("explicit capture discovery");
    let status = io.status();
    assert_eq!(status.inputs.len(), 2);
    assert_eq!(status.default_input.as_deref(), Some("coreaudio:mic"));
    assert!(status.inputs.iter().any(|device| device.is_default));
    assert!(
        status
            .inputs
            .iter()
            .all(|device| !device.key.starts_with("name:"))
    );
    // Nothing was ever chosen, so the default input is what would open.
    assert_eq!(
        status
            .opened_input
            .as_ref()
            .map(|opened| opened.name.as_str()),
        Some("Built-in Microphone")
    );
    assert!(
        !status.input.input_monitor,
        "monitoring must never default on"
    );
}

#[test]
fn a_fresh_install_captures_stereo() {
    let io = AudioIo::new(input_backend(), None);
    let status = io.status();
    assert_eq!(status.input.input_channels, InputChannels::Stereo);
    assert_eq!(status.input.input_channels.count(), 2);
}

#[test]
fn input_selection_persists_next_to_the_output_selection() {
    let (dir, path) = temp_path("input-persist");
    let io = AudioIo::new(input_backend(), Some(path.clone()));
    let status = io.apply_input(&InputSelection {
        input_device: Some("coreaudio:usb-in".to_string()),
        input_device_name: None,
        input_channels: InputChannels::Stereo,
        input_monitor: true,
        input_monitor_gain_db: -6.0,
    });
    assert!(status.input_failure.is_none(), "{:?}", status.input_failure);
    assert_eq!(
        status.opened_input.as_ref().map(|o| o.channels),
        Some(2),
        "stereo capture opens two channels"
    );
    let text = std::fs::read_to_string(&path).expect("settings file");
    assert!(text.contains("\"input_device\""), "{text}");
    assert!(text.contains("\"input_channels\": \"stereo\""), "{text}");
    assert!(text.contains("\"input_monitor\": true"), "{text}");

    let restarted = AudioIo::new(input_backend(), Some(path));
    let restored = restarted.status();
    assert_eq!(
        restored.input.input_device.as_deref(),
        Some("coreaudio:usb-in")
    );
    assert_eq!(restored.input.input_channels, InputChannels::Stereo);
    assert!(restored.input.input_monitor);
    assert_eq!(restored.input.input_monitor_gain_db, -6.0);
    // The output side of the same file survived the round trip.
    assert_eq!(
        restored.opened.as_ref().map(|opened| opened.name.as_str()),
        Some("Speakers")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_missing_input_device_falls_back_to_the_default_and_keeps_the_key() {
    let (dir, path) = temp_path("input-missing");
    std::fs::create_dir_all(&dir).unwrap();
    // Written by an older build: no capture keys at all.
    std::fs::write(
        &path,
        r#"{"output_device":"coreaudio:speakers","sample_rate":48000}"#,
    )
    .unwrap();
    let io = AudioIo::new(input_backend(), Some(path.clone()));
    let status = io.status();
    assert!(!status.input.input_monitor);
    // A file from before the capture keys existed takes the current default,
    // which is stereo.
    assert_eq!(status.input.input_channels, InputChannels::Stereo);

    // Store a capture device, then restart with a machine it is gone from.
    AudioIo::new(input_backend(), Some(path.clone())).apply_input(&InputSelection {
        input_device: Some("coreaudio:usb-in".to_string()),
        input_device_name: Some("USB Interface Input".to_string()),
        ..InputSelection::default()
    });
    let reduced = Arc::new(
        FakeBackend::new(three_devices(), Some("coreaudio:speakers".to_string())).with_inputs(
            vec![input(
                "coreaudio:mic",
                "Built-in Microphone",
                &[48_000],
                1,
                true,
            )],
            Some("coreaudio:mic".to_string()),
        ),
    );
    let io = AudioIo::new(reduced.clone(), Some(path.clone()));
    io.refresh_inputs().expect("explicit capture discovery");
    let status = io.status();
    assert!(status.missing_input);
    assert_eq!(
        status.input.input_device.as_deref(),
        Some("coreaudio:usb-in"),
        "the stored key is kept so a reconnected device comes back"
    );
    assert_eq!(
        status
            .opened_input
            .as_ref()
            .map(|opened| opened.name.as_str()),
        Some("Built-in Microphone"),
        "the default captures in the meantime"
    );
    let notice = status
        .input_notices
        .iter()
        .find(|notice| matches!(notice, AudioNotice::MissingInputDevice { .. }))
        .expect("a missing-input notice");
    match notice {
        AudioNotice::MissingInputDevice { name, .. } => assert_eq!(name, "USB Interface Input"),
        other => panic!("unexpected notice {other:?}"),
    }

    // Naming a device that is not connected is refused and changes nothing.
    let refused = io.apply_input(&InputSelection {
        input_device: Some("coreaudio:gone".to_string()),
        ..InputSelection::default()
    });
    assert!(matches!(
        refused.input_failure,
        Some(AudioFailure::InputOpenFailed { .. })
    ));
    assert_eq!(
        refused.input.input_device.as_deref(),
        Some("coreaudio:usb-in"),
        "a refused choice never replaces the stored one"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failed_input_open_snaps_back_to_the_previous_device_with_the_reason() {
    let backend = input_backend();
    let io = AudioIo::new(backend.clone() as Arc<dyn DeviceBackend>, None);
    io.apply_input(&InputSelection {
        input_device: Some("coreaudio:mic".to_string()),
        ..InputSelection::default()
    });
    backend.fail_input_open("USB Interface Input", "the device is busy");
    let status = io.apply_input(&InputSelection {
        input_device: Some("coreaudio:usb-in".to_string()),
        input_device_name: Some("USB Interface Input".to_string()),
        ..InputSelection::default()
    });
    match status.input_failure.expect("a failure") {
        AudioFailure::InputOpenFailed { device, error, .. } => {
            assert_eq!(device, "USB Interface Input");
            assert_eq!(error, "the device is busy");
        }
        other => panic!("unexpected failure {other:?}"),
    }
    assert_eq!(
        status.input.input_device.as_deref(),
        Some("coreaudio:mic"),
        "the rejected choice must not stick"
    );
    assert_eq!(
        status
            .opened_input
            .as_ref()
            .map(|opened| opened.name.as_str()),
        Some("Built-in Microphone")
    );
}

#[test]
fn capture_opens_at_the_output_rate_when_the_device_offers_it() {
    let backend = input_backend();
    let io = AudioIo::new(backend.clone() as Arc<dyn DeviceBackend>, None);
    io.apply(&AudioSelection {
        sample_rate: Some(48_000),
        ..AudioSelection::default()
    });
    let status = io.apply_input(&InputSelection {
        input_device: Some("coreaudio:usb-in".to_string()),
        input_channels: InputChannels::Stereo,
        ..InputSelection::default()
    });
    assert!(status.input_failure.is_none());
    let opened = status.opened_input.expect("open capture");
    assert_eq!(opened.sample_rate, 48_000);
    let opens = backend.input_opens();
    let spec = opens.last().expect("an open attempt").1.clone();
    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.channels, 2);
    assert!(
        status
            .input_notices
            .iter()
            .all(|notice| !matches!(notice, AudioNotice::InputRateSubstituted { .. })),
        "{:?}",
        status.input_notices
    );
}

#[test]
fn an_input_without_the_output_rate_is_substituted_and_says_so() {
    let backend = Arc::new(
        FakeBackend::new(three_devices(), Some("coreaudio:speakers".to_string())).with_inputs(
            vec![input(
                "coreaudio:mic",
                "Built-in Microphone",
                &[44_100],
                1,
                true,
            )],
            Some("coreaudio:mic".to_string()),
        ),
    );
    let io = AudioIo::new(backend, None);
    io.apply(&AudioSelection {
        sample_rate: Some(48_000),
        ..AudioSelection::default()
    });
    let status = io.apply_input(&InputSelection::default());
    let opened = status.opened_input.expect("open capture");
    assert_eq!(opened.sample_rate, 44_100);
    assert!(
        status.input_notices.iter().any(|notice| matches!(
            notice,
            AudioNotice::InputRateSubstituted {
                requested: 48_000,
                chosen: 44_100
            }
        )),
        "{:?}",
        status.input_notices
    );
}

#[test]
fn a_stereo_request_on_a_mono_input_is_narrowed_with_a_notice() {
    let backend = Arc::new(
        FakeBackend::new(three_devices(), Some("coreaudio:speakers".to_string())).with_inputs(
            vec![input(
                "coreaudio:mic",
                "Built-in Microphone",
                &[48_000],
                1,
                true,
            )],
            Some("coreaudio:mic".to_string()),
        ),
    );
    let io = AudioIo::new(backend, None);
    let status = io.apply_input(&InputSelection {
        input_channels: InputChannels::Stereo,
        ..InputSelection::default()
    });
    assert_eq!(status.opened_input.expect("open capture").channels, 1);
    assert!(
        status.input_notices.iter().any(|notice| matches!(
            notice,
            AudioNotice::InputChannelsReduced {
                requested: 2,
                chosen: 1
            }
        )),
        "{:?}",
        status.input_notices
    );
}

#[test]
fn capture_frames_reach_the_registered_sink() {
    let backend = input_backend();
    let io = AudioIo::new(backend.clone() as Arc<dyn DeviceBackend>, None);
    io.apply_input(&InputSelection::default());
    let sink = Arc::new(CaptureSink::default());
    io.open_input_stream(sink.clone()).expect("open capture");
    backend.push_input(&[0.25, -0.25, 0.5, -0.5], 1);
    assert_eq!(sink.frames(), vec![0.25, -0.25, 0.5, -0.5]);
    assert_eq!(
        io.status()
            .opened_input
            .expect("opened input")
            .name
            .as_str(),
        "Built-in Microphone"
    );
}

#[test]
fn a_dead_capture_stream_is_reported_as_a_notice() {
    let backend = input_backend();
    let io = AudioIo::new(backend.clone() as Arc<dyn DeviceBackend>, None);
    io.apply_input(&InputSelection::default());
    let sink = Arc::new(CaptureSink::default());
    io.open_input_stream(sink.clone()).expect("open capture");
    backend.disconnect_input("the microphone was unplugged");
    assert_eq!(
        sink.errors.lock().unwrap().as_slice(),
        &["the microphone was unplugged".to_string()]
    );
    io.note_input_failure("the microphone was unplugged");
    let status = io.status();
    assert!(
        status
            .input_notices
            .iter()
            .any(|notice| matches!(notice, AudioNotice::InputFailed { error } if error == "the microphone was unplugged")),
        "{:?}",
        status.input_notices
    );
}

#[test]
fn a_backend_with_no_inputs_reports_no_input_devices() {
    let io = AudioIo::new(
        Arc::new(FakeBackend::new(
            three_devices(),
            Some("coreaudio:speakers".to_string()),
        )),
        None,
    );
    assert!(!io.has_inputs());
    let status = io.apply_input(&InputSelection::default());
    assert_eq!(status.input_failure, Some(AudioFailure::NoInputDevices));
}

#[test]
fn output_startup_and_status_defer_capture_discovery() {
    let backend = input_backend();
    let io = AudioIo::new(backend.clone(), None);
    assert_eq!(io.sample_rate(), 48_000);
    assert!(io.has_devices());
    assert!(!io.has_inputs());
    assert!(io.status().inputs.is_empty());
    io.refresh().unwrap();
    assert!(!io.state.lock().unwrap().input_listed);
    io.input_selection();
    assert!(!io.state.lock().unwrap().input_listed);
    assert_eq!(backend.lock().input_lists, 0);
    assert!(!io.input_devices_listed());
    let spec = io.input_spec();
    assert_eq!(backend.lock().input_lists, 1);
    assert!(io.input_devices_listed());
    assert!(spec.channels > 0);
    assert!(io.has_inputs());
    assert!(io.state.lock().unwrap().input_listed);
}
