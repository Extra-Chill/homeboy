# Agent Task Generic Loop Contract

Homeboy owns the generic loop, action, artifact, evidence, provider-outcome, and diagnostic-ranking contracts. Runtime providers own their runtime-package execution details and report them through the generic outcome and evidence fields.

Versioned schema identifiers are exported by `homeboy agent-task contract`:

- `homeboy/agent-task-loop-action/v1`
- `homeboy/agent-task-artifact-declaration/v1`
- `homeboy/agent-task-artifact-handoff/v1`
- `homeboy/agent-task-provider-outcome-contract/v1`
- `homeboy/agent-task-evidence-ref/v1`
- `homeboy/agent-task-diagnostic-ranking/v1`

Contract fixtures live under `tests/fixtures/agent_task_contract/` and intentionally use neutral provider/runtime names. They cover successful required artifact handoff, structurally valid provider outcomes with nested runtime import diagnostics, local file evidence refs, and generic missing required artifact symptoms.
