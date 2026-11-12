# Diagnostics: debug and perf switches

Two cargo features decide what a build *can* report; two command-line flags
decide whether it does. A normal build pays nothing: with a feature off the call
sites fold away.

| Feature | Flag | Builds in |
|---|---|---|
| `debug-log` | `--debug` | debug/trace call sites, level raised to Debug |
| `perf-log` | `--perf` | counters and timers, plus a reporter thread |

Both binaries accept the flags. For the GUI, pass them to the executable:

```sh
./target/diagnostics/perf/oc-gui --debug --perf song.ocp
```

## Where the log goes

`~/Library/Logs/OpenCompose/<program>-<YYYYmmdd-HHMMSS>-<pid>.log`

The path is printed on startup. Lines are stamped with seconds since start,
which is what correlates with a moment you noticed while reproducing:

```
[    0.217] INFO  oc_gui_lib: gui: start version=0.1.0 debug=true perf=true ...
[   41.375] INFO  oc_engine::render_cache::cache: Mix + effects took 1.30s
[   42.001] INFO  oc_diag: perf[exit] cmd.render_offline.ms  n=1 mean=41004.512ms max=41004.512ms
```

A log file is only created when `--debug` or `--perf` is given; without a flag
the binary would leave an empty file per invocation. Warnings and errors still
reach stderr, and reproducing with `--debug` is what produces a file.

## Warnings

Warnings reach stderr in every build. A render counts a repeated warning and
reports it as one line, and streaming playback downgrades a routing fallback
that changes nothing. See [render warnings](render-warnings.md) for the rule
and for the interpreter's trace flag.

## What `--debug` adds

- startup banner: program, version, build features, OS, arch, CPU count
- project load: name, track count, sample rate
- plugin host: bundle path, class name, vendor, parameter count, load duration;
  every step of a VST3 editor attach (`createView`, `attached`, the returned
  result), and the reason when an editor is refused
- commands: render offline, preset parameter batches, pattern/preset previews,
  save (names, argument sizes, durations)
- engine: render entry/exit with frames and rendered seconds

## What `--perf` adds

Counters, printed as a `perf[tick]` block every 5 s and a final `perf[exit]`
block:

| Counter | Meaning |
|---|---|
| `render.buffer.ms` | one offline buffer render |
| `cache.full.ms`, `cache.incremental.ms` | full and incremental cache renders |
| `plugin.vst3.load.ms` | one VST3 load |
| `plugin.vst3.editor.open.ms` | one editor attach |
| `cmd.render_offline.ms`, `cmd.set_preset_params.ms`, `cmd.preview_pattern.ms`, `cmd.save_file.ms` | the UI-thread commands that stall most often |
| `audio.callback.n`, `audio.callback.ms` | device callback count and worst duration |
| `audio.underrun` | callbacks that ran the buffer dry while playing |

The audio callback only bumps atomics: counters are interned when the device
opens, so the realtime thread never allocates or locks.

## Building the diagnostics binaries

```sh
export CARGO_TARGET_DIR=target/diagnostics
cargo build -p oc-cli --features debug-log,perf-log
cargo build -p oc-gui --profile perf --features debug-log,perf-log
```

Run these commands from the repository root with the development frontend running.
A separate target directory lets you keep diagnostic and ordinary builds side by side.

## Sending a report

1. Reproduce with `--debug --perf`.
2. Send the log file plus: what you did, what you expected, and `uptime` if a
   number looks off.
3. For a render that is too slow, include the `perf[exit]` block — it separates a
   slow stage (`cache.*`, `render.buffer.ms`) from a slow UI command (`cmd.*`).
