# CI result JSON contract

Homeboy CI results are exposed through the existing global `--output <path>` flag.
The flag writes the same JSON envelope Homeboy prints to stdout, but without log
text, group markers, timestamps, or GitHub Actions annotations. CI wrappers should
upload those files as artifacts instead of asking downstream tools to scrape logs.

This document names the stable contract a PR review agent can consume.

## Envelope

Every artifact file is a standard Homeboy CLI response:

```json
{
  "success": true,
  "data": { "...": "command-specific payload" }
}
```

Failures use the same envelope:

```json
{
  "success": false,
  "error": {
    "code": "validation.invalid_argument",
    "message": "Human-readable message",
    "details": {},
    "hints": []
  }
}
```

Consumers should treat the envelope as stable:

- `success` is the top-level pass/fail signal for the command invocation.
- `data` is present when Homeboy produced a command payload.
- `error` is present when Homeboy failed before producing a command payload.
- `error.code` is a stable machine-readable error code.
- `error.message` is for humans and should not be parsed.

The command-specific payload may grow additive fields. Consumers should ignore
unknown fields.

## Preferred PR artifact

For PR review agents, the preferred artifact is a single `homeboy review` output:

```sh
mkdir -p "$RUNNER_TEMP/homeboy-results"

homeboy \
  --output "$RUNNER_TEMP/homeboy-results/review.json" \
  review "$COMPONENT_ID" \
  --path "$GITHUB_WORKSPACE" \
  --changed-since "$BASE_REF" \
  --summary
```

Upload `$RUNNER_TEMP/homeboy-results` as a GitHub Actions artifact named
`homeboy-ci-results`.

Recommended artifact layout:

```text
homeboy-ci-results/
  review.json
  review.log
```

The log is optional and human-facing. Agents should read `review.json` first and
only fetch logs when they need extra debugging context.

## `review.json` payload

`review.json` wraps `ReviewCommandOutput` under `data`:

```json
{
  "success": false,
  "data": {
    "command": "review",
    "summary": {
      "passed": false,
      "status": "failed",
      "component": "data-machine",
      "scope": "changed-since",
      "changed_since": "origin/main",
      "total_findings": 3,
      "changed_file_count": 7,
      "hints": []
    },
    "audit": {
      "stage": "audit",
      "ran": true,
      "passed": false,
      "exit_code": 1,
      "finding_count": 2,
      "hint": "Deep dive: homeboy audit data-machine --changed-since=origin/main",
      "output": { "...": "AuditCommandOutput" }
    },
    "lint": { "...": "ReviewStage<LintCommandOutput>" },
    "test": { "...": "ReviewStage<TestCommandOutput>" }
  }
}
```

Stable fields for PR review agents:

- `data.command`: always `review` for the preferred artifact.
- `data.summary.passed`: aggregate pass/fail across stages that ran.
- `data.summary.status`: stable string status (`passed` or `failed`).
- `data.summary.component`: component label used for the run.
- `data.summary.scope`: `changed-since`, `changed-only`, or `full`.
- `data.summary.changed_since`: git ref used for PR scoping, when present.
- `data.summary.total_findings`: aggregate findings across ran stages.
- `data.summary.changed_file_count`: scoped changed-file count, when known.
- `data.summary.hints`: machine-preserved human guidance.
- `data.<stage>.ran`: whether `audit`, `lint`, or `test` ran.
- `data.<stage>.passed`: stage pass/fail when it ran.
- `data.<stage>.exit_code`: stage exit code.
- `data.<stage>.finding_count`: normalized count for quick triage.
- `data.<stage>.skipped_reason`: why the stage did not run, when skipped.
- `data.<stage>.hint`: exact deep-dive command shape for humans.
- `data.<stage>.output`: full structured stage payload.

Stage payloads preserve the same structured data as invoking the stage directly:

- `data.audit.output`: `AuditCommandOutput`, including baseline comparison and
  changed-since scoped audit findings when those modes are active.
- `data.lint.output`: `LintCommandOutput`, including lint findings and baseline
  comparison when available.
- `data.test.output`: `TestCommandOutput`, including test counts, failures, drift,
  and coverage fields when those modes are active.

## Legacy per-command artifacts

Existing CI wrappers may still run individual commands and upload one JSON file per
command. That shape remains valid, but it is a lower-level contract:

```sh
homeboy --output "$RUNNER_TEMP/homeboy-results/audit.json" audit "$COMPONENT_ID" --path "$GITHUB_WORKSPACE" --changed-since "$BASE_REF" --json-summary
homeboy --output "$RUNNER_TEMP/homeboy-results/lint.json"  lint  "$COMPONENT_ID" --path "$GITHUB_WORKSPACE" --changed-since "$BASE_REF" --summary
homeboy --output "$RUNNER_TEMP/homeboy-results/test.json"  test  "$COMPONENT_ID" --path "$GITHUB_WORKSPACE" --changed-since "$BASE_REF" --json-summary
```

Recommended artifact layout for per-command mode:

```text
homeboy-ci-results/
  audit.json
  audit.log
  lint.json
  lint.log
  test.json
  test.log
```

Review agents should prefer `review.json` when present, then fall back to
per-command files for older action runs.

## GitHub check linkage

Homeboy core does not know the GitHub run URL. The GitHub Action layer should add
that metadata beside the Homeboy payload, either in a manifest file or check-run
output summary.

Recommended manifest:

```json
{
  "schema": "homeboy.ci-results.v1",
  "producer": "homeboy-action",
  "repo": "Extra-Chill/data-machine",
  "head_sha": "abc123",
  "run_id": "1234567890",
  "run_attempt": "1",
  "artifact_name": "homeboy-ci-results",
  "check_url": "https://github.com/Extra-Chill/data-machine/actions/runs/1234567890"
}
```

The manifest is action-owned metadata. `review.json`, `audit.json`, `lint.json`,
and `test.json` stay Homeboy-owned payloads.

## Consumer rules

PR review agents should:

- Wait while the GitHub check is pending instead of reviewing stale results.
- Fall back to `audit.json`, `lint.json`, and `test.json` when `review.json` is absent.
- Use `success`, `data.summary.passed`, and per-stage `passed` fields for status.
- Use `finding_count` for quick triage and the nested stage `output` for details.
- Use `changed-since` scoped payloads to avoid repeating unrelated baseline findings.
- Keep a link to the GitHub check or run URL from action-owned metadata.
- Ignore unknown additive fields.
- Avoid scraping human logs unless the JSON envelope is absent or malformed.

## Related

- [JSON output contract](output-system.md)
- [review](../commands/review.md)
- [audit](../commands/audit.md)
- [lint](../commands/lint.md)
- [test](../commands/test.md)
- Issue [#1825](https://github.com/Extra-Chill/homeboy/issues/1825)
