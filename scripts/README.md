# Repository scripts

The CI entry points are wired in `.github/workflows/ci.yml`. These manual
harnesses are kept for targeted investigations; run them from the repository
root.

## `check-test-binaries.py`

Verify the integration-test binary budget: every crate keeps one aggregate
`all` binary unless it has an explicit entry in the script's allowlist.

```sh
python3 scripts/check-test-binaries.py
```

## `test-cases.sh`

Run named cases from one already-built test binary without linking a binary per
case:

```sh
printf 'mixdown::a_track\ndynamics::the_ratio\n' | scripts/test-cases.sh oc-engine
```

The second argument selects a non-default test target. The script reads names
from stdin and forwards them to `cargo test -p <crate> --test <binary> -- ...`.

## `check-random-patches.py`

Render seeded `ocp randomize` variations and check they stay inside the factory
bounds. The harness loads `check-factory-presets.py` for the probe, bounds and
meter:

```sh
python3 scripts/check-random-patches.py --ocp target/debug/ocp --count 100
```

Use `--determinism` to prove that the same seed produces byte-identical output,
and `--keep` to retain the generated `.ocp` and WAV files.

## `doc-coverage.sh`

Measure documented public items and enforce the current floor (also called by
the documentation gate in a separate CI job):

```sh
scripts/doc-coverage.sh --min 58
```
