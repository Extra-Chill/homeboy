# Rust Bench Extension

The Rust extension owns Rust-specific benchmark adapters and emits the same
Homeboy `BenchResults` envelope consumed by core. Homeboy core should only see
normalized scenarios, numeric metrics, metadata, and artifact pointers.

## Default `bench-*` Binaries

The default contract is unchanged: Cargo binaries named `bench-*` are discovered
from `src/bin/bench-*.rs` or explicit `[[bin]]` manifest entries and executed as
release binaries.

Use this path for small purpose-built workloads that can emit one JSON payload:

```json
{"timings_ns":[12345,12678],"peak_rss_bytes":41943040}
```

The runner normalizes those timings into `mean_ms`, `p50_ms`, `p95_ms`,
`p99_ms`, `min_ms`, and `max_ms`.

## Criterion Adapter

Criterion support is additive and opt-in:

```bash
HOMEBOY_RUST_BENCH_CRITERION=1 homeboy bench my-rust-component
```

The adapter discovers Cargo bench targets, runs `cargo bench --bench <target>`,
then normalizes `target/criterion/**/new/estimates.json` files into Homeboy
scenarios. Criterion estimate files are preserved as scenario artifacts.

Use Criterion when the project already has statistical microbenchmarks or needs
Criterion reports. Use `bench-*` binaries when a lightweight Homeboy-only
workload is enough.

If Criterion is requested but no bench targets are declared, the runner reports a
warning and emits a valid empty result instead of requiring Homeboy core to know
anything about Criterion.

## Rust Perf Profiles

Rust developer-loop profiles are also opt-in:

```bash
HOMEBOY_RUST_BENCH_PROFILES=1 homeboy bench my-rust-component
```

The first slice emits these normalized scenarios:

- `rust-clean-build` — runs `cargo clean`, then measures `cargo build --release`.
- `rust-warm-build` — prebuilds once, then measures warm `cargo build --release`.
- `rust-changed-file-check` — touches one source file, measures `cargo check`, then restores the file.
- `rust-test` — prebuilds test binaries, then measures `cargo test --no-run`.

Each profile scenario includes metadata identifying `cache_mode`, `change_mode`,
the command, the changed file when applicable, and active Rust toolchain
acceleration under `metadata.rust_toolchain`. This keeps cache/change and
toolchain semantics in the Rust extension while Homeboy core compares ordinary
scenario metrics.

Set `HOMEBOY_RUST_BENCH_CHANGED_FILE=path/to/file.rs` to choose the changed-file
profile target. When omitted, the runner uses `src/lib.rs` or `src/main.rs`.

## Homeboy Lab Guidance

Use these profiles for local Lab runs where hardware and cache state are stable.
Run enough repetitions before ratcheting baselines, especially for clean builds
that are sensitive to disk and dependency cache noise.

Examples:

```bash
HOMEBOY_RUST_BENCH_PROFILES=1 homeboy bench my-rust-component --iterations 5
HOMEBOY_RUST_BENCH_PROFILES=1 homeboy bench my-rust-component --scenario rust-warm-build --iterations 10
HOMEBOY_RUST_BENCH_CRITERION=1 homeboy bench my-rust-component --scenario criterion-my-bench
```

For the fast local development loop and cache/linker acceleration knobs, see
[`dev-loop.md`](dev-loop.md).
