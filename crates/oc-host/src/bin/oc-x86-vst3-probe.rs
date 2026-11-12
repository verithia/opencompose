//! Feasibility probe: host one VST3 bundle from an x86_64 process under Rosetta.
//!
//! Loads the bundle, instantiates component and controller, activates, plays
//! note 60 at velocity 127 on channel 0, renders two seconds at 48 kHz in
//! 512-frame blocks, and reports peak/RMS plus the timing of each phase.
//! Build it for `x86_64-apple-darwin` and run it through `arch -x86_64`.
//!
//! This is a throwaway measurement tool, not a product entry point: it exists
//! only to answer whether an x86_64 plug-in host can carry a bundle the arm64
//! host cannot `dlopen`.

use std::path::PathBuf;
use std::time::Instant;

use oc_host::vst3_instance::Vst3Instance;

const SAMPLE_RATE: u32 = 48_000;
const BLOCK: u32 = 512;
const SECONDS: u32 = 2;

fn main() {
    let mut plugin: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin" => plugin = args.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(plugin) = plugin else {
        eprintln!("usage: oc-x86-vst3-probe --plugin <bundle.vst3>");
        std::process::exit(2);
    };

    say(format!("arch={}", std::env::consts::ARCH));
    say(format!("translated={}", proc_translated()));
    say(format!("plugin={}", plugin.display()));
    say(format!(
        "bundle_exists={}",
        std::path::Path::new(&plugin).exists()
    ));

    // Phase 1: module load + instantiate + activate.
    let load_start = Instant::now();
    let mut instance = match Vst3Instance::load(&plugin, SAMPLE_RATE, BLOCK) {
        Ok(instance) => {
            say(format!(
                "load=ok load_ms={}",
                load_start.elapsed().as_millis()
            ));
            instance
        }
        Err(error) => {
            say(format!(
                "load=failed load_ms={} error={error}",
                load_start.elapsed().as_millis()
            ));
            oc_host::exit_process(1);
        }
    };

    let info = instance.class_info();
    say(format!(
        "class name={:?} vendor={:?} version={:?} category={:?} instrument={}",
        info.name,
        info.vendor,
        info.version,
        info.category,
        info.is_instrument()
    ));
    say(format!(
        "outputs buses={} channels={:?} stereo_pairs={} params={} tail_samples={}",
        instance.output_bus_count(),
        instance.output_bus_channels(),
        instance.output_count(),
        instance.get_params().len(),
        instance.tail_samples()
    ));

    // Phase 2: an idle block before any note, so a non-zero peak below can be
    // attributed to the note rather than to the plug-in's own noise floor.
    let idle = instance.process_block(BLOCK);
    say(format!(
        "idle peak={:.6} rms={:.6}",
        peak(&idle),
        rms(&idle)
    ));

    // Phase 3: note 60 at velocity 127 on channel 0, then two seconds.
    instance.note_on_channel(0, 60, 127.0, 0);
    let blocks = (SAMPLE_RATE * SECONDS).div_ceil(BLOCK);
    let mut rendered: Vec<f32> = Vec::with_capacity((blocks * BLOCK * 2) as usize);
    let render_start = Instant::now();
    for index in 0..blocks {
        let block = instance.process_block(BLOCK);
        if index == 0 {
            say(format!("first_block peak={:.6}", peak(&block)));
        }
        rendered.extend_from_slice(&block);
    }
    let render_ms = render_start.elapsed().as_millis();
    let (note_peak, note_rms) = (peak(&rendered), rms(&rendered));
    let frames = rendered.len() / 2;
    let realtime = (SECONDS * 1000) as f64 / render_ms.max(1) as f64;
    say(format!(
        "note_render frames={frames} blocks={blocks} peak={note_peak:.6} rms={note_rms:.6} render_ms={render_ms} realtime_x={realtime:.2}"
    ));
    say(format!("silent_frames={}", count_silent(&rendered)));

    // Phase 4: note off, one more block, confirm the process is still up.
    instance.note_off_channel(0, 60, 0);
    let after = instance.process_block(BLOCK);
    say(format!("post_note_off peak={:.6}", peak(&after)));
    say("process_alive=yes");

    oc_host::exit_process(0);
}

fn say(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

fn count_silent(samples: &[f32]) -> usize {
    samples.iter().filter(|s| **s == 0.0).count()
}

/// `1` for translated x86_64 on Apple silicon, `0` for native, or `-1` when unknown.
#[cfg(target_os = "macos")]
fn proc_translated() -> i32 {
    let name = c"sysctl.proc_translated";
    let mut value: i32 = 0;
    let mut size = std::mem::size_of::<i32>() as libc::size_t;
    // SAFETY: `value` and `size` are valid for the duration of the call and the
    // sysctl either writes one `int` or leaves `value` at zero.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status == 0 { value } else { -1 }
}

#[cfg(not(target_os = "macos"))]
fn proc_translated() -> i32 {
    -1
}

#[cfg(all(test, not(target_os = "macos")))]
#[test]
fn translation_status_is_unknown_without_the_platform_query() {
    assert_eq!(proc_translated(), -1);
}
