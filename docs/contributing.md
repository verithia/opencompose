# Contributing

Use a focused branch and commit only the files needed for the change. Name the
area and result in the commit subject, for example
`fix(transport): resume playback from the requested cursor`. Record verification
and any behavior that still needs manual testing in the change description.

By contributing, you license your change under the license of the component
you modify: AGPL-3.0-or-later for the application, engine and libraries, and
MIT OR Apache-2.0 for the language components (`oc-dsl`, `oc-device-schema`,
`oc-lsp` and `editors/vscode-ocp`). Code that the permissive components depend
on must itself be permissively licensed; do not add a dependency from those
crates on an AGPL crate.

Read `AGENTS.md` before changing the repository. This guide explains how to
apply its engineering rules. These are contribution and acceptance standards;
their presence does not mean every existing subsystem already meets them.

## Find the owner before changing behavior

Put each domain rule in one owning module. Device ranges, defaults, units and
choices belong in `oc-device-schema`. Other shared rules, including MIDI
mapping, media decoding, clock conversion and catalog verification, need an
equally explicit owner. GUI, CLI, agent tools, playback and export should call
that owner through thin adapters.

When copies disagree, establish the intended contract before deleting one.
Test affected callers for parity, including fallback catalogs and extreme
values. Two lanes that use the same preset can still own independent voices,
controllers, effects and tails; sharing a definition does not authorize
merging their state.

Verify workflows through their promised entry points. If installation is
advertised as one click or one agent call, a successful manual installation
script does not prove that workflow. Record a fundamental gap when testing
needs privileged assistance; do not conceal it by performing the missing
product step outside the application.

Keep dependency direction clear. Pure DSP and shared domain types should not
depend on device I/O, GUI components or command dispatch. Use named request
and result types instead of long positional tuples and boolean switches.
Split large modules along responsibilities and ownership boundaries. Keep a
mechanical extraction separate from a behavior fix so reviewers can assess
each change.

## Make contracts explicit

Deserialize JSON and external strings at adapters, then use domain types.
Represent session states, playback owners, provider kinds and errors with
tags or enums. Preserve error causes and expose stable codes and parameters
that the UI can localize. Do not infer state from English sentences, prompt
prefixes or display labels.

Derive frontend bindings, argument schemas and enum choices from the owner
where practical. Otherwise check field identities, units, defaults and enum
members for parity. A matching count does not prove matching contracts.
Tool registration, dispatch, mutation policy and approval must agree on the
same typed specification. Permissions come from the trusted caller context,
not model-supplied arguments.

Preserve serialized keys, public identifiers and compatibility aliases. An
internal rename does not authorize a wire-format change. Test intentional
migrations with old input and a new round trip. Keep protocol handling
separate from localized presentation.

## Preserve documents and asynchronous ownership

Validate an edit before publishing it. A failed import, patch decode or
installation must leave the document, undo and redo stacks, and published
files unchanged. A failed history operation must not move its entry between
stacks. Use a reliable revision or equality contract for change detection;
a hash collision must not silently discard an edit.

Capture document, target and gesture identity when asynchronous work starts.
Check them at mutation admission and again before publishing the result.
Ignoring a stale UI response is insufficient if the backend has already
changed the wrong document. An old pointer release or delayed completion
must not end a newer gesture, clear its draft or overwrite its value.
Cancellation must define who retires resources and whether work actually
stops. A timeout or dropped future alone does not prove cleanup.

For media and AI sessions, persist the context needed to reopen the session,
including required attachments and their references. Surface persistence
failures. Do not replace required audio with an unsaved placeholder and
report the session as durable.

## Stream from the first correct block

Play, seek and previews must not wait for a whole song or range render,
unrelated library decoding or unnecessary future-source preparation. Prepare
the dependencies needed at the requested cursor outside audio processing.
Preserve authored program changes, sample addresses, interpolation neighbors
and effect state. A silent placeholder is not a correct first audio block.
A background worker by itself does not establish streaming behavior.

Separate preparation, command dispatch, first delivered audio and steady
throughput in measurements. Measure cold starts through the production
worker, ring buffer and device callback. State whether filesystem caches or
in-process caches are warm. Record profile, feature set, block size, workers,
platform and load. Follow [Performance targets](performance-targets.md) and
report budget misses without relaxing the target to make a test pass.

In real-time processing, including reset, seek and parameter changes, avoid
allocation, locks, filesystem access, network calls and thread creation.
Prepare scratch buffers, asset storage and event capacity in advance, then
reuse them through bounded processing APIs. Exercise allocation counters
around production paths, including sidechains and auxiliary outputs. Keep
parallel preparation and offline track work separate from per-voice audio
processing.

## Design playback as an interaction state machine

Give arrangement playback, track preview, clip preview and preset audition
explicit owners. Distinguish preparing, playing, paused, finished, cancelled
and failed states. Define what the same button, Space, Stop, selection change
and a new preview do in each state.

A paused preview resumes from its cursor. A finished preview must not resume
an exhausted stream. Explicitly pausing a preview must not accidentally
resume a parked arrangement. A seek while paused must establish the position
used by the next Play request. Associate completion with the correct output
generation and consumed range, rather than an unrelated arrangement end.

Test rapid and delayed sequences: pause during preparation, resume before
the pause reply, finish during a pause request, replace the document, switch
preview targets and seek repeatedly. Check new PCM and cursor movement after
resume. A changed icon, successful command, old cumulative peak or reused
buffer alone does not prove resumed audio.

## Bound inputs through the actual producer

Treat presets, projects, media, catalogs and archives as untrusted. Bound
input and decoded bytes, records, dimensions, nesting, loop counts and work.
Use checked arithmetic and the correct text encoding when converting offsets;
an LSP UTF-16 column is not a UTF-8 byte offset.

Carry captured limits through reads and conversions. A metadata check does
not protect a later helper that reads a new length or allocates without a
limit. Refuse growth beyond admitted capacity. Bound decompression and PCM
production before appending output, and account for retained temporary data.
Use small injected limits to test boundaries without allocating huge files.

Fallbacks must retain the same safety limits and input identity. Reuse an
admitted immutable snapshot when a second path would otherwise reread the
file. A known limit failure must not select an unbounded fallback. Do not hold
a global cache lock across file I/O or expensive decoding. Publish only
validated results for the intended source identity.

Share installation verification across GUI, CLI and agent entry points:
trust roots, signatures, checksums, version and rollback policy, archive
containment, atomic publication and cleanup. Keep media separate from metadata
sidecars. Development trust keys and test servers must not silently become
production defaults.

## Own native and subprocess lifecycles

For every native boundary, specify construction, activation, processing,
deactivation and destruction, including partial failure. Own libraries,
callbacks, buffers and registrations until every native user has released
them. State the actual invariants behind each `unsafe` block and `Send` or
`Sync` assertion. Comments must describe established ownership, not assumed
single-threaded use.

Establish UI-thread authority at application bootstrap. A constructor's
thread, a test worker or an always-true platform check cannot establish it.
Keep native UI/control work separate from processor work. Dispatch through
the registered owner, and retain ownership if safe finalization cannot be
delivered. Never weaken this rule to make a test fixture pass.

Apply the appropriate exception and panic containment for each ABI. Do not
let failures unwind across foreign boundaries or silently truncate an
unsupported block size. Return a structured error or a documented, visible
fallback instead of substituting silence.

For helper processes, bound stdout and stderr while draining them during
execution. Cover descendants and inherited pipes in deadline handling;
exiting the direct child does not prove all writers are closed. Reap owned
processes and preserve spawn, read, wait, timeout and protocol failures as
distinct causes. Prefer deterministic pipe/deadline tests to longer sleeps.

## Keep the interface usable across contexts

Use shared parameter and pointer-gesture controls, with keyboard access,
visible focus, cancellation and release outside the control. Respect editing
contexts when handling Space, Undo and other global shortcuts. Keep context
menus nonmutating until the user selects an action.

Route visible text through localization, including fallback states and
errors. Check theme and locale changes at runtime. Derive initial theme,
language and UI scale from supported system settings, while preserving an
explicit user override. Test long translated labels, narrow panels and
supported UI scales; a layout that fits one screenshot is insufficient.
Use source-derived visualizations and display units from the domain owner.

When native UI testing is authorized, use one test instance, preserve real
before/after screenshots and build identity, then close that instance and
verify it exited. Follow the user's audio and desktop-use constraints.
Label headless, simulated-device and hardware tests accurately; one does not
substitute for another's evidence.

## Test a change

Add a regression test for a bug fix and an automated test for new behavior.
Test at the layer that owns the behavior. Use a Rust test for engine behavior
or Vitest for component behavior. Prefer an existing test executable.

Exercise the production algorithm through explicit injected dependencies or
test backends. Do not replace the algorithm under `cfg(test)` and present the
substitute as release coverage. Reject unknown IPC commands in mocks. Share
fixtures, use guarded temporary directories and avoid process-global
environment mutation in parallel tests. Test refusal, cleanup, stale replies
and partial failure as well as the successful path.

For audio changes, verify the intended sound behavior: pitch, timing, loop
boundaries, transients, routing, tails and relevant before/after comparisons.
Buffer dimensions and peak levels alone do not establish musical quality.
When a workflow requires an agent to evaluate its own render, check that it
actually receives the rendered audio through the supported listening path.
Distinguish listening evidence from numerical analysis and follow the
user's playback constraints.

Use one integration test executable per crate unless process isolation is
required and documented. Add cases to its aggregator and filter by name.
During iteration, run the owning unit tests or integration module and the
affected frontend files. Batch related checks and run broader suites once
at delivery rather than linking many binaries for every small commit.

```sh
cargo fmt --all -- --check
cargo check -p oc-engine --all-targets
cargo test -p oc-engine --test all -- sf2::
scripts/check-docs.sh
```

For frontend changes, run `npx tsc -b --noEmit` and the relevant Vitest files
from `crates/oc-gui/frontend`. See [Testing and verification](testing.md) for
workspace checks and performance harnesses.

Check the actual feature sets you ship. An all-target test build can unify a
test-support feature that hides production-only warnings or behavior. Verify
production and relevant no-default-feature builds separately, along with
affected Windows, macOS and Linux paths. Use the supported toolchain declared
by the workspace, and test that declaration before documenting it.

CI must execute the real frontend reference graph (`tsc -b --noEmit`), tests,
formatters, Python checks and unsafe-documentation checks. Inspect coverage
and exit behavior: an empty type-check, permissive mock or `|| true` can make
a green job prove nothing. Diagnose existing failures separately; do not
hide them with an unrelated fix, longer timeout or skipped assertion.

Enable the optional pre-commit hook with
`git config core.hooksPath .githooks`.

## Deliver a reviewable change

Keep a focused commit for each finished part. Stage exact intended files and
preserve unrelated working edits. When worktrees run concurrently, account
for shared Git state and build artifacts; a stash or executable from another
checkout is not an isolated baseline. Keep generated media and test output
out of the source tree, and remove owned temporary resources after testing.

Describe the trigger, resulting behavior, owning implementation and executed
checks. Distinguish implemented, compiled, tested, manually verified and
accepted states. Include remaining limits, failed checks and platforms not
tested. A partial child fix does not close a broader review group, and a
large passing test count does not establish coverage of a specific workflow.

Re-check review findings against current source before editing: line numbers,
counts and suspected causes age. Keep internal review reports, task ledgers,
screenshots and machine-specific measurements outside the repository. Keep
public design and contributor guidance portable and reproducible.

## Write documentation

Follow the [documentation style](style.md) and
[compatibility policy](compatibility.md). Use code comments to explain
invariants or constraints. Put public design rationale and reproducible
measurements in the documentation. Describe the current behavior and use
examples that run from the repository or a standard installation.

Add a chapter to `docs/SUMMARY.md` when you add a Markdown file under `docs/`.
For Rust APIs, use intra-doc links and document applicable errors and panics.
Check changes with `cargo doc -p <crate> --no-deps`.

## Build the documentation site

```sh
scripts/docs.sh build
scripts/docs.sh serve
```

The site builds with mdBook and Rustdoc. See
[Navigating the API reference](api-reference.md) for how to find public types
and functions.
