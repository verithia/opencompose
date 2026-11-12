//! Write the generated synth catalog module used by the GUI frontend.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let default_path = root.join("../oc-gui/frontend/src/data/synthCatalog.gen.ts");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--check") {
        let on_disk = std::fs::read_to_string(&default_path)?;
        if on_disk == oc_device_schema::synth::render_typescript() {
            return Ok(());
        }
        return Err("frontend synth catalog is stale; run the generator".into());
    }
    let path = args
        .first()
        .filter(|arg| !arg.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or(default_path);
    std::fs::write(&path, oc_device_schema::synth::render_typescript())?;
    println!("wrote {}", path.display());
    Ok(())
}
