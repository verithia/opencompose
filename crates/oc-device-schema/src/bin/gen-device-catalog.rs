//! Write the generated device catalog module used by the GUI frontend.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let default_path = root.join("../oc-gui/frontend/src/data/deviceCatalog.gen.ts");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--check") {
        let on_disk = std::fs::read_to_string(&default_path)?;
        let units = std::fs::read_to_string(default_path.with_file_name("units.gen.ts"))?;
        let gm = std::fs::read_to_string(default_path.with_file_name("gmPrograms.gen.ts"))?;
        if gm == oc_device_schema::gm::render_typescript()
            && on_disk == oc_device_schema::frontend::render_typescript()
            && units == oc_device_schema::units::render_typescript()
        {
            return Ok(());
        }
        return Err("frontend device catalog is stale; run the generator".into());
    }
    let path = args
        .first()
        .filter(|arg| !arg.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or(default_path);
    std::fs::write(&path, oc_device_schema::frontend::render_typescript())?;
    std::fs::write(
        path.with_file_name("units.gen.ts"),
        oc_device_schema::units::render_typescript(),
    )?;
    std::fs::write(
        path.with_file_name("gmPrograms.gen.ts"),
        oc_device_schema::gm::render_typescript(),
    )?;
    println!("wrote {}", path.display());
    Ok(())
}
