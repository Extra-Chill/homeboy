# `homeboy audit-baseline`

## Synopsis

```sh
homeboy audit-baseline refresh <component-id|path> [options]
```

## Description

Refresh generated audit baseline data in `homeboy.json` without rewriting unrelated component configuration. The command runs the existing scoped audit baseline workflow for files changed since a git ref, writes only `baselines.audit`, and reports added/resolved fingerprints.

This is the preferred PR-branch workflow when `main` changes and the only expected churn is generated audit baseline data.

## Commands

- `refresh`: Recompute audit baseline entries for files changed since a git ref.

## Options

- `--changed-since <REF>`: Refresh baseline entries for files changed since this ref. Defaults to `origin/main`.
- `--path <PATH>`: Override the component checkout path for this invocation.
- `--extension <ID>`: One-shot extension override for the current invocation; repeat to layer multiple extension hints.

## PR Branch Workflow

```sh
# Bring the branch up to date first.
git fetch origin
git rebase origin/main

# Refresh only generated audit baseline data for files touched by the branch.
homeboy audit-baseline refresh homeboy --changed-since origin/main
```

The JSON output includes:

- `added_fingerprints`: fingerprints present after refresh that were absent before refresh.
- `resolved_fingerprints`: fingerprints present before refresh that are absent after refresh.
- `previous_source`: whether the comparison used the working-tree baseline, the git-ref baseline, or no previous baseline.

If `homeboy.json` already contains merge-conflict markers, resolve non-baseline config conflicts first, then rerun `homeboy audit-baseline refresh`. Automatic merge-conflict repair for generated baseline arrays is tracked in [#3518](https://github.com/Extra-Chill/homeboy/issues/3518).

## Related

- [audit](audit.md) — run audits, save full baselines, and compare drift.
