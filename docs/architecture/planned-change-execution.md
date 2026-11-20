# Planned Change Execution

Homeboy workflows use `HomeboyPlan` to describe planned work while retaining
command-specific execution results and CLI behavior.

## Plan-Step Execution

`crates/homeboy-core/src/execution.rs` provides shared dispatch helpers:

- `execute_plan_steps()` dispatches non-disabled, non-skipped steps.
- `execute_plan_steps_filtered()` additionally accepts a caller-defined skip
  predicate. A dispatcher returning `None` marks a plan-only step; a stop
  predicate decides whether a produced result blocks the rest of the plan.
- `PlanExecutionRun` collects workflow-specific results and whether execution
  stopped early.
- `executable_plan_step_needs()` returns only execution dependencies. Steps
  with `needs_kind: display_order` retain stable presentation ordering without
  blocking independent executable steps.

Release and review workflows use these helpers without projecting their results
into a separate generic lifecycle envelope.

## Change Artifacts

[`change_artifact::ChangeArtifact`](../../crates/homeboy-core/src/change_artifact.rs)
contains the runner/Lab change payload, including its source snapshot, patch or
file delta, provenance, and digest. See [Apply and publish contract](apply-publish-contract.md)
for the local mutation boundary.

`execution::ChangeArtifactProvenance` remains a separate provenance record used
by release and changelog guards; it is not a change artifact envelope.

## Command Behavior

Plan, dry-run, capture, apply, and execute behavior remains command-specific.
Operator confirmations authorize conditions rather than selecting an execution
mode: for example, `deploy --confirm-dangerous` authorizes a non-canonical source
mode, while deploy executes by default.
