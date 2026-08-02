# Test Tiers

The commands in this section are the **local** edit-loop tiers. CI does not run
any of them: the CI gate is `cargo test`, for the reasons in
[Process-per-test isolation](#process-per-test-isolation-cargo-nextest). Read
that section before assuming a local nextest pass predicts a CI pass.

The default Rust tier is the bounded unit suite:

```sh
cargo nextest run --profile default --lib
```

The non-fail-fast profile reports the full failure set in one run:

```sh
cargo nextest run --profile ci --lib
```

For local edit loops, use the quick profile with the module or test filter you are changing:

```sh
cargo nextest run --profile quick --lib <filter>
```

The profile timeouts are guardrails for hangs, not performance targets. When doing
test-suite speed work, capture before/after wall time and the slowest tests from
the nextest summary so lock contention and fixture setup costs stay visible.

Full-pipeline audit/refactor regressions that intentionally run broad audit machinery live in the explicit slow tier:

```sh
cargo test --lib --features slow-tests code_audit
cargo test --lib --features slow-tests collect_refactor_sources_audit_write_uses_audit_refactor_engine
```

Use the slow tier when changing audit detector orchestration, audit fixability planning, or audit-driven refactor planning. These tests remain runnable, but they are not part of the default unit gate because they scan real fixture/checkouts and dominated local suite wall-clock time.

## Process-Global Environment and Test Threads

`homeboy.json` sets the Rust extension's `rust_cargo_test_threads` to `1`, so the
gate runs `cargo test --workspace -- --test-threads=1`.

This is not a performance choice, it is a correctness one. `HomeGuard`
(`homeboy_core::test_support`) isolates a test by mutating *process-global*
environment — `HOME`, `XDG_DATA_HOME`, `HOMEBOY_ARTIFACT_ROOT`,
`HOMEBOY_RUNTIME_TMPDIR`, the invocation runtime directory — and restoring it on
`Drop`. The `home_lock` mutex it holds serializes *writers*. It cannot serialize
*readers*: any test on another thread that reads `HOME`, directly or through
`paths::homeboy()` and friends, can observe a foreign or missing value while a
guard's window is open. libtest shares one process across its threads, so there
is no thread-local escape hatch. (Rust made `std::env::set_var` `unsafe` in the
2024 edition for this reason.)

One thread per test binary closes that window. Cargo already runs test binaries
sequentially, so the cost is confined to intra-binary parallelism, and the
guard-holding tests — the slow ones — were already serialized by the mutex.

This is a workaround, not a fix. It buys correctness with every bit of
intra-binary parallelism the gate had, and it does nothing about the underlying
design: the tests still mutate process-global state. The structural fix is
injectable config and path roots so tests never touch process-global environment
at all (#7505), across roughly 2,072 `with_isolated_home` call sites. The
mechanical fix that retires the same defect class without touching those call
sites is process-per-test execution, below.

Until one of those lands, do not raise this setting, and do not add new ad-hoc
`std::env::set_var` helpers in test modules: use
`HomeGuard`/`with_isolated_home` so the mutation is at least covered by the
shared lock.

## Process-per-test isolation (cargo-nextest)

`cargo nextest` runs **one process per test**. A process-global `set_var` is
then global to a single test, so the reader/writer window described above cannot
open at all: there is no shared `HOME` for a second test to read. That retires
the whole defect class by construction rather than working around it, and it
does so without giving up intra-binary parallelism, which is the entire cost of
`rust_cargo_test_threads = 1`.

**CI does not run nextest, and cannot be switched to it from this repository
alone.** Selecting it here today would produce a red `Test` gate on a fully
passing suite. Three gaps must close first, and two of them live in repositories
this one does not control. They are listed in the order they must land.

### 1. Result parsing — `homeboy-extensions`

This is the gap that makes a premature flip *red* rather than merely useless.

`test_run_status` (`crates/homeboy-extension/src/test/run.rs`) treats absent
counts as a failure, deliberately: `test_measurement` maps `None` to
`Measurement::unreported()`, which assesses to
`Unmeasured(NoStructuredCounts)`, and `Unknown` collapses to `"failed"`. An
unmeasured gate has never been allowed to render green (#10685).

Counts reach that function from `homeboy-extensions`:
`rust/scripts/parse-test-results.sh` invokes the shared adapter set with exactly
one adapter, `cargo-test`, and that adapter matches only lines beginning
`test result:`. Core's fallback text parser
(`crates/homeboy-extension/src/test/parsing.rs`) is PHPUnit-shaped —
`Tests:`/`Failures:`/`Errors:` — with a `passed=<n> failed=<n>` key-value
fallback. `cargo nextest run` emits neither; its summary is
`Summary [ 1.234s] N tests run: N passed, M skipped`. Nothing matches, so
`test_counts` is `None` and the phase reports `failed` regardless of outcome.

`rust.json` declares no `test.result_parse` spec, and a component cannot supply
one — `result_parse` is read from the extension manifest via `load_extension`,
not from `homeboy.json`. So there is no in-repo lever. `homeboy-extensions`
needs a `nextest` result adapter (and matching failure-name extraction in
`rust/scripts/parse-test-failures.py`, which is also `test result:`-shaped)
before the runner can be flipped anywhere.

Note that core already parses nextest *durations*
(`crates/homeboy-extension/src/test/durations/mod.rs` reads the nextest
`Summary` line). Durations are advisory and do not feed `test_counts`, so that
existing support does not satisfy the measurement invariant.

### 2. Install — `homeboy-action` for the PR gate, this repo for the release gate

The two `Test` gates have different shapes:

- **Release** (`.github/workflows/release.yml`, `gate-test`) is a normal
  step-based job in this repository. An install step —
  `taiki-e/install-action` with `tool: cargo-nextest`, which fetches a prebuilt
  binary in seconds rather than paying a `cargo install` build — can be added
  here directly.
- **PR** (`.github/workflows/ci.yml`, job `homeboy`) calls the reusable workflow
  `Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2`. A caller cannot
  inject steps into a reusable workflow, and that workflow exposes no
  pre-command hook. `homeboy-action` contains no reference to `nextest` and its
  `auto-setup` does not install it. The PR gate therefore cannot get nextest
  without a change in `homeboy-action`.

Homeboy has no `pre:test` hook either — `homeboy.json` hooks are limited to
`pre:version:bump`, `post:version:bump`, `post:release`, and `post:deploy`
(`crates/homeboy-core/src/engine/hooks.rs`) — so there is no runner-driven
install path that would cover both gates.

Until an install exists on a given gate, leave `rust_nextest_fallback` at its
default `true` for that gate. The setting is a trap only when it is the *whole*
story: a silent fallback to `cargo test` with no thread cap would leave the race
unguarded. It is safe here precisely because `rust_cargo_test_threads = 1` is
still set, and that setting applies **only** when the resolved runner is
`cargo`, which is exactly the fallback path. Where nextest *is* installed, set
`HOMEBOY_RUST_NEXTEST_FALLBACK=0` on that job so a missing binary exits 127 with
the install diagnostic instead of quietly reverting — otherwise a no-op is
indistinguishable from success.

### 3. Profile selection — this repo

`.config/nextest.toml` already defines tuned `default`, `ci`, and `quick`
profiles, but the extension runner builds `cargo nextest run --manifest-path …`
plus scope args and never passes `--profile`. Nextest defaults to the `default`
profile unless `NEXTEST_PROFILE` is set, so the `ci` profile is **not** selected
today and would not be selected by an install alone.

That matters: `ci` sets `fail-fast = false`. Without it, nextest inherits
fail-fast and reproduces the exact complaint that motivated #11242 — cargo
aborting the remainder of the workspace so most of the suite never executes. Any
job that runs nextest should set `NEXTEST_PROFILE: ci`.

The timeout interaction is favourable and needs no budget change. `default` is
`period = 30s, terminate-after = 4` and `ci` is `period = 60s,
terminate-after = 2`; both **kill** a test at 120s. Against the 2700s phase
budget (`HOMEBOY_TEST_TIMEOUT_SECONDS`) and the action's 3000s outer backstop,
that converts a hung test from a 2700s phase stall into a bounded, named,
120s test failure — the same win #11234 and #11235 delivered by hand, but
automatic. Do not change the 2700s budget to accommodate it.

The one behavioural risk to measure on the first real run: a test that
legitimately takes over 120s becomes a failure rather than a slow pass. The
deliberately slow tier is already excluded from the gate behind
`--features slow-tests`, so the exposure should be small, but it is unverified.

### Landing order

1. `homeboy-extensions`: add a `nextest` result adapter and nextest failure-name
   parsing, and release the extension. Homeboy resolves extensions at the latest
   published release, so this propagates to CI on its own.
2. `homeboy-action`: install `cargo-nextest` in the reusable CI workflow's
   `candidate`/`baseline` jobs, so the PR gate has the binary.
3. This repo: add the install to `gate-test` in `release.yml`, set
   `NEXTEST_PROFILE: ci` and `HOMEBOY_RUST_NEXTEST_FALLBACK: '0'` on every job
   that has the binary, then set `extensions.rust.settings.rust_test_runner` to
   `nextest` in `homeboy.json`.
4. Only once a run demonstrably reports `Running cargo nextest...` with parsed,
   non-null test counts, drop `rust_cargo_test_threads`. Removing it earlier
   would leave the race unguarded on any path that fell back to `cargo test`.

Steps 1 and 2 are independent and can land in either order; step 3 must not
precede step 1.

## Hermetic CLI Fixtures

Ordinary Rust tests must use `homeboy::test_support::HermeticTestContext` for
Homeboy subprocesses. It supplies owned HOME, config, data, artifact, runtime,
temporary, daemon, and runner locations, and requires an explicit binary choice:
`TestBinary::HomeboyFixture` for Cargo's fixture binary or
`TestBinary::CurrentTest` for the running test executable. This prevents tests
from reading operator configuration or resolving an installed `homeboy` through
`PATH`.

Host integration tests are opt-in: place them behind an explicit Cargo feature
or an explicit command-line opt-in and document the required host service,
credentials, and cleanup contract beside the test. They may use host state only
when that contract is the behavior under test; they are excluded from the
ordinary Rust gate.
