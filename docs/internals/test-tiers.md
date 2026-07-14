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
