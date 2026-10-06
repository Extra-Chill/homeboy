# Canonical execution budgets and non-mutating plan reads

Trackers: #13753, #13755.

## Root cause and named deletion

Base `38bbde660e` writes version-1 execution budgets, but omitted authored
budgets deserialize as version 0. Lifecycle admission/read validation accepts
v0, and an execution-only plan reader acquires the config lock and rewrites
the plan to upgrade it. The supported reader and writer therefore maintain a
second budget contract and a read-time mutation solely for old plans.

Delete `legacy_execution_budget`, `AgentTaskExecutionBudget::migrate_legacy`,
`migrate_execution_budget`, and both execution-only controller-plan store
reader layers. Inspection and execution now use the same rooted validating
reader. Actual plan writes and queued claims still retain their existing locks.
Two unnecessary plan clones disappear from those writes.

Production accounting: **14 added / 84 removed, net 70 fewer physical lines**.
Tests: 47 added / 29 removed; four command-documentation lines added. Evidence
documentation is counted separately. No adapter, migration or store is added.

## Supported behavior and intentional break

Explicit budgets require `version: 1`; v0 and unsupported versions are rejected
instead of restamped. Omitted authored budgets use the existing Rust schedule
default, now also the JSON default, without rewriting persisted bytes. Provider
execution/retry/rotation ceilings and absolute deadlines retain their semantics.

This intentionally retires backward compatibility for v0/unversioned explicit
budgets and the execution-specific store reader. It does not reset or migrate
old state. Service execution entry points resolve aliases in their injected
store and then use the canonical read path.

## Real-store proof

The existing budget fixtures now verify actual persisted plan behavior:

- Inspection and execution readback of omitted authored defaults produce the
  same current budget and leave the exact stored bytes unchanged.
- Version 0 and 99 fail both reads and plan writes; the file is unchanged.
- A supplied budget missing its version fails execution readback without a
  rewrite.

Overlaying only these test files on unchanged base `38bbde660e` fails both
focused cases: the base yields a v0 default and accepts an explicit v0 budget.
Nextest `09849516-d5a9-4092-85e2-310f39295abf`: 0 passed, 2 failed,
2,759 skipped.

## Candidate verification

Exact production/test source: `24728bfb9d`. Direct Linux verification uses the
default stack (`RUST_MIN_STACK` unset), eight build jobs, and a disk-backed
temporary directory with capacity reserve gates intact.

```sh
cargo fmt --all --check
env -u RUST_MIN_STACK CARGO_BUILD_JOBS=8 cargo nextest run \
  -p homeboy-lab-contract -p homeboy-agents -p homeboy-cli \
  -E 'package(homeboy-lab-contract) | test(agent_task_lifecycle) | test(agent_task_scheduler) | test(cook) | test(execution_budget) | test(commands::agent_task)' \
  --no-fail-fast --status-level fail --final-status-level fail
git diff --check
```

**2,171 passed, 3,801 skipped**, Nextest
`eeab952a-8d23-49a7-9c3f-79b27ca032b7`. This is scoped consumer verification,
not a full repository suite. Existing tests exercise retries, rotations,
deadline containment, queued admission, durable lifecycle recovery and Cook
continuation. Authoritative differential CI is required before merge.

## Execution and AI provenance

Implementation/finalization occurs outside Homeboy coding orchestration under
Chris Huber's explicit simplification authorization, in an isolated
tracker-linked worktree with direct Lab verification. OpenAI
`openai/gpt-6.1-sol` via OpenCode traced producers/callers, implemented and
reviewed the consolidation, updated the meaningful real-store fixtures, and
ran the recorded baseline/candidate checks.
