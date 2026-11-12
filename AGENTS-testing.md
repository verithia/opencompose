# AGENTS-testing.md

How to test, verify and measure changes, and what CI must enforce. The hard
rules in [AGENTS.md](AGENTS.md) still apply.

## What a change needs

- Every bug fix ships a regression test, and every feature ships a test, at
  the layer that owns the behavior: Rust tests for engine, DSP and commands;
  Vitest for components.
- Test the production implementation through injected dependencies or explicit
  test backends. Never swap the algorithm under `cfg(test)` and call that
  release coverage.
- Cover failure and boundaries: malformed or oversized files, Unicode and
  UTF-16 positions, invalid ranges, failed patches, stale asynchronous replies,
  cancellation and cleanup.
- Isolate fixtures with guarded temporary directories. Do not mutate
  process-global environment in parallel tests.
- For audio, check the sound itself: pitch, timing, loop points, transients,
  routing and tails. Buffer sizes and peak levels alone prove nothing.

## Rust test layout

Linking dominates build time, so each crate has a single integration-test
binary: `autotests = false` plus `[[test]] name = "all" path = "tests/all.rs"`.
Add a new file as a module in `tests/all.rs`:

```rust
#[path = "my_case.rs"]
mod my_case;
```

A crate gets an extra `[[test]]` binary only if it needs process isolation,
such as a global allocator or main-thread UI. Add a comment in `Cargo.toml`
saying why, and an entry in `scripts/check-test-binaries.py`.

## Commands

While iterating, run only the owner's tests. Run the broader set once, at
delivery.

```sh
# Rust
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p oc-engine --test all -- <filter>
cargo test --workspace

# Performance and CPU-budget harnesses (opt-in)
cargo test --workspace --features perf-tests --profile perf

# Frontend (from crates/oc-gui/frontend/)
npx tsc -b --noEmit
npx vitest run

# Docs, scripts and repository checks
scripts/check-docs.sh
python3 scripts/check-test-binaries.py
python3 scripts/check-licenses.py
python3 scripts/third-party-licenses.py --check   # needs npm ci in the frontend
ruff format --check scripts/
ruff check scripts/
```

`npx tsc` only works after `npm ci`. Without `node_modules`, `npx` runs a stub
that exits 0, which is a false green.

## Performance evidence

- Follow `docs/performance-targets.md`. Report budget misses instead of
  relaxing a target.
- Measure preparation, dispatch, first delivered audio and steady throughput
  separately. Say whether caches are warm. Record profile, features, block
  size, workers, platform and load.
- Docs carry reproducible methods and targets. They may also show one labelled
  example measurement (hardware, tier, load, profile, median) to illustrate the
  protocol. Raw session logs, repeat tables and unfinished runs belong in
  `./private/`.

## Native UI verification

Run native UI verification only when the user authorizes it. Use one test
instance, keep before/after screenshots and the build identity in
`./private/`, then quit the instance and confirm it exited.

## CI contract

CI (`.github/workflows/ci.yml`) must run, and fail on error:

- `npx tsc -b --noEmit` against the real project references, plus Vitest
- `cargo fmt --check`, `cargo check`, `cargo test` and clippy with
  `-D warnings`, with undocumented unsafe blocks denied
- `ruff format --check`, `ruff check` and the `scripts/tests` regressions
- `scripts/check-docs.sh`, `scripts/check-test-binaries.py`,
  `scripts/check-licenses.py` and `scripts/third-party-licenses.py --check`

Never hide a failure with `|| true`, `continue-on-error`, a longer timeout or a
skipped assertion. Before claiming a green check proves a requirement, check
what it actually covers.
