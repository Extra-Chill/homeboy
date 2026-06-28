# Start Here

Homeboy is a component-aware automation CLI. It gives local developers, CI jobs, scheduled automation, and coding agents one consistent way to run checks, capture evidence, and operate projects.

If you only remember one thing: Homeboy turns common engineering workflows into repeatable commands with structured evidence. Start with `review`, then add CI profiles, persisted run artifacts, runners, and releases as your workflow needs them.

## 1. Run A Local Review Gate

From a repository with Homeboy configuration, run:

```bash
homeboy review --changed-since origin/main
```

`review` is the PR-shaped umbrella for scoped `audit`, `lint`, and `test` checks. Use individual commands when you need to focus on one stage:

```bash
homeboy audit
homeboy lint
homeboy test
homeboy build
```

## 2. Add Portable Repo Config

The smallest useful `homeboy.json` identifies the component and the extension that knows how to operate it:

```json
{
  "id": "my-project",
  "extensions": {
    "rust": {}
  }
}
```

Homeboy core stays generic. Extensions provide ecosystem behavior such as Cargo, WP-CLI, Node.js, GitHub releases, package managers, and platform-specific test/lint commands.

## 3. Produce Structured Evidence

Most commands can write JSON evidence while still printing human output:

```bash
homeboy review --changed-since origin/main --output homeboy-results/review.json
homeboy bench my-project --output homeboy-results/bench.json
homeboy runs show <run-id> --output homeboy-results/run.json
```

That JSON is the handoff point for CI, scheduled automation, and coding agents.

## 4. Pick Your Next Path

- I want to review a branch: [Review a branch](workflows/review-a-branch.md)
- I want to reproduce CI: [Reproduce CI](workflows/reproduce-ci.md)
- I want benchmark, trace, fuzz, or run artifacts: [Capture evidence](workflows/capture-evidence.md)
- I need runner/offload behavior: [Use runners](workflows/use-runners.md)
- I need release automation: [Release a component](workflows/release-a-component.md)
- I need the mental model: [Concepts](concepts/index.md)
- I need exact CLI or config details: [Reference](reference/index.md)
- I maintain Homeboy: [Internals](internals/index.md)
- I am following an operator runbook: [Operations](operations/index.md)

## 5. Use Embedded Docs

The same docs are embedded in the binary:

```bash
homeboy docs list
homeboy docs index
homeboy docs commands/commands-index
```

## What To Read Next

Read by task, not by command name:

- Start with [Review a branch](workflows/review-a-branch.md) for the core quality gate.
- Add [Reproduce CI](workflows/reproduce-ci.md) when the component declares CI profiles.
- Add [Capture evidence](workflows/capture-evidence.md) when a result must be consumed by a reviewer, CI job, or agent.
- Add [Use runners](workflows/use-runners.md) when the command should run off the controller machine.
- Add [Release a component](workflows/release-a-component.md) only after the review/evidence loop is clear.
