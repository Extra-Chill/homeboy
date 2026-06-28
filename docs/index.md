# Homeboy Documentation

Homeboy documentation is organized by reader intent. If you are new, start with [Start here](start-here.md). If you already know what you need, jump to workflows, concepts, reference, internals, or operations.

These docs are also embedded in the `homeboy` binary:

```bash
homeboy docs list
homeboy docs <topic>
```

## Start Here

- [Start here](start-here.md) - first local run, smallest useful config, JSON evidence, and next steps.
- [README](../README.md) - project overview and installation.

## Workflows

Task-oriented guides for using Homeboy:

- [Workflows index](workflows/index.md)
- [Review a branch](workflows/review-a-branch.md)
- [Reproduce CI](workflows/reproduce-ci.md)
- [Capture evidence](workflows/capture-evidence.md)
- [Use runners](workflows/use-runners.md)
- [Set up Lab runners](workflows/set-up-lab-runners.md)
- [Set up extensions](workflows/set-up-extensions.md)
- [Run agent task loops](workflows/run-agent-task-loops.md)
- [Manage local environments](workflows/manage-local-environments.md)
- [Release a component](workflows/release-a-component.md)
- [Deploy and operate fleets](workflows/deploy-and-operate-fleets.md)

## Concepts

Mental models and vocabulary:

- [Concepts index](concepts/index.md)
- [Homeboy model](concepts/homeboy-model.md)
- [Structured evidence](concepts/structured-evidence.md)
- [Headless agent orchestration](concepts/headless-agent-orchestration.md)
- [Code Factory](concepts/code-factory.md)

## Reference

Exact CLI, configuration, schema, and output details:

- [Reference index](reference/index.md)
- [Root command and global flags](reference/cli/homeboy-root-command.md)
- [Command index](commands/commands-index.md)
- [Configuration reference](reference/configuration.md)
- [Template variables](reference/template-variables.md)
- [Configuration schemas](reference/schemas/index.md)
- [JSON output contract](architecture/output-system.md)
- [CI result JSON contract](architecture/ci-results-contract.md)

## Internals

Maintainer architecture and implementation contracts:

- [Internals index](internals/index.md)
- [Architecture overview](internals/developer-guide/architecture-overview.md)
- [Architecture cleanup map](internals/developer-guide/architecture-cleanup-map.md)
- [Docs maintenance](internals/docs-maintenance/index.md)
- [Embedded docs topic resolution](architecture/embedded-docs-topic-resolution.md)

## Operations

Runbooks for operators and agents:

- [Operations index](operations/index.md)
- [Release-gate proof path](operations/release-gate-proof-path.md)
- [Controller to runner reverse-runner setup](operations/controller-runner-reverse-runner.md)
- [Artifact loop for runner and matrix workflows](operations/artifact-loop-runner-matrix.md)

## Historical Reference

- [Changelog](changelog.md)
- [Cross-compilation](cross-compilation.md)
