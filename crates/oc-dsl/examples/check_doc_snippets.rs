//! Parse every ` ```ocp ` block in the given Markdown files.
//!
//! Usage:
//!
//! ```text
//! cargo run -p oc-dsl --example check_doc_snippets -- docs/manual/dsl.md ...
//! ```
//!
//! A block is checked as a complete document first. If that fails, it is tried
//! again in each context a fragment can legitimately appear in: project
//! properties, a track body, a preset body, a pattern body (`events:`) and a
//! pattern body of drum hits (`hits:`). A block that parses in none of them is
//! reported with the file, the line and the first error, and the exit code is 1.
//!
//! A fence whose info string contains `ignore` is skipped, for a block that
//! illustrates a shape rather than a document. Fences that do not name `ocp` are
//! not checked at all.
//!
//! The same check runs over the escaped snippets in the product's
//! `dsl-examples.json`, so a syntax drift shows up even when the GUI test that
//! normally parses them is not part of the run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// One fragment context, with the prefix that makes the fragment a document.
struct Context {
    name: &'static str,
    prefix: &'static str,
}

const CONTEXTS: &[Context] = &[
    Context {
        name: "document",
        prefix: "",
    },
    Context {
        name: "top-level blocks",
        prefix: "@project \"Snippet\"\n",
    },
    Context {
        name: "pattern body",
        prefix: "@project \"Snippet\"\n@pattern snippet | instrument: synth/lead\n  events:\n",
    },
    Context {
        name: "drum pattern body",
        prefix: "@project \"Snippet\"\n@pattern snippet | instrument: drums/acoustic_kit\n  hits:\n",
    },
    Context {
        name: "track body",
        prefix: "@project \"Snippet\"\n@track snippet | instrument: synth/lead\n",
    },
    Context {
        name: "preset body",
        prefix: "@project \"Snippet\"\n@preset snippet\n",
    },
    Context {
        name: "bus body",
        prefix: "@project \"Snippet\"\n@bus snippet\n",
    },
    Context {
        name: "master body",
        prefix: "@project \"Snippet\"\n@master\n",
    },
    Context {
        name: "phrase body",
        prefix: "@project \"Snippet\"\n@track snippet | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 9:1]\n",
    },
    Context {
        name: "project properties",
        prefix: "@project \"Snippet\"\n",
    },
];

fn main() -> ExitCode {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut examples: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--examples" {
            examples = args.next().map(PathBuf::from);
        } else if arg.starts_with("--") {
            eprintln!("check_doc_snippets: unknown option {arg}");
            return ExitCode::from(2);
        } else {
            paths.push(PathBuf::from(arg));
        }
    }
    if paths.is_empty() && examples.is_none() {
        eprintln!("usage: check_doc_snippets [--examples <json>] <markdown>...");
        return ExitCode::from(2);
    }

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for path in &paths {
        match check_file(path) {
            Ok(count) => checked += count,
            Err(error) => failures.push(error),
        }
    }
    if let Some(path) = examples {
        match check_examples(&path) {
            Ok(count) => checked += count,
            Err(error) => failures.push(error),
        }
    }

    if failures.is_empty() {
        println!("check_doc_snippets: {checked} snippet(s) parse");
        return ExitCode::SUCCESS;
    }
    for failure in &failures {
        eprintln!("{failure}");
    }
    eprintln!(
        "check_doc_snippets: {} snippet(s) do not parse",
        failures.len()
    );
    ExitCode::FAILURE
}

/// Check one Markdown file. Returns how many blocks were checked.
fn check_file(path: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    let mut in_fence = false;
    let mut is_ocp = false;
    let mut skip = false;
    let mut block = String::new();
    let mut block_line = 0usize;
    let mut checked = 0usize;
    let mut failures = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            if !in_fence {
                in_fence = true;
                let info = info.trim();
                is_ocp = info == "ocp" || info.starts_with("ocp ") || info.starts_with("ocp,");
                skip = info.contains("ignore");
                block.clear();
                block_line = number + 1;
                continue;
            }
            in_fence = false;
            if is_ocp && !skip {
                checked += 1;
                if let Err(error) = check_fragment(&block) {
                    failures.push(format!(
                        "{}:{block_line}: ocp snippet does not parse: {error}",
                        path.display()
                    ));
                }
            }
            is_ocp = false;
            skip = false;
            continue;
        }
        if in_fence && is_ocp {
            block.push_str(line);
            block.push('\n');
        }
    }

    if in_fence && is_ocp && !skip {
        failures.push(format!(
            "{}:{block_line}: unterminated ocp fence",
            path.display()
        ));
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    Ok(checked)
}

/// Check every fragment in the product's escaped-example file.
fn check_examples(path: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("{}: invalid JSON: {error}", path.display()))?;
    let examples = value
        .get("examples")
        .and_then(|examples| examples.as_array())
        .ok_or_else(|| format!("{}: no `examples` array", path.display()))?;
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for example in examples {
        let id = example
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("<unnamed>");
        let Some(fragment) = example.get("fragment").and_then(|f| f.as_str()) else {
            failures.push(format!(
                "{}: example '{id}' has no fragment",
                path.display()
            ));
            continue;
        };
        let wrap = example.get("wrap").and_then(|w| w.as_str()).unwrap_or("");
        let document = format!("{wrap}\n{fragment}\n");
        checked += 1;
        if let Err(error) = oc_dsl::parse(&document) {
            failures.push(format!(
                "{}: example '{id}' does not parse: {error}",
                path.display()
            ));
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    Ok(checked)
}

/// Parse one block in the first context that accepts it.
fn check_fragment(block: &str) -> Result<(), String> {
    if block.trim().is_empty() {
        return Ok(());
    }
    let mut first_error = None;
    for context in CONTEXTS {
        let document = format!("{}{block}", context.prefix);
        match oc_dsl::parse(&document) {
            Ok(_) => return Ok(()),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(format!("as a {}: {error}", context.name));
                }
            }
        }
    }
    Err(first_error.unwrap_or_else(|| "no context accepted the block".into()))
}
