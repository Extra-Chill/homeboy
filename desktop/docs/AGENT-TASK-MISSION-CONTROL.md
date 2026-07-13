# Agent-Task Mission Control

Homeboy Desktop is a visual control surface for Homeboy-owned `agent-task` lifecycle records. It is not a chat client and does not execute providers, manage task state, or apply patches itself.

## Contract Boundary

The Agent Tasks workspace calls `homeboy agent-task contract`, `providers`, `list`, `status --full`, `logs --full`, `artifacts --full`, `submit`, `run-plan`, `cancel`, `retry`, `resume`, and `promote --dry-run` through the structured CLI result envelope. Contract data is retained as `JSONValue`, so new Homeboy fields appear in the UI without Desktop maintaining duplicate command or lifecycle models.

## Implemented Workflow

- Build a mission from a goal, newline-delimited source references, workspace, provider backend, concurrency, attempts, and JSON policy.
- Queue with `submit` or run with `run-plan`; Homeboy persists the durable run and owns provider dispatch.
- Select a runner, inspect its Homeboy-backed run list, and view full lifecycle, fanout, event, failure, timeout/no-op, artifact, evidence, replay, and diagnostic payloads.
- Cancel, retry, or resume through the corresponding Homeboy lifecycle command.
- Dry-run promotion of the selected durable run into a managed worktree with an optional deterministic verification command.

## Core Blocker

The local daemon currently exposes generic `/jobs` and `/runs/{id}/artifacts` endpoints, not a stable agent-task endpoint family for provider discovery, durable run listing, cursor-based events, lifecycle actions, promotion, or runner capacity. Desktop therefore uses the typed CLI contract. A Homeboy core daemon API must provide those agent-task operations before this workspace can replace CLI polling with live daemon updates.

Desktop composes the v1 plan using Homeboy's checked-in fixture shape: `plan_id`, request `task_id`, structured `source_refs`, `workspace`, and `policy`, plus `options.max_concurrency` and `options.retry.max_attempts`. A non-mutating `validate-plan` command would still let Desktop prove a draft is admissible before it creates a durable run.
