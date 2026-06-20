# `homeboy audit-baseline`

## Synopsis

```sh
homeboy audit-baseline refresh <component-id|path> [options]
homeboy audit-baseline merge <component-id|path> [options]
```

## Description

Refresh generated audit baseline data in `homeboy.json` without rewriting unrelated component configuration. The command runs the existing scoped audit baseline workflow for files changed since a git ref, writes only `baselines.audit`, and reports added/resolved fingerprints.

This is the preferred PR-branch workflow when `main` changes and the only expected churn is generated audit baseline data.

## Commands

- `refresh`: Recompute audit baseline entries for files changed since a git ref.
- `merge`: Auto-resolve an in-progress merge/rebase conflict in `homeboy.json` when the only conflict is generated audit baseline data.

## Options

### `refresh`

- `--changed-since <REF>`: Refresh baseline entries for files changed since this ref. Defaults to `origin/main`.
- `--path <PATH>`: Override the component checkout path for this invocation.
- `--extension <ID>`: One-shot extension override for the current invocation; repeat to layer multiple extension hints.

### `merge`

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

## Resolving Baseline-Only Conflicts

When `main` moves and two branches both ratchet the audit baseline, a `git merge`/`git rebase` can leave `homeboy.json` conflicted only in the generated `baselines.audit` data. `merge` resolves that case deterministically:

```sh
# During an in-progress merge/rebase with a conflicted homeboy.json:
homeboy audit-baseline merge homeboy
# homeboy.json is rewritten with the union of both baseline sides and staged.
git rebase --continue   # or: git merge --continue
```

Behavior:

- Reads the conflict stages directly from the git index (`:1:` base, `:2:` ours, `:3:` theirs).
- Preserves all non-baseline config verbatim from the current/resolved side.
- Merges the generated `known_fingerprints` arrays as their **union** — the canonical merge of two deterministic baseline projections — and recomputes `item_count`.
- Emits `added_fingerprints`/`resolved_fingerprints` relative to the conflict base.
- **Refuses** (with a manual-resolution message) when `ours` and `theirs` differ in anything outside generated audit baseline data, rather than guessing at component config.

This is the conflict-aware follow-up to the `refresh` workflow ([#3518](https://github.com/Extra-Chill/homeboy/issues/3518), following [#3515](https://github.com/Extra-Chill/homeboy/issues/3515)). If `homeboy.json` has non-baseline config conflicts, resolve those by hand first, then rerun.

## Related

- [audit](audit.md) — run audits, save full baselines, and compare drift.
