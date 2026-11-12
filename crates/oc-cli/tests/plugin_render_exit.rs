//! Native CLI render behavior, using owned fixtures rather than installed plugins.

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn vst3_tail_render(params: &str) -> (std::process::Output, bool) {
    let source = format!(
        "@project\n  tempo: 120\n@preset voice\n  source: vst3\n  plugin: \"test.fake.one_bus\"\n  params: {{{params}}}\n@track lead | preset: voice, instrument: synth/lead\n  @phrase held [1:1 - 1:2]\n    C4 q mf\n"
    );
    let (output, exists, _) = vst3_component_render(&source);
    (output, exists)
}
// Controlled ABI components are acquired inside the shared UI preparation
// owner. This proves the real CLI/cache/lease pipeline, not dynamic-module or
// factory loading; the compiled C++ native-main audit proves native ABI calls.
fn vst3_component_render(source: &str) -> (std::process::Output, bool, Vec<f32>) {
    let result = component_render(source, false);
    (result.output, result.exists, result.samples)
}
struct ComponentRender {
    output: std::process::Output,
    exists: bool,
    samples: Vec<f32>,
    stems: std::collections::HashMap<String, Vec<f32>>,
    source_renders: Option<f64>,
    failed_output: Option<Vec<u8>>,
    stem_manifest_exists: bool,
}
fn component_render(source: &str, with_stems: bool) -> ComponentRender {
    component_render_bytes(source.as_bytes(), with_stems, false)
}
fn component_render_bytes(source: &[u8], with_stems: bool, compressed: bool) -> ComponentRender {
    render_component_case(ComponentCase {
        source,
        with_stems,
        compressed,
        existing_output: None,
    })
}
struct ComponentCase<'a> {
    source: &'a [u8],
    with_stems: bool,
    compressed: bool,
    existing_output: Option<&'a [u8]>,
}
fn render_component_case(case: ComponentCase<'_>) -> ComponentRender {
    let ComponentCase {
        source,
        with_stems,
        compressed,
        existing_output,
    } = case;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    struct CaseDir(std::path::PathBuf);
    impl Drop for CaseDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = CaseDir(oc_audio::private_fs::test_root().join(format!(
        "vst3-tail-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )));
    std::fs::create_dir_all(&dir.0).unwrap();
    let project = dir
        .0
        .join(if compressed { "tail.ocpc" } else { "tail.ocp" });
    let wav = dir.0.join("tail.wav");
    std::fs::write(&project, source).unwrap();
    if let Some(bytes) = existing_output {
        std::fs::write(&wav, bytes).unwrap();
    }
    let stdout = dir.0.join("stdout.txt");
    let stderr = dir.0.join("stderr.txt");
    let mut command = crate::ocp();
    let profile = dir.0.join("profile");
    command.env("OPENCOMPOSE_CONFIG_DIR", &profile);
    command
        .arg("render")
        .arg(&project)
        .arg("--output")
        .arg(&wav)
        .args([
            "--sample-rate",
            "48000",
            "--sample-format",
            "f32",
            "--no-loudness-normalisation",
            "--pre-roll",
            "0",
        ]);
    let stem_dir = dir.0.join("stems");
    if with_stems {
        command.arg("--stems").arg(&stem_dir).arg("--perf");
    }
    let mut child = ChildGuard(
        command
            .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "VST3 tail render exceeded its deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let read = |path: &std::path::Path| -> Vec<f32> {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<f32>()
            // Prepared controlled components apply exact 1/8 attenuation so
            // the ordinary clipping guard accepts even a four-output sum.
            // Restore fixture reference units after export, never in renderer.
            .map(|sample| sample.unwrap() * 8.0)
            .collect()
    };
    let mut stems = std::collections::HashMap::new();
    if with_stems && stem_dir.join("stems.json").exists() {
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(stem_dir.join("stems.json")).unwrap()).unwrap();
        for entry in manifest["stems"].as_array().unwrap() {
            stems.insert(
                entry["name"].as_str().unwrap().to_owned(),
                read(&stem_dir.join(entry["file"].as_str().unwrap())),
            );
        }
    }
    let stderr_bytes = std::fs::read(&stderr).unwrap();
    let mut source_renders = None;
    if with_stems
        && let Some(path) = String::from_utf8_lossy(&stderr_bytes)
            .lines()
            .find_map(|line| line.strip_prefix("diagnostics log: "))
    {
        let path = std::path::Path::new(path);
        assert!(
            path.starts_with(profile.join("logs")),
            "the child diagnostic must be privately owned: {}",
            path.display()
        );
        let report = std::fs::read_to_string(path).unwrap();
        source_renders = report
            .lines()
            .filter(|line| line.contains("render.cache.source_renders"))
            .filter_map(|line| {
                line.split("max=")
                    .nth(1)?
                    .split("sources")
                    .next()?
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
            .next_back();
        std::fs::remove_file(path).unwrap();
    }
    let failed_output = if !status.success() && wav.exists() {
        Some(std::fs::read(&wav).unwrap())
    } else {
        None
    };
    ComponentRender {
        output: std::process::Output {
            status,
            stdout: std::fs::read(stdout).unwrap(),
            stderr: stderr_bytes,
        },
        exists: wav.exists(),
        samples: if status.success() && wav.exists() {
            read(&wav)
        } else {
            Vec::new()
        },
        stems,
        source_renders,
        failed_output,
        stem_manifest_exists: stem_dir.join("stems.json").exists(),
    }
}

#[test]
fn native_vst3_tail_preparation_uses_the_normal_cli_main_executor() {
    let (output, wav) = vst3_tail_render("Gain: 0.75");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(wav, "a valid prepared instrument must render its output");
}

#[test]
fn native_vst3_tail_preparation_refuses_unknown_authored_parameters_before_output() {
    let (output, wav) = vst3_tail_render("Gain: 0.75, missing_native_control: 0.25");
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{error}");
    assert!(error.contains("tail preparation"), "{error}");
    assert!(error.contains("missing_native_control"), "{error}");
    assert!(error.contains("unknown parameter"), "{error}");
    assert!(!wav, "failed preparation must not publish an audio file");
}

// The normal CLI resolver acquires an actual compiled VST2 module. Tail and
// audio preparation use the executable's real main-thread registration.
fn vst2_tail_render(params: &str) -> ComponentRender {
    component_render(
        &format!(
            "@project\n  tempo: 120\n@preset voice\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {{{params}}}\n@track lead | preset: voice, instrument: synth/lead\n  @phrase held [1:1 - 1:2]\n    C4 q mf\n"
        ),
        false,
    )
}

#[test]
fn native_vst2_tail_preparation_uses_the_cli_main_and_real_module() {
    let result = vst2_tail_render("Gain: 0.75");
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.exists);
    assert!(result.samples.iter().any(|sample| sample.abs() > 0.0));
}

#[test]
fn native_vst2_tail_preparation_refuses_unknown_parameters_before_output() {
    let result = vst2_tail_render("Gain: 0.75, missing_native_control: 0.25");
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert_eq!(result.output.status.code(), Some(1), "{error}");
    assert!(
        error.contains("tail preparation") && error.contains("missing_native_control"),
        "{error}"
    );
    assert!(!result.exists);
}

#[test]
fn native_vst2_tail_preparation_does_not_discard_state_lookalike_parameters() {
    let result = vst2_tail_render("state_file.dll: \"not-a-state-file\"");
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert_eq!(result.output.status.code(), Some(1), "{error}");
    assert!(
        error.contains("tail preparation") && error.contains("state_file.dll"),
        "{error}"
    );
    assert!(!result.exists);
}

#[test]
fn native_vst2_tail_preparation_checks_real_state_resources_before_output() {
    let result = vst2_tail_render("state_file: \"missing-fixture-state.bin\"");
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert_eq!(result.output.status.code(), Some(1), "{error}");
    assert!(error.contains("missing-fixture-state.bin"), "{error}");
    assert!(!result.exists);
}

#[test]
fn native_vst2_owned_audio_applies_runtime_gain_to_the_real_module() {
    // The score's initial CC7 setup also controls this fixture's gain. Place
    // both observations after explicit automation points beyond that setup.
    let source = "@project\n  tempo: 120\n@preset voice\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {Gain: 0.25}\n@track lead | preset: voice, instrument: synth/lead\n  automation: pluginparam_4761696e {1:2=0.25, 1:3=0.75}\n  @phrase held [1:1 - 2:1]\n    C4 w mf\n";
    let result = component_render(source, false);
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert!(result.output.status.success(), "{error}");
    assert!(result.exists);
    let level = |start: usize| {
        let window = &result.samples[start * 2..(start + 1000) * 2];
        assert!(window.iter().all(|sample| sample.is_finite()));
        window.iter().map(|sample| sample.abs() as f64).sum::<f64>() / window.len() as f64
    };
    let quiet = level(30000);
    let loud = level(54000);
    assert!(quiet > 0.01, "the native MIDI gate must produce audio");
    assert!((loud / quiet - 3.0).abs() < 0.001, "{quiet} -> {loud}");
}

#[test]
fn native_vst2_owned_audio_refuses_unknown_runtime_bindings_before_output() {
    let source = "@project\n  tempo: 120\n@preset voice\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@track lead | preset: voice, instrument: synth/lead\n  automation: pluginparam_6d697373696e675f72756e74696d655f636f6e74726f6c {1:1=0.25}\n  @phrase held [1:1 - 1:2]\n    C4 q mf\n";
    let result = component_render(source, false);
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert_eq!(result.output.status.code(), Some(1), "{error}");
    assert!(error.contains("missing_runtime_control"), "{error}");
    assert!(!result.exists);
}

#[test]
fn native_vst2_owned_audio_shares_declared_main_output_routing() {
    let prefix = "@project\n  tempo: 120\n@preset voice\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {Gain: 0.1, output_pair: 0}\n";
    let track = |name: &str| {
        format!(
            "@track {name} | preset: voice, instrument: synth/lead\n  @phrase held [1:1 - 1:2]\n    C4 q mf\n"
        )
    };
    let single = component_render(&format!("{prefix}{}", track("first")), false);
    let shared = component_render(
        &format!("{prefix}{}{}", track("first"), track("second")),
        true,
    );
    for rendered in [&single, &shared] {
        assert!(
            rendered.output.status.success(),
            "{}",
            String::from_utf8_lossy(&rendered.output.stderr)
        );
        assert!(rendered.exists);
    }
    let range = 5000 * 2..6000 * 2;
    assert!(
        single.samples[range.clone()]
            .iter()
            .any(|sample| sample.abs() > 0.01)
    );
    for (&one, &both) in single.samples[range.clone()]
        .iter()
        .zip(&shared.samples[range])
    {
        assert!((both - one * 2.0).abs() < 0.0001, "{one} -> {both}");
    }
    assert!(shared.stem_manifest_exists);
    assert_eq!(shared.stems.len(), 2);
}

#[test]
fn native_vst2_owned_audio_keeps_original_inherited_routing_alias_order() {
    let source = "@project\n  tempo: 120\n@preset parent\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {plugin_output_pair: 1, output_pair: 2}\n@preset child\n  extends: parent\n  params: {plugin_output_pair: 0}\n@track lead | preset: child, instrument: synth/lead\n  @phrase held [1:1 - 1:2]\n    C4 q mf\n";
    let result = component_render(source, false);
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.exists);
    assert!(result.samples.iter().any(|sample| sample.abs() > 0.01));
}

#[test]
fn native_vst2_owned_source_routing_failure_preserves_export_and_refuses_stems() {
    let source = b"@project\n  tempo: 120\n@preset voice\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n@track lead | preset: voice, instrument: synth/lead\n  performance: { @0 ch0 note_on 60 100; @0 ch0 cc 119 127; @1 ch0 note_off 60 0; }\n";
    let sentinel = b"existing export must survive";
    for existing_output in [None, Some(sentinel.as_slice())] {
        let result = render_component_case(ComponentCase {
            source,
            with_stems: true,
            compressed: false,
            existing_output,
        });
        let error = String::from_utf8_lossy(&result.output.stderr);
        assert_eq!(result.output.status.code(), Some(1), "{error}");
        assert!(error.contains("VST2 runtime structure changed"), "{error}");
        assert_eq!(result.exists, existing_output.is_some());
        assert_eq!(result.failed_output.as_deref(), existing_output);
        assert!(
            !result.stem_manifest_exists,
            "failed source published a stem manifest"
        );
        assert!(result.stems.is_empty());
    }
}

#[test]
fn native_clap_render_services_main_callbacks_between_blocks() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = oc_audio::private_fs::test_root().join("native-clap-main-callback");
    std::fs::create_dir_all(&dir).unwrap();
    let plugin = dir.join("require-main-callback.clap");
    std::fs::copy(
        oc_host::test_support::clap_module_fixture().expect("native CLAP fixture must be built"),
        &plugin,
    )
    .unwrap();
    let project = dir.join("native.ocp");
    let wav = dir.join("native.wav");
    let plugin_path = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    std::fs::write(&project, format!(
        "@ocp \"0.2.0\"\n@project \"Native main callbacks\"\n  tempo: 120\n  time_signature: 4/4\n@preset silent\n  source: clap\n  plugin: {plugin_path}\n@track keys | preset: silent, instrument: keys/piano\n  @phrase held [1:1 - 2:1]\n    C4 w mf\n"
    )).unwrap();
    let stdout_path = dir.join("stdout.txt");
    let stderr_path = dir.join("stderr.txt");
    let child = crate::ocp()
        .arg("render")
        .arg(&project)
        .arg("--output")
        .arg(&wav)
        .args([
            "--sample-rate",
            "48000",
            "--no-loudness-normalisation",
            "--pre-roll",
            "0",
        ])
        .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            panic!("native CLAP CLI render did not finish before its test deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "native CLI render failed: {}",
        std::fs::read_to_string(stderr_path).unwrap()
    );
    let mut reader = hound::WavReader::open(wav).unwrap();
    assert_eq!(reader.spec().sample_rate, 48_000);
    assert_eq!(reader.spec().channels, 2);
    assert!(
        reader.duration() > 512,
        "must process more than one native block"
    );
    assert!(reader.samples::<i16>().all(|sample| sample.unwrap() == 0));
}

/// The fake component declares two stereo output buses and no audio input, the
/// instrument shape. `ocp render` must refuse it as a normal error — exit 1,
/// the readable message on stderr, no WAV — instead of the mid-render 139.
#[test]
fn render_of_an_instrument_insert_exits_nonzero_without_a_wav() {
    let dir = oc_audio::private_fs::test_root().join("insert-refusal");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let project_path = dir.join("insert.ocp");
    let output_path = dir.join("out.wav");
    std::fs::write(
        &project_path,
        r#"@ocp "0.2.0"

@project "instrument insert"
  tempo: 120
  time_signature: 4/4

@preset tone
  source: builtin
  plugin: "builtin.test_tone"
  params: {frequency: 220, amplitude: 0.2, waveform: 0}

@track Lead | preset: tone, instrument: synth/lead_square
  effects:
    - plugin:"test.fake.two_bus"
  @phrase held [1:1 - 2:1]
    C4 w mf
"#,
    )
    .expect("write project");

    let output = crate::ocp()
        .args([
            "render",
            project_path.to_str().expect("utf-8 path"),
            "--output",
            output_path.to_str().expect("utf-8 path"),
            "--sample-rate",
            "48000",
            "--no-loudness-normalisation",
            "--pre-roll",
            "0",
        ])
        .output()
        .expect("run ocp render");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an instrument insert must fail the render, not abort: {:?}\n{stderr}",
        output.status
    );
    assert!(
        stderr.contains(
            "VST3 Plugin: an instrument plugin (0 audio inputs, 2 stereo outputs) cannot be \
             used as an effect on track 'Lead'; put it on an instrument track, or wrap it in a bus"
        ),
        "the refusal is not the readable message:\n{stderr}"
    );
    assert!(
        !output_path.exists(),
        "a refused render left {} behind",
        output_path.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every actual offline group block must reach the owned processor, including
/// the release tail. The controlled component emits its process ordinal as PCM.
#[test]
fn the_offline_vst3_renderer_calls_the_plugin_on_every_block() {
    let source = "@project\n  tempo: 120\n@preset voice\n  source: vst3\n  plugin: \"test.fake.block_ordinal\"\n@track lead | preset: voice, instrument: synth/lead\n  @phrase held [1:1 - 4:1]\n    C4 w mf\n    C4 w mf\n    C4 q mf\n";
    let (output, exists, samples) = vst3_component_render(source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(exists);
    let frames = samples.len() / 2;
    assert!(
        frames >= 4 * 48_000,
        "the offline pipeline must render at least four seconds"
    );
    let first = samples[0];
    assert!(first > 0.0);
    let mut calls = 0;
    for (block_index, block) in samples.chunks(128 * 2).enumerate() {
        calls += 1;
        let expected = first * (block_index + 1) as f32;
        assert!(
            block.iter().all(|value| (*value - expected).abs() < 1e-6),
            "offline block {block_index} was skipped, duplicated, or processed twice"
        );
    }
    assert_eq!(
        calls,
        frames.div_ceil(128),
        "every offline block reaches the plugin"
    );
}

#[test]
fn owned_vst3_cli_group_fans_main_and_auxiliary_outputs_into_the_mix() {
    let source = "@project\n  tempo: 120\n@preset voice\n  source: vst3\n  plugin: \"test.fake.two_bus\"\n@track host | preset: voice, instrument: keys/piano\n  effects:\n    - gain: factor=0.5\n  mix: send(host.out2->drums=100%)\n  @phrase line [1:1 - 2:1]\n    C4 q\n@bus drums\n  routes: [master]\n";
    let (output, exists, samples) = vst3_component_render(source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(exists);
    assert!(samples.len() > 256);
    // Main 1/1.5 at half gain plus native aux 2/2.5 at unity.
    for pair in samples.chunks_exact(2) {
        assert!((pair[0] - 2.5).abs() < 1e-6);
        assert!((pair[1] - 3.25).abs() < 1e-6);
    }
}

fn four_port_source(ports: &[u16]) -> String {
    let mut source = String::from(
        "@project \"four ports\"\n  tempo: 120\n@preset four\n  source: vst3\n  plugin: \"test.fake.four_bus\"\n",
    );
    for (index, port) in ports.iter().enumerate() {
        source.push_str(&format!("@track out{index} | preset: four, instrument: keys/piano, port: {port}\n  @phrase body [1:1 - 2:1]\n    C4 q mf\n"));
    }
    source
}
fn successful_component(source: &str) -> Vec<f32> {
    let result = component_render(source, false);
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.exists);
    result.samples
}
#[test]
fn four_tracks_capture_four_declared_ports_and_sum_to_per_port_renders() {
    let mixed = successful_component(&four_port_source(&[0, 1, 2, 3]));
    let mut per_port = vec![0.0f32; mixed.len()];
    for port in 0..4 {
        let single = successful_component(&four_port_source(&[port]));
        assert_eq!(single.len(), mixed.len());
        for (sum, sample) in per_port.iter_mut().zip(single) {
            *sum += sample;
        }
    }
    assert_eq!(
        mixed, per_port,
        "summing the tracks must equal rendering the instance port by port"
    );
    assert!((mixed[0] - 10.0).abs() < 1e-6, "{:?}", &mixed[..4]);
    assert!((mixed[1] - 12.0).abs() < 1e-6, "{:?}", &mixed[..4]);
}
#[test]
fn requesting_a_port_beyond_the_declared_buses_is_a_readable_error() {
    let result = component_render(&four_port_source(&[4]), false);
    assert_eq!(result.output.status.code(), Some(1));
    assert!(!result.exists);
    let error = String::from_utf8_lossy(&result.output.stderr);
    assert!(error.contains("declares 4 output ports"), "{error}");
    assert!(error.contains("track 'out0'"), "{error}");
    assert!(error.contains("output port 5"), "{error}");
}
#[test]
fn legacy_plugin_output_pair_still_selects_the_absolute_pair() {
    let source = "@project\n  tempo: 120\n@preset four\n  source: vst3\n  plugin: \"test.fake.four_bus\"\n@track out | preset: four, instrument: keys/piano\n  @phrase body [1:1 - 2:1]\n    C4 q mf\n  engine_params: {plugin_output_pair: 3}\n";
    let audio = successful_component(source);
    assert!((audio[0] - 3.0).abs() < 1e-6, "{:?}", &audio[..4]);
    assert!((audio[1] - 3.5).abs() < 1e-6, "{:?}", &audio[..4]);
}

#[test]
fn shared_plugin_output_coalesces_one_owners_placements_and_fans_distinct_stems_once() {
    let source = r#"@project
  tempo: 120
@preset voice
  source: vst3
  plugin: "test.fake.two_bus"
@pattern first | preset: voice, instrument: synth/lead
  events:
    @0 C4 q mf
@pattern second | preset: voice, instrument: synth/lead
  events:
    @0 C4 q mf
@track sum | preset: voice, instrument: synth/lead, port: 0, pair: 0
  clip: first [1:1 - 1:2]
  clip: second [1:1 - 1:2]
@track second_output | preset: voice, instrument: synth/lead, port: 1, pair: 0
"#;
    let mut project = oc_dsl::parse(source).unwrap();
    let effect = oc_dsl::parse(
        "@project\n@track fx | instrument: synth/lead\n  effects:\n    - gain: factor=0.5\n",
    )
    .unwrap()
    .tracks
    .remove(0)
    .effects;
    for pattern in &mut project.patterns {
        pattern.effects = effect.clone();
    }
    let bytes = oc_dsl::diff::compressed::encode_project(&project).unwrap();
    let result = component_render_bytes(&bytes, true, true);
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.exists);
    assert_eq!(
        result.stems.len(),
        2,
        "two track source lanes fan the one shared owner"
    );
    assert_eq!(
        result.source_renders,
        Some(2.0),
        "the actual cache source counter must be exactly two"
    );
    let first = &result.stems["sum"];
    let second = &result.stems["second_output"];
    assert_eq!(first.len(), result.samples.len());
    assert_eq!(second.len(), result.samples.len());
    assert_eq!(
        first[0], 0.5,
        "the common pattern insert runs once on owner zero"
    );
    assert_eq!(
        second[0], 2.0,
        "the second native output is not multiplied by pattern placements"
    );
    for ((mixed, first), second) in result.samples.iter().zip(first).zip(second) {
        assert!((*mixed - (*first + *second)).abs() < 1e-6);
    }
}

#[test]
fn explicit_unmapped_vst3_program_is_refused_instead_of_treated_as_generated_setup() {
    let source = "@project\n@preset voice\n  source: vst3\n  plugin: \"test.fake.one_bus\"\n  program: 12\n@track lead | preset: voice, instrument: synth/lead\n  @phrase held [1:1 - 1:2]\n    C4 q\n";
    let result = component_render(source, false);
    assert_eq!(result.output.status.code(), Some(1));
    assert!(!result.exists);
    assert!(String::from_utf8_lossy(&result.output.stderr).contains("UnsupportedMidi"));
}

#[test]
fn independent_performance_tracks_can_reuse_a_vst3_preset_without_sharing_an_instance() {
    let mut project=oc_dsl::parse("@project\n@preset voice\n  source: vst3\n  plugin: \"test.fake.one_bus\"\n@track first | preset: voice, instrument: synth/lead\n@track second | preset: voice, instrument: synth/lead\n").unwrap();
    for track in &mut project.tracks {
        track.performance = Some(oc_dsl::performance::PerformanceTimeline {
            events: vec![
                oc_dsl::performance::PerformanceEvent {
                    quarter: 0.0,
                    channel: 0,
                    message: oc_dsl::performance::ChannelCommand::NoteOn {
                        note: 60,
                        velocity: 100,
                    },
                },
                oc_dsl::performance::PerformanceEvent {
                    quarter: 1.0,
                    channel: 0,
                    message: oc_dsl::performance::ChannelCommand::NoteOff {
                        note: 60,
                        velocity: 64,
                    },
                },
            ],
        });
    }
    let result = component_render_bytes(
        &oc_dsl::diff::compressed::encode_project(&project).unwrap(),
        false,
        true,
    );
    assert!(
        result.output.status.success(),
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.exists);
    assert!(!result.samples.is_empty());
    assert_eq!(
        &result.samples[..2],
        &[2.0, 3.0],
        "two independent owned sources each reach the mix"
    );
    project.tracks[0].output_port = Some(0);
    project.tracks[1].output_port = Some(1);
    let result = component_render_bytes(
        &oc_dsl::diff::compressed::encode_project(&project).unwrap(),
        false,
        true,
    );
    assert_eq!(result.output.status.code(), Some(1));
    assert!(!result.exists);
    assert!(
        String::from_utf8_lossy(&result.output.stderr).contains("cannot share a plugin instance")
    );
}
