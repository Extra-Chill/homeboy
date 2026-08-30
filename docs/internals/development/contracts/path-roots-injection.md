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

A crate mid-migration tends to grow ambient/rooted sibling pairs. This is the
shape `homeboy-release` carried before #12746, quoted as it stood then --
neither function exists in this form today, which is the point:

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

## Migrate top-down, never bottom-up

The instinct is to attack the crate with the most ambient calls. That is
backwards, and the codebase punishes it twice over.

An ambient wrapper can only be deleted when its callers can supply a root. If
they cannot, "converting" the callee just relocates the resolution outward and
multiplies it:

| target | ambient calls in the callee | call sites that would have to resolve instead |
|---|---|---|
| `config::*` entity CRUD | 17 | **61** |
| `defaults::load_config` | 6 | **113** |
| `OperationRecordStore` | 3 | 14 |

Converting `config.rs` today would turn 17 ambient resolutions into 61. That is
not progress measured badly; it is a regression.

So the ordering law is:

```text
process boundary  (main.rs -> CliRuntime::run_from_args)
      |  resolve here first
      v
command entry     (commands/<name>::run)
      v
subsystem entry   (orchestrator::run_with_plan, workflow::run_command_*)
      v
stateful stores   (OperationRecordStore, ReleaseWorkspace)
      v
leaf helpers / core primitives   (config::*, defaults::*, paths::*)
      ^  delete these wrappers LAST
```

Each layer can only shed its wrappers after the layer above it holds roots.
`OperationRecordStore` demonstrated this: it was untouchable until
`workflow::run_command_with_workspace` resolved, and then it fell in one pass.

`homeboy-core` therefore looks like the biggest prize (96 ambient calls) and is
actually the **last** work, not the first. Its `_in_root` siblings already
exist for essentially every ambient function; nothing is blocked on designing
them. What is blocked is the callers.

### How to pick the next increment

The readiness test is **not** how many ambient calls a file has, and it is not
how many callers those wrappers have. It is:

> Does a root already reach the callee — either because a carrier struct passes
> through it, or because its caller is itself a boundary?

Measure the **depth from the wrapper to the nearest thing that holds, or can
trivially hold, a root**:

| depth | meaning | example | verdict |
|---|---|---|---|
| 0 | a carrier struct already threads through the callee | `ReleaseExecutionContext`, `ReleaseWorkspace` | ready |
| 1 | the wrapper's caller is a command or process entry | `crates/homeboy-cli/src/commands/deferred_workload.rs` | ready |
| many | intermediate functions with no other reason to know about paths | `crates/homeboy-cli/src/commands/agent_task/fanout.rs`, below | **not ready** |

`crates/homeboy-cli/src/commands/deferred_workload.rs` was depth 1: five wrappers,
one caller each, and
those callers were `run()` (a command entry), `cli_runtime.rs` (the process
boundary itself), and one sibling command. All five collapsed in one pass.

### The counter-example: agent_task/fanout.rs

Fanout looks like an easy target by every surface metric — two ambient wrappers,
and only two production callers between them. It is not ready, and the reason is
depth:

```text
fanout()                                  <- the only real boundary
  -> cook_batch / run_batch_cook_fanout
    -> cook_batch_inner / run_batch_cook_fanout_plan
      -> run_batch_cook_fanout_plan_with_executor
        -> run_batch_cook_fanout_plan_with_executor_claim
          -> claim_fanout_run_batch_coordinator
            -> persist_fanout_run_batch_record
              -> secure_batch_plan_execution
                -> private_batch_plan_path        <- the ambient call
```

Rooting this means adding a `data_root: &Path` parameter to eight functions
whose job has nothing to do with paths, plus a `pub(crate)` signature with an
external caller in `crates/homeboy-cli/src/commands/infra/route.rs` — to remove
**one** duplicate
resolution per command. That is the definition of adding plumbing.

Leave it. Either a carrier appears as the layer above is rooted, or the
duplicate resolution falls out when one does. **Low caller count is not low
depth, and only depth predicts whether the change is net-negative.**

What *was* safe to take here: `private_batch_plan_dir()` had exactly one caller
and it was a test. A wrapper whose last non-test caller has already disappeared
is dead weight regardless of depth — delete it on sight.

## Dead wrappers: direction matters

Two different things look identical to a reachability scan, and only one is debt.

**A dead *ambient* wrapper is debt.** It resolves from process state, nothing
calls it, and `pub` items are not dead-code analysed so the compiler will never
say so. Delete on sight.

**A dead *rooted* sibling is unused capacity.** It takes an explicit root, and
nothing calls it *yet* because the callers above it have not been rooted. It is
ahead of demand, not behind it. Deleting it means re-adding the same five lines
when that layer's turn comes.

`extension_store` has one of each, side by side:

| function | callers | verdict |
|---|---|---|
| `is_extension_linked` (ambient) | 20+ | live |
| `is_extension_linked_in_root` (rooted) | **0** | **keep** — it is the injection point core's extension subsystem ambient sites will need |
| `broken_extension_links` (ambient) | 1, a test | **delete** |
| `broken_extension_links_in_root` (rooted) | 0 → 1 | keep, test now calls it |

Before deleting anything a scan calls dead, check which half it is.

### Scanning for them correctly

A wrapper delegates to its rooted sibling in one of two argument shapes, and a
pattern that only knows the first will silently under-report:

```rust
let root = paths::homeboy()?;          // shape A: resolve, then call
foo_in_root(&root, id)

foo_in_root(&paths::homeboy()?, id)    // shape B: resolve inside the call
```

The first sweep of this codebase only matched shape A and reported the seam
closed. Re-scanning for both found three more dead wrappers, including one in
`config.rs`. Match both, and search `crates/`, `src/`, **and** the repo-root
`tests/` tree — files there are pulled into libs by `#[path]` and a scan scoped
to `crates/` reads as a false zero for anything they reference.

Also beware module-qualified shadowing. `gh_actions.rs` has a private
`mod helpers` exporting its own rooted `list_runs_cache_path`, so unqualified
call sites resolve to that rather than to the same-named ambient function in
`gh_actions_cache`. Confirm with a module-qualified grep before believing a
caller count.

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

## What actually gates `with_isolated_home`

Ambient-resolution counts are the intermediate measure. The number #7505 asks
about is `with_isolated_home`: ~2,529 call sites, each taking a process-global
mutex, which is why the suite serializes.

A test can drop the wrapper only when **everything it reaches** takes explicit
roots. That is a transitive property, and it is not visible from the test body.
Measuring which core helper each block depends on, over 2,322 analysable
blocks:

| blocker referenced directly | blocks | share |
|---|---|---|
| `ObservationStore::open_initialized` | 359 | 15% |
| `engine::temp` / `RunDir::create` | 93 | 4% |
| `defaults::load_config` / `save_config` | 57 | 2% |
| `paths::*` directly | 51 | 2% |
| `component`/`project`/`server` loaders | 25 | 1% |
| `config::*` entity CRUD | 3 | 0% |
| **no direct reference** — reaches one transitively | **1,764** | **76%** |

Two things follow.

**`ObservationStore::open_initialized` is the single highest-leverage target.**
It is referenced directly by 359 test blocks and has 504 call sites overall.
Nothing else comes close.

**The 76% is the real shape of the problem.** Most tests do not name a blocker;
they call something that calls something that resolves. `RunDir::create()` is
the clearest example — it looks inert, and it reaches
`engine::temp::ensure_runtime_tmp_dir` → `paths::homeboy_data()`. Every test
that builds a `RunDir` is pinned to the ambient home by a call two frames down
with no path in its name.

### Do not bulk-edit off a scan here

A regex over test bodies cannot answer "does this reach the filesystem through
an ambient root". Both directions fail:

- **False positives.** A body mentioning `store` may only be using an injected
  one.
- **False negatives.** A test calling `install_for_component(...)` names no
  blocker and writes half the config root.

A scan of the extension subsystem and `homeboy-deploy` for blocks with "no
blocker" returned 97 candidates, and spot-checking found tests that plainly
install extensions into `<config>/extensions`. The list was noise.

Free a test only when the call graph proves it, one at a time. #12774 freed two
`OperationRecordStore` tests on that basis: every call went through a store
bound to a data root, and every path it touched was a `*_in_roots` join below
it. That is the standard of proof this needs — the same standard that keeps
being right about dead wrappers, and the same one that catches the scans.

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
