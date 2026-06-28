# Workflows

Workflow docs are task-oriented. They explain which Homeboy commands to use together and link to exact command reference when you need flags, schemas, or output details.

## Common Workflows

- [Review a branch](review-a-branch.md) - run the scoped audit, lint, and test umbrella reviewers care about.
- [Reproduce CI](reproduce-ci.md) - run declared CI profiles and classify baseline-versus-head outcomes.
- [Capture evidence](capture-evidence.md) - collect benchmark, trace, fuzz, and persisted run artifacts for humans and agents.
- [Use runners](use-runners.md) - route hot commands through configured runners and inspect runner health.
- [Set up Lab runners](set-up-lab-runners.md) - configure runner execution targets, readiness, secrets, and proof-capable offload.
- [Set up extensions](set-up-extensions.md) - install extension behavior and understand the core/extension contract boundary.
- [Run agent task loops](run-agent-task-loops.md) - operate durable multi-agent loops, controllers, events, retries, and human handoffs.
- [Manage local environments](manage-local-environments.md) - operate rigs, combined-fixes stacks, and task worktrees.
- [Release a component](release-a-component.md) - plan and apply releases from component metadata and commit history.
- [Deploy and operate fleets](deploy-and-operate-fleets.md) - inspect project targets, preview deploys, and operate fleets safely.

## Related Reference

- [Command index](../commands/commands-index.md)
- [JSON output contract](../architecture/output-system.md)
- [Persisted runs](../commands/runs.md)
- [Runner command](../commands/runner.md)
- [Agent task command](../commands/agent-task.md)
- [Rig command](../commands/rig.md)
- [Deploy command](../commands/deploy.md)
