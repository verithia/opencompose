# Performance targets

The product targets, the reference session they are measured on, and the protocol
that keeps the numbers comparable. A change that misses a target is a regression;
a change that moves a number without this protocol is not evidence.

## Targets

| # | Target | Value | Applies to |
|---|---|---|---|
| 1 | Offline render throughput | **≥ 10 s of audio per wall second** (render time ≤ 10 % of the rendered duration) | the reference session below, on 4 cores |
| 2 | First audio block | **< 50 ms** from the play request to the first block reaching the device | any project, cold or warm; lazy source building is allowed inside the budget |
| 3 | Automation | the reference session carries automation lanes on every synth and instrument track | target 1 is measured with them active |

"10 s per second" is throughput, not headroom on one stem: it includes every
effect on every bus and the master chain.

The 50 ms budget is the *first block*, not the whole mix being ready. A project
may build sources lazily, but the device must start producing audio inside the
budget; the rest of the graph may fill in behind the stream.

## Reference session

One project, checked into `benchmarks/session-20tracks.ocp`, so every
measurement uses the same material:

- **20 tracks**: 8 OpenSynth synth tracks, 8 instrument tracks (SoundFont
  programs), 4 sampler tracks (drum kits).
- **Automation** on the synth and instrument tracks: filter brightness, volume
  and pan lanes, so the graph evaluates automation every block rather than once.
- **4 buses**, each with a rich chain: compression, EQ, saturation and a
  time-based effect at minimum, plus a send from the tracks that feed it.
- **Master** with its own compression and limiting.
- Fixed length (32 bars at 120 BPM = 64 s of audio), fixed tempo, no plugins, no
  external sample libraries, so the render is deterministic across machines.

Plugins are deliberately excluded: their cost belongs to the plugin, varies per
install, and would make the gate machine-dependent. Plugin projects are measured
by the same script with `OCP_BENCH_PROJECT`, but they are not the gate.

## Protocol

```sh
# macOS: QoS tier 0, 4 cores, warm release binary, no loudness normalisation
OCP_RENDER_THREADS=4 taskpolicy -t 0 scripts/perf/render_bench.sh

# the manual form of the same render
OCP_RENDER_THREADS=4 taskpolicy -t 0 ./target/release/ocp render \
    benchmarks/session-20tracks.ocp --output /tmp/perf.wav \
    --sample-rate 48000 --no-loudness-normalisation

# the harness does the arithmetic and fails under the target
scripts/perf/render_bench.sh
```

Rules:

- **`taskpolicy -t 0` on macOS, always.** A background-QoS process is scheduled
  onto the efficiency cores. The same binary on the same track read **17.0 s of
  user CPU at default QoS against 7.09 s at tier 0** (2.4x), and the same session
  read 2.58–3.01x against 9.06–9.26x. The tier, not the load average, is the
  largest source of spread between runs; a number taken without the tier is not
  comparable with one taken with it.
- **Warm binary.** Build first; the harness does not count compilation.
- **`OCP_RENDER_THREADS=4`** is the four-core constraint on a machine with more
  cores. Without it the render uses the machine's parallelism and the number is
  not comparable.
- **Release build** for the published number. A debug number may be quoted only
  with the profile named, and never against these targets.
- **Throughput = rendered seconds ÷ wall seconds**, measured around the render
  call (process start and parse excluded where the tool reports them).
- **Three runs, the median decides.** The harness repeats the render
  (`OCP_BENCH_REPEAT`, default 3) and prints every run's ratio next to the load
  average it was taken at; the median is what passes or fails the target. A
  single run on a shared machine is a sample, not a measurement.
- **Quote the tier and the load with every number** — `10.4x (tier 0, load 3.8)`
  is evidence; a bare ratio is not. Compare code against another binary in an
  interleaved A/B taken in the same window, never against a ratio recorded on a
  different day.
- **The first-block target** is measured by the playback harness in
  `crates/oc-gui` (`commands::tests::review_preview_latency` prints the stage
  table) and by the app's own log (`Play dispatched` → first callback), not by
  the offline renderer.

For a cold simulated-device sample, run the following command once per
process. Repeat it three times and report the median. Set `OCP_STREAM_PRELOAD=1`
to press Play immediately after queuing the open warm-up. Leave it unset to
start without preload. `OCP_PROFILE_PLAYBACK=1` includes preparation timings.
For a calibration-cache comparison, point `OCP_PRESET_TRIM_CACHE_DIR` at
an empty directory on the first run and reuse it for a fresh-process hit.
Release builds use the per-user cache automatically. The bounded cache holds
exact gains from the existing calibration probe for self-contained native
presets. File-backed presets and hosted effects still measure on the existing
path.

```sh
OCP_STREAM_PROJECT="$PWD/examples/opensynth_showcase.ocp" OCP_RENDER_THREADS=4 \
  taskpolicy -t 0 cargo test -p oc-gui --profile perf --lib -- \
  first_block_tests::review_fresh_project_first_block --ignored --exact --nocapture
```

Set `OCP_STREAM_REQUIRE_AUDIBLE=1` for an audible project to additionally
require non-silent PCM from the fresh simulated output instance. The report
prints first-delivery latency separately from time to non-silent PCM; a silent
callback alone does not verify that the prepared sources can play.

## Perf test set and profile

The timing/throughput harnesses are behind the `perf-tests` feature, so a plain
`cargo test --workspace` neither compiles nor runs them; the default CI gate is
the non-perf suite. The set is the two vocal CPU-budget unit tests,
`range_window_perf`, `audio_clip_perf`, the six `synth_perf` harnesses and the
two `device_cpu` profiles. Run the whole set explicitly through the `perf`
profile, which inherits `dev` (line tables, debug assertions) and sets
`opt-level = 2` for `oc-engine`, `oc-dsp`, `oc-synth-core`, `oc-synth-fx` and
`oc-gui` (the crates the harnesses and the real-time path measure):

```sh
cargo test --workspace --features perf-tests --profile perf
```

Build the GUI with `cargo build -p oc-gui --profile perf` to put it in
`target/perf/oc-gui`; `target/debug/` holds the default build and test
artifacts only. Plain `dev` optimises nothing, which is what keeps the default
build and test compile fast.

The `device_cpu` reverb harness's full/legacy ceiling is 3.5: the reverb
measures 2.48 under release and 2.83–2.91 under `perf` (best of seven), so the
ceiling sits above both profiles with margin. The synth harnesses also require `oc-synth-core`/`oc-synth-fx` in the profile: at dev
speed the dense patch misses the 0.5x-realtime assertion by 4x.

A scoped harness run works the same way:

```sh
OCP_RENDER_THREADS=4 cargo test -p oc-engine --features perf-tests --profile perf \
    --test synth range_window_perf:: -- --nocapture
```

## Reference measurement (example)

The table below is one measurement on the reference machine, provided as an
example of what the protocol produces. It is not a contract: the targets and the
protocol above are. Raw per-session logs — every repeat, its load average, the
instrumented stage runs — stay outside the repository, so re-measure before
drawing a conclusion about a change.

| Target | Measured | Where |
|---|---|---|
| 1. Render ≥ 10 s/s on 4 cores | **10.07x** median of three runs (10.07 / 9.41 / 10.65), tier 0, load 9.0–9.8, 4 threads, GM font | `scripts/perf/render_bench.sh` |
| 2. First block < 50 ms | 25–37 ms for a builtin preset audition, 29–34 ms for a track preview (debug, production path) | `docs/playback-latency.md` |

The reference machine is a 12-core Apple M4 Pro. Quote its numbers with the
tier and the load, as the protocol requires, and remember that the values move
between runs.

The reference session renders 66.002 s of audio (32 bars at 120 BPM plus the
2 s instrument/effect tail) at 48 kHz. The SoundFont tracks use GM programs, so
the harness runs with a GM font through `OCP_SF2`; the harness default is the
checked-in test fixture, which measures within 4 % of the GM font at the same
load. `ocp render --report` prints peak, true peak, RMS, crest and integrated
loudness for the rendered WAV.

**Read a ratio only with its tier and its load.** The protocol above is the
only way these numbers compare: the same binary on the same session reads
2.58–3.01x at background QoS and 9.06–9.26x at tier 0, so a ratio that does not
name its tier is not evidence.

On the reference machine the source phase is the bound: the reference session's
texture track alone sets the phase end, and the pad track is within 7 % of it,
so a per-voice saving converts to wall 1:1 only until the pad becomes the bound.

### Where the four-thread wall time goes (example run)

One instrumented run with `OCP_RENDER_STAGES=1`, 4 threads, 66.002 s of audio
on the reference machine. Stage times move ±15 % between runs. `bus fan-in` sits inside `bus chains`; every other row
is a separate slice of the render.

| Stage | Time (ms) |
|---|---:|
| pattern previews | 0 (skipped) |
| soundfont source tracks | 1046.8 |
| builtin source tracks | 3892.9 |
| track FX chains (parallel) | 563.0 |
| track routing and sends | 415.6 |
| bus chains (incl. fan-in) | 795.3 |
| — bus fan-in | 11.4 |
| master chain | 514.8 |
| output gain + WAV write | 21.9 |
| **sum of the rows** | **7261** |

The rows are non-overlapping stage measurements, not the enclosing
`mix + buses + master` span; `interpret` (1.3–2.1 ms), `graph build` (0.4–0.5 ms)
and `output gain + loudness` (0.6–0.7 ms) fall below the millisecond scale of
this table. Stage runs and harness runs are separate executions, and the same
binary's wall varies between 6.15 s and 7.35 s across runs, so a stage sum and a
harness wall do not line up run for run.

What shapes the largest rows:

- **Pattern previews.** `render_patterns` renders every referenced pattern
  through the SoundFont player for the pattern browser, which a one-shot export
  never reads (about 0.87 s on the reference session).
  `RenderCache::set_pattern_previews(false)` skips it on the export path only;
  the GUI cache keeps it.
- **Bus chains.** The four reference buses each route straight to master, so
  nothing orders their chains. They run on the same four workers, and the
  returns are summed into master in declaration order afterwards, so the mix
  keeps the serial path's rounding. Nested buses or a bus used as another bus's
  sidechain keep the ordered serial path.
- **Builtin source tracks.** The 12 built-in jobs are ranked by voice-seconds
  times unison width, which starts the expensive tracks first. A queue in track
  order would leave the pad or the texture track in the last wave and end the
  phase at about 7.8 s; ranked, the phase ends at 3.9–4.9 s.

### Remaining serial time and the floor

The rows below come from the same example run as the stage table.

After the parallel source phase and the parallel buses, what is left in order is
deliberately ordered work:

| Serial item | Time (ms) | Why it cannot overlap |
|---|---:|---|
| track FX chains | 563 | one barrier, one chain per track |
| track routing and sends | 416 | each bus sums its tracks in track order |
| bus chains + fan-in | 795 | buses consume routed tracks; returns sum in declaration order |
| master chain | 515 | one stateful chain over the whole program |
| WAV write | 22 | after the master chain |
| **serial tail** | **2311** | |

The source phase is the rest (about 4.9 s in the instrumented run, 55–65 % of
the wall) and is bounded by two numbers: total per-track voice work divided by
four, and the heaviest single track. In the reference session those are roughly
19–21 s ÷ 4 ≈ 4.8–5.3 s and 4.0–4.7 s (the pad and texture tracks), so the
source phase is already within about 10 % of its scheduling bound.

That puts the current floor at about 6.2–7.0 s of wall on four cores
(9.4–10.7x), which is the range the status table's runs land in. Going clearly past
10x needs less per-voice synth work (the pad and texture tracks pay for unison
and polyphony over the full 66 s), not more scheduling: with today's per-voice
cost the 2.3 s ordered tail is over a third of a 6.5 s render and stops
shrinking.


### Range-window cost

A `tracks` range request is not the render gate, but the agent loop makes it
often enough that it has its own harness and protocol:

```sh
OCP_RENDER_THREADS=4 cargo test -p oc-engine --features perf-tests --profile perf \
    --test synth range_window_perf:: -- --nocapture
```

The harness renders `benchmarks/session-20tracks.ocp` in one process and times,
three times each on a fresh cache, a cold `tracks=[0,1]` window at
`[10 s, 11 s)`, a full render, the first and repeat warm window on the same
span, a window at `[60 s, 61 s)`, a 4-track window at `[20 s, 22 s)`, and a full
window after the subsets (which must report a cache hit). It prints the stage
table, the window path the engine chose and the snapshot bytes; the medians are
what counts, and the load average is printed beside every run.

Medians at 4 threads:

| Scenario | Time (ms) |
|---|---:|
| cold subset window (one full render + mix) | 791 |
| warm subset window, first (no snapshot) | 225 |
| warm subset window, repeat (snapshot) | 211 |
| warm window at 60–61 s (pass runs from frame 0) | 1255 |
| warm 4-track window at 20–22 s | 599 |
| full window after the subsets (cache hit) | 4.1 |
| full render (control) | 5263 |

A subset window runs only the selected tracks' chains (0 ms on a snapshot,
8–25 ms when recomputed), then the routing, bus and master graph up to the
window end plus the master latency. Running every track's chain and the
whole-song graph instead costs 2.2–2.5 s on a warm cache, and a cold subset
would add the full source render on top. The snapshot's own saving is
the selected chains — 8–15 ms per 11 s window for these two tracks — so the
bounded pass and the selected-only sourcing are the larger terms; a window late
in a long song still pays the pass from frame 0.

Snapshots cost 8 bytes per frame, 384 KB per covered track-second at 48 kHz,
and are capped at 64 MiB (`RenderCache::set_post_fx_snapshot_budget`) with LRU
eviction. On the reference session a 2-track window at 10–11 s holds 8,455,680
bytes, at 60–61 s 46,855,680 bytes, and a 4-track window at 20–22 s 63,759,360
bytes (just under the cap).

## Why these two targets and not a single number

They fail differently. Target 1 is throughput on a saturated offline renderer:
it degrades when per-sample work or per-block allocation grows, and it is the
number that decides whether a 30-track song renders in a coffee break or in four
minutes. Target 2 is a deadline on the realtime path: it degrades when the play
path does work inline, and it is what makes the instrument feel immediate. A
change can easily improve one and break the other — moving work from the render
thread into the play path, or pre-rendering to hit the deadline — so both are
gated.

## CPU-budget tests are expressed in timing units, not seconds

A test that asserts "analysis must cost less than 0.5 s per audio second" is a statement about the machine at
that moment, so it fails on a loaded laptop and passes on an idle one — the worst outcome being that a real
regression is dismissed as a flake. The two vocal CPU-budget tests therefore measure a fixed reference
workload in the same process (`oc-dsp`'s `test_support::reference_seconds`, four million square roots plus
multiply-adds, best of three) and assert a **ratio** to it. They are part of the `perf-tests` set and are
only meaningful under the `perf` profile, where `oc-dsp` is `opt-level = 2`; a plain `dev` build runs them
unoptimised and the numbers are not comparable.

Measured under `--profile perf`: the analysis sits at 4.0 units and the
pitch shift at 4.6 (44.1 kHz), 5.5 (48 kHz) and 6.0–6.2 (96 kHz) units across repeated runs. The budgets
are twice the observed values — 8 for the analysis and 12 for the shift — so a doubling of the work fails
while load, which scales both sides of the ratio, cancels.

The budgets are tight enough to catch a change in work: lowered to 3 and 4, the
tests fail with
`analysis used 0.010 s per audio second, 4.0x the timing unit (budget 3)` and
`44100 Hz used 0.011 s per second, 4.6x the timing unit (budget 4)`.
