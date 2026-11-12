# Testing and verification

Run the tests that cover your change during development. Run the workspace
and frontend suites once before handing off a batch of changes.

## Rust

Most crates collect integration cases in a single executable named `all`.
Pass a test name or module prefix to the executable to run the relevant cases.

```sh
cargo check -p oc-engine --all-targets
cargo test -p oc-engine --test all -- sf2::
cargo test --workspace
```

Use `--lib` for a library test. A test that requires its own process, such as
one that replaces the global allocator, has a separate target.

## Frontend

From `crates/oc-gui/frontend`, install dependencies with `npm ci`, then run
TypeScript and the relevant Vitest file. Omit the filename for the full suite.

```sh
npx tsc -b --noEmit
npx vitest run src/__tests__/i18n.test.ts
```

## Documentation and repository checks

```sh
scripts/check-docs.sh
scripts/check-conflict-markers.sh
```

The documentation checker validates chapter membership, local links, agent
tool names, and parseable OCP examples. Use `--no-snippets` to check the
documentation structure without compiling the DSL example checker.

## Performance

The throughput and CPU-budget harnesses use the opt-in `perf-tests` feature
and the optimized `perf` profile.

```sh
cargo test --workspace --features perf-tests --profile perf
```

Record the build profile, thread count, platform, and load when you report a
timing. See [Performance targets](performance-targets.md) for the measurement
protocol.
