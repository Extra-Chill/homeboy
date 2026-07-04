# Test Tiers

The default Rust gate is the bounded unit suite:

```sh
cargo nextest run --lib --no-fail-fast
```

Full-pipeline audit/refactor regressions that intentionally run broad audit machinery live in the explicit slow tier:

```sh
cargo test --lib --features slow-tests code_audit
cargo test --lib --features slow-tests collect_refactor_sources_audit_write_uses_audit_refactor_engine
```

Use the slow tier when changing audit detector orchestration, audit fixability planning, or audit-driven refactor planning. These tests remain runnable, but they are not part of the default unit gate because they scan real fixture/checkouts and dominated local suite wall-clock time.
