//! Performance logging retains the production source-path stage table.
#![cfg(feature = "perf-log")]

#[test]
fn source_render_perf_log_retains_stages_without_console_output() {
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Guard(oc_test_support::temp_dir("cli-perf-stages", "source"));
    let project = directory.0.join("project.ocp");
    let profile = directory.0.join("profile");
    let audio = directory.0.join("output.wav");
    std::fs::write(
        &project,
        "@project\n  tempo: 120\n@preset tone\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track lead | preset: tone, instrument: synth/lead\n  mix: output=limited\n  @phrase note [1:1 - 1:2]\n    C4 q\n@bus limited\n  routes: [master]\n  effects:\n    - limiter: ceiling=-0.8dB\n@master\n  effects:\n    - limiter: ceiling=-0.8dB\n",
    )
    .unwrap();
    let output = crate::ocp()
        .env("OPENCOMPOSE_CONFIG_DIR", &profile)
        .env_remove("OCP_RENDER_STAGES")
        .arg("render")
        .arg(&project)
        .args([
            "--perf",
            "--pre-roll",
            "0",
            "--no-loudness-normalisation",
            "--sample-rate",
            "48000",
            "--sample-format",
            "f32",
            "-o",
        ])
        .arg(&audio)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(audio.is_file());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("stage timings:"));
    let diagnostics = std::fs::read_dir(profile.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(
        diagnostics.contains("render stage interpret"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("render stage wav write"),
        "{diagnostics}"
    );
    let line = diagnostics
        .lines()
        .find(|line| line.contains("offline mix clock: "))
        .expect("production render reports its timeline");
    assert!(line.contains(
        "timeline_start_frame=0 sample_rate=48000 program_frames=120000 \
         pre_master_alignment_frames=240 master_insert_latency_frames=240 output_frames=120240"
    ));
    let mut wav = hound::WavReader::open(audio).unwrap();
    assert_eq!(wav.duration(), 120_240);
    let source_start = wav
        .samples::<f32>()
        .enumerate()
        .find(|(_, sample)| sample.as_ref().unwrap().abs() > 1e-6)
        .map(|(index, _)| index / 2)
        .expect("test tone is audible");
    assert!((240..242).contains(&source_start));
}
