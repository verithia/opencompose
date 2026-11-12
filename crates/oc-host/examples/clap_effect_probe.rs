//! Single-process CLAP effect lifecycle probe.
//!
//! Loads one effect, pushes audio through the host entry point the offline
//! renderer uses, tears the instance down, and exits. The process exit status is
//! the measurement: a crash here is a host-contract or plugin fault, not a test
//! assertion.
//!
//! ```text
//! cargo run -p oc-host --features test-support --example clap_effect_probe -- \
//!   --plugin "/Library/Audio/Plug-Ins/CLAP/<vendor> <product>.clap" \
//!   --id com.<vendor>.<product> --seconds 2
//! ```

use std::path::PathBuf;
use std::time::Duration;

use oc_audio::{AudioEffect, PluginTransport};
use oc_host::clap_instance::ClapInstance;

struct Config {
    path: PathBuf,
    id: String,
    sample_rate: u32,
    sleep_after_load: Duration,
    timeline: Option<PluginTransport>,
    query_extensions: bool,
    sidechain: bool,
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn load(config: &Config) -> ClapInstance {
    let instance = ClapInstance::load(&config.path, &config.id, config.sample_rate, 512)
        .expect("load CLAP effect");
    if !config.sleep_after_load.is_zero() {
        std::thread::sleep(config.sleep_after_load);
    }
    if config.query_extensions {
        let _ = AudioEffect::latency_samples(&instance);
        let _ = AudioEffect::tail_samples(&instance);
        let _ = instance.get_params();
    }
    instance
}

fn process(mut instance: ClapInstance, config: &Config, audio: &[f32]) {
    let output = if config.sidechain {
        let key = vec![0.0f32; audio.len()];
        instance.process_effect_block_with_sidechain(audio, Some(&key), config.timeline.clone())
    } else {
        instance.process_effect_block_with_transport(audio, config.timeline.clone())
    };
    assert_eq!(output.len(), audio.len());
    drop(instance);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(plugin) = arg(&args, "--plugin") else {
        eprintln!("missing --plugin");
        std::process::exit(2);
    };
    let id = arg(&args, "--id").unwrap_or_else(|| {
        eprintln!("missing --id");
        std::process::exit(2);
    });
    let seconds = arg(&args, "--seconds")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(2.0);
    let mode = arg(&args, "--mode").unwrap_or_else(|| "main".into());
    let sample_rate = 48_000u32;
    let config = Config {
        path: PathBuf::from(&plugin),
        id,
        sample_rate,
        sleep_after_load: Duration::from_millis(
            arg(&args, "--sleep-after-load-ms")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
        ),
        timeline: flag(&args, "--transport").then(|| PluginTransport::new(0, sample_rate, 120.0)),
        query_extensions: flag(&args, "--query-extensions"),
        sidechain: flag(&args, "--with-sidechain"),
    };

    let frames = (seconds * sample_rate as f64) as usize;
    let mut audio = vec![0.0f32; frames * 2];
    for frame in 0..frames {
        let value = (std::f64::consts::TAU * 220.0 * frame as f64 / sample_rate as f64).sin() * 0.1;
        audio[frame * 2] = value as f32;
        audio[frame * 2 + 1] = value as f32;
    }

    if flag(&args, "--print-ports") {
        let instance = load(&config);
        for port in instance.debug_audio_ports() {
            println!(
                "port input={} index={} channels={} flags=0x{:x} name={:?}",
                port.0, port.1, port.2, port.3, port.4
            );
        }
        drop(instance);
        return;
    }

    match mode.as_str() {
        "main" => process(load(&config), &config, &audio),
        "worker" => {
            let instance = load(&config);
            std::thread::scope(|scope| {
                let config = &config;
                scope
                    .spawn(move || process(instance, config, &audio))
                    .join()
                    .expect("worker thread panicked");
            });
        }
        "fresh-worker" => {
            std::thread::scope(|scope| {
                let config = &config;
                scope
                    .spawn(move || process(load(config), config, &audio))
                    .join()
                    .expect("worker panicked");
            });
        }
        other => {
            eprintln!("unknown --mode {other}");
            std::process::exit(2);
        }
    }

    println!("PROBE OK");
}
