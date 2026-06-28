# Headless Agent Orchestration

Homeboy is useful for agents because it turns engineering work into durable, inspectable command contracts. Agents can run the same workflows humans and CI use, inspect structured evidence, and hand work back with reviewer-readable proof.

## Why Agents Need This

Agent work breaks down when every repo has a different way to test, review, benchmark, release, or prove a result. Homeboy gives agents a common surface:

```bash
homeboy review --changed-since origin/main --output review.json
homeboy runs show <run-id>
homeboy agent-task review <run-id>
homeboy manifest
```

The terminal output stays useful for humans. The JSON and run artifacts stay useful for agents, CI, dashboards, and scheduled automation.

## Headless By Default

Homeboy workflows do not require a chat session or interactive terminal to be authoritative. Durable ids carry the work:

- Agent-task run ids.
- Controller or loop ids.
- Persisted run ids.
- Artifact ids.
- Worktree and branch names.
- PR, issue, and event keys.

This lets a terminal operator, GitHub Action, cron job, runner daemon, or chat bridge observe the same state and resume the same workflow.

## Exponential Engineering

Homeboy contributes to exponential engineering by making parallel work comparable and composable:

- Many branches can run the same review gate.
- Many agents can produce the same patch/evidence envelope.
- Many runners can execute hot workloads without overloading the controller.
- Many projects can be inspected through the same status/deploy/fleet surface.
- Many artifacts can be compared without scraping logs.

The multiplier is not “more prompts.” The multiplier is stable contracts: every agent can cook, verify, retry, report, and hand off using the same evidence shapes.

## The Agent Loop

A healthy agent loop looks like this:

1. Resolve component, task, workspace, and base ref.
2. Run or dispatch work through `agent-task cook`, `agent-task run-plan`, or a controller.
3. Promote patch artifacts into a managed worktree.
4. Run deterministic gates with `review`, `test`, `bench`, `trace`, or custom extension commands.
5. Record JSON output, persisted runs, artifacts, and evidence refs.
6. Retry, request changes, mark human-ready, or finalize a PR through durable state.

## What Core Owns

Homeboy core owns generic orchestration:

- Command safety and manifest metadata.
- Structured JSON envelopes.
- Runs, artifacts, and evidence references.
- Runner routing and resource policy.
- Agent-task lifecycle, controller state, retries, and handoffs.
- Component/project/fleet scope resolution.

## What Extensions Own

Extensions own domain behavior:

- Language/framework lint, test, build, and release commands.
- Platform CLIs such as Cargo, WP-CLI, package managers, or cloud tools.
- Domain-specific sidecars, fuzz workloads, trace scenarios, and deploy verification.
- Runtime setup and readiness checks.

This boundary keeps Homeboy generic while still making real projects operable.

## What Reviewers Get

Reviewer-facing output should include:

- The command that produced proof.
- The base ref or target environment.
- The JSON output path or artifact link.
- The run id or controller id.
- Any gate failures and deep-dive commands.
- A clear human-ready or changes-requested state.

## Start Here

- [Review a branch](../workflows/review-a-branch.md)
- [Capture evidence](../workflows/capture-evidence.md)
- [Run agent task loops](../workflows/run-agent-task-loops.md)
- [Set up Lab runners](../workflows/set-up-lab-runners.md)
- [Set up extensions](../workflows/set-up-extensions.md)
