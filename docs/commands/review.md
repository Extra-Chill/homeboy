# Review Command

Run scoped audit + lint + test in a single invocation against PR-style changes.

## Synopsis

```bash
homeboy review [component] --changed-since=<ref>
homeboy review [component] --changed-only
homeboy review [component]
```

## Description

`homeboy review` is a thin umbrella that fans out the existing scoped runs of
`audit`, `lint`, and `test` against the same set of changed files, then prints a
single consolidated report. It answers the question:

> *"What would a reviewer see if I ran homeboy on just my PR diff?"*

The umbrella owns no scoping logic of its own — every scope flag is forwarded to
the underlying commands, which already share a common `--changed-since` plumbing
(`core/git/changes.rs::get_files_changed_since`). Stages run **sequentially** in
the order **audit → lint → test**, matching the canonical verification order.
Output is deterministic and matches each command's per-stage output.

## Arguments

- `[component]`: Component ID. Optional — auto-discovered from the current
  working directory via `homeboy.json`, just like `lint`, `audit`, and `test`.

## Scope flags

- `--changed-since <REF>`: Run audit, lint, and test only against files changed
  since this git ref (branch, tag, or SHA). Triple-dot diff against `HEAD`,
  excludes deletes, handles shallow CI clones automatically. Mutually exclusive
  with `--changed-only`.
- `--changed-only`: Run against files modified in the working tree (staged,
  unstaged, untracked). **Only the lint stage scopes natively** — audit and test
  do not currently accept working-tree-only scoping, so they run against the
  full component when this flag is passed. The consolidated summary surfaces
  this limitation as a hint. Use `--changed-since` for full umbrella scoping.

If neither flag is passed, all three stages run against the entire component —
equivalent to running `audit`, `lint`, and `test` back-to-back without scope.

## Component Requirements

`review` delegates to `audit`, `lint`, and `test`. Lint and test stages require linked extensions that provide those capabilities; review does not run arbitrary component shell commands.

Useful remediation paths when review reports missing extensions:

- Link the relevant extension: `homeboy component set <id> --extension <extension_id>`
- Inspect installed extensions: `homeboy extension list`
- Use a rig `command` step for one-off checks that do not belong in an extension.

## Other flags

- `--summary`: Forward the per-stage summary flag to each command (`--summary`
  on lint, `--json-summary` on audit and test).
- `--baseline` / `--ignore-baseline` / `--ratchet`: Forwarded to every stage
  that participates in the baseline engine.

## Examples

```bash
# CI pattern: review a feature branch against trunk
homeboy review --changed-since=trunk

# Review a specific component against a release tag
homeboy review my-plugin --changed-since=v1.2.0

# Quick local check of working-tree edits (lint only scopes)
homeboy review --changed-only

# Full sweep — equivalent to running audit + lint + test back-to-back
homeboy review my-plugin

# Render a PR-comment markdown section directly to a file, then post it
homeboy review my-plugin --changed-since=main --report=pr-comment > /tmp/section.md
homeboy git pr comment my-plugin --number 42 --comment-key ci:my-plugin \
  --section-key review --body-file /tmp/section.md \
  --header "## Homeboy Results — \`my-plugin\`"
```

## Empty-changeset short-circuit

When `--changed-since=<ref>` or `--changed-only` produces an empty file list,
review prints a single line and exits cleanly:

```text
No files changed since trunk — skipping review
```

All three stages are reported as `ran: false` with `skipped_reason: "no files
changed"` in the JSON envelope. No extension setup is performed.

## Output

Returns the standard CLI envelope `{success, data}`. The `data` payload
consolidates all three stages:

```json
{
  "success": true,
  "data": {
    "command": "review",
    "summary": {
      "passed": true,
      "status": "passed",
      "component": "my-plugin",
      "scope": "changed-since",
      "changed_since": "trunk",
      "total_findings": 0,
      "changed_file_count": 7,
      "hints": []
    },
    "audit": {
      "stage": "audit",
      "ran": true,
      "passed": true,
      "exit_code": 0,
      "finding_count": 0,
      "hint": "Deep dive: homeboy audit my-plugin --changed-since=trunk",
      "output": { "...": "full AuditCommandOutput" }
    },
    "lint": { "...": "full LintCommandOutput" },
    "test": { "...": "full TestCommandOutput" }
  }
}
```

Each stage's `output` field carries the same structured payload that running
`homeboy <stage>` directly would produce, so downstream consumers (the sectioned
PR-comment primitive, CI wrappers) can render per-stage detail without needing
a separate invocation.

## Output formats

`review` supports two output shapes, selected via `--report`.

### Default — JSON envelope

The default output is the structured `{success, data: ReviewCommandOutput}`
envelope shown above. Suitable for programmatic consumers, CI wrappers, and
the agent surface. Every field that a per-stage command would emit is
preserved under `data.audit.output`, `data.lint.output`, `data.test.output`.

### `--report=pr-comment` — markdown PR-comment section

Renders the same envelope into a markdown PR-comment section, ready to pipe
into `homeboy git pr comment --body-file`. The renderer emits **only the
section body** — the consumer (`homeboy git pr comment --header`) owns the
wrapping `### Title` heading.

Per-stage shape:

- Header line per stage: `:white_check_mark: **<stage>**` for pass,
  `:x: **<stage>**` for fail, `:fast_forward: **<stage>** — skipped (<reason>)`
  when the stage was skipped (e.g. empty changeset).
- Audit body: top finding categories (by `convention`) with counts, capped at
  10 categories with a `… N more` overflow line.
- Lint body: top sniff codes (by `category`) with counts, same 10-cap.
- Test body: failure summary line (`**N failed** out of M total`) plus pass
  and skip counts. Per-test failure names are not surfaced — that data isn't
  on `TestCommandOutput`.
- Each ran stage ends with a `> Deep dive: homeboy <cmd> ...` blockquote
  pointing the reviewer at the per-stage command for full detail.

Above the stages, the renderer emits a scope banner
(`:zap: Scope: **changed files only** (since \`<ref>\`)` or
`:information_source: Scope: **full**`) and a total-findings line
(`**N** finding(s) across M stage(s)`).

**Out of scope for this renderer.** Action-level signals — autofix banners,
fallback-binary warnings, tooling-version footers, and scope-mode resolution
notes — are not present in `ReviewCommandOutput` and are not rendered. The
GitHub Action layer emits those as separate sections.

Example:

```bash
homeboy review my-plugin --changed-since=main --report=pr-comment
```

```markdown
:zap: Scope: **changed files only** (since `main`)

**4** finding(s) across 3 stage(s)

:x: **audit**
- **ability-shape** — 3 finding(s)
- **naming-convention** — 1 finding(s)
- _Total: 4 finding(s)_
> Deep dive: homeboy audit my-plugin --changed-since=main

:white_check_mark: **lint**
> Deep dive: homeboy lint my-plugin --changed-since=main

:white_check_mark: **test**
- 87 passed
- 2 skipped
> Deep dive: homeboy test my-plugin --changed-since=main
```

## Exit codes

- `0`: Every stage that ran exited 0.
- `1`: At least one stage reported findings or test failures (`exit_code == 1`).
- `2`: At least one stage hit an infrastructure failure (`exit_code >= 2`).

## Related

- [audit](audit.md) — convention drift detection (run individually for deep dive)
- [lint](lint.md) — code-style validation (only stage that natively supports `--changed-only`)
- [test](test.md) — test execution + drift detection
- [refactor](refactor.md) — apply automated fixes after review identifies issues
- Issue [#1500](https://github.com/Extra-Chill/homeboy/issues/1500) — design and motivation
