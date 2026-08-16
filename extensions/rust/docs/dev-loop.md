# Rust Development Loop

The Rust extension owns Rust/Cargo toolchain acceleration. Homeboy core should
only receive generic command environment and benchmark metadata; it should not
grow Rust, Cargo, sccache, linker, or nextest behavior.

## Portable Toolchain Readiness

Before a portable lint operation, the extension declares separate structured
probes for `cargo --version` and `cargo fmt --version`. Each probe uses a
`program` plus literal `args` array, so the runner invokes Cargo directly
rather than interpreting a compound shell command. The extension shape check
rejects legacy `command` probe fields; repair guidance remains diagnostic
metadata for the operator.

## Shared Cargo Target Directory

The Rust env provider sets a stable `CARGO_TARGET_DIR` per component identity:

```bash
${XDG_DATA_HOME:-$HOME/.local/share}/homeboy/cargo-targets/<component>-<repo-hash>
```

This keeps target artifacts reusable across primary checkouts and worktrees for
the same repository while avoiding cross-project target collisions. If the user
sets `CARGO_TARGET_DIR` explicitly, the provider emits `{}` and does not
override it.

## sccache

Enable sccache through extension settings or the environment:

```bash
HOMEBOY_RUST_SCCACHE=1 homeboy test my-rust-component
HOMEBOY_RUST_SCCACHE=1 homeboy lint my-rust-component
HOMEBOY_RUST_SCCACHE=1 homeboy bench my-rust-component
```

Equivalent component setting:

```json
{
  "extensions": {
    "rust": {
      "rust_sccache": true
    }
  }
}
```

When enabled, the env provider sets `RUSTC_WRAPPER=sccache` only if `sccache` is
on `PATH` and `RUSTC_WRAPPER` is not already set. Existing `RUSTC_WRAPPER` wins.

## Linker Acceleration

Linker acceleration is explicit because linker availability and compatibility
vary by host:

```bash
HOMEBOY_RUST_LINKER=mold homeboy test my-rust-component
HOMEBOY_RUST_LINKER=lld homeboy bench my-rust-component
```

Equivalent component setting:

```json
{
  "extensions": {
    "rust": {
      "rust_linker": "mold"
    }
  }
}
```

The provider appends the matching Rust flag only when the linker binary is on
`PATH`:

- `mold` -> `RUSTFLAGS="$RUSTFLAGS -C link-arg=-fuse-ld=mold"`
- `lld` -> `RUSTFLAGS="$RUSTFLAGS -C link-arg=-fuse-ld=lld"`

Unsupported or unavailable requests are reported through `HOMEBOY_RUST_*_STATUS`
environment metadata and do not force Cargo to run with a broken linker flag.

## Test Runner Selection

`rust_test_runner` chooses between `cargo` (default) and `nextest`. When
`nextest` is selected but `cargo-nextest` is not installed, `rust_nextest_fallback`
(default true) falls back to `cargo test`; set it false to fail with an install
diagnostic instead.

Sharded replay and unsharded runs count nextest differently because only one of
them has a plan to count against:

- **Sharded replay** reconciles the emitted `libtest-json-plus` identities
  against the shard manifest and hard-errors on any mismatch. A shard that does
  not execute exactly what was planned is a broken shard, not a test failure.
- **Unsharded runs** have no manifest. `rust_nextest_measured_counts` (default
  true) parses the same event stream into a Homeboy test result without
  reconciliation. Both counters share `scripts/nextest_events_lib.py`, so they
  cannot drift on how nextest encodes identity or which events are outcomes.

`rust_nextest_measured_counts` does **not** select the runner — it only decides
whether a run that already chose nextest is measured. It matters because nextest
emits no `test result:` lines, so the cargo-test result adapter matches nothing:
without it, an unsharded nextest run produces no structured counts at all. Set
`HOMEBOY_RUST_NEXTEST_MEASURED_COUNTS=0` to opt back out.

A measured run reports its counts directly:

```
Rust test runner: cargo nextest (measured counts from libtest-json-plus)
Rust nextest result: total=482 passed=479 failed=0 skipped=3
```

When the stream carries no terminal test event — a compile failure, or a run
killed before the first test — no test result is written at all. Homeboy reads a
missing `test.results` as "no structured counts", which is the intended signal;
an all-zero result would be indistinguishable from a suite that legitimately ran
nothing and would report a broken build as a pass.

Two invocation details differ from shard replay on purpose:

- `--no-tests warn` (replay uses `fail`). Replay preflights exact membership, so
  an empty match there is a broken plan. Unsharded, the filter is derived from
  changed file paths, and an empty match is the scope-derivation gap the runner
  absorbs by widening to the full suite — which requires a zero exit code.
- Skip counts reflect only the skips nextest reported. nextest omits tests it
  never ran from its default event stream, so unsharded skip counts do not
  reconstruct `#[ignore]` membership the way a shard manifest can.

## Fast Local Profile

For local iteration, use the narrowest command that proves the change:

```bash
HOMEBOY_RUST_SCCACHE=1 homeboy lint my-rust-component --step fmt
HOMEBOY_RUST_SCCACHE=1 homeboy test my-rust-component --skip-lint -- --lib
HOMEBOY_RUST_SCCACHE=1 HOMEBOY_RUST_LINKER=mold homeboy bench my-rust-component --scenario rust-warm-build --iterations 5
```

Use `HOMEBOY_RUST_BENCH_PROFILES=1` to measure the loop itself:

```bash
HOMEBOY_RUST_SCCACHE=1 HOMEBOY_RUST_BENCH_PROFILES=1 \
  homeboy bench my-rust-component --iterations 5
```

The profile scenarios are documented in `rust/docs/bench.md` and include clean
build, warm build, changed-file check, and test build timings.

## Reporting Contract

`homeboy bench` already supports generic scenario metadata, so the Rust bench
runner records active toolchain state under `metadata.rust_toolchain` and each
Rust profile scenario records the same data under `scenario.metadata.rust_toolchain`.

Current generic metadata coverage is bench-only. There is no generic Homeboy
contract for attaching extension-owned environment/toolchain metadata to lint or
test result envelopes, so the Rust extension cannot surface sccache/linker state
there without adding Rust-specific behavior to core.
