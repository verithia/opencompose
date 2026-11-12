# oc-diag

Opt-in debug logging and performance counters, compiled away when unused.

## What it owns

Two cargo features decide whether instrumentation exists at all, and two runtime
flags decide whether it runs:

| Feature | Runtime flag | Effect |
|---|---|---|
| `debug-log` | `--debug` | debug/trace call sites exist; level raised to Debug |
| `perf-log` | `--perf` | counters and timers exist; a reporter thread prints them |

With a feature off, the corresponding call sites compile to nothing, so a normal
build pays neither code size nor a branch. Without either flag the logger still
installs, at Warn, so a panic or a warning still reaches the log file.

Logs go to the platform log directory (`log_dir`, `log_path`). The reporter
thread is started by `spawn_reporter` and prints counters on an interval.

## Key types

| Type | Role |
|---|---|
| [`Counter`](../api/oc_diag/struct.Counter.html) | A named counter with an atomic value. |
| [`Timer`](../api/oc_diag/struct.Timer.html) | Times a scope and feeds the result to a counter on drop. |
| `counter`, `counter_in`, `perf_counter` | Look up or create a counter by name; the macro-free call sites. |
| `report`, `log_report` | Print the current counters, or write them to the log. |

## Depends on

Nothing inside the workspace. `log` is re-exported so call sites use one
version.

## Rules

- Instrumentation is not logic. A decision must never depend on whether a
  feature is compiled in; the `DEBUG_COMPILED` and `PERF_COMPILED` constants are
  for reporting, not for branching behaviour.
- Counters are cheap but not free. Put them around stages (a render pass, a
  block source), not inside a per-sample loop.
- A timer that reports microseconds pays for a clock read twice; measure only
  where the number changes a decision.
- Keep messages in English and stable: log lines are grepped by operators.

## API

[`api/oc_diag/`](../api/oc_diag/index.html).
