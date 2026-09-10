# Test Tiers

The commands in this section are the **local** edit-loop tiers. CI does not run
any of them: the CI gate is `homeboy review test homeboy`, for the reasons in
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

`homeboy.json` selects nextest with `rust_test_runner = "nextest"`, so local and
release runs get process-per-test isolation and `num-cpus` parallelism. It also
keeps `rust_cargo_test_threads = 1`, which now applies only to the Cargo
fallback taken when nextest is not installed. The reusable PR workflow selects
nextest for its inventory-bound shard jobs.

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

One thread per test binary closes that window on the Cargo fallback. Nextest
closes it more directly by running each test in its own process.

The Cargo cap is a fallback workaround for a race that is specific to sharing
one process across threads. Sharded nextest replay does not have that race, and
`rust_nextest_shard_threads` is now `0` — nextest's num-cpus parallelism.

It was `1` for a second and independent reason, which no longer holds. Process-
per-test isolates environment but not the filesystem, and the controller-runtime
admission store and the rig registry used to be machine-global directories
beneath the real `$HOME`. Run `32294476539` caught one test reading another's
admission lease, and ten `workspace::tests::prune::*` failures traced to shared
rig-registry writes.

Both stores now take explicit roots for every production caller (#7505), and
both failure families pass in parallel. Measured on an 8-core host,
`homeboy-lab-runner --lib` went from 410.3s serial to 98.5s parallel with an
identical failure set, and neither it nor `homeboy-agents --lib` has any
parallel-only failure.

The two settings therefore now differ. The Cargo cap stays `1` until `HomeGuard`
stops mutating process-global environment; the nextest cap is lifted.

Unsharded nextest runs were previously unmeasurable — they emit no
`test result:` lines, so the cargo-test adapter matched nothing and produced no
Homeboy test result at all. That is why local and release stayed on serialized
Cargo even after the underlying race was fixed. `homeboy-extensions` rust
v1.36.0 derives counts from `libtest-json-plus` instead, which is what made
selecting nextest here possible. Note that the nextest branch replaces the
command binary outright, so `rust_cargo_test_threads` is not applied to it.

The structural fix remains injectable config and path roots so tests never touch
process-global environment at all (#7505), across roughly 2,072
`with_isolated_home` call sites.

Do not raise the Cargo fallback setting, and do not add new ad-hoc
`std::env::set_var` helpers in test modules: use
`HomeGuard`/`with_isolated_home` so the mutation is at least covered by the
shared lock.

## Process-per-test isolation and CI sharding

`cargo nextest` runs one process per test, so process-global environment changes
cannot leak into another test in the same process. The PR Test gate uses the
generic inventory contract from `homeboy-extensions` and four deterministic
shards from `homeboy-action@v2`. Each shard validates exact inventory membership,
runs tests in isolated processes, and publishes structured counts before the single required
`homeboy / Test` verdict is reconciled.

The reusable workflow installs prebuilt nextest, sets `NEXTEST_PROFILE=ci`, and
disables fallback for shard planning and replay. The CI profile disables
fail-fast and terminates a test after two 60-second slow periods. The
deliberately slow tier remains excluded unless `slow-tests` is enabled. General
unsharded nextest output is not yet a measured Homeboy result, so local and
release gates remain on serialized Cargo.

The first full sharded run is the acceptance evidence for #11399. It must show
complete inventory coverage, four terminal shard results, and one reconciled
Test verdict inside the admitted per-shard budget.

## Hermetic CLI Fixtures

Ordinary Rust tests must use `homeboy::test_support::HermeticTestContext` for
Homeboy subprocesses. It supplies owned HOME, config, data, artifact, runtime,
temporary, daemon, and runner locations, explicitly replacing inherited
`HOMEBOY_DATA_DIR` and `HOMEBOY_DAEMON_STATE_DIR` so process discovery and
lifecycle/source ownership stay in the test namespace. It requires an explicit binary choice:
`TestBinary::HomeboyFixture` for Cargo's fixture binary or
`TestBinary::CurrentTest` for the running test executable. This prevents tests
from reading operator configuration or resolving an installed `homeboy` through
`PATH`.

Host integration tests are opt-in: place them behind an explicit Cargo feature
or an explicit command-line opt-in and document the required host service,
credentials, and cleanup contract beside the test. They may use host state only
when that contract is the behavior under test; they are excluded from the
ordinary Rust gate.
