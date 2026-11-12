//! The `ocp` binary: one subcommand per task, argument parsing only.
//!
//! The work lives in the workspace crates; this file decides flags, paths and
//! exit codes, and prints one reason per failure on stderr.

use anyhow::Result;
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

mod conformance;
mod midi;
mod play;
mod sample;

use oc_cli::plugin_index;
use oc_cli::plugin_install;

#[derive(Parser)]
#[command(
    name = "ocp",
    version,
    about = "OpenCompose - Universal music description language",
    long_about = "A tool for parsing, validating, and rendering OpenCompose DSL files.\n\n\
                  OpenCompose bridges AI generation, human editing, and audio engines \
                  through a single structured music description language."
)]
struct Cli {
    /// Write a diagnostics log with debug detail to ~/Library/Logs/OpenCompose
    #[arg(long, global = true)]
    debug: bool,

    /// Collect render and stage timings and write them to the diagnostics log
    #[arg(long, global = true)]
    perf: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate built-in effect parameters/routing using DSP, without rendering audio.
    ValidateBuiltinEffects {
        #[arg(value_name = "FILE")]
        input: PathBuf,
        #[arg(long, default_value_t = 48000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
        sample_rate: u32,
    },
    /// Resolve a plugin using the renderer's registry (metadata only).
    ResolvePlugin { id: String },
    /// Scan installed plugin bundles
    Scan {
        /// Retry bundles an earlier scan quarantined
        #[arg(long)]
        rescan: bool,
    },
    /// Inspect the built-in mixer device registry
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Dump a VST3 or VST2 plugin's parameter inventory as a table or JSON.
    PluginParams {
        /// Plugin id, name, or bundle path.
        id: String,
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Import and apply a `.ffp` preset (binary or ASCII).
    PluginPreset {
        #[command(subcommand)]
        command: PluginPresetCommand,
    },
    /// Comping: align audio-clip takes to a reference take
    Clip {
        #[command(subcommand)]
        command: ClipCommand,
    },
    /// Vocal tools: offline pitch analysis
    Vocal {
        #[command(subcommand)]
        command: VocalCommand,
    },
    /// Bake a track leveler's own gain decision into its gain automation
    Level {
        /// Project file
        #[arg(value_name = "PROJECT")]
        project: PathBuf,

        /// Track whose leveler is baked; the device must be its last effect
        #[arg(long, value_name = "TRACK")]
        track: String,

        /// Replace the project file with the baked one
        #[arg(short, long, conflicts_with = "output")]
        write: bool,

        /// Write the baked project here instead of replacing the input
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Emit JSON instead of a table
        #[arg(long)]
        json: bool,

        /// Sample rate of the dry pass; bake at the rate you will render with
        #[arg(long, default_value_t = 48_000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
        sample_rate: u32,

        /// Fallback SoundFont for tracks without a project-owned instrument resource
        #[arg(long, env = "OCP_SOUNDFONT")]
        soundfont: Option<PathBuf>,
    },
    /// Compile a complete project AST JSON into validated OCP on stdout.
    CompileProject {
        #[arg(value_name = "JSON_FILE")]
        input: PathBuf,
    },
    /// Parse an .ocp file and check for syntax errors
    Parse {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Output AST as JSON
        #[arg(short, long)]
        json: bool,
    },

    /// Validate an .ocp file for semantic correctness
    Validate {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,
    },

    /// Format an .ocp file (normalize whitespace and ordering)
    #[command(name = "fmt")]
    Format {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Write formatted output back to file (default: stdout)
        #[arg(short, long)]
        write: bool,
    },

    /// Export to other formats (MIDI, JSON)
    Export {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Output format
        #[arg(short, long, value_enum)]
        format: ExportFormat,

        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Render an .ocp file using its declared instruments and mixer
    Render {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Output audio file path; defaults to `output.<container extension>`
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Optional fallback SoundFont for tracks without a project-owned instrument resource
        #[arg(long, env = "OCP_SOUNDFONT")]
        soundfont: Option<PathBuf>,

        /// Audio container to write; defaults to the project's export setting
        #[arg(long, value_name = "CONTAINER", value_parser = parse_container)]
        format: Option<oc_dsl::ast::ExportContainer>,

        /// Sample rate in hertz (8000-192000); defaults to the project's export rate
        #[arg(long, value_parser = parse_sample_rate)]
        sample_rate: Option<u32>,

        /// Render tracks independently through the full mix graph
        #[arg(long)]
        per_track_mix: bool,

        /// Fixed non-positive output headroom for per-track offline rendering
        #[arg(long, default_value = "0", allow_hyphen_values = true)]
        output_gain_db: f32,

        /// Minimum offline export length including intentional rests (0..600s); never trims events
        #[arg(long, default_value = "0")]
        minimum_duration_seconds: f64,

        /// Render in N isolated instances and compare the outputs
        #[arg(long, value_name = "N")]
        reruns: Option<u32>,

        /// Fail unless every rerun is byte-identical
        #[arg(long)]
        require_identical: bool,

        /// Silent wall-clock warm-up per plugin instance before the graph starts
        #[arg(
            long,
            default_value = "4.0",
            value_name = "SECONDS",
            allow_hyphen_values = true
        )]
        pre_roll: f64,

        /// Also export one pre-master file per track and bus, plus stems.json
        #[arg(long, value_name = "DIR")]
        stems: Option<PathBuf>,

        /// Render without the master loudness limiter and target trim
        #[arg(long)]
        no_loudness_normalisation: bool,

        /// WAV or FLAC sample depth; overrides the project's export setting (alias: --bit-depth)
        #[arg(long, value_name = "DEPTH", visible_alias = "bit-depth")]
        sample_format: Option<oc_engine::wav::SampleFormat>,

        /// FLAC compression level, 0 (fastest) to 8 (smallest); FLAC only
        #[arg(long, value_name = "LEVEL", value_parser = clap::value_parser!(u8).range(0..=8))]
        flac_level: Option<u8>,

        /// MP3 constant bitrate in kbps (default 192); MP3 only
        #[arg(long, value_name = "KBPS", conflicts_with = "mp3_vbr")]
        mp3_bitrate: Option<u32>,

        /// MP3 variable bitrate quality, 0 (best) to 9 (smallest); MP3 only
        #[arg(
            long,
            value_name = "QUALITY",
            value_parser = clap::value_parser!(u8).range(0..=9),
            conflicts_with = "mp3_bitrate"
        )]
        mp3_vbr: Option<u8>,

        /// TPDF dither at one LSB before integer quantisation; WAV only
        #[arg(long, conflicts_with = "no_dither")]
        dither: bool,

        /// Disable dither even when the project enables it
        #[arg(long, conflicts_with = "dither")]
        no_dither: bool,

        /// Resampling quality step (0 fastest to 7 best); overrides the project export setting
        #[arg(long, value_name = "STEP", value_parser = clap::value_parser!(u8).range(0..=7))]
        resample_quality: Option<u8>,

        /// Print the level of the rendered mix: peak, true peak, RMS, crest, loudness
        #[arg(long)]
        report: bool,
    },

    /// Render one track's post-FX signal over a beat range to a 32-bit float WAV
    Bounce {
        /// Project file
        #[arg(value_name = "PROJECT")]
        project: PathBuf,

        /// Track whose signal is rendered
        #[arg(long, value_name = "TRACK")]
        track: String,

        /// First beat of the range, as `bar:beat[:subdivision]`
        #[arg(long, value_name = "BAR:BEAT")]
        from: String,

        /// End of the range (exclusive), as `bar:beat[:subdivision]`
        #[arg(long, value_name = "BAR:BEAT")]
        to: String,

        /// Output WAV path
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,

        /// Optional fallback SoundFont for tracks without a project-owned instrument resource
        #[arg(long, env = "OCP_SOUNDFONT")]
        soundfont: Option<PathBuf>,

        /// Sample rate in hertz (8000-192000); defaults to the project's export rate
        #[arg(long, value_parser = parse_sample_rate)]
        sample_rate: Option<u32>,
    },

    /// Write N seeded, safety-bounded variations of a project's OpenSynth patch
    Randomize {
        /// Input .ocp file carrying the patch to vary
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Preset to vary (default: the first OpenSynth preset)
        #[arg(long)]
        preset: Option<String>,

        /// Number of variations to write
        #[arg(long, default_value = "1")]
        count: u32,

        /// Base seed; variation i uses seed + i
        #[arg(long, default_value = "1")]
        seed: u64,

        /// Output directory for the generated .ocp files
        #[arg(short, long, default_value = "random-patches")]
        output: PathBuf,
    },

    /// Convert a MIDI file to .ocp format
    Midi2ocp {
        /// Input MIDI file
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Output .ocp file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// MIDI file import/export, port listing, monitoring
    Midi {
        #[command(subcommand)]
        command: MidiCommand,
    },

    /// Batch-convert a directory of MIDI files to (DSL, WAV) training pairs
    BatchData {
        /// Directory containing MIDI files
        #[arg(value_name = "MIDI_DIR")]
        midi_dir: PathBuf,

        /// Output directory for .ocp and .wav files
        #[arg(short, long, default_value = "training_data")]
        output: PathBuf,

        /// Path to a SoundFont (.sf2) file
        #[arg(long, env = "OCP_SOUNDFONT")]
        soundfont: PathBuf,

        /// Sample rate
        #[arg(long, default_value = "44100")]
        sample_rate: u32,
    },

    /// Generate synthetic MIDI files
    GenerateMidi {
        /// Output directory
        #[arg(short, long, default_value = "data/midi_raw")]
        output: PathBuf,

        /// Number of files to generate
        #[arg(short, long, default_value = "100")]
        count: u32,

        /// Random seed
        #[arg(long, default_value = "42")]
        seed: u64,
    },

    /// Batch convert a directory of MIDI files to .ocp (fast, parallel, no audio)
    BatchMidi2ocp {
        /// Directory containing MIDI files (recursive)
        #[arg(value_name = "MIDI_DIR")]
        midi_dir: PathBuf,

        /// Output directory for .ocp files
        #[arg(short, long, default_value = "ocp_output")]
        output: PathBuf,

        /// Prefix for output filenames (e.g. subdir_stem.ocp)
        #[arg(long)]
        flatten: bool,
    },

    /// Play a project with optional live MIDI input and output
    Play {
        /// Input file path
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// MIDI input port name (see `ocp midi list`)
        #[arg(long, value_name = "PORT")]
        midi_in: Option<String>,

        /// MIDI output port name (generated notes, thru, and optional clock)
        #[arg(long, value_name = "PORT")]
        midi_out: Option<String>,

        /// Send 24-PPQN clock on the MIDI output
        #[arg(long)]
        midi_clock: bool,

        /// Forward live input to the MIDI output
        #[arg(long)]
        midi_thru: bool,

        /// Fallback SoundFont for tracks without a project-owned instrument
        #[arg(long, env = "OCP_SOUNDFONT")]
        soundfont: Option<PathBuf>,

        /// Sample rate
        #[arg(long, default_value = "44100")]
        sample_rate: u32,
    },

    /// Search and install plugins from the catalog
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },

    /// Build, verify, or key-sign the remote plugin index
    PluginIndex {
        #[command(subcommand)]
        command: PluginIndexCommand,
    },

    /// Search a sample catalogue and pull one sample
    Sample {
        #[command(subcommand)]
        command: sample::SampleCommand,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Resolve, download, verify, and install one catalog entry
    Install {
        /// Catalog entry id, e.g. asb2m10.dexed
        #[arg(value_name = "ID")]
        id: String,

        /// Install from a signed index base URL
        #[arg(long, value_name = "URL")]
        index: Option<String>,

        /// Install from a local catalog JSON file (default: the bundled catalog)
        #[arg(long, value_name = "FILE")]
        catalog: Option<PathBuf>,

        /// Pinned Ed25519 public key for --index (32 raw bytes or hex)
        #[arg(long, value_name = "FILE", env = "OCP_PLUGIN_INDEX_PUBKEY")]
        public_key: Option<PathBuf>,

        /// Target platform (default: this host)
        #[arg(long)]
        platform: Option<String>,

        /// Preferred build: vst3, vst2, clap, au, lv2, or standalone
        #[arg(long)]
        format: Option<String>,

        /// Install directory (default: the host plugin directory)
        #[arg(long, value_name = "DIR")]
        into: Option<PathBuf>,

        /// Resolve the plan and print it without downloading anything
        #[arg(long)]
        dry_run: bool,

        /// Open the vendor installer for a build that cannot be unpacked
        #[arg(long)]
        open: bool,

        /// Replace an existing bundle
        #[arg(long)]
        overwrite: bool,

        /// Emit the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Run the VST3 conformance battery over every installed bundle
    Conformance {
        /// Output format: table, json or markdown
        #[arg(long, default_value = "table", value_name = "FORMAT")]
        format: String,

        /// Test only this bundle (repeatable); default is every VST3 the scanner finds
        #[arg(long = "plugin", value_name = "PATH")]
        plugin: Vec<PathBuf>,

        /// Skip the editor step
        #[arg(long)]
        no_editor: bool,

        /// Per-step deadline in the child process
        #[arg(long, default_value_t = 5000, value_name = "MS")]
        timeout_ms: u64,

        /// Internal: run one bundle's battery and stream JSON Lines
        #[arg(long, hide = true)]
        child: bool,
    },
}

#[derive(Subcommand)]
enum PluginIndexCommand {
    /// Build a signed manifest and content-addressed catalog from catalog JSON
    Build {
        /// Catalog JSON input
        #[arg(long, value_name = "FILE")]
        catalog: PathBuf,

        /// Output directory for index.json, index.json.sig, and the catalog
        #[arg(long, value_name = "DIR")]
        out: PathBuf,

        /// Ed25519 PKCS#8 private key (or OCP_PLUGIN_INDEX_KEY)
        #[arg(long, value_name = "FILE", env = "OCP_PLUGIN_INDEX_KEY")]
        key: Option<PathBuf>,

        /// Minimum app version that may consume this catalog
        #[arg(long, default_value = "0.1.0")]
        min_app_version: String,

        /// Opaque catalog version (default: derived from generated_at)
        #[arg(long)]
        catalog_version: Option<String>,

        /// zstd compression level (1-22)
        #[arg(long, default_value_t = plugin_index::DEFAULT_ZSTD_LEVEL, value_parser = clap::value_parser!(i32).range(1..=22))]
        level: i32,

        /// Override manifest timestamp (RFC 3339 UTC); SOURCE_DATE_EPOCH also works
        #[arg(long, value_name = "RFC3339")]
        generated_at: Option<String>,

        /// Fallback mirror as NAME=https://BASE/PATH/ (repeatable)
        #[arg(long = "mirror", value_name = "NAME=URL")]
        mirrors: Vec<String>,
    },

    /// Verify a manifest signature and catalog hash in a directory
    Verify {
        /// Directory containing index.json, index.json.sig, and the catalog
        #[arg(value_name = "DIR")]
        dir: PathBuf,

        /// Pinned Ed25519 public key (32 raw bytes or hex; or OCP_PLUGIN_INDEX_PUBKEY)
        #[arg(long, value_name = "FILE", env = "OCP_PLUGIN_INDEX_PUBKEY")]
        public_key: PathBuf,
    },

    /// Generate an Ed25519 keypair for signing the plugin index
    Keygen {
        /// Directory for plugin-index.key and plugin-index.pub
        #[arg(long, default_value = ".")]
        out: PathBuf,

        /// Overwrite an existing keypair
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum DevicesCommand {
    /// List built-in devices and their parameter schemas
    List {
        /// Emit the fixed JSON contract
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PluginPresetCommand {
    /// Parse a `.ffp` preset, apply its plain values to a plugin, and report the round trip.
    Import {
        /// VST3 plugin id, name, or bundle path.
        plugin: String,
        /// `.ffp` preset file (binary or ASCII).
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Emit the import report as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ClipCommand {
    /// Align @audio_clip takes to a reference clip and write offset/stretch into the project
    Align {
        /// Project file
        #[arg(value_name = "PROJECT")]
        project: PathBuf,
        /// Name of the reference @audio_clip
        #[arg(value_name = "REFERENCE")]
        reference: String,
        /// Names of the take @audio_clips to align
        #[arg(value_name = "TAKE", required = true)]
        takes: Vec<String>,
        /// Sample rate used for onset analysis
        #[arg(long, default_value_t = 48_000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
        sample_rate: u32,
        /// Report without writing the project
        #[arg(long)]
        dry_run: bool,
        /// Write `invert: true` when the take measures out of phase
        #[arg(long)]
        invert: bool,
        /// Write the aligned project here instead of back to the input file
        #[arg(short, long, conflicts_with = "dry_run")]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum VocalCommand {
    /// Analyse one audio clip and print its f0 frames and notes
    Analyze {
        /// Project file
        #[arg(value_name = "PROJECT")]
        project: PathBuf,
        /// @audio_clip name, or a track name that owns exactly one clip
        #[arg(value_name = "CLIP")]
        clip: String,
        /// Emit JSON instead of a table
        #[arg(long)]
        json: bool,
        /// Write the derived .ocn here instead of the media cache directory
        #[arg(short, long, value_name = "OCN")]
        output: Option<PathBuf>,
        /// Sample rate of the analysed playback buffer
        #[arg(long, default_value_t = 48_000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
        sample_rate: u32,
    },
    /// Resolve the stored note decisions and bake the clip's playback window
    Apply {
        /// Project file
        #[arg(value_name = "PROJECT")]
        project: PathBuf,
        /// @audio_clip name, or a track name that owns exactly one clip
        #[arg(value_name = "CLIP")]
        clip: String,
        /// Write the baked project here (same directory as the source project)
        #[arg(short, long, value_name = "OCP", conflicts_with_all = ["in_place", "dry_run"])]
        output: Option<PathBuf>,
        /// Replace the source project with the baked one
        #[arg(long, conflicts_with_all = ["output", "dry_run"])]
        in_place: bool,
        /// Resolve and report without writing audio or a project
        #[arg(long, conflicts_with_all = ["output", "in_place"])]
        dry_run: bool,
        /// Emit JSON instead of a table
        #[arg(long)]
        json: bool,
        /// Sample rate of the baked playback buffer
        #[arg(long, default_value_t = 48_000, value_parser = clap::value_parser!(u32).range(8000..=192000))]
        sample_rate: u32,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum ExportFormat {
    Json,
    Midi,
}

#[derive(Subcommand)]
enum MidiCommand {
    /// List available MIDI input and output ports
    List,
    /// Print timestamped events from an input port
    Monitor {
        /// MIDI input port name (see `ocp midi list`)
        #[arg(long, value_name = "PORT")]
        input: String,
    },
    /// Import an SMF into a new .ocp project
    Import {
        /// Input MIDI file
        #[arg(value_name = "FILE")]
        input: PathBuf,

        /// Output .ocp file
        #[arg(long, value_name = "OCP")]
        out: PathBuf,

        /// Split multi-channel source tracks into one project track per channel
        #[arg(long)]
        split_channels: bool,

        /// Keep empty source tracks (dropped by default)
        #[arg(long)]
        keep_empty: bool,

        /// Accept and ignore the default drop-empty behaviour explicitly
        #[arg(long)]
        drop_empty: bool,

        /// Print the serialisable import summary as JSON
        #[arg(long)]
        json_summary: bool,
    },

    /// Export an .ocp project or track selection to an SMF
    Export {
        /// Input .ocp file
        #[arg(value_name = "OCP")]
        input: PathBuf,

        /// Output MIDI file
        #[arg(long, value_name = "MID")]
        out: PathBuf,

        /// SMF format (0 or 1)
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(0..=1))]
        format: u8,

        /// Ticks per quarter note
        #[arg(long, default_value_t = 480, value_parser = clap::value_parser!(u16).range(96..=960))]
        ppq: u16,

        /// Track selection: an index or an inclusive range "a,b" / "a-b" (0-based)
        #[arg(long)]
        tracks: Option<String>,

        /// Print the serialisable export report as JSON
        #[arg(long)]
        json_report: bool,
    },
}

fn first_state_difference(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> String {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            for (key, value) in x {
                if y.get(key) != Some(value) {
                    return first_state_difference(
                        value,
                        y.get(key).unwrap_or(&serde_json::Value::Null),
                        &format!("{path}.{key}"),
                    );
                }
            }
        }
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) if x.len() == y.len() => {
            for (index, (left, right)) in x.iter().zip(y).enumerate() {
                if left != right {
                    return first_state_difference(left, right, &format!("{path}[{index}]"));
                }
            }
        }
        _ => {}
    }
    format!("{path}: expected {a}, reopened {b}")
}

/// `OCP_SIMD` forces an element-wise backend for bit-identity checks and benchmarks.
fn apply_simd_override() {
    let Ok(requested) = std::env::var("OCP_SIMD") else {
        return;
    };
    let requested = requested.to_ascii_lowercase();
    match oc_dsp::simd::SimdLevel::from_name(&requested) {
        Some(level) if oc_dsp::simd::select(level) => {
            eprintln!("SIMD: {} (OCP_SIMD)", level.name());
        }
        Some(level) => eprintln!(
            "SIMD: {} is not available; using {}",
            level.name(),
            oc_dsp::simd::level().name()
        ),
        None => eprintln!(
            "SIMD: unknown OCP_SIMD \"{requested}\"; using {}",
            oc_dsp::simd::level().name()
        ),
    }
}

/// Run the command line and hand the process to [`oc_host::exit_process`].
///
/// A hosted plugin registers process-global C++ destructors that run from
/// `exit()`; one of them tears down UI state created on another thread and dies
/// with SIGSEGV. The host does not get to fix that destructor, so after a plugin
/// module has been loaded the process leaves without running it.
fn main() {
    if let Err(error) = oc_audio::private_fs::initialize_config_root_from_env() {
        eprintln!("Configuration profile: {error}");
        oc_host::exit_process(1);
    }

    // SAFETY: this literal executable main owns native control preparation and
    // pumps the same queue through its offline boundaries and final shutdown.
    let registration = unsafe {
        oc_host::application_main::ApplicationMainQueue::register_runtime_current(
            oc_host::application_main::MainRuntime::CommandLine,
        )
    };
    let main_queue = match registration {
        Ok(queue) => queue,
        Err(error) => {
            eprintln!("Error: {error}");
            oc_host::exit_process(1);
        }
    };
    // The CLI runs native preparation and offline rendering on its actual main
    // thread; this opt-in scope services CLAP main callbacks between foreign calls.
    let _offline_main = match oc_host::clap_main::ClapOfflineMainScope::enter() {
        Ok(scope) => scope,
        Err(error) => {
            eprintln!("Error: {error}");
            oc_host::exit_process(1);
        }
    };
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:?}");
            1
        }
    };
    if let Err(error) = main_queue
        .executor()
        .begin_shutdown()
        .and_then(|_| main_queue.pump())
    {
        eprintln!("Plugin shutdown: {error}");
    }
    oc_host::exit_process(code);
}

fn run() -> Result<()> {
    // `ocp` doubles as the isolated scan helper when no dedicated helper
    // binary sits beside it, so intercept that mode before clap parses argv.
    let args: Vec<String> = std::env::args().collect();
    if args
        .iter()
        .any(|arg| arg == oc_host::scanner::HELPER_MODE_ARG)
    {
        return oc_host::scanner::run_helper_cli(&args).map_err(anyhow::Error::msg);
    }
    let cli = Cli::parse();
    apply_simd_override();
    let log_directory = oc_audio::private_fs::explicit_config_dir().map(|root| root.join("logs"));
    let log_path = oc_diag::init_with_options(oc_diag::InitOptions {
        program: "opencompose-cli",
        debug: cli.debug,
        perf: cli.perf,
        directory: log_directory.as_deref(),
    });
    if let Some(path) = &log_path
        && (cli.debug || cli.perf)
    {
        eprintln!("diagnostics log: {}", path.display());
    }
    let _reporter = oc_diag::spawn_reporter(std::time::Duration::from_secs(5));
    let _report_on_exit = ReportOnExit;

    match cli.command {
        Commands::ResolvePlugin { id } => {
            use oc_host::{PluginFormat, find_plugin_info};
            let found = [
                PluginFormat::Vst3,
                PluginFormat::Vst2,
                PluginFormat::Clap,
                PluginFormat::Lv2,
                PluginFormat::Internal,
            ]
            .into_iter()
            .filter_map(|format| find_plugin_info(&id, format))
            .collect::<Vec<_>>();
            if found.len() != 1 {
                anyhow::bail!(
                    "Expected one renderer plugin match for {id}, got {}",
                    found.len()
                );
            }
            println!("{}", serde_json::to_string(&found[0])?);
        }
        Commands::Scan { rescan } => {
            let (plugins, report) = if rescan {
                let (plugins, report) = oc_host::rescan_all_plugins();
                (plugins, Some(report))
            } else {
                (oc_host::scan_all_plugins(), None)
            };
            for plugin in &plugins {
                println!(
                    "{}\t{:?}\t{:?}\t{}",
                    plugin.name, plugin.format, plugin.category, plugin.path
                );
            }
            match report {
                Some(report) => eprintln!(
                    "{} plugins; retried {} quarantined bundles, {} still failing",
                    plugins.len(),
                    report.retried,
                    report.failed
                ),
                None => eprintln!("{} plugins", plugins.len()),
            }
        }
        Commands::PluginParams { id, json } => {
            use oc_host::vst3_instance::Vst3Instance;
            use oc_host::{PluginFormat, find_plugin_info};
            let Some(info) = find_plugin_info(&id, PluginFormat::Vst3) else {
                let Some(info) = find_plugin_info(&id, PluginFormat::Vst2) else {
                    anyhow::bail!("No VST3 or VST2 plugin matched {id}");
                };
                let instance =
                    oc_host::vst2::Vst2Instance::load(std::path::Path::new(&info.path), 48000, 512)
                        .map_err(anyhow::Error::msg)?;
                let params = instance.get_params();
                if json {
                    println!("{}", serde_json::to_string_pretty(&params)?);
                    return Ok(());
                }
                println!("{} — {} parameters", info.name, params.len());
                println!("{}", info.path);
                println!(
                    "{:>6}  {:<44} {:<8} {:>10} {:>10}",
                    "index", "name", "label", "default", "current"
                );
                for param in params {
                    println!(
                        "{:>6}  {:<44} {:<8} {:>10.4} {:>10.4}  {}",
                        param.index,
                        param.name,
                        param.label,
                        param.default,
                        param.value,
                        param.display
                    );
                }
                return Ok(());
            };
            let instance = Vst3Instance::load(std::path::Path::new(&info.path), 48000, 512)
                .map_err(anyhow::Error::msg)?;
            let params = instance.get_param_details();
            if json {
                println!("{}", serde_json::to_string_pretty(&params)?);
                return Ok(());
            }
            println!("{} — {} parameters", info.name, params.len());
            println!("{}", info.path);
            println!(
                "{:>10}  {:<44} {:<8} {:>12} {:>12} {:>12} {:>6}  flags",
                "id", "name", "unit", "min", "max", "default", "steps"
            );
            for p in params {
                let mut flags = String::new();
                if p.read_only {
                    flags.push('R');
                }
                if p.hidden {
                    flags.push('H');
                }
                if p.list {
                    flags.push('L');
                }
                if p.can_automate {
                    flags.push('A');
                }
                println!(
                    "{:>10}  {:<44} {:<8} {:>12} {:>12} {:>12} {:>6}  {}",
                    p.id,
                    p.name,
                    p.unit,
                    p.min_text,
                    p.max_text,
                    p.default_text,
                    p.step_count,
                    flags
                );
            }
        }
        Commands::PluginPreset { command } => match command {
            PluginPresetCommand::Import { plugin, file, json } => {
                use oc_host::plugin_preset;
                use oc_host::vst3_instance::Vst3Instance;
                use oc_host::{PluginFormat, find_plugin_info};
                let bytes = std::fs::read(&file).map_err(|error| {
                    anyhow::anyhow!("Failed to read {}: {error}", file.display())
                })?;
                let preset = plugin_preset::parse_preset(&bytes).map_err(|error| {
                    anyhow::anyhow!("Failed to parse {}: {error}", file.display())
                })?;
                let info = find_plugin_info(&plugin, PluginFormat::Vst3)
                    .ok_or_else(|| anyhow::anyhow!("No VST3 plugin matched {plugin}"))?;
                let mut instance = Vst3Instance::load(std::path::Path::new(&info.path), 48000, 512)
                    .map_err(anyhow::Error::msg)?;
                let report =
                    plugin_preset::apply_to_instance(&mut instance, &preset).map_err(|error| {
                        anyhow::anyhow!("Failed to import {}: {error}", file.display())
                    })?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    return Ok(());
                }
                println!(
                    "{} — {} v{} ({})",
                    preset.plugin, report.signature, report.version, report.format
                );
                if let Some(name) = &report.preset_name {
                    println!("preset: {name}");
                }
                println!(
                    "parsed {} params, mapped {}, applied {}",
                    report.parsed, report.mapped, report.applied
                );
                if !report.unmapped.is_empty() {
                    println!(
                        "unmapped ({}): {}",
                        report.unmapped.len(),
                        report.unmapped.join(", ")
                    );
                }
                if !report.rejected.is_empty() {
                    println!(
                        "rejected ({}): {}",
                        report.rejected.len(),
                        report.rejected.join(", ")
                    );
                }
                if !report.mismatched.is_empty() {
                    println!("readback mismatches ({}):", report.mismatched.len());
                    for mismatch in &report.mismatched {
                        println!(
                            "  {}: expected plain {}, read back {:?}",
                            mismatch.name, mismatch.expected_plain, mismatch.readback_plain
                        );
                    }
                }
            }
        },
        Commands::CompileProject { input } => {
            let raw: serde_json::Value = if input == std::path::Path::new("-") {
                serde_json::from_reader(std::io::stdin().lock())?
            } else {
                serde_json::from_slice(&std::fs::read(input)?)?
            };
            let project: oc_dsl::ast::Project = serde_json::from_value(raw.clone())?;
            if raw != serde_json::to_value(&project)? {
                anyhow::bail!(
                    "Expected complete canonical project state; unknown or omitted fields are not accepted"
                );
            }
            oc_dsl::validate(&project).map_err(|e| anyhow::anyhow!("Invalid project: {:?}", e))?;
            let text = oc_dsl::serialize(&project);
            let reopened =
                oc_dsl::parse(&text).map_err(|e| anyhow::anyhow!("Compiled parse: {}", e))?;
            oc_dsl::validate(&reopened)
                .map_err(|e| anyhow::anyhow!("Compiled validation: {:?}", e))?;
            if serde_json::to_value(&project)? != serde_json::to_value(&reopened)? {
                anyhow::bail!(
                    "Project state is not exactly representable by the OCP serializer: {}",
                    first_state_difference(
                        &serde_json::to_value(&project)?,
                        &serde_json::to_value(&reopened)?,
                        "$"
                    )
                );
            }
            print!("{}", text);
        }
        Commands::Parse { input, json } => {
            let source = std::fs::read_to_string(&input)?;
            let project =
                oc_dsl::parse(&source).map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;

            if json {
                let json_str = serde_json::to_string_pretty(&project)?;
                println!("{}", json_str);
            } else {
                println!(
                    "OK: \"{}\" — {} tracks, {} buses, {} sections",
                    project.name.as_deref().unwrap_or("untitled"),
                    project.tracks.len(),
                    project.buses.len(),
                    project.sections.len(),
                );
                for (i, track) in project.tracks.iter().enumerate() {
                    println!(
                        "  Track {}: {} [{}] — {} phrases, {} patterns, {} effects",
                        i,
                        track.name,
                        track.instrument.as_path(),
                        track.phrases.len(),
                        track.patterns.len(),
                        track.effects.len(),
                    );
                }
            }
        }
        Commands::ValidateBuiltinEffects { input, sample_rate } => {
            let source = std::fs::read_to_string(&input)?;
            let project = oc_dsl::parse(&source)?;
            oc_dsl::validate(&project)
                .map_err(|e| anyhow::anyhow!("Semantic validation failed: {e:?}"))?;
            print_legacy_effect_warnings(&project);
            let builtin_project = builtin_effect_project(&project)
                .map_err(|e| anyhow::anyhow!("Preset resolution failed: {e}"))?;
            oc_engine::graph::AudioGraph::try_from_project_with_source_latencies(
                &builtin_project,
                sample_rate,
                &[],
            )
            .map_err(|e| anyhow::anyhow!("Built-in DSP preflight failed: {e}"))?;
            println!(
                "{{\"builtin_effects_valid\":true,\"sample_rate\":{sample_rate},\"audio_rendered\":false}}"
            );
        }
        Commands::Devices { command } => match command {
            DevicesCommand::List { json } => {
                if json {
                    println!("{}", devices_list_json()?);
                } else {
                    println!("{:<16} {:<10} {:>6}  NAME", "ID", "CATEGORY", "PARAMS");
                    for device in oc_engine::dsp::schema::DEVICES {
                        println!(
                            "{:<16} {:<10} {:>6}  {}",
                            device.id,
                            device.category.as_str(),
                            device.params.len(),
                            device.name
                        );
                    }
                }
            }
        },
        Commands::Validate { input } => {
            let source = std::fs::read_to_string(&input)?;
            match oc_dsl::parse(&source) {
                Ok(project) => {
                    oc_dsl::validate(&project)
                        .map_err(|e| anyhow::anyhow!("Semantic validation failed: {:?}", e))?;
                    let root = input
                        .parent()
                        .filter(|path| !path.as_os_str().is_empty())
                        .unwrap_or_else(|| std::path::Path::new("."));
                    let missing = oc_dsl::media::missing(&project, root);
                    if !missing.is_empty() {
                        anyhow::bail!("{}", missing.join("\n"));
                    }
                    oc_engine::audio_clip::validate(&project, input.parent())
                        .map_err(|e| anyhow::anyhow!("Audio clip validation failed: {}", e))?;
                    print_legacy_effect_warnings(&project);
                    // The renderer's own factory: a value it cannot type is
                    // refused here too, not only when the render starts.
                    let refusals = oc_engine::graph::validate_effect_parameters(&project, 48_000);
                    if !refusals.is_empty() {
                        anyhow::bail!("{}", refusals.join("\n"));
                    }
                    println!(
                        "Valid: \"{}\" — {} tracks",
                        project.name.as_deref().unwrap_or("untitled"),
                        project.tracks.len(),
                    );
                }
                Err(e) => {
                    eprintln!("Invalid: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Format { input, write } => {
            let source = std::fs::read_to_string(&input)?;
            let project =
                oc_dsl::parse(&source).map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;
            let formatted = oc_dsl::serialize(&project);

            if write {
                std::fs::write(&input, &formatted)?;
                println!("Formatted: {}", input.display());
            } else {
                print!("{}", formatted);
            }
        }
        Commands::Export {
            input,
            format,
            output,
        } => {
            let fmt_name = match format {
                ExportFormat::Json => "JSON",
                ExportFormat::Midi => "MIDI",
            };
            eprintln!(
                "Exporting: {} → {} ({})",
                input.display(),
                output
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "stdout".to_string()),
                fmt_name
            );
            match format {
                ExportFormat::Json => {
                    let source = std::fs::read_to_string(&input)?;
                    let project = oc_dsl::parse(&source)
                        .map_err(|error| anyhow::anyhow!("Parse error: {}", error))?;
                    oc_dsl::validate(&project)
                        .map_err(|errors| anyhow::anyhow!("Invalid project: {errors:?}"))?;
                    // JSON is the render-ready state, so a hairpin arrives as the
                    // gain clip it expands to; `parse --json` keeps the source AST.
                    let expanded = oc_dsl::dynamics::expand(&project)
                        .map_err(|error| anyhow::anyhow!("Expansion error: {}", error))?;
                    let json = serde_json::to_string_pretty(&expanded)? + "\n";
                    if let Some(path) = output {
                        std::fs::write(path, json)?;
                    } else {
                        print!("{}", json);
                    }
                }
                ExportFormat::Midi => {
                    let out = output
                        .ok_or_else(|| anyhow::anyhow!("MIDI export requires --output <FILE>"))?;
                    export_midi_file(&input, &out, 1, 480, None, false)?;
                }
            }
        }
        Commands::Clip { command } => match command {
            ClipCommand::Align {
                project,
                reference,
                takes,
                sample_rate,
                dry_run,
                invert,
                output,
            } => align_clips(
                &project,
                &reference,
                &takes,
                sample_rate,
                dry_run,
                invert,
                output,
            )?,
        },
        Commands::Vocal { command } => match command {
            VocalCommand::Analyze {
                project,
                clip,
                json,
                output,
                sample_rate,
            } => vocal_analyze(&project, &clip, json, output, sample_rate)?,
            VocalCommand::Apply {
                project,
                clip,
                output,
                in_place,
                dry_run,
                json,
                sample_rate,
            } => vocal_apply(
                &project,
                &clip,
                output,
                in_place,
                dry_run,
                json,
                sample_rate,
            )?,
        },
        Commands::Level {
            project,
            track,
            write,
            output,
            json,
            sample_rate,
            soundfont,
        } => level_bake(
            &project,
            &track,
            write,
            output,
            json,
            sample_rate,
            soundfont,
        )?,
        Commands::Render {
            input,
            output,
            soundfont,
            format,
            sample_rate,
            per_track_mix,
            output_gain_db,
            minimum_duration_seconds,
            reruns,
            require_identical,
            pre_roll,
            stems,
            no_loudness_normalisation,
            sample_format,
            flac_level,
            mp3_bitrate,
            mp3_vbr,
            dither,
            no_dither,
            resample_quality,
            report,
        } => {
            let collect_stages = oc_diag::perf_enabled();
            let _render_timer = oc_diag::perf_counter("render.wall.ms").map(oc_diag::Timer::start);
            if collect_stages {
                oc_engine::stage_log::begin();
            }

            // Parse
            let mut project = oc_engine::project_file::load(&input)?.project;
            // A bare instrument family gets its built-in default only when no
            // legacy fallback is supplied; `--soundfont` must stay authoritative.
            if soundfont.is_some() {
                oc_dsl::instruments::clear_injected(&mut project);
            } else {
                oc_dsl::instruments::apply_defaults(&mut project);
            }
            oc_dsl::validate(&project)
                .map_err(|e| anyhow::anyhow!("Semantic validation failed: {:?}", e))?;
            // The flag wins, then the project header, then the historical WAV.
            let container = format
                .or_else(|| project.properties.export.map(|export| export.container))
                .unwrap_or_default();
            let output = resolve_output_path(output, container)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            println!("Rendering: {} → {}", input.display(), output.display());
            // The project may remember the rate it was exported at; an explicit
            // flag still wins, so a one-off render can leave the file alone.
            let sample_rate = effective_sample_rate(sample_rate, &project);
            if let Some(step) = resample_quality {
                project.properties.export.get_or_insert_default().resample = Some(step);
            }
            print_legacy_effect_warnings(&project);
            // AU presets that cannot be hosted warn during the render, with the
            // unit and the reason; a resolution failure there is not fatal.
            oc_engine::render_warnings::clear();
            if no_loudness_normalisation {
                if let Some(target) = project
                    .master
                    .as_ref()
                    .and_then(|master| master.loudness_target)
                {
                    println!("  Loudness normalisation disabled (target {target} LUFS ignored)");
                }
                if let Some(master) = &mut project.master {
                    master.loudness_target = None;
                }
            }
            println!(
                "  Parsed: \"{}\" — {} tracks",
                project.name.as_deref().unwrap_or("untitled"),
                project.tracks.len()
            );
            let spec = resolve_encode_spec(
                &project,
                container,
                sample_rate,
                EncodeFlags {
                    depth: sample_format,
                    flac_level,
                    mp3_bitrate,
                    mp3_vbr,
                    dither,
                    no_dither,
                    report,
                },
            )
            .map_err(|error| anyhow::anyhow!("{error}"))?;
            match spec.container {
                oc_dsl::ast::ExportContainer::Mp3 => {
                    println!("  Export: mp3, {}", describe_mp3_rate(&spec));
                }
                _ => println!(
                    "  Export: {}, {} samples{}",
                    spec.container,
                    spec.depth(),
                    if spec.dither() {
                        ", TPDF dither at 1 LSB"
                    } else {
                        ""
                    }
                ),
            }
            if spec.container == oc_dsl::ast::ExportContainer::Flac {
                println!("  FLAC compression: level {}", spec.flac.level);
            }
            println!(
                "  Resample: {}",
                oc_engine::resample::render_quality(&project)
            );
            oc_diag::diag_info!(
                "render: project={:?} tracks={} sample_rate={} threads={:?}",
                project.name.as_deref().unwrap_or("untitled"),
                project.tracks.len(),
                sample_rate,
                std::env::var("OCP_RENDER_THREADS").ok()
            );

            if !pre_roll.is_finite() || !(0.0..=600.0).contains(&pre_roll) {
                anyhow::bail!("--pre-roll must be finite and within 0..600 seconds");
            }
            let runs = reruns.unwrap_or(if require_identical { 2 } else { 1 });
            if runs == 0 {
                anyhow::bail!("--reruns must be at least 1");
            }
            if require_identical && runs < 2 {
                anyhow::bail!("--require-identical needs at least two reruns");
            }
            if stems.is_some() && runs != 1 {
                anyhow::bail!(
                    "--stems performs one export pass; drop --reruns/--require-identical"
                );
            }
            if stems.is_some() && (per_track_mix || output_gain_db != 0.0) {
                anyhow::bail!(
                    "--stems cannot be combined with --per-track-mix or --output-gain-db"
                );
            }

            // Without a legacy fallback, resolve each track's project-owned source.
            let project_sources = soundfont.is_none();
            let soundfont = soundfont.unwrap_or_default();
            let plugin_path = project_sources
                || per_track_mix
                || stems.is_some()
                || minimum_duration_seconds != 0.0
                || !project.audio_clips().is_empty()
                || oc_engine::render::has_gain_automation(&project)
                || project
                    .presets
                    .iter()
                    .any(|preset| !preset.effects.is_empty());

            let mut exported: Option<oc_engine::encode::EncodeReport> = None;

            if let Some(stem_dir) = &stems {
                if let Some(parent) = output.parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.is_dir()
                {
                    anyhow::bail!("output directory '{}' does not exist", parent.display());
                }
                let mut plugin_instances = load_project_clap_instances(&project, sample_rate)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                let export = oc_engine::render::render_with_stems(
                    &project,
                    &soundfont,
                    sample_rate,
                    &mut plugin_instances,
                    input.parent(),
                    minimum_duration_seconds,
                    pre_roll,
                )
                .map_err(|e| anyhow::anyhow!("Render error: {}", e))?;
                for warning in oc_engine::render_warnings::take() {
                    eprintln!("warning: {warning}");
                }
                let mix_report = oc_engine::encode::encode_to_file(&output, &spec, &export.mix)
                    .map_err(|e| anyhow::anyhow!("Render error: {}", e))?;
                let song = project
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| {
                        input
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().to_string())
                            .unwrap_or_else(|| "untitled".into())
                    });
                let manifest =
                    oc_engine::render::write_stems_export(&project, stem_dir, &song, &export, spec)
                        .map_err(|e| anyhow::anyhow!("Stems export error: {}", e))?;
                for warning in oc_engine::render_warnings::take() {
                    eprintln!("warning: {warning}");
                }
                println!(
                    "  Stems: mix + {} stem(s) at {} Hz, {} frames; manifest {}",
                    export.stems.len(),
                    sample_rate,
                    export.mix.len() / 2,
                    manifest.display()
                );
                exported = Some(mix_report);
            } else {
                let render_once =
                    |target: &std::path::Path| -> Result<oc_engine::encode::EncodeReport, String> {
                        if plugin_path {
                            let mut plugin_instances =
                                load_project_clap_instances(&project, sample_rate)?;
                            oc_engine::stage_log::begin();
                            let rendered = oc_engine::render::render_to_wav_per_track_with_plugins_at_minimum_duration_with_pre_roll(
                        &project,
                        &soundfont,
                        target,
                        output_gain_db,
                        &mut plugin_instances,
                        input.parent(),
                        minimum_duration_seconds,
                        pre_roll,
                        spec,
                    );
                            print_stage_table(&oc_engine::stage_log::take());
                            rendered
                        } else {
                            oc_engine::render::render_to_wav(&project, &soundfont, target, spec)
                        }
                    };
                let render_run =
                    |target: &std::path::Path| -> Result<oc_engine::encode::EncodeReport, String> {
                        let rendered = render_once(target);
                        for warning in oc_engine::render_warnings::take() {
                            eprintln!("warning: {warning}");
                        }
                        rendered
                    };

                if runs == 1 {
                    exported = Some(
                        render_run(&output).map_err(|e| anyhow::anyhow!("Render error: {}", e))?,
                    );
                } else {
                    let temp_root =
                        std::env::temp_dir().join(format!("ocp-rerun-{}", std::process::id()));
                    std::fs::create_dir_all(&temp_root)?;
                    let mut hashes = Vec::with_capacity(runs as usize);
                    let mut first: Option<PathBuf> = None;
                    for run in 1..=runs {
                        let target =
                            temp_root.join(format!("run-{run}.{}", spec.container.extension()));
                        let report = match render_run(&target) {
                            Ok(report) => report,
                            Err(error) => {
                                let _ = std::fs::remove_dir_all(&temp_root);
                                return Err(anyhow::anyhow!("Render error: {}", error));
                            }
                        };
                        let hash = sha256_file(&target)?;
                        println!("  run {run}/{runs}: {hash}");
                        hashes.push(hash);
                        if first.is_none() {
                            first = Some(target);
                            exported = Some(report);
                        }
                    }
                    let mut sorted = hashes.clone();
                    sorted.sort();
                    sorted.dedup();
                    let identical = sorted.len() == 1;
                    if identical {
                        let warm_up = if project_has_plugin_instrument(&project) {
                            format!(", pre-roll {pre_roll}s")
                        } else {
                            String::new()
                        };
                        println!(
                            "  Determinism: {runs}/{runs} runs byte-identical (sha256 {}{warm_up}, {} Hz)",
                            hashes[0], sample_rate
                        );
                    } else if require_identical {
                        let _ = std::fs::remove_dir_all(&temp_root);
                        anyhow::bail!(
                            "deterministic render failed: {runs} isolated runs produced {} distinct outputs ({}). A plugin that loads sample data asynchronously, or a device that seeds itself from the clock, is the likely cause; raise --pre-roll (currently {pre_roll}s).",
                            sorted.len(),
                            sorted.join(", ")
                        );
                    } else {
                        eprintln!(
                            "warning: {runs} isolated runs produced {} distinct outputs ({}); keeping run 1",
                            sorted.len(),
                            sorted.join(", ")
                        );
                    }
                    let first = first.expect("at least one run");
                    if let Some(parent) = output.parent()
                        && !parent.as_os_str().is_empty()
                        && !parent.is_dir()
                    {
                        let _ = std::fs::remove_dir_all(&temp_root);
                        anyhow::bail!("output directory '{}' does not exist", parent.display());
                    }
                    std::fs::copy(&first, &output)?;
                    let _ = std::fs::remove_dir_all(&temp_root);
                }
            }
            if report {
                print_level_report(&output, exported)?;
            }
            if collect_stages {
                log_stage_table(&oc_engine::stage_log::take());
            } else {
                println!("  Wrote {}", output.display());
            }
        }
        Commands::Bounce {
            project: input,
            track,
            from,
            to,
            output,
            soundfont,
            sample_rate,
        } => {
            let mut project = oc_engine::project_file::load(&input)?.project;
            // A bare instrument family gets its built-in default only when no
            // legacy fallback is supplied; `--soundfont` must stay authoritative.
            if soundfont.is_some() {
                oc_dsl::instruments::clear_injected(&mut project);
            } else {
                oc_dsl::instruments::apply_defaults(&mut project);
            }
            oc_dsl::validate(&project)
                .map_err(|e| anyhow::anyhow!("Semantic validation failed: {:?}", e))?;
            print_legacy_effect_warnings(&project);
            let sample_rate = effective_sample_rate(sample_rate, &project);
            let clock = oc_dsl::timing::ProjectClock::new(&project.properties)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let from_beat = bar_beat_quarters(&clock, &from)?;
            let to_beat = bar_beat_quarters(&clock, &to)?;
            if to_beat <= from_beat {
                anyhow::bail!("bounce range end must be after its start");
            }
            let index = project
                .tracks
                .iter()
                .position(|candidate| candidate.name == track)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no track named '{track}' in {}; the project has {} track(s)",
                        input.display(),
                        project.tracks.len()
                    )
                })?;
            if let Some(parent) = output.parent()
                && !parent.as_os_str().is_empty()
                && !parent.is_dir()
            {
                anyhow::bail!("output directory '{}' does not exist", parent.display());
            }
            let output = match output.extension().and_then(|value| value.to_str()) {
                None => output.with_extension("wav"),
                Some(extension) if extension.eq_ignore_ascii_case("wav") => output,
                Some(extension) => anyhow::bail!(
                    "output '{}' has the .{extension} extension but a bounce writes .wav",
                    output.display()
                ),
            };
            let soundfont = soundfont.unwrap_or_default();
            let mut plugin_instances = load_project_clap_instances(&project, sample_rate)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            oc_engine::render_warnings::clear();
            let samples = oc_engine::render::render_track_post_fx_range(
                &project,
                &soundfont,
                sample_rate,
                &mut plugin_instances,
                input.parent(),
                index,
                from_beat,
                to_beat,
            )
            .map_err(|e| anyhow::anyhow!("Bounce error: {}", e))?;
            for warning in oc_engine::render_warnings::take() {
                eprintln!("warning: {warning}");
            }
            oc_engine::wav::write_stereo_f32_wav(&output, &samples, sample_rate)
                .map_err(|e| anyhow::anyhow!("Bounce error: {}", e))?;
            let frames = samples.len() / 2;
            let peak = samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            println!(
                "  Bounced track '{}': {} frames ({:.3}s) at {} Hz, 32-bit float",
                track,
                frames,
                frames as f64 / f64::from(sample_rate),
                sample_rate
            );
            if peak > 0.0 {
                println!("  Peak: {:+.2} dBFS", 20.0 * (peak as f64).log10());
            }
            println!("  Output: {}", output.display());
        }
        Commands::Randomize {
            input,
            preset,
            count,
            seed,
            output,
        } => {
            if count == 0 {
                anyhow::bail!("--count must be at least 1");
            }
            let source = std::fs::read_to_string(&input)?;
            let project =
                oc_dsl::parse(&source).map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;
            oc_dsl::validate(&project)
                .map_err(|e| anyhow::anyhow!("Semantic validation failed: {:?}", e))?;
            let resolved = oc_dsl::preset::resolve(&project).map_err(|e| anyhow::anyhow!("{e}"))?;
            let target = match preset.as_deref() {
                Some(name) => resolved
                    .presets
                    .iter()
                    .find(|candidate| candidate.name == name),
                None => resolved.presets.iter().find(|candidate| {
                    candidate.plugin.as_deref() == Some(oc_synth::synth::PLUGIN_ID)
                }),
            }
            .ok_or_else(|| {
                anyhow::anyhow!("{} has no OpenSynth preset to randomise", input.display())
            })?;
            if target.plugin.as_deref() != Some(oc_synth::synth::PLUGIN_ID) {
                anyhow::bail!("preset '{}' is not an OpenSynth patch", target.name);
            }
            std::fs::create_dir_all(&output)?;
            let slug = filename_slug(&target.name);
            let params = target.params.clone();
            for index in 0..count {
                let mut variant = resolved.clone();
                let slot = variant
                    .presets
                    .iter_mut()
                    .find(|candidate| candidate.name == target.name)
                    .expect("target preset is in the resolved project");
                slot.params = oc_synth::synth::random::randomize_params(
                    &params,
                    seed.wrapping_add(u64::from(index)),
                );
                let path = output.join(format!("{slug}-{index:03}.ocp"));
                std::fs::write(&path, oc_dsl::serialize(&variant))?;
                println!("{}", path.display());
            }
            println!(
                "Wrote {count} variation(s) of '{}' to {}",
                target.name,
                output.display()
            );
        }
        Commands::Midi2ocp { input, output } => {
            let project = oc_data::midi2dsl::midi_to_project(&input)
                .map_err(|e| anyhow::anyhow!("MIDI conversion: {}", e))?;
            let ocp_text = oc_dsl::serialize(&project);

            if let Some(out_path) = output {
                std::fs::write(&out_path, &ocp_text)?;
                println!(
                    "Converted: {} → {} ({} tracks)",
                    input.display(),
                    out_path.display(),
                    project.tracks.len()
                );
            } else {
                print!("{}", ocp_text);
            }
        }
        Commands::Midi { command } => match command {
            MidiCommand::List => midi::list()?,
            MidiCommand::Monitor { input } => midi::monitor(&input)?,
            MidiCommand::Import {
                input,
                out,
                split_channels,
                keep_empty,
                drop_empty: _,
                json_summary,
            } => {
                let data = oc_engine::midi::read_smf_file(&input)?;
                let name = input
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_string);
                let options = oc_engine::midi::ImportOptions {
                    split_channels,
                    drop_empty: !keep_empty,
                    ..Default::default()
                };
                let result = oc_engine::midi::import_smf(&data, name, &options, None)?;
                oc_dsl::validate(&result.project)
                    .map_err(|errors| anyhow::anyhow!("Imported project is invalid: {errors:?}"))?;
                std::fs::write(&out, oc_dsl::serialize(&result.project))?;
                if json_summary {
                    println!("{}", serde_json::to_string_pretty(&result.summary)?);
                } else {
                    println!(
                        "Imported {} track(s) from {} ({} notes, {} tempo change(s), {} CC lane(s)) → {}",
                        result.project.tracks.len(),
                        input.display(),
                        result
                            .summary
                            .tracks
                            .iter()
                            .map(|track| track.note_count)
                            .sum::<usize>(),
                        result.summary.tempo_changes,
                        result
                            .summary
                            .tracks
                            .iter()
                            .map(|track| track.lanes.len())
                            .sum::<usize>(),
                        out.display(),
                    );
                    for warning in &result.summary.warnings {
                        eprintln!("warning: {warning}");
                    }
                }
            }
            MidiCommand::Export {
                input,
                out,
                format,
                ppq,
                tracks,
                json_report,
            } => {
                export_midi_file(&input, &out, format, ppq, tracks.as_deref(), json_report)?;
            }
        },
        Commands::BatchData {
            midi_dir,
            output,
            soundfont,
            sample_rate,
        } => {
            println!(
                "Batch data pipeline: {} → {}",
                midi_dir.display(),
                output.display()
            );
            let results =
                oc_data::pipeline::process_directory(&midi_dir, &output, &soundfont, sample_rate);

            // Write manifest
            let manifest_path = output.join("manifest.json");
            let json = serde_json::to_string_pretty(&results)?;
            std::fs::write(&manifest_path, json)?;
            println!("Manifest written to {}", manifest_path.display());
        }
        Commands::GenerateMidi {
            output,
            count,
            seed,
        } => {
            println!(
                "Generating {} synthetic MIDI files in {}",
                count,
                output.display()
            );
            let generated = oc_data::generate_midi::generate_dataset(&output, count, seed);
            println!("Generated {} MIDI files", generated);
        }
        Commands::BatchMidi2ocp {
            midi_dir,
            output,
            flatten,
        } => run_batch_midi2ocp(&midi_dir, &output, flatten)?,
        Commands::Play {
            input,
            midi_in,
            midi_out,
            midi_clock,
            midi_thru,
            soundfont,
            sample_rate,
        } => {
            let source = std::fs::read_to_string(&input)?;
            let project =
                oc_dsl::parse(&source).map_err(|e| anyhow::anyhow!("Parse error: {}", e))?;
            oc_dsl::validate(&project)
                .map_err(|e| anyhow::anyhow!("Semantic validation failed: {:?}", e))?;
            play::run(
                &project,
                input.parent(),
                play::PlayOptions {
                    midi_in,
                    midi_out,
                    midi_clock,
                    midi_thru,
                    soundfont,
                    sample_rate,
                },
            )?;
        }
        Commands::Sample { command } => sample::run(command)?,
        Commands::Plugin { command } => match command {
            PluginCommand::Install {
                id,
                index,
                catalog,
                public_key,
                platform,
                format,
                into,
                dry_run,
                open,
                overwrite,
                json,
            } => {
                let report = plugin_install::install(&plugin_install::InstallOptions {
                    id,
                    catalog,
                    index,
                    public_key,
                    platform,
                    format,
                    into,
                    dry_run,
                    open,
                    overwrite,
                })
                .map_err(anyhow::Error::msg)?;
                if json {
                    println!("{}", plugin_install::report_json(&report));
                } else if report.dry_run {
                    println!(
                        "plan only: {} ({}, {}) via {}",
                        report.name, report.format, report.kind, report.url
                    );
                    println!("  sha256 {}", report.sha256);
                    println!("  bytes  {}", report.bytes);
                    println!("  target {}", report.target);
                } else {
                    let path = report.path.clone().unwrap_or_default();
                    println!(
                        "{} {} ({}, {}) -> {path}",
                        if report.opened {
                            "opened installer for"
                        } else {
                            "installed"
                        },
                        report.name,
                        report.format,
                        report.kind
                    );
                    println!("  url    {}", report.url);
                    println!("  sha256 {}", report.sha256);
                    println!("  bytes  {}", report.bytes);
                    if let Some(note) = &report.note {
                        println!("  note   {note}");
                    }
                }
            }
            PluginCommand::Conformance {
                format,
                plugin,
                no_editor,
                timeout_ms,
                child,
            } => {
                if child {
                    let Some(path) = plugin.first() else {
                        anyhow::bail!("--child needs --plugin");
                    };
                    let code = oc_host::conformance::run_child(path, timeout_ms, no_editor);
                    oc_host::exit_process(code);
                }
                conformance::run(conformance::Options {
                    format,
                    plugins: plugin,
                    no_editor,
                    timeout_ms,
                })
                .map_err(anyhow::Error::msg)?;
            }
        },
        Commands::PluginIndex { command } => match command {
            PluginIndexCommand::Build {
                catalog,
                out,
                key,
                min_app_version,
                catalog_version,
                level,
                generated_at,
                mirrors,
            } => {
                let mirrors = mirrors
                    .iter()
                    .map(|mirror| plugin_index::parse_mirror_arg(mirror))
                    .collect::<Result<Vec<_>>>()?;
                let report = plugin_index::build(&plugin_index::BuildOptions {
                    catalog_path: catalog,
                    out_dir: out,
                    key_path: key,
                    catalog_version,
                    min_app_version,
                    generated_at,
                    level,
                    mirrors,
                })?;
                println!(
                    "catalog: {}\ncatalog raw: {} bytes\ncatalog zstd: {} bytes ({:.1}% of raw)\nmanifest: {} bytes\nsignature: {} bytes\nsha256: {}\ncatalog_version: {}\ngenerated_at: {}\nkey_id: {}",
                    report.catalog_path,
                    report.raw_bytes,
                    report.compressed_bytes,
                    100.0 * report.compressed_bytes as f64 / report.raw_bytes.max(1) as f64,
                    report.manifest_bytes,
                    report.signature_bytes,
                    report.sha256,
                    report.catalog_version,
                    report.generated_at,
                    report.key_id,
                );
                println!("upload in this order:");
                for upload in &report.uploads {
                    println!("  {upload}");
                }
            }
            PluginIndexCommand::Verify { dir, public_key } => {
                let report = plugin_index::verify(&dir, &public_key)?;
                println!(
                    "OK catalog_version={} generated_at={} entries={} raw={} compressed={} key_id={}",
                    report.catalog_version,
                    report.generated_at,
                    report.entries,
                    report.raw_bytes,
                    report.compressed_bytes,
                    report.key_id,
                );
            }
            PluginIndexCommand::Keygen { out, force } => {
                let report = plugin_index::keygen(&out, force)?;
                println!(
                    "private key: {} (chmod 600, never commit)\npublic key: {}\npublic key hex: {}\nkey_id: {}",
                    report.key_path.display(),
                    report.public_key_path.display(),
                    report.public_key,
                    report.key_id,
                );
            }
        },
    }

    Ok(())
}

fn vocal_analyze(
    input: &std::path::Path,
    selector: &str,
    json: bool,
    output: Option<PathBuf>,
    sample_rate: u32,
) -> Result<()> {
    let source = std::fs::read_to_string(input)?;
    let project =
        oc_dsl::parse(&source).map_err(|error| anyhow::anyhow!("Parse error: {error}"))?;
    let clip =
        oc_engine::vocal_analysis::resolve_clip(&project, selector).map_err(anyhow::Error::msg)?;
    let params = oc_dsp::vocal_analysis::DetectorParams::default();
    let result = oc_engine::vocal_analysis::analyze_clip(
        &project,
        clip,
        sample_rate,
        input.parent(),
        &params,
        output.as_deref(),
    )
    .map_err(anyhow::Error::msg)?;

    if json {
        let frames: Vec<serde_json::Value> = result
            .analysis
            .frames
            .iter()
            .map(|frame| {
                serde_json::json!({
                    "time_seconds": frame.time_s,
                    "f0_hz": frame.f0_hz,
                    "confidence": frame.confidence,
                    "voiced": frame.voiced,
                })
            })
            .collect();
        let notes: Vec<serde_json::Value> = result
            .analysis
            .notes
            .iter()
            .map(|note| {
                serde_json::json!({
                    "start_seconds": note.start_s,
                    "end_seconds": note.end_s,
                    "onset_seconds": note.onset_s,
                    "offset_seconds": note.offset_s,
                    "f0_median_hz": note.f0_median_hz,
                    "confidence": note.confidence,
                    "voiced_ratio": note.voiced_ratio,
                })
            })
            .collect();
        let report = serde_json::json!({
            "clip": result.clip_name,
            "track": result.track,
            "file": result.file,
            "sample_rate": result.target_rate,
            "analysis_rate_hz": result.analysis.analysis_rate_hz,
            "hop_seconds": result.analysis.hop_seconds,
            "clip_start_seconds": result.clip_start_s,
            "duration_seconds": result.duration_s,
            "analyzer_version": oc_engine::vocal_analysis::ANALYZER_VERSION,
            "detector_params_hash": result.key.detector.hash(),
            "cache": if result.from_cache { "hit" } else { "miss" },
            "output": result.output.as_ref().map(|path| path.display().to_string()),
            "notes": notes,
            "frames": frames,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let voiced = result
        .analysis
        .frames
        .iter()
        .filter(|frame| frame.voiced)
        .count();
    println!(
        "clip '{}' (track '{}'): {:.3} s at {} Hz, analysis {:.1} Hz, {} frames",
        result.clip_name,
        result.track,
        result.duration_s,
        result.target_rate,
        result.analysis.analysis_rate_hz,
        result.analysis.frames.len(),
    );
    match (&result.output, result.from_cache) {
        (Some(path), true) => println!("cache: hit ({})", path.display()),
        (Some(path), false) => println!("cache: miss -> {}", path.display()),
        (None, _) => println!("cache: disabled"),
    }
    println!("  #  start      end    onset   offset   f0 Hz  conf  voiced");
    for (index, note) in result.analysis.notes.iter().enumerate() {
        println!(
            "{:>3}  {:>7.3}  {:>7.3}  {:>7.3}  {:>7.3}  {:>7.2}  {:>4.2}  {:>6.3}",
            index + 1,
            note.start_s,
            note.end_s,
            note.onset_s,
            note.offset_s,
            note.f0_median_hz,
            note.confidence,
            note.voiced_ratio,
        );
    }
    let ratio = if result.analysis.frames.is_empty() {
        0.0
    } else {
        100.0 * voiced as f64 / result.analysis.frames.len() as f64
    };
    println!(
        "{} notes, {:.1}% voiced frames",
        result.analysis.notes.len(),
        ratio
    );
    Ok(())
}

fn vocal_apply(
    input: &std::path::Path,
    selector: &str,
    output: Option<PathBuf>,
    in_place: bool,
    dry_run: bool,
    json: bool,
    sample_rate: u32,
) -> Result<()> {
    if !in_place && !dry_run && output.is_none() {
        anyhow::bail!("choose --output <FILE>, --in-place or --dry-run");
    }
    let source = std::fs::read_to_string(input)?;
    let project =
        oc_dsl::parse(&source).map_err(|error| anyhow::anyhow!("Parse error: {error}"))?;
    oc_dsl::validate(&project)
        .map_err(|errors| anyhow::anyhow!("Validation failed: {errors:?}"))?;
    let target = if dry_run {
        None
    } else if in_place {
        Some(input.to_path_buf())
    } else {
        output.clone()
    };
    let applied =
        oc_engine::vocal_tune::apply(&project, input, selector, sample_rate, target.as_deref())
            .map_err(anyhow::Error::msg)?;
    if json {
        let rate = f64::from(applied.resolved.sample_rate);
        let timing_for = |note: &oc_engine::vocal_tune::ResolvedNote| {
            let index = note.detected_index?;
            let entry = applied
                .resolved
                .timing
                .iter()
                .find(|entry| entry.detected_index == index)?;
            let measured = entry.measured;
            Some(serde_json::json!({
                "requested_move_ms": entry.move_seconds * 1000.0,
                "applied_move_ms": (entry.target.0 as f64 - entry.source.0 as f64) / rate * 1000.0,
                "requested_length_ms": entry.requested_length_s * 1000.0,
                "applied_length_ms": (entry.target.1 - entry.target.0) as f64 / rate * 1000.0,
                "onset_residual_ms": measured.map(|value| value.onset_residual_ms),
                "length_error_ms": measured.map(|value| value.length_error_ms),
                "audible_length_ms": measured.map(|value| value.audible_length_s * 1000.0),
                "audible_length_error_ms": measured.map(|value| value.audible_length_error_ms),
                "audible_onset_seconds": measured.map(|value| value.audible_start_s),
            }))
        };
        let note_json = |note: &oc_engine::vocal_tune::ResolvedNote| {
            serde_json::json!({
                "anchor_seconds": note.anchor_s,
                "detected_start_seconds": note.detected_start_s,
                "detected_hz": note.detected_hz,
                "detected_confidence": note.detected_confidence,
                "target_cents": note.target_cents,
                "amount_percent": note.amount,
                "shift_cents": note.shift_cents,
                "resulting_hz": note.resulting_hz,
                "outcome": note.outcome.as_str(),
                "vibrato": note.vibrato.as_ref().map(|edit| serde_json::json!({
                    "mode": edit.mode.as_str(),
                    "detected_depth_cents": edit.depth_cents,
                    "points": edit.cents.len(),
                })),
                "warnings": note.warnings,
                "timing": timing_for(note),
            })
        };
        let notes: Vec<serde_json::Value> = applied.resolved.notes.iter().map(note_json).collect();
        let proposals: Vec<serde_json::Value> =
            applied.resolved.proposals.iter().map(note_json).collect();
        let baked = applied.baked.as_ref().map(|baked| {
            serde_json::json!({
                "path": baked.path.display().to_string(),
                "file": baked.file,
                "frames": baked.frames,
                "sample_rate": baked.sample_rate,
                "cache": if baked.from_cache { "hit" } else { "miss" },
            })
        });
        let shift_report = applied.report.map(|report| {
            serde_json::json!({
                "frames": report.frames,
                "shifted_frames": report.shifted_frames,
                "wet_frames": report.wet_frames,
                "voiced_frames": report.voiced_frames,
                "boundary_step_ratio": report.boundary_step_ratio,
                "peak": report.peak,
            })
        });
        let time_report = applied.time.map(|report| {
            serde_json::json!({
                "frames": report.frames,
                "notes": report.notes,
                "edited_frames": report.edited_frames,
                "room_frames": report.room_frames,
                "boundary_step_ratio": report.boundary_step_ratio,
                "peak": report.peak,
            })
        });
        let report = serde_json::json!({
            "project_out": applied.project_out.as_ref().map(|path| path.display().to_string()),
            "clip": applied.resolved.clip,
            "track": applied.resolved.track,
            "tune": applied.resolved.tune,
            "sample_rate": applied.resolved.sample_rate,
            "analyzer_version": oc_engine::vocal_analysis::ANALYZER_VERSION,
            "bake_version": oc_engine::vocal_tune::version_stamp(),
            "analysis_cache": if applied.resolved.analysis_from_cache { "hit" } else { "miss" },
            "pass_through": applied.resolved.pass_through(),
            "shifted_notes": applied.resolved.shifted_notes(),
            "max_shift_cents": applied.resolved.max_shift_cents(),
            "unoverridden_notes": applied.resolved.unoverridden_notes,
            "snap_policy": applied.resolved.snap.as_str(),
            "reference": applied.resolved.reference,
            "reference_notes": applied.resolved.reference_notes,
            "reference_fallbacks": applied.resolved.reference_fallbacks,
            "notes": notes,
            "proposals": proposals,
            "warnings": applied.resolved.warnings,
            "baked": baked,
            "shift": shift_report,
            "time": time_report,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "clip '{}' (track '{}'), tune '{}': {} at {} Hz",
        applied.resolved.clip,
        applied.resolved.track,
        applied.resolved.tune,
        if applied.resolved.analysis_from_cache {
            "cached analysis"
        } else {
            "fresh analysis"
        },
        applied.resolved.sample_rate
    );
    println!(
        "  detector {} / bake v{} / snap {}",
        oc_engine::vocal_analysis::ANALYZER_VERSION,
        oc_engine::vocal_tune::version_stamp(),
        applied.resolved.snap.as_str()
    );
    let print_note = |index: usize, note: &oc_engine::vocal_tune::ResolvedNote| {
        let detected = match (note.detected_hz, note.detected_start_s) {
            (Some(hz), Some(start)) => format!("{hz:>7.2} Hz @{start:.3}s"),
            _ => "        -".to_string(),
        };
        let confidence = note
            .detected_confidence
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".to_string());
        let target = note
            .target_cents
            .map(|cents| format!("{cents:.1}c"))
            .unwrap_or_else(|| "-".to_string());
        let shift = note
            .shift_cents
            .map(|cents| format!("{cents:+.1}c"))
            .unwrap_or_else(|| "-".to_string());
        let result = note
            .resulting_hz
            .map(|hz| format!("{hz:.2} Hz"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:>3}  {:>7.3}  {detected}  {confidence:>5}  {target:>9}  {:>6.0}%  {shift:>8}  {result:>9}  {}",
            index + 1,
            note.anchor_s,
            note.amount,
            note.outcome.as_str()
        );
    };
    println!("  #  anchor   detected    conf   target     amount     shift    result     status");
    for (index, note) in applied.resolved.notes.iter().enumerate() {
        print_note(index, note);
    }
    if !applied.resolved.proposals.is_empty() {
        println!("  automatic proposals:");
        for (index, note) in applied.resolved.proposals.iter().enumerate() {
            print_note(index, note);
        }
    }
    if !applied.resolved.timing.is_empty() {
        println!("  timing edits:");
        println!("  #  requested  applied   residual  length     span err  audible err");
        for timing in &applied.resolved.timing {
            let measured = timing.measured;
            println!(
                "{:>3}  {:+7.1}ms  {:+7.1}ms  {:>7}  {:>7.1}ms  {:>7}  {:>7}",
                timing.detected_index + 1,
                timing.move_seconds * 1000.0,
                (timing.target.0 as f64 - timing.source.0 as f64)
                    / f64::from(applied.resolved.sample_rate)
                    * 1000.0,
                measured.map_or_else(
                    || "-".to_string(),
                    |value| format!("{:+.1}ms", value.onset_residual_ms)
                ),
                timing.requested_length_s * 1000.0,
                measured.map_or_else(
                    || "-".to_string(),
                    |value| format!("{:+.1}ms", value.length_error_ms)
                ),
                measured.map_or_else(
                    || "-".to_string(),
                    |value| format!("{:+.1}ms", value.audible_length_error_ms)
                ),
            );
        }
        if let Some(time) = &applied.time {
            println!(
                "  time pass: {} note(s), {} room-filled frames, seam step {:.2}x the input",
                time.notes, time.room_frames, time.boundary_step_ratio
            );
        }
    }
    println!(
        "{} stored override(s), {} proposal(s), {} untouched detected note(s): {} need a shift, {} timing edit(s)",
        applied.resolved.notes.len(),
        applied.resolved.proposals.len(),
        applied.resolved.unoverridden_notes,
        applied.resolved.shifted_notes(),
        applied.resolved.timing_notes()
    );
    for warning in &applied.resolved.warnings {
        eprintln!("warning: {warning}");
    }
    for note in &applied.resolved.notes {
        for warning in &note.warnings {
            eprintln!("warning: anchor {:.3}s: {warning}", note.anchor_s);
        }
    }
    match &applied.baked {
        Some(baked) => println!(
            "baked: {} ({} frames, cache {})",
            baked.path.display(),
            baked.frames,
            if baked.from_cache { "hit" } else { "miss" }
        ),
        None => println!("dry run: no audio or project written"),
    }
    if let Some(shift) = &applied.report {
        println!(
            "  shifted {:.0}% of the window, boundary step {:.2}x the input",
            100.0 * shift.wet_frames as f64 / shift.frames.max(1) as f64,
            shift.boundary_step_ratio
        );
    }
    if let Some(path) = &applied.project_out {
        println!("wrote: {}", path.display());
    }
    Ok(())
}

/// Bake the track leveler's decision into its gain lane.
fn level_bake(
    input: &std::path::Path,
    track: &str,
    write: bool,
    output: Option<PathBuf>,
    json: bool,
    sample_rate: u32,
    soundfont: Option<PathBuf>,
) -> Result<()> {
    let source = std::fs::read_to_string(input)?;
    let mut project =
        oc_dsl::parse(&source).map_err(|error| anyhow::anyhow!("Parse error: {error}"))?;
    oc_dsl::validate(&project)
        .map_err(|errors| anyhow::anyhow!("Validation failed: {errors:?}"))?;
    let mut plugins = load_project_clap_instances(&project, sample_rate)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let soundfont = soundfont.unwrap_or_default();
    let bake = oc_engine::leveler_bake::bake(
        &mut project,
        track,
        sample_rate,
        input.parent(),
        &soundfont,
        &mut plugins,
    )
    .map_err(anyhow::Error::msg)?;
    // The lane reaches a file only after the whole document still validates.
    oc_dsl::validate(&project)
        .map_err(|errors| anyhow::anyhow!("Baked project is invalid: {errors:?}"))?;
    let target = if write {
        Some(input.to_path_buf())
    } else {
        output.clone()
    };
    if let Some(path) = &target {
        std::fs::write(path, oc_dsl::serialize(&project))?;
    }
    if json {
        let report = serde_json::json!({
            "track": bake.track,
            "sample_rate": bake.sample_rate,
            "control_period_ms": bake.control_period_seconds * 1000.0,
            "control_ticks": bake.control_ticks,
            "points_before_thinning": bake.points_before_thinning,
            "points": bake.points,
            "start_seconds": bake.start_seconds,
            "end_seconds": bake.end_seconds,
            "min_gain_db": bake.min_gain_db,
            "max_gain_db": bake.max_gain_db,
            "params": bake.params,
            "project_out": target.as_ref().map(|path| path.display().to_string()),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "track '{}': baked the leveler into gain automation at {} Hz",
        bake.track, bake.sample_rate
    );
    let params: Vec<String> = bake
        .params
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    println!("  law: {}", params.join(", "));
    println!(
        "  control {:.1} ms, {} tick(s) -> {} point(s), thinned from {}",
        bake.control_period_seconds * 1000.0,
        bake.control_ticks,
        bake.points,
        bake.points_before_thinning
    );
    println!(
        "  gain {:+.2}..{:+.2} dB over {:.3}..{:.3} s",
        bake.min_gain_db, bake.max_gain_db, bake.start_seconds, bake.end_seconds
    );
    match &target {
        Some(path) => println!("wrote: {}", path.display()),
        None => println!("dry run: no project written (pass --write or --output)"),
    }
    Ok(())
}

fn align_clips(
    input: &std::path::Path,
    reference: &str,
    takes: &[String],
    sample_rate: u32,
    dry_run: bool,
    invert: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    if takes.is_empty() {
        anyhow::bail!("at least one take is required");
    }
    let source = std::fs::read_to_string(input)?;
    let mut project =
        oc_dsl::parse(&source).map_err(|error| anyhow::anyhow!("Parse error: {error}"))?;
    oc_dsl::validate(&project)
        .map_err(|errors| anyhow::anyhow!("Validation failed: {errors:?}"))?;
    let clips = project
        .audio_clips()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let find = |name: &str| {
        clips
            .iter()
            .position(|clip| clip.name == name)
            .ok_or_else(|| anyhow::anyhow!("audio clip '{name}' not found"))
    };
    let reference_index = find(reference)?;
    let mut take_indices = Vec::with_capacity(takes.len());
    for take in takes {
        let index = find(take)?;
        if index == reference_index {
            anyhow::bail!("take '{take}' is the reference clip");
        }
        if take_indices.contains(&index) {
            anyhow::bail!("take '{take}' is listed twice");
        }
        take_indices.push(index);
    }
    let root = input.parent();
    let reference_clip = &clips[reference_index];
    let (reference_start, reference_length) =
        oc_engine::audio_clip::range_frames(&project, reference_clip, sample_rate)
            .map_err(|error| anyhow::anyhow!("reference '{}': {error}", reference_clip.name))?;
    let reference_path = oc_engine::audio_clip::resolve_path(&reference_clip.file, root)
        .map_err(|error| anyhow::anyhow!("reference '{}': {error}", reference_clip.name))?;
    let reference_media = oc_engine::media_cache::shared()
        .resampled(
            &reference_path,
            sample_rate,
            oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "reference '{}': cannot decode {}: {error}",
                reference_clip.name,
                reference_path.display()
            )
        })?;
    let reference_frames = (reference_media.len() / 2).min(reference_length as usize);
    if reference_frames == 0 {
        anyhow::bail!("reference '{}': range is empty", reference_clip.name);
    }
    let reference_seconds = reference_start as f64 / f64::from(sample_rate);

    let mut plans = Vec::with_capacity(take_indices.len());
    for index in take_indices {
        let clip = &clips[index];
        let path = oc_engine::audio_clip::resolve_path(&clip.file, root)
            .map_err(|error| anyhow::anyhow!("take '{}': {error}", clip.name))?;
        let media = oc_engine::media_cache::shared()
            .resampled(
                &path,
                sample_rate,
                oc_engine::resample::ResampleQuality::DEFAULT_RENDER,
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "take '{}': cannot decode {}: {error}",
                    clip.name,
                    path.display()
                )
            })?;
        let (start, length) = oc_engine::audio_clip::range_frames(&project, clip, sample_rate)
            .map_err(|error| anyhow::anyhow!("take '{}': {error}", clip.name))?;
        let frames = (media.len() / 2).min(length as usize);
        if frames == 0 {
            anyhow::bail!("take '{}': range is empty", clip.name);
        }
        let alignment = oc_engine::align::analyze(
            &reference_media[..reference_frames * 2],
            &media[..frames * 2],
            sample_rate,
        )
        .map_err(|error| anyhow::anyhow!("take '{}': {error}", clip.name))?;
        let correction = oc_engine::align::correction(
            &alignment,
            reference_seconds,
            start as f64 / f64::from(sample_rate),
            sample_rate,
        )
        .map_err(|error| anyhow::anyhow!("take '{}': {error}", clip.name))?;
        let media_seconds = frames as f64 / f64::from(sample_rate);
        let range_seconds = length as f64 / f64::from(sample_rate);
        if correction.offset_seconds >= media_seconds || correction.offset_seconds >= range_seconds
        {
            anyhow::bail!(
                "take '{}': alignment trims {:.1} ms, the whole clip",
                clip.name,
                correction.offset_seconds * 1000.0
            );
        }
        plans.push((index, alignment, correction));
    }

    for (index, alignment, correction) in &plans {
        let clip = project.audio_clip_mut(*index).unwrap();
        oc_engine::align::write_into(clip, correction)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let was_inverted = clip.invert;
        if invert {
            clip.invert = alignment.is_inverted();
        }
        let polarity_changed = clip.invert != was_inverted;
        println!(
            "  take '{}': {} of {} onsets matched, tempo {:.4}, stretch {:.4}, head trim {:.1} ms, residual {:.1} ms",
            clip.name,
            alignment.matched,
            alignment.reference_onsets,
            alignment.tempo_ratio,
            correction.ratio,
            correction.offset_seconds * 1000.0,
            alignment.max_residual_seconds * 1000.0,
        );
        println!(
            "    polarity: correlation {:+.3}, {}",
            alignment.correlation,
            if alignment.is_inverted() {
                "out of phase"
            } else {
                "in phase"
            }
        );
        if invert && alignment.is_inverted() {
            println!("    wrote invert: true");
        } else if polarity_changed {
            println!("    cleared invert");
        }
        if correction.is_identity() && !polarity_changed {
            println!("    already aligned; clip left untouched");
        }
        if !alignment.within_usable_tempo() {
            eprintln!(
                "warning: take '{}': tempo correction {:.1}% is outside the ±{:.0}% usable band",
                clip.name,
                (alignment.tempo_ratio - 1.0) * 100.0,
                oc_engine::align::USABLE_TEMPO_DRIFT * 100.0,
            );
        }
    }

    let formatted = oc_dsl::serialize(&project);
    if dry_run {
        println!(
            "Dry run: {} take(s) aligned; {} unchanged",
            plans.len(),
            input.display()
        );
        return Ok(());
    }
    let target = output.unwrap_or_else(|| input.to_path_buf());
    std::fs::write(&target, formatted)?;
    println!("Aligned {} take(s) in {}", plans.len(), target.display());
    Ok(())
}

fn devices_list_json() -> Result<String> {
    Ok(serde_json::to_string(&oc_engine::dsp::schema::catalog())?)
}

fn export_midi_file(
    input: &std::path::Path,
    out: &std::path::Path,
    format: u8,
    ppq: u16,
    tracks: Option<&str>,
    json_report: bool,
) -> Result<()> {
    let source = std::fs::read_to_string(input)?;
    let project =
        oc_dsl::parse(&source).map_err(|error| anyhow::anyhow!("Parse error: {error}"))?;
    oc_dsl::validate(&project)
        .map_err(|errors| anyhow::anyhow!("Validation failed: {errors:?}"))?;
    let scope = match tracks {
        Some(value) => parse_track_scope(value)?,
        None => oc_engine::midi::ExportScope::Project,
    };
    let options = oc_engine::midi::ExportOptions {
        format,
        ppq,
        scope,
        channels: Vec::new(),
    };
    let result = oc_engine::midi::export_smf(&project, &options)?;
    std::fs::write(out, &result.bytes)?;
    if json_report {
        println!("{}", serde_json::to_string_pretty(&result.report)?);
    } else {
        println!(
            "Exported {} track(s), {} note(s) → {} ({} bytes)",
            result.report.written.len(),
            result
                .report
                .written
                .iter()
                .map(|track| track.notes)
                .sum::<usize>(),
            out.display(),
            result.bytes.len(),
        );
        for report in &result.report.written {
            println!(
                "  [{}] {} ch{} — {} notes, {} automation events, program {}",
                report.smf_track,
                report.name,
                report.channel,
                report.notes,
                report.automation_events,
                report
                    .program
                    .map(|program| program.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
        }
        for loss in &result.report.losses {
            println!("  loss {:?}: {} ({})", loss.kind, loss.count, loss.detail);
        }
    }
    Ok(())
}

fn parse_track_scope(value: &str) -> Result<oc_engine::midi::ExportScope> {
    let range = value.split_once([',', '-']);
    let Some((start, end)) = range else {
        let index = value.parse::<usize>()?;
        return Ok(oc_engine::midi::ExportScope::Track(index));
    };
    let start = start.parse::<usize>()?;
    let end = end.parse::<usize>()?;
    if end < start {
        anyhow::bail!("Track range '{value}' ends before it starts");
    }
    Ok(oc_engine::midi::ExportScope::Tracks {
        start,
        end: end + 1,
    })
}

fn project_has_plugin_instrument(project: &oc_dsl::ast::Project) -> bool {
    project.presets.iter().any(|preset| {
        matches!(preset.source.as_deref(), Some("clap" | "vst3" | "vst2"))
            || (preset.source.as_deref() == Some("sample")
                && preset
                    .plugin
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case("sfizz")))
    })
}

fn filename_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "patch".into()
    } else {
        slug.to_string()
    }
}

fn print_legacy_effect_warnings(project: &oc_dsl::ast::Project) {
    for warning in oc_engine::dsp::legacy_warnings(project) {
        eprintln!("warning: {warning}");
    }
}

fn builtin_effect_project(project: &oc_dsl::ast::Project) -> Result<oc_dsl::ast::Project, String> {
    fn keep_builtin(effects: &mut Vec<oc_dsl::ast::Effect>) {
        effects.retain(|effect| !matches!(effect.effect_type, oc_dsl::ast::EffectType::Plugin(_)));
    }
    let mut project = oc_dsl::preset::resolve(project)?;
    for track in &mut project.tracks {
        keep_builtin(&mut track.effects);
    }
    for preset in &mut project.presets {
        keep_builtin(&mut preset.effects);
    }
    for bus in &mut project.buses {
        keep_builtin(&mut bus.effects);
    }
    if let Some(master) = &mut project.master {
        keep_builtin(&mut master.effects);
    }
    Ok(project)
}

/// Print peak, true peak, RMS, crest and integrated loudness of a rendered file.
///
/// The file has already been quantised, so a clipped mix reads 0.00 dBFS here;
/// the encoder's pre-clamp report names the level the mix actually reached.
fn print_level_report(
    path: &std::path::Path,
    report: Option<oc_engine::encode::EncodeReport>,
) -> anyhow::Result<()> {
    let buffer = oc_engine::analysis::read_wav(path).map_err(|e| anyhow::anyhow!("{}", e))?;
    let channels = buffer.channel_count().max(1);
    let frames = buffer.frames();
    if frames == 0 || channels == 0 {
        println!("  Level: empty file");
        return Ok(());
    }
    let mut interleaved = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let left = buffer.channels[0][frame] as f32;
        let right = buffer.channels.get(1).map_or(left, |c| c[frame] as f32);
        interleaved.push(left);
        interleaved.push(right);
    }
    let metrics = oc_engine::analysis::measure_stereo_interleaved(&interleaved, buffer.sample_rate);
    let true_peak = oc_engine::analysis::true_peak_dbfs(&interleaved);
    let loudness = metrics
        .integrated_lufs
        .map_or("—".to_string(), |lufs| format!("{lufs:.1} LUFS"));
    println!(
        "  Level: peak {:.2} dBFS, true peak {:.2} dBTP, RMS {:.2} dBFS, crest {:.1} dB, {loudness}",
        metrics.peak_dbfs, true_peak, metrics.rms_dbfs, metrics.crest_db
    );
    if let Some(report) = report {
        println!(
            "  Mix before quantisation: peak {:+.2} dBFS, true peak {:+.2} dBTP{}",
            report.pre_clamp_peak_dbfs,
            report.pre_clamp_true_peak_dbfs,
            if report.clipped {
                " (clipped at full scale in the written file)"
            } else if report.above_full_scale {
                " (peaks above full scale; the float file keeps them)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

/// Writes the counters collected during the run into the log when `main` ends.
struct ReportOnExit;

impl Drop for ReportOnExit {
    fn drop(&mut self) {
        oc_diag::log_report("exit");
    }
}

/// One `label  ms` line per render stage, in completion order.
fn stage_lines(stages: &[oc_engine::stage_log::StageTiming]) -> Vec<String> {
    stages
        .iter()
        .map(|stage| {
            let label = match stage.track {
                Some(track) => format!("{} #{track}", stage.label),
                None => stage.label.to_string(),
            };
            format!(
                "{label:<26} {:>8.1} ms",
                stage.elapsed.as_secs_f64() * 1000.0
            )
        })
        .collect()
}

/// Parse `--format`: a container this build can write.
fn parse_container(value: &str) -> Result<oc_dsl::ast::ExportContainer, String> {
    value.parse()
}

/// Parse `--sample-rate`: a rate the sampler can run.
fn parse_sample_rate(value: &str) -> Result<u32, String> {
    let rate: u32 = value.parse().map_err(|_| {
        format!(
            "'{value}' is not a whole number of hertz; enter a sample rate such as 44100 or 48000"
        )
    })?;
    oc_engine::encode::validate_sample_rate(rate)?;
    Ok(rate)
}

/// Position a `bar:beat:subdivision` flag names, in quarter notes.
fn bar_beat_quarters(clock: &oc_dsl::timing::ProjectClock, text: &str) -> Result<f64> {
    let position = parse_bar_beat(text)?;
    clock
        .quarter_notes_at(&position)
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// `bar:beat:subdivision` as the DSL writes it; beat defaults to 1 and
/// subdivision to 0, so `9` and `9:1` both mean the first beat of bar 9.
fn parse_bar_beat(text: &str) -> Result<oc_dsl::ast::BarBeat> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        anyhow::bail!("position '{text}' must be bar, bar:beat or bar:beat:subdivision, e.g. 9:1");
    }
    let mut numbers = [1_u32, 1_u32, 0_u32];
    for (index, part) in parts.iter().enumerate() {
        numbers[index] = part.trim().parse::<u32>().map_err(|_| {
            anyhow::anyhow!(
                "position '{text}' has a non-numeric part '{part}'; use bar:beat:subdivision, e.g. 9:1"
            )
        })?;
    }
    if numbers[0] == 0 || numbers[1] == 0 {
        anyhow::bail!("position '{text}' uses 0; bars and beats are 1-based");
    }
    if numbers[2] >= 4 {
        anyhow::bail!(
            "position '{text}' has subdivision {}; a beat holds subdivisions 0-3",
            numbers[2]
        );
    }
    Ok(oc_dsl::ast::BarBeat::new(
        numbers[0], numbers[1], numbers[2],
    ))
}

/// Rate one render runs at: the flag, else the project's export rate, else the
/// historical 44100 default.
fn effective_sample_rate(flag: Option<u32>, project: &oc_dsl::ast::Project) -> u32 {
    flag.or_else(|| {
        project
            .properties
            .export
            .and_then(|export| export.sample_rate)
    })
    .unwrap_or(44_100)
}

/// Export options given on the command line, each overriding one project value.
#[derive(Debug, Clone, Copy, Default)]
struct EncodeFlags {
    depth: Option<oc_engine::wav::SampleFormat>,
    flac_level: Option<u8>,
    mp3_bitrate: Option<u32>,
    /// VBR quality index, 0 best to 9 smallest.
    mp3_vbr: Option<u8>,
    dither: bool,
    no_dither: bool,
    /// `--report` re-reads the written file as WAV.
    report: bool,
}

/// Effective export options: flags win, then the project header, then the
/// historical 16-bit undithered WAV default.
fn resolve_encode_spec(
    project: &oc_dsl::ast::Project,
    container: oc_dsl::ast::ExportContainer,
    sample_rate: u32,
    flags: EncodeFlags,
) -> Result<oc_engine::encode::EncodeSpec, String> {
    let mut spec = oc_engine::encode::EncodeSpec::for_project(project, sample_rate);
    spec.container = container;
    if let Some(depth) = flags.depth {
        match container {
            oc_dsl::ast::ExportContainer::Wav => spec.wav.depth = depth,
            oc_dsl::ast::ExportContainer::Flac => spec.flac.depth = depth,
            oc_dsl::ast::ExportContainer::Mp3 => {
                return Err(format!(
                    "--sample-format is a WAV or FLAC option; MP3 has no sample depth because the encoder takes 16-bit PCM, so drop the depth (it was {depth})"
                ));
            }
        }
    }
    if let Some(level) = flags.flac_level {
        if container != oc_dsl::ast::ExportContainer::Flac {
            return Err(format!(
                "--flac-level is a FLAC option; --format is {container}. Add --format flac or drop the level"
            ));
        }
        spec.flac.level = level;
    }
    if (flags.mp3_bitrate.is_some() || flags.mp3_vbr.is_some())
        && container != oc_dsl::ast::ExportContainer::Mp3
    {
        let flag = if flags.mp3_bitrate.is_some() {
            "--mp3-bitrate"
        } else {
            "--mp3-vbr"
        };
        return Err(format!(
            "{flag} is an MP3 option; --format is {container}. Add --format mp3 or drop the flag"
        ));
    }
    if let Some(bitrate) = flags.mp3_bitrate {
        spec.mp3.bitrate = bitrate;
        spec.mp3.vbr_quality = None;
    }
    if let Some(quality) = flags.mp3_vbr {
        spec.mp3.vbr_quality = Some(quality);
    }
    if flags.dither && container != oc_dsl::ast::ExportContainer::Wav {
        return Err(match container {
            oc_dsl::ast::ExportContainer::Mp3 => {
                "dither is a WAV option: MP3 has no dither step, so drop --dither".into()
            }
            _ => "dither is a WAV option: FLAC is lossless, so dither would be stored, not removed"
                .into(),
        });
    }
    if flags.report && container == oc_dsl::ast::ExportContainer::Mp3 {
        return Err(
            "--report re-reads the written file as WAV; an MP3 export is lossy, so drop --report"
                .into(),
        );
    }
    if flags.dither {
        spec.wav.dither = true;
    } else if flags.no_dither {
        spec.wav.dither = false;
    }
    spec.validate()?;
    Ok(spec)
}

/// One line naming the MP3 rate control the file will use.
fn describe_mp3_rate(spec: &oc_engine::encode::EncodeSpec) -> String {
    match spec.mp3.vbr_quality {
        Some(quality) => format!("VBR quality {quality}"),
        None => format!("{} kbps CBR", spec.mp3.bitrate),
    }
}

/// Resolve `--output` against the container: default name, or a refused mismatch.
fn resolve_output_path(
    output: Option<std::path::PathBuf>,
    container: oc_dsl::ast::ExportContainer,
) -> Result<std::path::PathBuf, String> {
    let extension = container.extension();
    let Some(path) = output else {
        return Ok(std::path::PathBuf::from(format!("output.{extension}")));
    };
    match path.extension().and_then(|value| value.to_str()) {
        None => {
            let mut path = path;
            path.set_extension(extension);
            Ok(path)
        }
        Some(found) if found.eq_ignore_ascii_case(extension) => Ok(path),
        Some(found) => Err(format!(
            "output '{}' has the .{found} extension but this export writes .{extension}",
            path.display()
        )),
    }
}

/// Print the offline render stage table when `OCP_RENDER_STAGES` is set.
fn print_stage_table(stages: &[oc_engine::stage_log::StageTiming]) {
    if std::env::var_os("OCP_RENDER_STAGES").is_none() {
        return;
    }
    println!("  stage timings:");
    for line in stage_lines(stages) {
        println!("    {line}");
    }
}

/// Mirror the render stages into the diagnostics log under `--perf`.
fn log_stage_table(stages: &[oc_engine::stage_log::StageTiming]) {
    for line in stage_lines(stages) {
        oc_diag::log::info!("render stage {line}");
    }
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn load_project_clap_instances(
    project: &oc_dsl::ast::Project,
    sample_rate: u32,
) -> Result<std::collections::HashMap<String, oc_host::clap_instance::ClapInstance>, String> {
    use oc_host::{PluginFormat, find_plugin_info};

    let resolved = oc_dsl::preset::resolve(project)?;
    let project = &resolved;

    let mut instances = std::collections::HashMap::new();
    for preset in project.presets.iter().filter(|preset| {
        preset.source.as_deref() == Some("clap")
            && preset
                .plugin
                .as_deref()
                .map(|id| !id.starts_with("builtin."))
                .unwrap_or(false)
    }) {
        let plugin_id = preset.plugin.as_deref().unwrap();
        let info = find_plugin_info(plugin_id, PluginFormat::Clap).ok_or_else(|| {
            format!(
                "CLAP plugin '{}' for preset '{}' was not found",
                plugin_id, preset.name
            )
        })?;
        let instance = oc_host::clap_instance::ClapInstance::load(
            std::path::Path::new(&info.path),
            &info.id,
            sample_rate,
            512,
        )?;
        instances.insert(preset.name.clone(), instance);
    }
    Ok(instances)
}

/// One MIDI source and the distinct `.ocp` path it converts to.
struct BatchJob {
    source: PathBuf,
    target: PathBuf,
}

/// What a batch run did, with the paths that need the operator's attention.
#[derive(Default)]
struct BatchOutcome {
    total: usize,
    converted: usize,
    skipped: usize,
    errors: usize,
    problems: Vec<String>,
}

/// The `.ocp` path a source maps to.
///
/// `flatten` prefixes the parent directory name, so same-named files from
/// different subdirectories keep distinct targets. A stem that is not valid
/// UTF-8 becomes a lossy name: naming must never abort a batch.
fn batch_target(
    source: &std::path::Path,
    midi_dir: &std::path::Path,
    output: &std::path::Path,
    flatten: bool,
) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = if flatten {
        source
            .parent()
            .filter(|parent| parent.as_os_str() != midi_dir.as_os_str())
            .and_then(std::path::Path::file_name)
            .map(|dir| dir.to_string_lossy().into_owned())
    } else {
        None
    };
    match prefix {
        Some(dir) => output.join(format!("{dir}_{stem}.ocp")),
        None => output.join(format!("{stem}.ocp")),
    }
}

fn collect_midi_files(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_midi_files(&path, files);
            } else if let Some(ext) = path.extension()
                && (ext == "mid" || ext == "midi")
            {
                files.push(path);
            }
        }
    }
}

/// Assign every source a target and drop the sources whose target is already
/// claimed. Sources are sorted first, so the same input keeps the target on
/// every run; the other claimants are reported instead of overwriting it.
fn plan_batch(
    midi_dir: &std::path::Path,
    output: &std::path::Path,
    flatten: bool,
    mut sources: Vec<PathBuf>,
) -> (Vec<BatchJob>, Vec<String>) {
    sources.sort();
    let mut claimed: std::collections::HashMap<PathBuf, PathBuf> = std::collections::HashMap::new();
    let mut jobs = Vec::with_capacity(sources.len());
    let mut problems = Vec::new();
    for source in sources {
        let target = batch_target(&source, midi_dir, output, flatten);
        match claimed.get(&target) {
            Some(first) => problems.push(format!(
                "duplicate target {}: {} and {}",
                target.display(),
                first.display(),
                source.display()
            )),
            None => {
                claimed.insert(target.clone(), source.clone());
                jobs.push(BatchJob { source, target });
            }
        }
    }
    (jobs, problems)
}

enum BatchFailure {
    /// The target already exists; the batch counts it as skipped.
    Skipped,
    /// The input did not convert or the output did not land; the text names
    /// the path that failed.
    Failed(String),
}

/// Convert one MIDI file and publish it.
///
/// `exists` is only a cheap precheck that keeps a rerun from parsing every
/// input again; `create_new` below is what decides, so a target that appears in
/// between is skipped rather than overwritten.
fn convert_batch_job(job: &BatchJob) -> Result<(), BatchFailure> {
    if job.target.exists() {
        return Err(BatchFailure::Skipped);
    }
    let project = oc_data::midi2dsl::midi_to_project(&job.source)
        .map_err(|error| BatchFailure::Failed(format!("{}: {error}", job.source.display())))?;
    let text = oc_dsl::serialize(&project);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&job.target)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(BatchFailure::Skipped);
        }
        Err(error) => {
            return Err(BatchFailure::Failed(format!(
                "{}: {error}",
                job.target.display()
            )));
        }
    };
    if let Err(error) = std::io::Write::write_all(&mut file, text.as_bytes()) {
        drop(file);
        // A failed write must not leave a truncated project behind.
        let _ = std::fs::remove_file(&job.target);
        return Err(BatchFailure::Failed(format!(
            "{}: {error}",
            job.target.display()
        )));
    }
    Ok(())
}

fn batch_midi2ocp(
    midi_dir: &std::path::Path,
    output: &std::path::Path,
    flatten: bool,
) -> Result<BatchOutcome> {
    std::fs::create_dir_all(output)?;
    let mut sources = Vec::new();
    collect_midi_files(midi_dir, &mut sources);
    let total = sources.len();
    println!("Found {} MIDI files in {}", total, midi_dir.display());

    let (jobs, duplicate_problems) = plan_batch(midi_dir, output, flatten, sources);
    let converted = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let errors = AtomicUsize::new(duplicate_problems.len());
    let failures = std::sync::Mutex::new(Vec::new());
    let done = AtomicUsize::new(0);

    jobs.par_iter().for_each(|job| {
        match convert_batch_job(job) {
            Ok(()) => {
                converted.fetch_add(1, Ordering::Relaxed);
            }
            Err(BatchFailure::Skipped) => {
                skipped.fetch_add(1, Ordering::Relaxed);
            }
            Err(BatchFailure::Failed(problem)) => {
                errors.fetch_add(1, Ordering::Relaxed);
                failures.lock().unwrap().push(problem);
            }
        }

        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        if d.is_multiple_of(10000) {
            println!(
                "  [{}/{}] converted={}, skipped={}, errors={}",
                d,
                total,
                converted.load(Ordering::Relaxed),
                skipped.load(Ordering::Relaxed),
                errors.load(Ordering::Relaxed),
            );
        }
    });

    let mut problems = duplicate_problems;
    let mut failures = failures.into_inner().unwrap_or_default();
    failures.sort();
    problems.extend(failures);

    let outcome = BatchOutcome {
        total,
        converted: converted.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
        problems,
    };
    for problem in outcome.problems.iter().take(BATCH_PROBLEM_LIMIT) {
        println!("  {problem}");
    }
    if outcome.problems.len() > BATCH_PROBLEM_LIMIT {
        println!(
            "  ... and {} more",
            outcome.problems.len() - BATCH_PROBLEM_LIMIT
        );
    }
    println!(
        "Done! converted={}, skipped={}, errors={}, total={}",
        outcome.converted, outcome.skipped, outcome.errors, outcome.total,
    );
    Ok(outcome)
}

/// Run the batch and fail the command when any input did not convert.
fn run_batch_midi2ocp(
    midi_dir: &std::path::Path,
    output: &std::path::Path,
    flatten: bool,
) -> Result<()> {
    let outcome = batch_midi2ocp(midi_dir, output, flatten)?;
    if outcome.errors > 0 {
        anyhow::bail!("{} of {} MIDI inputs failed", outcome.errors, outcome.total);
    }
    Ok(())
}

/// How many problem paths a batch prints before it summarises the rest.
const BATCH_PROBLEM_LIMIT: usize = 8;

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::path::{Path, PathBuf};

    #[test]
    fn clip_align_command_is_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "clip",
            "align",
            "song.ocp",
            "ref",
            "take-a",
            "take-b",
            "--sample-rate",
            "44100",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Clip {
                command:
                    super::ClipCommand::Align {
                        project,
                        reference,
                        takes,
                        sample_rate,
                        dry_run,
                        output,
                        invert,
                    },
            } => {
                assert_eq!(project, std::path::PathBuf::from("song.ocp"));
                assert_eq!(reference, "ref");
                assert_eq!(takes, vec!["take-a".to_string(), "take-b".to_string()]);
                assert_eq!(sample_rate, 44_100);
                assert!(!dry_run);
                assert!(output.is_none());
                assert!(!invert);
            }
            _ => panic!("expected clip align"),
        }
    }

    #[test]
    fn vocal_analyze_command_is_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "vocal",
            "analyze",
            "song.ocp",
            "lead",
            "--json",
            "--output",
            "lead.ocn",
            "--sample-rate",
            "44100",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Vocal {
                command:
                    super::VocalCommand::Analyze {
                        project,
                        clip,
                        json,
                        output,
                        sample_rate,
                    },
            } => {
                assert_eq!(project, std::path::PathBuf::from("song.ocp"));
                assert_eq!(clip, "lead");
                assert!(json);
                assert_eq!(output, Some(std::path::PathBuf::from("lead.ocn")));
                assert_eq!(sample_rate, 44_100);
            }
            _ => panic!("expected vocal analyze"),
        }
    }

    #[test]
    fn level_bake_command_is_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "level",
            "song.ocp",
            "--track",
            "lead",
            "--output",
            "baked.ocp",
            "--sample-rate",
            "44100",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Level {
                project,
                track,
                write,
                output,
                json,
                sample_rate,
                soundfont,
            } => {
                assert_eq!(project, std::path::PathBuf::from("song.ocp"));
                assert_eq!(track, "lead");
                assert!(!write);
                assert_eq!(output, Some(std::path::PathBuf::from("baked.ocp")));
                assert!(!json);
                assert_eq!(sample_rate, 44_100);
                assert!(soundfont.is_none());
            }
            _ => panic!("expected level"),
        }
    }

    #[test]
    fn render_determinism_flags_are_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "render",
            "song.ocp",
            "--reruns",
            "3",
            "--require-identical",
            "--pre-roll",
            "2.5",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Render {
                reruns,
                require_identical,
                pre_roll,
                ..
            } => {
                assert_eq!(reruns, Some(3));
                assert!(require_identical);
                assert_eq!(pre_roll, 2.5);
            }
            _ => panic!("expected render"),
        }
    }

    #[test]
    fn render_stems_and_loudness_flags_are_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "render",
            "song.ocp",
            "--stems",
            "stems-out",
            "--no-loudness-normalisation",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Render {
                stems,
                no_loudness_normalisation,
                ..
            } => {
                assert_eq!(stems, Some(std::path::PathBuf::from("stems-out")));
                assert!(no_loudness_normalisation);
            }
            _ => panic!("expected render"),
        }
    }

    #[test]
    fn bounce_command_is_wired() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "bounce",
            "song.ocp",
            "--track",
            "lead",
            "--from",
            "9:1",
            "--to",
            "17:1",
            "--output",
            "/tmp/bounce.wav",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Bounce {
                project,
                track,
                from,
                to,
                output,
                soundfont,
                sample_rate,
            } => {
                assert_eq!(project, std::path::PathBuf::from("song.ocp"));
                assert_eq!(track, "lead");
                assert_eq!(from, "9:1");
                assert_eq!(to, "17:1");
                assert_eq!(output, std::path::PathBuf::from("/tmp/bounce.wav"));
                assert!(soundfont.is_none());
                assert!(sample_rate.is_none());
            }
            _ => panic!("expected bounce"),
        }
    }

    #[test]
    fn bounce_positions_parse_bar_beat_subdivision() {
        let clock =
            oc_dsl::timing::ProjectClock::new(&oc_dsl::ast::ProjectProperties::default()).unwrap();
        assert_eq!(super::bar_beat_quarters(&clock, "9").unwrap(), 32.0);
        assert_eq!(super::bar_beat_quarters(&clock, "9:1").unwrap(), 32.0);
        assert_eq!(super::bar_beat_quarters(&clock, "9:2").unwrap(), 33.0);
        assert_eq!(super::bar_beat_quarters(&clock, "9:1:2").unwrap(), 32.5);
        assert!(super::bar_beat_quarters(&clock, "0:1").is_err());
        assert!(super::bar_beat_quarters(&clock, "9:1:4").is_err());
        assert!(super::bar_beat_quarters(&clock, "9:x").is_err());
    }

    #[test]
    fn render_defaults_to_a_single_run() {
        let cli = super::Cli::try_parse_from(["ocp", "render", "song.ocp"]).unwrap();
        match cli.command {
            super::Commands::Render {
                reruns,
                require_identical,
                pre_roll,
                ..
            } => {
                assert_eq!(reruns, None);
                assert!(!require_identical);
                assert_eq!(pre_roll, 4.0);
            }
            _ => panic!("expected render"),
        }
    }

    #[test]
    fn render_export_format_flags_are_wired_and_dither_is_a_switch() {
        let cli = super::Cli::try_parse_from([
            "ocp",
            "render",
            "song.ocp",
            "--sample-format",
            "i24",
            "--dither",
        ])
        .unwrap();
        match cli.command {
            super::Commands::Render {
                sample_format,
                dither,
                no_dither,
                ..
            } => {
                assert_eq!(sample_format, Some(oc_engine::wav::SampleFormat::I24));
                assert!(dither);
                assert!(!no_dither);
            }
            _ => panic!("expected render"),
        }
        let cli = super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--no-dither"]).unwrap();
        match cli.command {
            super::Commands::Render {
                dither, no_dither, ..
            } => {
                assert!(!dither);
                assert!(no_dither);
            }
            _ => panic!("expected render"),
        }
        assert!(
            super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--dither", "--no-dither"])
                .is_err(),
            "--dither and --no-dither must conflict"
        );
        assert!(
            super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--sample-format", "i20"])
                .is_err(),
            "an unknown sample format must be rejected"
        );
    }

    #[test]
    fn export_options_rank_flags_above_the_project_and_refuse_float_dither() {
        let project =
            oc_dsl::parse("@project\n  tempo: 120\n  export: {format: i24, dither: true}\n")
                .unwrap();
        let project_options = super::resolve_encode_spec(
            &project,
            oc_dsl::ast::ExportContainer::Wav,
            48_000,
            super::EncodeFlags::default(),
        )
        .unwrap();
        assert_eq!(project_options.wav.depth, oc_engine::wav::SampleFormat::I24);
        assert!(project_options.wav.dither);
        assert_eq!(project_options.sample_rate, 48_000);

        let flags = super::resolve_encode_spec(
            &project,
            oc_dsl::ast::ExportContainer::Wav,
            48_000,
            super::EncodeFlags {
                depth: Some(oc_engine::wav::SampleFormat::F32),
                no_dither: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(flags.wav.depth, oc_engine::wav::SampleFormat::F32);
        assert!(!flags.wav.dither);

        let defaults = super::resolve_encode_spec(
            &oc_dsl::parse("@project\n  tempo: 120\n").unwrap(),
            oc_dsl::ast::ExportContainer::Wav,
            44_100,
            super::EncodeFlags::default(),
        )
        .unwrap();
        assert_eq!(defaults.wav.depth, oc_engine::wav::SampleFormat::I16);
        assert!(!defaults.wav.dither);

        let refused = super::resolve_encode_spec(
            &project,
            oc_dsl::ast::ExportContainer::Wav,
            48_000,
            super::EncodeFlags {
                depth: Some(oc_engine::wav::SampleFormat::F32),
                dither: true,
                ..Default::default()
            },
        )
        .expect_err("float dither must be refused");
        assert!(refused.contains("meaningless for f32"), "{refused}");
    }

    #[test]
    fn the_container_flag_accepts_every_container_and_names_what_it_cannot_write() {
        let cli = super::Cli::try_parse_from(["ocp", "render", "song.ocp"]).unwrap();
        match cli.command {
            super::Commands::Render { format, .. } => {
                assert_eq!(
                    format, None,
                    "the project header decides when no flag is given"
                );
            }
            _ => panic!("expected render"),
        }

        for (flag, expected) in [
            ("wav", oc_dsl::ast::ExportContainer::Wav),
            ("flac", oc_dsl::ast::ExportContainer::Flac),
            ("mp3", oc_dsl::ast::ExportContainer::Mp3),
        ] {
            let cli = super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--format", flag])
                .unwrap();
            match cli.command {
                super::Commands::Render { format, .. } => {
                    assert_eq!(format, Some(expected), "{flag}");
                }
                _ => panic!("expected render"),
            }
        }

        let error = super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--format", "ogg"])
            .err()
            .expect("an unknown container must be refused");
        let text = error.to_string();
        assert!(text.contains("unknown export container 'ogg'"), "{text}");
        assert!(text.contains("wav, flac, mp3"), "{text}");
    }

    #[test]
    fn the_output_extension_follows_the_container() {
        use oc_dsl::ast::ExportContainer;
        assert_eq!(
            super::resolve_output_path(None, ExportContainer::Wav).unwrap(),
            std::path::PathBuf::from("output.wav")
        );
        assert_eq!(
            super::resolve_output_path(None, ExportContainer::Flac).unwrap(),
            std::path::PathBuf::from("output.flac")
        );
        assert_eq!(
            super::resolve_output_path(Some("song".into()), ExportContainer::Flac).unwrap(),
            std::path::PathBuf::from("song.flac")
        );
        assert_eq!(
            super::resolve_output_path(None, ExportContainer::Mp3).unwrap(),
            std::path::PathBuf::from("output.mp3")
        );
        let error = super::resolve_output_path(Some("song.wav".into()), ExportContainer::Mp3)
            .expect_err("a WAV name cannot hold MP3 bytes");
        assert!(error.contains(".mp3"), "{error}");
        let error = super::resolve_output_path(Some("song.wav".into()), ExportContainer::Flac)
            .expect_err("a WAV name cannot hold FLAC bytes");
        assert!(error.contains(".wav"), "{error}");
        assert!(error.contains(".flac"), "{error}");
    }

    #[test]
    fn a_flac_spec_takes_the_flag_depth_and_level_and_refuses_wav_only_options() {
        use oc_dsl::ast::ExportContainer;
        let project = oc_dsl::parse(
            "@project\n  tempo: 120\n  export: {container: flac, flac: {depth: i16, level: 3}}\n",
        )
        .unwrap();
        let spec = super::resolve_encode_spec(
            &project,
            ExportContainer::Flac,
            48_000,
            super::EncodeFlags {
                depth: Some(oc_engine::wav::SampleFormat::I24),
                flac_level: Some(8),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(spec.flac.depth, oc_engine::wav::SampleFormat::I24);
        assert_eq!(spec.flac.level, 8);
        assert!(!spec.dither());

        let level = super::resolve_encode_spec(
            &project,
            ExportContainer::Wav,
            48_000,
            super::EncodeFlags {
                flac_level: Some(5),
                ..Default::default()
            },
        )
        .expect_err("--flac-level is a FLAC option");
        assert!(level.contains("--flac-level is a FLAC option"), "{level}");

        let dither = super::resolve_encode_spec(
            &project,
            ExportContainer::Flac,
            48_000,
            super::EncodeFlags {
                dither: true,
                ..Default::default()
            },
        )
        .expect_err("dither on FLAC must be refused");
        assert!(dither.contains("FLAC is lossless"), "{dither}");
    }

    #[test]
    fn an_mp3_spec_takes_the_bitrate_or_the_vbr_quality() {
        use oc_dsl::ast::ExportContainer;
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        let defaults = super::resolve_encode_spec(
            &project,
            ExportContainer::Mp3,
            48_000,
            super::EncodeFlags::default(),
        )
        .unwrap();
        assert_eq!(defaults.mp3.bitrate, 192, "192 kbps is the MP3 default");
        assert_eq!(defaults.mp3.vbr_quality, None);
        assert!(!defaults.dither());
        assert_eq!(super::describe_mp3_rate(&defaults), "192 kbps CBR");

        let flagged = super::resolve_encode_spec(
            &project,
            ExportContainer::Mp3,
            44_100,
            super::EncodeFlags {
                mp3_bitrate: Some(320),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(flagged.mp3.bitrate, 320);

        let vbr = super::resolve_encode_spec(
            &project,
            ExportContainer::Mp3,
            44_100,
            super::EncodeFlags {
                mp3_vbr: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(vbr.mp3.vbr_quality, Some(2));
        assert_eq!(super::describe_mp3_rate(&vbr), "VBR quality 2");

        // The project block fills in what the flags leave out.
        let declared = oc_dsl::parse(
            "@project\n  tempo: 120\n  export: {container: mp3, mp3: {bitrate: 160}}\n",
        )
        .unwrap();
        let from_project = super::resolve_encode_spec(
            &declared,
            ExportContainer::Mp3,
            48_000,
            super::EncodeFlags::default(),
        )
        .unwrap();
        assert_eq!(from_project.mp3.bitrate, 160);
    }

    #[test]
    fn mp3_only_flags_are_refused_for_another_format() {
        use oc_dsl::ast::ExportContainer;
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        for container in [ExportContainer::Wav, ExportContainer::Flac] {
            let error = super::resolve_encode_spec(
                &project,
                container,
                48_000,
                super::EncodeFlags {
                    mp3_bitrate: Some(192),
                    ..Default::default()
                },
            )
            .expect_err("--mp3-bitrate is an MP3 option");
            assert!(error.contains("--mp3-bitrate is an MP3 option"), "{error}");
            assert!(
                error.contains(&format!("--format is {container}")),
                "{error}"
            );

            let error = super::resolve_encode_spec(
                &project,
                container,
                48_000,
                super::EncodeFlags {
                    mp3_vbr: Some(2),
                    ..Default::default()
                },
            )
            .expect_err("--mp3-vbr is an MP3 option");
            assert!(error.contains("--mp3-vbr is an MP3 option"), "{error}");
        }

        // The two rate controls are mutually exclusive, so the CLI refuses both.
        assert!(
            super::Cli::try_parse_from([
                "ocp",
                "render",
                "song.ocp",
                "--format",
                "mp3",
                "--mp3-bitrate",
                "192",
                "--mp3-vbr",
                "2",
            ])
            .is_err(),
            "--mp3-bitrate and --mp3-vbr must conflict"
        );
        let cli = super::Cli::try_parse_from([
            "ocp",
            "render",
            "song.ocp",
            "--format",
            "mp3",
            "--mp3-vbr",
            "10",
        ]);
        assert!(cli.is_err(), "quality 10 is outside 0-9");
    }

    #[test]
    fn an_mp3_spec_refuses_an_unsupported_rate_and_names_the_allowed_set() {
        use oc_dsl::ast::ExportContainer;
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        let error = super::resolve_encode_spec(
            &project,
            ExportContainer::Mp3,
            96_000,
            super::EncodeFlags::default(),
        )
        .expect_err("96000 Hz has no MPEG Layer III form");
        assert!(error.contains("96000 Hz"), "{error}");
        assert!(
            error.contains("44100, 48000, 16000, 22050, 24000, 11025 or 12000"),
            "{error}"
        );

        // The bitrate table follows the MPEG version: 192 kbps has no MPEG-2 form.
        let error = super::resolve_encode_spec(
            &project,
            ExportContainer::Mp3,
            22_050,
            super::EncodeFlags::default(),
        )
        .expect_err("192 kbps is outside the MPEG-2 table");
        assert!(error.contains("8-160 kbps"), "{error}");

        for rate in [44_100u32, 48_000, 22_050, 24_000, 11_025, 12_000] {
            let bitrate = if rate >= 44_100 { 192 } else { 64 };
            super::resolve_encode_spec(
                &project,
                ExportContainer::Mp3,
                rate,
                super::EncodeFlags {
                    mp3_bitrate: Some(bitrate),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("{rate} Hz / {bitrate} kbps: {error}"));
        }
    }

    #[test]
    fn mp3_refuses_the_wav_only_options_with_a_one_line_reason() {
        use oc_dsl::ast::ExportContainer;
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        let cases = [
            (
                super::EncodeFlags {
                    depth: Some(oc_engine::wav::SampleFormat::I24),
                    ..Default::default()
                },
                "--sample-format",
            ),
            (
                super::EncodeFlags {
                    dither: true,
                    ..Default::default()
                },
                "no dither step",
            ),
            (
                super::EncodeFlags {
                    report: true,
                    ..Default::default()
                },
                "--report",
            ),
        ];
        for (flags, expected) in cases {
            let error = super::resolve_encode_spec(&project, ExportContainer::Mp3, 48_000, flags)
                .expect_err("MP3 refuses the WAV-only options");
            assert!(error.contains(expected), "{expected}: {error}");
            assert!(
                error.lines().count() == 1,
                "the reason must be one line: {error}"
            );
        }
    }

    #[test]
    fn the_sample_rate_flag_accepts_the_sampler_range_and_names_it() {
        for rate in ["8000", "44100", "48000", "192000"] {
            let cli =
                super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--sample-rate", rate])
                    .unwrap();
            match cli.command {
                super::Commands::Render { sample_rate, .. } => {
                    assert_eq!(sample_rate.expect("the flag was given").to_string(), rate);
                }
                _ => panic!("expected render"),
            }
        }

        for rate in ["0", "7999", "192001", "fast"] {
            let error =
                super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--sample-rate", rate])
                    .err()
                    .expect("an unusable sample rate must be refused");
            let text = error.to_string();
            assert!(
                text.contains("8000-192000") || text.contains("whole number of hertz"),
                "{rate}: {text}"
            );
        }
    }

    #[test]
    fn a_project_declared_export_rate_applies_unless_the_flag_names_one() {
        let plain = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        assert_eq!(super::effective_sample_rate(None, &plain), 44_100);
        assert_eq!(super::effective_sample_rate(Some(96_000), &plain), 96_000);

        let declared =
            oc_dsl::parse("@project\n  export: {container: wav, sample_rate: 48000}\n").unwrap();
        assert_eq!(super::effective_sample_rate(None, &declared), 48_000);
        assert_eq!(
            super::effective_sample_rate(Some(88_200), &declared),
            88_200,
            "the flag outranks the project, so a one-off render can differ"
        );
    }

    #[test]
    fn bit_depth_is_an_alias_for_sample_format() {
        let cli = super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--bit-depth", "i24"])
            .unwrap();
        match cli.command {
            super::Commands::Render { sample_format, .. } => {
                assert_eq!(sample_format, Some(oc_engine::wav::SampleFormat::I24));
            }
            _ => panic!("expected render"),
        }
        let cli =
            super::Cli::try_parse_from(["ocp", "render", "song.ocp", "--sample-format", "f32"])
                .unwrap();
        match cli.command {
            super::Commands::Render { sample_format, .. } => {
                assert_eq!(sample_format, Some(oc_engine::wav::SampleFormat::F32));
            }
            _ => panic!("expected render"),
        }
    }

    #[test]
    fn devices_list_command_is_wired() {
        let cli = super::Cli::try_parse_from(["ocp", "devices", "list", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            super::Commands::Devices {
                command: super::DevicesCommand::List { json: true }
            }
        ));
    }

    #[test]
    fn devices_list_json_matches_the_fixed_contract() {
        let value: serde_json::Value =
            serde_json::from_str(&super::devices_list_json().unwrap()).unwrap();
        let devices = value["devices"].as_array().unwrap();
        assert_eq!(devices.len(), oc_engine::dsp::schema::DEVICES.len());
        let mut device_ids = std::collections::HashSet::new();
        for device in devices {
            let object = device.as_object().unwrap();
            assert_eq!(object.len(), 5);
            for key in ["id", "name", "category", "params", "ports"] {
                assert!(object.contains_key(key), "device is missing {key}");
            }
            for port in device["ports"].as_array().unwrap() {
                let port = port.as_object().unwrap();
                assert_eq!(port.len(), 4);
                assert!(["audio", "control"].contains(&port["kind"].as_str().unwrap()));
                assert!(port["sums"].is_boolean());
            }
            assert!(
                device_ids.insert(device["id"].as_str().unwrap()),
                "duplicate device id"
            );
            assert!(
                ["eq", "dynamics", "time", "color", "utility"]
                    .contains(&device["category"].as_str().unwrap())
            );
            for param in device["params"].as_array().unwrap() {
                let object = param.as_object().unwrap();
                for key in [
                    "id", "name", "unit", "min", "max", "default", "curve", "steps",
                ] {
                    assert!(object.contains_key(key), "parameter is missing {key}");
                }
                // `kind`/`options` appear only for switched parameters.
                let kind = object.get("kind").and_then(|kind| kind.as_str());
                assert!(matches!(kind, None | Some("bool") | Some("enum")));
                if let Some(options) = object.get("options") {
                    assert_eq!(kind, Some("enum"), "options require an enum kind");
                    assert!(options.as_array().is_some_and(|all| !all.is_empty()));
                }
                // `description` is optional; the rest of the shape is fixed.
                let description = object.get("description");
                if let Some(text) = description {
                    assert!(text.as_str().is_some_and(|text| !text.is_empty()));
                }
                let expected = match kind {
                    None => 8,
                    Some("bool") => 9,
                    Some(_) => 10,
                };
                assert_eq!(
                    object.len(),
                    expected + description.is_some() as usize,
                    "unexpected parameter fields"
                );
                assert!(
                    oc_device_schema::ParamUnit::ALL
                        .iter()
                        .any(|unit| unit.as_str() == param["unit"].as_str().unwrap())
                );
                assert!(["linear", "log"].contains(&param["curve"].as_str().unwrap()));
                assert!(param["min"].is_number());
                assert!(param["max"].is_number());
                assert!(param["default"].is_number());
                // Switched parameters are stepped (bool 2, enum = option count).
                if kind.is_some() {
                    assert!(param["steps"].as_f64().is_some_and(|steps| steps >= 2.0));
                } else {
                    // Continuous parameters may still expose discrete positions.
                    assert!(
                        param["steps"].is_null()
                            || param["steps"].as_f64().is_some_and(|steps| steps >= 2.0)
                    );
                }
                let min = param["min"].as_f64().unwrap();
                let max = param["max"].as_f64().unwrap();
                let default = param["default"].as_f64().unwrap();
                assert!(min <= default && default <= max);
            }
        }
    }

    /// A batch fixture directory, removed when the test ends.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ocp-cli-batch-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A 26-byte type-0 MIDI file holding one empty track.
    const SILENT_MIDI: &[u8] = &[
        b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0x01, 0xE0, b'M', b'T', b'r', b'k', 0, 0,
        0, 4, 0x00, 0xFF, 0x2F, 0x00,
    ];

    fn write_midi(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, SILENT_MIDI).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn batch_reports_two_sources_that_share_a_target() {
        let scratch = ScratchDir::new("duplicate");
        let midi = scratch.join("midi");
        write_midi(&midi.join("a/song.mid"));
        write_midi(&midi.join("b/song.mid"));
        let output = scratch.join("out");

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!(
            (outcome.converted, outcome.skipped, outcome.errors),
            (1, 0, 1)
        );
        assert_eq!(outcome.problems.len(), 1, "{:?}", outcome.problems);
        let problem = &outcome.problems[0];
        assert!(problem.starts_with("duplicate target"), "{problem}");
        assert!(problem.contains("a/song.mid"), "{problem}");
        assert!(problem.contains("b/song.mid"), "{problem}");
        assert_eq!(names_in(&output), ["song.ocp"]);

        assert!(
            super::run_batch_midi2ocp(&midi, &output, false).is_err(),
            "a collision must fail the command"
        );
    }

    #[cfg(unix)]
    #[test]
    fn batch_counts_a_read_only_output_directory_as_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new("read-only");
        let midi = scratch.join("midi");
        write_midi(&midi.join("one.mid"));
        let output = scratch.join("out");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o555)).unwrap();

        // A process that may override permissions (root) has nothing to observe.
        if std::fs::write(output.join(".probe"), b"").is_ok() {
            std::fs::remove_file(output.join(".probe")).unwrap();
            return;
        }

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!(
            (outcome.converted, outcome.skipped, outcome.errors),
            (0, 0, 1)
        );
        assert!(
            outcome.problems[0].contains("one.ocp"),
            "{:?}",
            outcome.problems
        );
        assert!(
            names_in(&output).is_empty(),
            "a failed write must not leave a partial file"
        );
        assert!(
            super::run_batch_midi2ocp(&midi, &output, false).is_err(),
            "a write failure must fail the command"
        );

        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn batch_names_a_non_utf8_stem_lossily_without_panicking() {
        use std::ffi::{OsStr, OsString};
        use std::os::unix::ffi::OsStrExt;

        let source = PathBuf::from(OsStr::from_bytes(b"/tmp/caf\xe9.mid"));
        let target = super::batch_target(&source, Path::new("/tmp"), Path::new("/out"), false);
        assert_eq!(
            target.file_name().unwrap(),
            OsStr::new("caf\u{FFFD}.ocp"),
            "a non-UTF-8 stem must become a lossy name"
        );

        // Run the whole batch where the filesystem accepts such a name.
        let scratch = ScratchDir::new("non-utf8");
        let midi = scratch.join("midi");
        std::fs::create_dir_all(&midi).unwrap();
        if std::fs::write(midi.join(OsStr::from_bytes(b"caf\xe9.mid")), SILENT_MIDI).is_err() {
            return;
        }
        let output = scratch.join("out");
        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!((outcome.converted, outcome.errors), (1, 0));
        assert!(output.join(OsString::from("caf\u{FFFD}.ocp")).is_file());
    }

    #[test]
    fn batch_converts_every_input_of_a_normal_run() {
        let scratch = ScratchDir::new("normal");
        let midi = scratch.join("midi");
        write_midi(&midi.join("one.mid"));
        write_midi(&midi.join("two.midi"));
        write_midi(&midi.join("nested/three.mid"));
        let output = scratch.join("out");

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!(
            (outcome.converted, outcome.skipped, outcome.errors),
            (3, 0, 0)
        );
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(names_in(&output), ["one.ocp", "three.ocp", "two.ocp"]);
        super::run_batch_midi2ocp(&midi, &output, false).expect("no errors means success");

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!(
            (outcome.converted, outcome.skipped, outcome.errors),
            (0, 3, 0)
        );
    }

    #[test]
    fn batch_skips_an_existing_target_and_leaves_it_alone() {
        let scratch = ScratchDir::new("existing");
        let midi = scratch.join("midi");
        write_midi(&midi.join("one.mid"));
        let output = scratch.join("out");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("one.ocp"), "keep me").unwrap();

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!(
            (outcome.converted, outcome.skipped, outcome.errors),
            (0, 1, 0)
        );
        assert_eq!(
            std::fs::read_to_string(output.join("one.ocp")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn batch_flatten_keeps_same_stems_apart_and_detects_collisions() {
        let scratch = ScratchDir::new("flatten");
        let midi = scratch.join("midi");
        write_midi(&midi.join("a/song.mid"));
        write_midi(&midi.join("b/song.mid"));
        let output = scratch.join("out");

        let outcome = super::batch_midi2ocp(&midi, &output, true).unwrap();
        assert_eq!((outcome.converted, outcome.errors), (2, 0));
        assert_eq!(names_in(&output), ["a_song.ocp", "b_song.ocp"]);

        // Same directory and stem, different extension: one target, one error.
        let colliding = scratch.join("colliding/midi");
        write_midi(&colliding.join("a/song.mid"));
        write_midi(&colliding.join("a/song.midi"));
        let colliding_output = scratch.join("colliding/out");
        let outcome = super::batch_midi2ocp(&colliding, &colliding_output, true).unwrap();
        assert_eq!((outcome.converted, outcome.errors), (1, 1));
        assert_eq!(names_in(&colliding_output), ["a_song.ocp"]);
    }

    #[test]
    fn batch_counts_an_unreadable_input_and_fails_the_command() {
        let scratch = ScratchDir::new("corrupt");
        let midi = scratch.join("midi");
        write_midi(&midi.join("ok.mid"));
        std::fs::write(midi.join("broken.mid"), b"not a MIDI file").unwrap();
        let output = scratch.join("out");

        let outcome = super::batch_midi2ocp(&midi, &output, false).unwrap();
        assert_eq!((outcome.converted, outcome.errors), (1, 1));
        assert!(
            outcome.problems[0].contains("broken.mid"),
            "{:?}",
            outcome.problems
        );
        assert_eq!(names_in(&output), ["ok.ocp"]);
        assert!(super::run_batch_midi2ocp(&midi, &output, false).is_err());
    }
}
