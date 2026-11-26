//! Disposable native process for the VST2 bank compatibility reader.

use crate::child_process::{self, CommandSpec, RunOptions, StdioMode};
use crate::plugin_state_io::{MAX_PLUGIN_STATE_BYTES, read_regular_state_file};
use crate::scanner::parent_lifetime::watch_parent_stdin;
use crate::vst2::program_bank::migration_bank_id;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

pub const VST2_BANK_WORKER_ARG: &str = "--opencompose-vst2-bank-worker";
const MAX_WORKER_ERROR_BYTES: usize = 8 * 1024;

/// Convert an FXB snapshot in another process. The caller's executable must
/// dispatch `VST2_BANK_WORKER_ARG` before normal startup.
pub fn import_vst2_bank_isolated(
    bundle: &Path,
    source_name: &str,
    bank: &[u8],
) -> Result<Vec<u8>, String> {
    migration_bank_id(bank)?;
    let scratch = tempfile::Builder::new()
        .prefix("opencompose-vst2-bank-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let source = scratch.path().join("input.fxb");
    let target = scratch.path().join("output.vstpreset");
    fs::write(&source, bank).map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = CommandSpec::new(executable);
    command
        .arg(VST2_BANK_WORKER_ARG)
        .arg(bundle)
        .arg(source_name)
        .arg(source)
        .arg(&target);
    let result = child_process::run(
        command,
        StdioMode::Capture,
        RunOptions::new(Duration::from_secs(10)),
        None,
    )
    .map_err(|error| format!("VST2 bank migration worker failed: {error}"))?;
    if !result.status.success() {
        return Err(read_regular_state_file(
            &target.with_extension("error"),
            MAX_WORKER_ERROR_BYTES,
        )
        .ok()
        .and_then(|error| String::from_utf8(error).ok())
        .unwrap_or_else(|| format!("VST2 bank migration worker exited with {}", result.status)));
    }
    read_regular_state_file(&target, MAX_PLUGIN_STATE_BYTES)
        .map_err(|error| format!("VST2 bank worker did not return a preset: {error}"))
}

/// Run on the worker's native main thread after application-main registration.
pub fn run_vst2_bank_worker(args: &[OsString]) -> Result<(), String> {
    let [bundle, source_name, source, target] = args else {
        return Err("invalid VST2 bank worker request".into());
    };
    watch_parent_stdin()?;
    let source_name = source_name
        .to_str()
        .ok_or("the legacy VST2 name is not valid Unicode")?;
    let bank = read_regular_state_file(Path::new(source), MAX_PLUGIN_STATE_BYTES)?;
    let target = Path::new(target);
    match super::legacy_import::import_vst2_bank(Path::new(bundle), source_name, &bank) {
        Ok(converted) => write_new(target, &converted).map_err(|error| error.to_string()),
        Err(error) => {
            if error.len() <= MAX_WORKER_ERROR_BYTES {
                let _ = write_new(&target.with_extension("error"), error.as_bytes());
            }
            Err(error)
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::File::create_new(path)?.write_all(bytes)
}
