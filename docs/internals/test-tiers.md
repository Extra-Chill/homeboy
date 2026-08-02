# Test Tiers

The default Rust gate is the bounded unit suite:

```sh
cargo nextest run --profile default --lib
```

CI uses the non-fail-fast profile so one run reports the full failure set:

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

The real fix is injectable config and path roots so tests never touch
process-global environment at all (#7505). Until that lands, do not raise this
setting, and do not add new ad-hoc `std::env::set_var` helpers in test modules:
use `HomeGuard`/`with_isolated_home` so the mutation is at least covered by the
shared lock.

`cargo nextest` does not need this, because it runs each test in its own
process. It is not what CI uses today.

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
