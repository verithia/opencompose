//! Publish the checked result of a hosted plugin-state import.

use anyhow::{Context, Result, anyhow, bail};
use oc_host::plugin_state_io::{MAX_PLUGIN_STATE_BYTES, read_regular_state_file};
use std::io::Write;
use std::path::Path;

pub fn run(input: &Path, source_name: &str, plugin: &Path, output: &Path) -> Result<()> {
    if !output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"))
    {
        bail!("the output must have a .vstpreset extension");
    }
    if output.symlink_metadata().is_ok() {
        bail!("the output already exists: {}", output.display());
    }
    let bytes = read_regular_state_file(input, MAX_PLUGIN_STATE_BYTES)
        .map_err(|error| anyhow!("cannot read '{}': {error}", input.display()))?;
    let folder = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !folder.is_dir() {
        bail!("the output directory does not exist: {}", folder.display());
    }
    let migrated = oc_host::vst3_instance::import_vst2_bank_isolated(plugin, source_name, &bytes)
        .map_err(anyhow::Error::msg)?;
    let mut staged = tempfile::NamedTempFile::new_in(folder)?;
    staged.write_all(&migrated)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(output)
        .with_context(|| format!("cannot publish '{}'", output.display()))?;
    println!("Saved {}", output.display());
    Ok(())
}
