//! Isolated plugin-scan process. The host spawns it so a bundle that crashes or
//! hangs cannot take the application down with it.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match oc_host::scanner::run_helper_cli(&args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:?}");
            1
        }
    };
    // The helper loads the module in-process; leave without running its teardown.
    oc_host::exit_process(code);
}
