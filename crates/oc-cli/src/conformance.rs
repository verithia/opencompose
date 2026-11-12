//! `ocp plugin conformance`: one child per VST3 bundle, collected into a matrix.
//!
//! The child process hosts one bundle and streams its battery as JSON Lines.
//! This module spawns those children, enforces a hard deadline on top of the
//! child's per-step watchdog, parses the stream and renders the matrix.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oc_host::PluginFormat;
use oc_host::conformance::{MatrixReport, PluginReport, Step, StepReport, StepStatus};
use serde_json::Value;

/// Flags collected by clap for the conformance command.
pub struct Options {
    /// `table`, `json` or `markdown`.
    pub format: String,
    /// Explicit bundles; empty means every VST3 the scanner finds.
    pub plugins: Vec<PathBuf>,
    /// Skip the editor step.
    pub no_editor: bool,
    /// Per-step deadline in the child.
    pub timeout_ms: u64,
}

/// Run the matrix and print it in the requested format.
pub fn run(options: Options) -> Result<(), String> {
    let format = Format::parse(&options.format)?;
    let plugins = discover(&options.plugins)?;
    if plugins.is_empty() {
        return Err("no VST3 bundles found".to_string());
    }
    let mut reports = Vec::with_capacity(plugins.len());
    for (path, duplicates) in &plugins {
        reports.push(run_one(path, duplicates, &options)?);
    }
    let matrix = MatrixReport::new(reports, options.timeout_ms, !options.no_editor);
    match format {
        Format::Table => print!("{}", render_table(&matrix)),
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(&matrix).map_err(|error| error.to_string())?
        ),
        Format::Markdown => print!("{}", render_markdown(&matrix)),
    }
    let failed = matrix.summary.plugins - matrix.summary.passed_all;
    if failed > 0 {
        return Err(format!(
            "{failed} of {} plugins failed at least one step; see the matrix above",
            matrix.summary.plugins
        ));
    }
    Ok(())
}

enum Format {
    Table,
    Json,
    Markdown,
}

impl Format {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "table" | "text" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            other => Err(format!(
                "unknown --format '{other}'; use table, json or markdown"
            )),
        }
    }
}

fn discover(explicit: &[PathBuf]) -> Result<Vec<(PathBuf, Vec<PathBuf>)>, String> {
    if !explicit.is_empty() {
        let mut paths: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
        for path in explicit {
            if !path.exists() {
                return Err(format!("plugin bundle does not exist: {}", path.display()));
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("vst3") {
                return Err(format!("not a VST3 bundle: {}", path.display()));
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !paths.iter().any(|(known, _)| *known == canonical) {
                paths.push((canonical, Vec::new()));
            }
        }
        paths.sort_by(|left, right| left.0.cmp(&right.0));
        return Ok(paths);
    }
    let mut bundles: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for (path, format) in oc_host::discover_plugin_files() {
        if format != PluginFormat::Vst3 {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        match bundles
            .iter_mut()
            .find(|(known, _)| known.file_stem().and_then(|stem| stem.to_str()) == Some(name))
        {
            Some((_, duplicates)) => duplicates.push(path),
            None => bundles.push((path, Vec::new())),
        }
    }
    bundles.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(bundles)
}

fn run_one(path: &Path, duplicates: &[PathBuf], options: &Options) -> Result<PluginReport, String> {
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Unknown")
        .to_string();
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the ocp executable: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("plugin")
        .arg("conformance")
        .arg("--child")
        .arg("--plugin")
        .arg(path)
        .arg("--timeout-ms")
        .arg(options.timeout_ms.to_string());
    if options.no_editor {
        command.arg("--no-editor");
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = command.spawn().map_err(|error| {
        format!(
            "could not start the conformance child for {}: {error}",
            path.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "conformance child has no stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "conformance child has no stderr".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));

    // The child watchdog covers each step; this is the backstop for a module
    // that blocks the watchdog thread or leaves a thread behind.
    let hard_deadline = Duration::from_millis(options.timeout_ms.saturating_mul(12).max(30_000));
    let hard_deadline_ms = hard_deadline.as_millis().min(u128::from(u64::MAX)) as u64;
    let deadline = Instant::now() + hard_deadline;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "could not poll the conformance child for {}: {error}",
                    path.display()
                ));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            killed = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let mut report = parse_child(
        path,
        &name,
        &stdout,
        &stderr,
        status,
        killed,
        hard_deadline_ms,
    );
    report.duplicates = duplicates
        .iter()
        .map(|duplicate| duplicate.to_string_lossy().into_owned())
        .collect();
    Ok(report)
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    buffer
}

fn parse_child(
    path: &Path,
    fallback_name: &str,
    stdout: &[u8],
    stderr: &[u8],
    status: Option<std::process::ExitStatus>,
    killed: bool,
    hard_deadline_ms: u64,
) -> PluginReport {
    let text = String::from_utf8_lossy(stdout);
    let mut report = PluginReport::unreached(path, fallback_name);
    let mut fatal: Option<(String, String)> = None;
    let mut done_seen = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(kind) = value.get("fatal").and_then(Value::as_str) {
            let step = value
                .get("step")
                .and_then(Value::as_str)
                .unwrap_or("load")
                .to_string();
            fatal = Some((kind.to_string(), step));
        } else if let Some(step) = value.get("step")
            && let Ok(step) = serde_json::from_value::<StepReport>(step.clone())
            && let Some(slot) = report.steps.iter_mut().find(|slot| slot.step == step.step)
        {
            *slot = step;
        } else if value.get("done").is_some() {
            done_seen = true;
            if let Some(name) = value.get("name").and_then(Value::as_str)
                && !name.is_empty()
            {
                report.name = name.to_string();
            }
            if let Some(vendor) = value.get("vendor").and_then(Value::as_str) {
                report.vendor = vendor.to_string();
            }
            if let Some(version) = value.get("version").and_then(Value::as_str) {
                report.version = Some(version.to_string());
            }
        }
    }
    // The load step names the factory class and vendor before `done` arrives.
    if let Some(load) = report.steps.iter().find(|step| step.step == "load") {
        if let Some(name) = load.detail.get("class_name").and_then(Value::as_str)
            && !name.is_empty()
        {
            report.name = name.to_string();
        }
        if let Some(vendor) = load.detail.get("vendor").and_then(Value::as_str) {
            report.vendor = vendor.to_string();
        }
        if let Some(version) = load.detail.get("factory_version").and_then(Value::as_str)
            && !version.is_empty()
        {
            report.version = Some(version.to_string());
        }
        if let Some(category) = load.detail.get("category").and_then(Value::as_str) {
            report.category = category.to_string();
        }
    }
    let stderr_text = String::from_utf8_lossy(stderr);
    let child_stderr = stderr_tail(&stderr_text);
    let had_fatal = fatal.is_some();
    if let Some((kind, step)) = fatal {
        if let Some(slot) = report.steps.iter_mut().find(|slot| slot.step == step) {
            slot.status = if kind == "hang" {
                StepStatus::Hang
            } else {
                StepStatus::Crash
            };
            slot.error = Some(if child_stderr.is_empty() {
                format!("child reported {kind} in step {step}")
            } else {
                format!("child reported {kind} in step {step}: {child_stderr}")
            });
        }
    } else if killed {
        if let Some(slot) = report
            .steps
            .iter_mut()
            .find(|slot| slot.status == StepStatus::Skipped)
        {
            slot.status = StepStatus::Timeout;
            slot.error = Some(format!(
                "child exceeded the parent hard deadline of {hard_deadline_ms} ms"
            ));
        }
    } else if let Some(status) = status
        && !status.success()
    {
        if let Some(slot) = report
            .steps
            .iter_mut()
            .find(|slot| slot.status == StepStatus::Skipped)
        {
            slot.status = StepStatus::Crash;
            slot.error = Some(match status.code() {
                Some(code) => format!("child exited with code {code}: {child_stderr}"),
                None => format!("child was terminated by a signal: {child_stderr}"),
            });
        }
    } else if !done_seen
        && let Some(slot) = report
            .steps
            .iter_mut()
            .find(|slot| slot.status == StepStatus::Skipped)
    {
        slot.status = StepStatus::Crash;
        slot.error = Some(format!("child exited without a result: {child_stderr}"));
    }
    // A step a normally finishing child never reported is not evidence of
    // conformance. After a fatal record the remaining steps stay skipped.
    if done_seen
        && !had_fatal
        && !killed
        && let Some(slot) = report.steps.iter_mut().find(|slot| {
            slot.status == StepStatus::Skipped && slot.error.as_deref() == Some("not reached")
        })
    {
        slot.status = StepStatus::Crash;
        slot.error = Some("the child did not report this step".to_string());
    }
    report.exit_code = status.and_then(|status| status.code());
    if report.steps.iter().any(|step| step.status.is_failure()) && !child_stderr.is_empty() {
        report.child_stderr = Some(child_stderr);
    }
    report
}

fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("[VST3]") && !line.starts_with("diagnostics log:")
        })
        .collect();
    lines
        .iter()
        .rev()
        .take(3)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" | ")
}

fn render_table(matrix: &MatrixReport) -> String {
    let name_width = matrix
        .plugins
        .iter()
        .map(|plugin| plugin.name.len())
        .max()
        .unwrap_or(8)
        .clamp(18, 40);
    let mut output = String::new();
    output.push_str(&format!("{:<name_width$}", "plugin"));
    for step in Step::ALL {
        output.push(' ');
        output.push_str(&format!("{:<8}", step.name()));
    }
    output.push('\n');
    for plugin in &matrix.plugins {
        output.push_str(&format!("{:<name_width$}", plugin.name));
        for step in Step::ALL {
            let status = plugin
                .steps
                .iter()
                .find(|report| report.step == step.name())
                .map(|report| report.status.label())
                .unwrap_or("?");
            output.push(' ');
            output.push_str(&format!("{status:<8}"));
        }
        output.push('\n');
    }
    output.push('\n');
    output.push_str(&render_summary(matrix));
    output
}

fn render_markdown(matrix: &MatrixReport) -> String {
    let mut output = String::new();
    output.push_str("# VST3 conformance\n\n");
    output.push_str(&format!(
        "{} {}, {} pass every step. Per-step deadline {} ms, editor {}.\n\n",
        matrix.summary.plugins,
        if matrix.summary.plugins == 1 {
            "plugin"
        } else {
            "plugins"
        },
        matrix.summary.passed_all,
        matrix.timeout_ms,
        if matrix.editor { "enabled" } else { "skipped" }
    ));
    output.push_str("| Plugin |");
    for step in Step::ALL {
        output.push_str(&format!(" {} |", step.name()));
    }
    output.push('\n');
    output.push_str("|---|");
    for _ in Step::ALL {
        output.push_str("---|");
    }
    output.push('\n');
    for plugin in &matrix.plugins {
        output.push_str(&format!("| {} |", escape_cell(&plugin.name)));
        for step in Step::ALL {
            let status = plugin
                .steps
                .iter()
                .find(|report| report.step == step.name())
                .map(|report| report.status.label())
                .unwrap_or("?");
            output.push_str(&format!(" {status} |"));
        }
        output.push('\n');
    }
    output.push('\n');
    output.push_str(&render_summary(matrix));
    output
}

fn render_summary(matrix: &MatrixReport) -> String {
    let mut output = String::new();
    let noun = if matrix.summary.plugins == 1 {
        "plugin"
    } else {
        "plugins"
    };
    output.push_str(&format!(
        "{} {noun}, {} pass every step\n",
        matrix.summary.plugins, matrix.summary.passed_all
    ));
    if !matrix.summary.step_failures.is_empty() {
        output.push_str("step failures:");
        for step in Step::ALL {
            if let Some(count) = matrix.summary.step_failures.get(step.name()) {
                output.push_str(&format!(" {} {count};", step.name()));
            }
        }
        output.push('\n');
    }
    for (index, group) in matrix.summary.top_failure_kinds.iter().enumerate() {
        output.push_str(&format!(
            "{}. {}: {} ({}: {})\n",
            index + 1,
            group.step,
            group.error,
            group.count,
            group.plugins.join(", ")
        ));
    }
    output
}

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}
