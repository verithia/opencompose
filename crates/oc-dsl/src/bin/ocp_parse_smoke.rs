//! Smoke harness: parse one file, print the track count, and exit non-zero on
//! a parse failure. Used by build checks, not by the product.

use std::env;
use std::fs;
use std::process;

fn main() {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: ocp_parse_smoke <file.ocp>");
        process::exit(2);
    };
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("failed to read {path}: {err}");
            process::exit(2);
        }
    };
    if let Err(err) = oc_dsl::parse(&source) {
        eprintln!("{err}");
        process::exit(1);
    }
}
