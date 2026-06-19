# Planned Change Execution

Homeboy's planned-change vocabulary is the shared core language for work that
starts as a plan and may eventually mutate files or publish external state.

The lifecycle is:

```text
plan -> execute -> artifact -> approve -> apply -> publish
```

## Core Contract

The contract types live in `src/core/execution.rs` and are intentionally usable
without changing existing command JSON output shapes.

- `ExecutionRequest` captures the subject, selected `ExecutionMode`, optional
  `HomeboyPlan`, approval scope, inputs, and policy.
- `ExecutionRun` captures the run status, step results, produced artifacts,
  warnings, and metadata.
- `ExecutionStepResult` records one executed step without forcing each command
  to expose a new public output envelope.
- `ChangeArtifact` records a proposed or captured change with provenance,
  files, diff/path data, approval scope, and metadata.
- `ApplyRequest` and `ApplyResult` describe local worktree mutation.
- `PublishRequest` and `PublishResult` describe durable externalization such as
  commits, pushes, pull requests, releases, and deploys.

## Mode Mapping

Existing command flags keep their command-specific behavior and output. Internally
they can map to `ExecutionMode` when a caller needs shared planned-change
semantics:

| Existing CLI vocabulary | `ExecutionMode` | Meaning |
| --- | --- | --- |
| `--plan`, `preview` | `plan` | Describe intended work without running executable steps. |
| `--dry-run`, `dry-run` | `dry_run` | Run far enough to preview effects without durable writes. |
| `--capture-patch`, `capture-patch` | `capture_patch` | Execute and preserve proposed file changes as artifacts. |
| `--write`, `--apply`, `write`, `apply` | `apply` | Materialize an approved or command-permitted change locally. |
| `--execute`, `run`, `execute` | `execute` | Execute the requested workflow directly. |

Use `ExecutionMode::from_cli_value()` for value-style inputs. Boolean flag
handlers should map their selected command behavior to the same mode values at
the call site when they adopt the contract.

## Existing Workflow Mapping

- Release planning remains represented by `HomeboyPlan`; release run results can
  project into `ExecutionRun` and `ChangeArtifact` without changing release CLI
  JSON.
- Plan steps use `needs` for stable plan ordering and dependency display. When an
  edge is only presentational, set `needs_kind: display_order`; bounded parallel
  executors should use `executable_plan_step_needs()` so only true execution
  dependencies block independent read-only steps.
- Runner/Lab patch capture can project captured patches or deltas into
  `ChangeArtifact`; local mutation belongs in `ApplyResult`.
- Refactor write paths can treat `--write` as `ExecutionMode::Apply` while
  preserving their existing command outputs.
- Sample Runtime-style adapters should split local file mutation from publish steps:
  apply verifies and writes files, while publish commits, pushes, opens pull
  requests, releases, or deploys.

## Extension Guidance

Extensions do not need to adopt every type at once. A useful first slice is:

1. Accept or emit `ExecutionMode` when a command supports plan, dry-run,
   capture, apply, or execute behavior.
2. Emit `ChangeArtifact` for proposed file changes with enough provenance to
   identify the source run, step, command, and captured snapshot.
3. Return `ApplyResult` when an approved artifact mutates a local worktree.
4. Return `PublishResult` only for post-apply externalization.

This keeps the core vocabulary consistent for fleet cooking while allowing each
command and extension to preserve current public JSON contracts during migration.
