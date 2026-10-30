# CI result JSON contract

Homeboy CI results are exposed through the existing global `--output <path>` flag.
The flag writes the same JSON envelope Homeboy prints to stdout, but without log
text, group markers, timestamps, or GitHub Actions annotations. CI wrappers should
upload those files as artifacts instead of asking downstream tools to scrape logs.

This document names the stable contract a PR review agent can consume.

## Evidence location

CI evidence never lives inside the repository checkout. Homeboy adds exactly one
file to a repository (`homeboy.json`); every other byte it produces, including CI
and review evidence, lives in Homeboy-owned state or a runner-owned scratch
directory outside the tree. Homeboy core has no in-tree evidence default: the
`--output <path>` flag writes exactly where it is pointed.

CI wrappers resolve one results directory outside the checkout and pass absolute
paths under it to `--output`:

- an explicit `HOMEBOY_CI_RESULTS_DIR` set by the wrapper, or
- `$RUNNER_TEMP/homeboy-ci-results` on GitHub Actions.

There is no repo-relative `homeboy-ci-results/` directory and no fallback that
reads one. After `homeboy review ci run`, `homeboy review ci differential-gate`,
or `homeboy review` on a clean checkout, `git status --porcelain --ignored`
stays empty, and every evidence path reported in command output points outside
the tree.

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
HOMEBOY_CI_RESULTS_DIR="${HOMEBOY_CI_RESULTS_DIR:-$RUNNER_TEMP/homeboy-ci-results}"
mkdir -p "$HOMEBOY_CI_RESULTS_DIR"

homeboy \
  --output "$HOMEBOY_CI_RESULTS_DIR/review.json" \
  review "$COMPONENT_ID" \
  --path "$GITHUB_WORKSPACE" \
  --changed-since "$BASE_REF" \
  --summary
```

Upload `$HOMEBOY_CI_RESULTS_DIR` as a GitHub Actions artifact named
`homeboy-ci-results`. The artifact name is a transport label, not a path in the
checkout.

Recommended artifact layout (artifact root, i.e. `$HOMEBOY_CI_RESULTS_DIR`):

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
      "component": "sample-plugin",
      "scope": "changed-since",
      "changed_since": "origin/main",
      "total_findings": 3,
      "changed_file_count": 7,
      "hints": []
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
- `data.<stage>.ran`: whether `lint` or `test` ran.
- `data.<stage>.passed`: stage pass/fail when it ran.
- `data.<stage>.exit_code`: stage exit code.
- `data.<stage>.finding_count`: normalized count for quick triage.
- `data.<stage>.skipped_reason`: why the stage did not run, when skipped.
- `data.<stage>.hint`: exact deep-dive command shape for humans.
- `data.<stage>.output`: full structured stage payload.

Stage payloads preserve the same structured data as invoking the stage directly:

- `data.lint.output`: `LintCommandOutput`, including lint findings and baseline
  comparison when available.
- `data.test.output`: `TestCommandOutput`, including test counts, failures, drift,
  and coverage fields when those modes are active.

Persisted lint baselines use `lint:<population-digest>` keys. The population
includes files, tools, scope and filters; `--baseline` saves the current key.
Unscoped `lint` records are ignored. Lint baseline resolution is `unavailable`,
`scoped` or `git_base`; changed-since comparisons continue to measure the Git base.

## Pre-result runner failures

Lint and test extension runners also write structured fallback sidecars when the
runner exits non-zero before producing normal findings or test counts. This lets
orchestrators diagnose bootstrap, formatter, dependency, or pre-test failures
without scraping raw GitHub logs.

Fallback sidecars preserve the existing file roles:

- `lint-findings.json` remains a JSON array of source findings and is empty
  when a runner fails before it can produce one. The accompanying
  `lint-producers.json` entry records `homeboy-extension-runner` with
  `status: "error"`, `finding_count: 0`, and `metadata.failure` containing
  `phase`, `command`, `exit_code`, `stdout_tail`, `stderr_tail`, truncation
  flags, and `parsed_detail` when a JSON object can be recovered from runner
  output.
- `test-results.json` remains a JSON object. A pre-result test failure contains
  `status: "failed"`, `phase`, `command`, `exit_code`, output tails, and a
  nested `failure` object with the same structured fields. It intentionally does
  not invent test counts when no runner produced them.

Runners that already wrote a valid structured sidecar keep ownership of it; the
fallback replaces missing, empty, or malformed sidecars after a failed runner
exit.

## Legacy per-command artifacts

Existing CI wrappers may still run individual commands and upload one JSON file per
command. That shape remains valid, but it is a lower-level contract:

```sh
homeboy --output "$HOMEBOY_CI_RESULTS_DIR/lint.json"  lint  "$COMPONENT_ID" --path "$GITHUB_WORKSPACE" --changed-since "$BASE_REF" --summary
homeboy --output "$HOMEBOY_CI_RESULTS_DIR/test.json"  test  "$COMPONENT_ID" --path "$GITHUB_WORKSPACE" --changed-since "$BASE_REF" --json-summary
```

Recommended artifact layout for per-command mode (artifact root, outside the
checkout):

```text
homeboy-ci-results/
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
  "repo": "Extra-Chill/sample-plugin",
  "head_sha": "abc123",
  "run_id": "1234567890",
  "run_attempt": "1",
  "artifact_name": "homeboy-ci-results",
  "check_url": "https://github.com/Extra-Chill/sample-plugin/actions/runs/1234567890"
}
```

The manifest is action-owned metadata. `review.json`, `lint.json`,
and `test.json` stay Homeboy-owned payloads.

## Observation bundle artifacts

Observation bundles are the reserved CI artifact shape for persisted run history:
bench, rig, trace, and their recorded artifacts. They are separate from
`homeboy-ci-results`, which is the command-result artifact consumed by PR review
agents.

Reserved artifact name:

```text
homeboy-observations
```

Reserved artifact layout:

```text
homeboy-observations/
  manifest.json
  runs.json
  artifacts.json
  bench_metrics.json
  rig_steps.json
  trace_spans.json
  files/
    ...
```

The observation-bundle schema is owned by `homeboy runs export/import`. Until
that command surface lands, CI workflows should not add a production upload step
for this artifact. The intended workflow shape is:

```sh
homeboy bench studio --rig studio-dev --scenario admin-first-load
# Wave 4A dependency: homeboy runs export --since 1h --output "$RUNNER_TEMP/homeboy-observations"
```

Once `homeboy runs export` exists, CI wrappers should upload the generated
directory or zip as a GitHub Actions artifact named `homeboy-observations`.
Consumers should treat `manifest.json` as the schema/version entry point and
ignore unknown additive fields in every bundle file.

Artifact boundaries:

- `homeboy-ci-results`: command output JSON for immediate CI/review decisions.
- `homeboy-observations`: persisted observation-store history for later import,
  benchmark comparison, and external analysis.
- `homeboy-binary`: workflow-internal compiled binary handoff between jobs.

The Homeboy Action follow-up should be action-level wiring only: run the export
command after observation-producing commands, upload the reserved artifact name,
and leave the bundle schema to Homeboy core.

## Consumer rules

PR review agents should:

- Wait while the GitHub check is pending instead of reviewing stale results.
- Fall back to `lint.json` and `test.json` when `review.json` is absent.
- Use `success`, `data.summary.passed`, and per-stage `passed` fields for status.
- Use `finding_count` for quick triage and the nested stage `output` for details.
- Use `changed-since` scoped payloads to avoid repeating unrelated baseline findings.
- Treat `baseline-red` / `baseline_red` and `inconclusive` failure-origin statuses as
  baseline or evidence problems, not proven candidate regressions. These statuses
  should include the baseline command and artifact/evidence refs when available.
- Keep a link to the GitHub check or run URL from action-owned metadata.
- Ignore unknown additive fields.
- Avoid scraping human logs unless the JSON envelope is absent or malformed.

## Related

- [JSON output contract](output-system.md)
- [review](../commands/review.md)
- [lint](../commands/lint.md)
- [test](../commands/test.md)
- Issue [#1825](https://github.com/Extra-Chill/homeboy/issues/1825)
- Issue [#2022](https://github.com/Extra-Chill/homeboy/issues/2022)
