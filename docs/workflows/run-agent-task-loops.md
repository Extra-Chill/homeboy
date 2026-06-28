# Run Agent Task Loops

Use agent-task loops when work needs durable orchestration across multiple agent runs, retries, gates, artifacts, external events, and human handoffs. A loop is not a one-shot prompt. It is persisted controller state that can be inspected, resumed, stopped, and reconciled over time.

## Use This When

- A task needs more than one agent handoff or phase.
- Work should survive process restarts, chat disconnects, or runner handoffs.
- A controller needs to wait for CI, PR review, human approval, artifact availability, or a child controller.
- A loop should retry, request changes, run gates, or mark work human-ready based on durable state.

Use [Review a branch](review-a-branch.md), [Capture evidence](capture-evidence.md), or [Use runners](use-runners.md) for one-command workflows. Use loops when the workflow itself has state.

## 1. Choose The Loop Shape

Homeboy exposes three related surfaces:

- `agent-task cook`: one-shot PR cooking with promotion, gates, retries, commit, push, and PR finalization.
- `agent-task loop`: named durable loop definitions with on/off state, revolution limits, resume, and stop controls.
- `agent-task controller`: lower-level durable controller state for multi-agent loops, events, actions, waits, retries, gates, and human handoffs.

Use `cook` for one issue or one PR. Use `loop` when you have a named repeating workflow. Use `controller` when a workflow has explicit actions, events, waits, nested controllers, or policy results.

## 2. Run A One-Shot Cook

Start with `cook` when the goal is one branch and one reviewable PR:

```bash
homeboy agent-task cook \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --to-worktree homeboy@fix-issue \
  --task-url https://github.com/Extra-Chill/homeboy/issues/123 \
  --verify "homeboy test homeboy" \
  --prompt @task.txt
```

After the run, inspect the durable review envelope:

```bash
homeboy agent-task review <run-id> --to-worktree homeboy@fix-issue
homeboy agent-task status <run-id>
homeboy agent-task logs <run-id>
```

Use this path before designing a loop. It proves the provider, workspace, promotion, and deterministic gates work for the repo.

## 3. Define A Durable Loop

Use `agent-task loop` for a named workflow that can be turned on, resumed, and stopped:

```bash
homeboy agent-task loop define @.github/homeboy/controllers/site-loop.json \
  --on \
  --revolution-limit 5

homeboy agent-task loop status site-loop
homeboy agent-task loop resume site-loop
homeboy agent-task loop stop site-loop
```

Use `--off` to register or update loop state without executing handoffs. Use `--on --resume` when the operator wants to initialize and immediately run pending handoffs.

## 4. Run A Controller From A Spec

Use `controller run-from-spec` for bounded headless loop execution. This is the stable primitive for callers that have a complete controller spec and want Homeboy to execute a limited number of pending actions:

```bash
homeboy agent-task controller run-from-spec @controller.json \
  --max-actions 5
```

The command materializes the spec, initializes durable controller state when needed, executes up to the requested action budget, and returns one persisted status envelope with action results and lineage.

Use bounded execution deliberately. A loop should stop because there are no executable actions, the action budget is reached, a terminal controller state is reached, or the controller is waiting for an external event.

## 5. Apply External Events

Controllers are designed to wait for outside facts. Apply events such as CI completion, PR review, human merge, scheduled wakeups, or artifact availability:

```bash
homeboy agent-task controller events <loop-id> \
  --event-type github.pr.merged \
  --event-key Extra-Chill/homeboy#123 \
  --entity-id pr:123 \
  --payload @event.json
```

Use stable event keys so replays and resumed controllers do not duplicate work.

## 6. Mark Human-Ready Handoffs

Long-running loops should make human handoffs explicit:

```bash
homeboy agent-task controller mark-human-ready <loop-id> \
  --entity-id pr:123 \
  --reason "gates passed and review approved"
```

This records the reason in controller state instead of relying on a chat transcript or terminal scrollback.

## 7. Retry Or Request Changes Through The Controller

Use controller actions for retries and change requests rather than ad hoc reruns. The controller records parent/child run lineage and normalized feedback artifacts so downstream tools can explain what happened.

Typical loop policy actions include:

- `spawn_task`
- `fan_out`
- `spawn_controller`
- `wait_for_controller`
- `retry`
- `request_changes`
- `run_gates`
- `wait_for_event`
- `mark_human_ready`
- `complete`
- `abandon`
- `escalate`

Actions with deterministic `dedupe_key` values are recorded once, so replaying a resumed controller does not duplicate already-open tasks, child controllers, or PR work.

## 8. Keep Loop Evidence Durable

For every loop, preserve these identifiers in the issue, PR, run artifact, or operator notes:

- Loop id or controller id.
- Agent-task run ids spawned by the loop.
- Worktree or branch names for promoted patches.
- Gate commands and their result artifacts.
- Event keys applied to the controller.
- Human-ready reasons or escalation reasons.

Inspect state through Homeboy rather than through provider-local state:

```bash
homeboy agent-task controller status <loop-id>
homeboy --output homeboy-results/controller-status.json agent-task controller status <loop-id>
homeboy agent-task review <run-id>
homeboy runs show <run-id>
```

## Reference

- [agent-task command](../commands/agent-task.md)
- [Agent task generic loop contract](../architecture/agent-task-generic-loop-contract.md)
- [Use runners](use-runners.md)
- [Capture evidence](capture-evidence.md)
