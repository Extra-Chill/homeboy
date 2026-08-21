# PathRoots injection contract

`homeboy_core::paths::PathRoots` names one Homeboy home: a config root, a data
root, and an artifact root resolved together. Its own doc states the contract:

> Stateful services receive this value instead of repeatedly consulting
> process-global environment and overrides. This lets independent in-process
> workloads own distinct Homeboy state without serializing path resolution.

This page describes how to satisfy that contract in a crate that does not yet,
using `homeboy-release` as the worked example (#7505).

## Why ad hoc resolution is a defect, not a style preference

`PathRoots::from_environment()` reads process-global state. Two calls are two
independent answers. Any operation that resolves more than once is therefore
one operation spread across two possibly-different homes, and the failure mode
is silent: bytes land in one home while the record describing them lands in
another.

Concretely, before this change a single release resolved roots at least three
separate times:

```
release run
├── package step      -> from_environment()   artifacts + failed-attempt record
├── cleanup step      -> from_environment()   read record, acknowledge record
└── deploy step       -> from_environment()   save / clear recovery checkpoint
```

Nothing tied those three answers together. The same property is what forces
every test through `HomeGuard`'s process-global mutex: if a path can only be
found by reading the environment, then isolating a test means mutating the
environment, and mutating process-global state means no two tests can run at
once.

## The shape

**Resolve once at a named boundary. Pass the value down. Never resolve in a
step, a helper, or a store method.**

A boundary is a function that begins a unit of work a user asked for. In
`homeboy-release` there are exactly four:

| Boundary | Unit of work |
|---|---|
| `orchestrator::run_with_plan` | a release |
| `workflow::run_dry_run_preflights`' caller | a dry run |
| `package_recovery::package_existing_tag` | repackaging a published tag |
| `deployment::resume_deployment` | resuming a checkpointed deploy |

Everything below a boundary takes what it needs as a parameter — `&PathRoots`
when it needs more than one root, `&Path` when it needs exactly one:

```rust
// boundary: the one resolution for the whole release
let roots = homeboy_core::paths::PathRoots::from_environment()?;

// carrier: threaded onto the context every step already receives
let mut context = ReleaseExecutionContext { roots: roots.clone(), .. };

// step: reads the carrier, never the environment
executor::run_cleanup(context.roots.data(), context.component, &context.state)
```

Prefer threading onto a struct a callee already receives over adding a
parameter to a long chain. `ReleaseExecutionContext` already carried
`component`, `extensions`, and `options` to every step, so adding `roots`
reached ten call sites through one field.

## Collapse the pair; do not keep it

A crate mid-migration tends to grow ambient/rooted sibling pairs:

```rust
fn run_cleanup(component: &Component) -> Result<T> {                 // ambient
    run_cleanup_in_roots(PathRoots::from_environment()?.data(), component)
}
fn run_cleanup_in_roots(data_root: &Path, component: &Component) -> Result<T> { .. }
```

The pair is scaffolding, not an API. It is load-bearing only while some caller
still cannot supply a root. **When the last such caller converts, delete the
ambient wrapper and give the rooted function the plain name.** The end state is
one function that takes a root, not two functions that differ by a suffix.

That deletion is what makes this work net-negative. A change that only adds
`_in_roots` variants has added plumbing; a change that removes the wrapper has
removed an ambient resolution point.

The `_in_root(s)` suffix stays legitimate in one place: `homeboy_core::paths`
itself, where both forms are genuinely public API (`observation_db()` and
`observation_db_in_root()`). Elsewhere it should be temporary.

### Watch for return types that hide the failure

`execute_deployment` returned an unwrapped `ReleaseDeploymentResult`, so it
could not have called `from_environment()?` itself. The ambient
`save_recovery`/`remove_recovery` wrappers existed precisely to absorb that
`Result` out of sight. An ambient wrapper hanging off an infallible function is
a strong signal that resolution belongs further up.

## Tests are a boundary too

Tests that construct a carrier by hand, or call a rooted function directly,
occupy the position the real boundary holds. Resolve once in a local helper:

```rust
/// The isolated home each test below installs, named as roots.
fn test_roots() -> homeboy_core::paths::PathRoots {
    homeboy_core::paths::PathRoots::from_environment().expect("path roots")
}
```

This is not a loophole. The test genuinely is the entry point for its unit of
work. What matters is that the *production* path below it resolves nothing.

## Reporting progress

Raw `from_environment()` counts are a misleading headline, because a correct
boundary is also a call. Decompose instead:

- **ambient resolution points** — calls inside steps, helpers, and stores.
  This is the number that must reach zero.
- **boundary resolutions** — one per named unit of work. This number is
  supposed to be small and non-zero.
- **test helpers** — one per test module.

`homeboy-release` is the completed reference. It took two increments:

| | start | after #12746 | after store conversion |
|---|---|---|---|
| ambient | 7 | 3 | **0** |
| boundaries | 2 | 4 | 3 |
| test helpers | 1 | 4 | 8 |

Boundaries *decreased* in the second increment, which is the shape to expect
near the end: `deployment::resume_deployment` and the dry-run preflight entry
stopped resolving and started receiving, because a caller above them had roots
to give. A boundary is only a boundary while nothing above it can supply one.

The number that actually matters is resolutions per invocation. A
provider-staged `homeboy release` performed **eight** independent resolutions —
seven of them inside `OperationRecordStore`, one in the orchestrator — and now
performs exactly **one**, at `run_command_with_workspace`.

## Stateful stores

A store with associated (`Self::`) methods that each resolve a root is the
worst version of this defect, because the per-call resolution is invisible at
the call site. `OperationRecordStore::update(owner, f)` looks free; it was a
filesystem-root rediscovery.

Give the store the root at construction:

```rust
pub struct OperationRecordStore {
    data_root: PathBuf,
}

impl OperationRecordStore {
    pub fn in_roots(roots: &paths::PathRoots) -> Self { .. }
    pub fn update(&self, owner_run_ref: &str, ..) -> Result<OperationRecord> { .. }
}
```

Then hold the store — not the roots — on whatever already models the lifetime
of the work. `ReleaseWorkspace` gained a `store` field because provisioning and
finalization write the same record from different phases of one release; the
struct that spans both phases is the right owner.

Converting a store is mechanical but wide: 14 production call sites and 26 test
call sites here. Two collision shapes cost time and are worth pre-empting:

- **Indentation substring collisions.** A literal anchor of `        foo(` also
  matches a `            foo(` line, because the 12-space text is a substring of
  the 16-space line. Anchor on `\n` + indent, or on a unique neighbouring line.
- **Same call at the same indent in production and tests.** Anchor on the
  preceding statement, not the call itself.

Assert the expected match count before writing, and write the whole file only
after every assertion passes. Both collisions above surfaced as a failed
assertion rather than a bad edit.

## Verifying

`cargo check --workspace --tests` is the gate. The two errors this work
reliably produces are:

- `E0063` — a struct gained a `roots` field and test initializers do not set it.
  Expect one per hand-built carrier; there were 25 in `execution_dispatch.rs`.
- `E0061` — a collapsed function now takes an extra leading argument.

When scripting the call-site edits, assert the expected match count before
writing. A pattern like `run_deployment_step\(\n` matches the definition as
well as the calls, and the assertion is what catches it.
