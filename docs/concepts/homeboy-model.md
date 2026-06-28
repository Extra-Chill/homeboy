# Homeboy Model

Homeboy starts with a component and keeps the command model consistent across where that component runs.

## Component

A component is the buildable, testable, reviewable unit. Portable component config usually lives in `homeboy.json` at the repository root.

## Extension

Extensions provide ecosystem-specific behavior while Homeboy core stays generic. For example, an extension can teach Homeboy how to run Cargo, WP-CLI, Node.js, GitHub releases, or platform-specific audit rules.

## Command Surface

Homeboy exposes one operational surface for local developers, CI, scheduled jobs, and coding agents:

```bash
homeboy audit
homeboy lint
homeboy test
homeboy review
homeboy bench
homeboy trace
homeboy release
```

## Evidence

Commands print human-readable output and can write structured JSON with `--output`. Longer workflows can persist runs and artifacts for later inspection with `homeboy runs`.

## Runners

Runners let Homeboy route hot or remote-capable commands to another execution environment while preserving the same command contract.

## Operations

Projects, servers, and fleets add remote operations for deployments, SSH, logs, files, databases, and fleet fan-out. These are useful for configured environments but are not required for the local quality loop.
