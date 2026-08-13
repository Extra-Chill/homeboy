# Homeboy Action

GitHub Action for running [Homeboy](https://github.com/Extra-Chill/homeboy) review, bench, and release commands in CI.

Works with **any Homeboy extension** — WordPress, Rust, Node, or your own custom extension.

## Action Channel

Use the floating `v2` channel for normal GitHub Actions workflows:

```yaml
- uses: Extra-Chill/homeboy-action@v2
```

The action release stream is aligned with that channel. Release commits and tags use `v2.x.y`, and the floating `v2` tag moves to the latest compatible `v2.x.y` release.

## Breaking Change Notice

The generic source-mutation interface has been removed: `autofix`, `autofix-mode`, `autofix-open-pr`, `autofix-max-commits`, `autofix-commands`, and `autofix-label` are no longer accepted by the action or reusable workflow. Quality runs are read-only; audit, lint, test, review reporting, failure digests, categorized issue reconciliation, and final status enforcement remain available. Consumers using any removed input must remove it before adopting the next major action release.

## Quick Start

### PR Quality Checks

Prefer the reusable workflow so Homeboy Action owns the CI DAG and result
aggregation. It projects binary setup, command execution, artifact
publication, result reconciliation, and enforcement into named GitHub jobs and
steps. A failed command appears as `Run candidate Audit`, `Run candidate Lint`,
or `Run candidate Test` in the GitHub UI and `gh run view --log`, rather than
as an anonymous composite-action step. The workflow runs all requested quality
commands before failing, so an audit failure does not hide lint or test
feedback.

For pull requests with `differential-gating: 'true'`, the reusable workflow
materializes one candidate Homeboy binary, then runs candidate and base phases
in separate jobs. A final reconciliation check is the only pass/fail decision:
it accepts only artifacts bound to the repository, candidate SHA, base SHA,
command, component, action revision, CLI revision, and phase identity. Missing
or mismatched evidence fails closed. This DAG behavior is specific to the
reusable workflow; direct composite-action callers keep the combined behavior.

Set `test-shards` above `1` to use the generic Homeboy test inventory contract.
The candidate Test command is first invoked with `HOMEBOY_TEST_INVENTORY_ONLY=1`;
the selected extension writes `homeboy-test-inventory.json` with schema
`homeboy/test-inventory/v1`, stable test IDs, and optional `duration_ms` values.
Each shard then receives a digest-bound `HOMEBOY_TEST_SHARD_MANIFEST`.

Changed-scope resolution can map a tiny diff onto a huge test-file selection.
Set `max-changed-test-files` to a positive integer to cap how many test files
Homeboy may select before the Test phase runs; an oversized selection fails
fast with a `changed_scope_selection_exceeds_cap` finding instead of consuming
the test budget. The default leaves behaviour unchanged. The cap guards
*selection size*, `test-timeout-seconds` guards *run time*, and `test-shards`
partitions the selection to address both.

```yaml
name: CI
on: [pull_request]

concurrency:
  group: ci-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

jobs:
  homeboy:
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2
    with:
      extension: wordpress
      commands: review audit,review lint,review test
      php-version: '8.3'
    secrets: inherit
```

### Named-Phase Migration

Existing direct action calls remain supported and preserve the combined
composite lifecycle:

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: review lint,review test
```

Move CI quality gates to the reusable workflow when GitHub-native failure
attribution is required. Replace the action step with a job-level `uses` call,
move its `with` values under the job, and add `secrets: inherit` when the
action needs repository credentials. Keep direct action calls for release,
operations, and other cases that require the composite action in an existing
job. The reusable workflow retains structured result artifacts, failure
digests, phase timings, and final status enforcement.

```yaml
jobs:
  quality:
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2
    with:
      extension: wordpress
      commands: review lint,review test
    secrets: inherit
```


When validating Homeboy itself or another project that needs to build a binary
before running quality checks, keep the build as the hard gate and let the
reusable workflow run all quality commands after the binary is available:

```yaml
jobs:
  homeboy:
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2
    with:
      component: homeboy
      commands: review audit,review lint,review test
      build-command: cargo build --release
      build-artifact-path: target/release/homeboy
    secrets: inherit
```

### Continuous Release

Fully automated releases — no human input needed. Triggers on every push to main, checks for releasable conventional commits since the last tag, computes the version, generates changelog, bumps version targets, tags, creates a GitHub Release, and publishes.

```yaml
name: Release
on:
  push:
    branches: [main]
  workflow_dispatch:

concurrency:
  group: release
  cancel-in-progress: false

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
          token: ${{ secrets.GITHUB_TOKEN }}

      - uses: Extra-Chill/homeboy-action@v2
        id: release
        with:
          extension: rust
          component: my-project
          commands: release
```

The release command:

1. Scans conventional commits since the last version tag
2. Skips if no releasable commits (`chore:`, `ci:`, `docs:`, `test:` are ignored)
3. Computes version bump: `fix:` → patch, `feat:` → minor, `BREAKING CHANGE` → major
4. Generates changelog entries via `homeboy changelog add`
5. Bumps version targets (Cargo.toml, package.json, VERSION, etc.)
6. Finalizes changelog (`[Next]` → `[VERSION] - DATE`)
7. Commits, creates an annotated tag, pushes, and creates a GitHub Release

After the tag push, downstream build jobs can produce artifacts and then call Homeboy Action again with `release-head: 'true'` and `release-from-artifacts: <path>` to publish those artifacts through the native Homeboy release pipeline.

### Benchmarks

Run `homeboy bench` in CI and preserve the raw structured output for downstream review agents:

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: rust
    component: homeboy
    commands: bench
    rig: main,pr
    scenario: audit-self
    runs: 3
    iterations: 10
```

Bench runs write the exact `homeboy bench --output` payload to `homeboy-ci-results/bench.json`, upload it as a `homeboy-ci-results-<component>-<commands>-<job>-<os>-<runtime>` artifact, and render a compact PR-summary section when PR comments are enabled.

Homeboy Action also runs `homeboy runs export --since 24h --output homeboy-observations` after command execution and uploads the result as a separate `homeboy-observations-<component>-<commands>-<job>-<os>-<runtime>` artifact. This export is best-effort: if the current Homeboy version does not support observation export or no observations exist, the CI result is unchanged.

Set `import-observations: true` on jobs that should consume observation bundles uploaded by earlier jobs in the same workflow run. The action downloads `homeboy-observations-*` artifacts, imports each downloaded bundle with `homeboy runs import <dir>`, and continues cleanly when no matching artifacts exist.

Artifact boundaries:

- `homeboy-ci-results-<component>-<commands>-<job>-<os>-<runtime>`: immediate structured command outputs used for pass/fail decisions and PR rendering, such as `bench.json`.
- `homeboy-observations-<component>-<commands>-<job>-<os>-<runtime>`: persisted Homeboy run history exported for later import/query by agents and other tooling. The suffix prevents collisions when a workflow splits audit/lint/test across jobs, matrix dimensions, or multiple action invocations.

#### Continuous release outputs

| Output | Description |
|--------|-------------|
| `released` | `true` if a release was created, `false` if skipped |
| `release-version` | Version number (e.g. `0.63.0`) |
| `release-tag` | Git tag (e.g. `v0.63.0`) |
| `release-bump-type` | Bump type used (`patch`, `minor`, `major`) |

Use these outputs to gate downstream jobs:

```yaml
  build:
    needs: release
    if: needs.release.outputs.released == 'true'
    # ... build and publish steps
```

### Required Portable Config (`homeboy.json`)

`homeboy.json` at repository root is required by Homeboy Action.

```json
{
  "id": "my-project",
  "extensions": {
    "wordpress": {}
  }
}
```

## Inputs

| Input | Required | Default | Description |
|-------|----------|---------|-------------|
| `version` | No | `latest` | Homeboy version to install (e.g. `0.52.0`) |
| `source` | No | | Path to build homeboy from source (e.g. `.`). Falls back to release binary. |
| `extension` | No | | Extension ID (e.g. `wordpress`, `rust`, `node`) |
| `extension-source` | No | `Extra-Chill/homeboy-extensions` | Git URL to install the extension from |
| `commands` | No | `review audit,review lint,review test` | Comma-separated commands to run |
| `expected-commands` | No | *(falls back to `commands`)* | Full set of command types expected to run across the workflow (e.g. `review audit,review lint,review test`). Set this on every invocation when a workflow splits review audit/lint/test across separate steps, otherwise each invocation will close sibling invocations' issues during reconciliation. |
| `component` | No | *(repo name)* | Component name (auto-detected from repo) |
| `args` | No | | Extra arguments passed to each command |
| `rig` | No | | Bench rig pair/list passed to `homeboy bench --rig` |
| `scenario` | No | | Bench scenario ID passed to `homeboy bench --scenario` |
| `runs` | No | | Bench run count passed to `homeboy bench --runs` |
| `iterations` | No | | Bench iteration count passed to `homeboy bench --iterations` |
| `regression-threshold` | No | | Bench regression threshold passed to `homeboy bench --regression-threshold` |
| `differential-gating` | No | `false` | On PRs, compare `review audit`/`review test` counts against the base SHA and fail only when the PR is worse. Opt-in; `review lint` still gates on exit code. |
| `baseline-commands` | No | `auto` | Commands to rerun at the PR base when `differential-gating` is true. `auto` reruns requested `review audit`/`review lint`/`review test` commands; use a comma-separated subset such as `review audit` or `none` to skip baseline reruns. |
| `observation-window` | No | `24h` | Duration window passed to best-effort `homeboy runs export --since` for the separate matrix-safe observations artifact. |
| `execution-timeout-seconds` | No | `1800` | Per-command wall-clock budget. The action writes liveness notices and returns timeout evidence when the budget expires. |
| `test-timeout-seconds` | No | inherited / `1500` | Homeboy test-child budget. Direct action calls preserve `HOMEBOY_TEST_TIMEOUT_SECONDS` when omitted; the reusable workflow defaults to `1500`. Must leave cleanup margin below `execution-timeout-seconds`. |
| `max-changed-test-files` | No | *(disabled)* | Cap on the number of changed-scope test files Homeboy may select for a Test phase. Positive integers enable the guard and fail fast with `changed_scope_selection_exceeds_cap` when a diff maps to more files; empty leaves behaviour unchanged. Guards selection size, where `test-timeout-seconds` guards run time and `test-shards` partitions the selection. |
| `cleanup-timeout-seconds` | No | `15` | Process-group teardown budget after a command finishes or times out. The action retains command logs and fails with diagnostics if cleanup cannot finish. |
| `import-observations` | No | `false` | Download and best-effort import earlier `homeboy-observations-*` artifacts from the same workflow run before command execution. |
| `php-version` | No | | PHP version (sets up via `shivammathur/setup-php`) |
| `node-version` | No | | Node.js version (sets up via `actions/setup-node`) |
| `scope` | No | `auto` | Execution scope: `auto` uses changed scope on PRs and full scope elsewhere; `changed` forces `--changed-since` when a base SHA is available; `full` scans the full workspace. |
| `auto-issue` | No | *(auto)* | Reconcile categorized audit, lint, and test issues on non-PR runs. Empty means enabled for non-PR events and disabled for PRs; set `false` to suppress issue maintenance. |
| `comment-key` | No | *(auto)* | Shared PR comment key so multiple jobs aggregate into one sticky comment |
| `comment-section-key` | No | *(auto)* | Section key within the shared PR comment |
| `comment-section-title` | No | *(auto)* | Visible heading for this section in the shared PR comment |
| `pr-policy` | No | | Path to a repo-local PR policy file for deterministic PR open/update and auto-merge eligibility |
| `pr-open-policy` | No | | Path to a repo-local PR open/update policy file. Defaults to `pr-policy` when empty. |
| `pr-policy-merge` | No | `false` | Merge the PR when `pr-policy` marks it safe |
| `pr-policy-merge-method` | No | `squash` | Merge method for `pr-policy-merge`: `merge`, `squash`, or `rebase` |
| `release-dry-run` | No | `false` | Preview the release without making changes |
| `release-branch` | No | `main` | Branch that releases are allowed from |
| `release-head` | No | `false` | Finish a release at the current HEAD/tag with `--head` |
| `release-from-artifacts` | No | | Publish existing artifacts with `--from-artifacts <path>` |
| `release-skip-publish` | No | `false` | Skip package and publish steps |
| `release-skip-github-release` | No | `false` | Skip GitHub Release creation |
| `release-verify-github-release` | No | `true` | Verify that a successful release has a GitHub Release |

Quality command inputs `audit`, `lint`, `test`, `build`, and `audit-baseline` are action-level shorthand. Homeboy Action runs them through Homeboy's review umbrella, for example `commands: test` emits `homeboy review test <component> --path <workspace>`. Commands that remain top-level in Homeboy, including `bench` and `refactor`, are emitted as top-level commands.

## Outputs

| Output | Description |
|--------|-------------|
| `results` | JSON object with pass/fail for each command (e.g. `{"lint":"pass","test":"fail"}`) |
| `binary-source` | How the binary was obtained: `source`, `fallback`, or `release` |
| `released` | Whether a release was created (`true`/`false`) |
| `release-version` | The released version number (e.g. `1.2.3`) |
| `release-tag` | The release git tag (e.g. `v1.2.3`) |
| `release-bump-type` | The bump type used (`patch`, `minor`, `major`) |
| `pr-policy-safe` | Whether the PR policy marked the PR safe for auto-merge (`true`/`false`) |
| `pr-policy-merged` | Whether the PR policy gate merged the PR (`true`/`false`) |
| `pr-policy-report` | Markdown summary from the PR policy gate |

## Examples

### Review Lint Only (Fast PR Check)

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: review lint
    args: --errors-only
    php-version: '8.3'
```

### Full Suite with Audit

```yaml
jobs:
  homeboy:
    uses: Extra-Chill/homeboy-action/.github/workflows/ci.yml@v2
    with:
      extension: wordpress
      commands: review audit,review lint,review test
      php-version: '8.3'
      node-version: '20'
    secrets: inherit
```

### Scope

By default, `scope: auto` uses changed-file scope for pull requests and full-workspace scope for non-PR events. Set `scope: changed` to force changed-file scope when a pull request base SHA is available, or `scope: full` to scan the full workspace.

### PR Scoped Checks (Changed Files)

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: review lint,review test,review audit
    php-version: '8.3'
    scope: 'changed'
```

### Split Jobs, Shared PR Comment

Use the reusable workflow for normal CI. Split jobs are still supported for
specialized cases, but the workflow author is responsible for dependency
semantics. Avoid chaining `audit -> lint -> test` with `needs` unless skipped
downstream checks are intentional.

```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: review lint

  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: review test

  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: review audit
```

All three jobs write to the **same PR comment** automatically.

### Deterministic PR Policy Gate

Use `pr-policy` to classify whether a PR is safe for deterministic auto-merge after Homeboy checks pass. Homeboy core reads changed files from GitHub, applies repo-local author, branch, path, and content rules, then exposes `pr-policy-safe` and optionally merges the PR.

```yaml
- uses: actions/checkout@v6

- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: review lint,review test
    pr-policy: .homeboy/pr-policy.yml
    pr-policy-merge: 'true'
    pr-policy-merge-method: squash
```

Example policy:

```yaml
merge:
  title: World PR merge policy
  allowed_authors:
    - github-actions[bot]
  allowed_head_branches:
    - world-day/**
  allowed_paths:
    - content/**
    - themes/world-of-wordpress/patterns/**
  blocked_paths:
    - .github/**
    - bundles/**
    - plugins/**
  blocked_content_patterns:
    - 'eval[[:space:]]*\('
    - 'shell_exec[[:space:]]*\('
  require_same_repository: true
  delete_branch_on_merge: true
```

Flat policy files from earlier action versions continue to work as merge policies. The gate fails closed for missing policy, unknown changed files, blocked paths, unexpected authors, fork PRs when `require_same_repository` is true, or blocked content patterns. Unsafe PRs are not merged; the action still emits outputs so a workflow can decide whether to fail, comment, or route to human review.

### Continuous Release with Quality Gate

Full example with quality checks before release and cargo-dist builds after:

```yaml
name: Release
on:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      dry-run:
        type: boolean
        default: false

concurrency:
  group: release
  cancel-in-progress: false

jobs:
  # Fast exit if nothing to release
  check:
    runs-on: ubuntu-latest
    outputs:
      should-release: ${{ steps.check.outputs.should-release }}
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - name: Check for releasable commits
        id: check
        run: |
          # ... scan conventional commits since last tag
          # Set should-release=true if fix:/feat:/breaking commits exist

  # Quality gate (only if releasing)
  gate:
    needs: check
    if: needs.check.outputs.should-release == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - run: cargo fmt --check && cargo clippy && cargo test
      - uses: Extra-Chill/homeboy-action@v2
        with:
          source: '.'
          extension: rust
          commands: review audit

  # Version bump + changelog + tag
  prepare:
    needs: [check, gate]
    runs-on: ubuntu-latest
    outputs:
      released: ${{ steps.release.outputs.released }}
      release-tag: ${{ steps.release.outputs.release-tag }}
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
          token: ${{ secrets.GITHUB_TOKEN }}
      - uses: Extra-Chill/homeboy-action@v2
        id: release
        with:
          extension: rust
          commands: release

  # Build artifacts (only if released)
  build:
    needs: prepare
    if: needs.prepare.outputs.released == 'true'
    # ... cargo-dist, upload artifacts

  # Publish prebuilt artifacts through Homeboy release.publish
  publish:
    needs: [prepare, build]
    if: needs.prepare.outputs.released == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          ref: ${{ needs.prepare.outputs.release-tag }}
          fetch-depth: 0
      - uses: actions/download-artifact@v7
        with:
          pattern: artifacts-*
          path: artifacts
          merge-multiple: true
      - uses: Extra-Chill/homeboy-action@v2
        with:
          source: '.'
          extension: rust
          commands: release
          release-head: 'true'
          release-from-artifacts: artifacts
```

### Recommended CI Profile

Prefer two lanes:

1. **PR lane:** fast, scoped feedback for the author.
2. **Main lane:** full-suite signal that can maintain issues when something reaches `main`.

This keeps PR comments lightweight while preventing the issue tracker from becoming a noisy task queue for speculative or changed-file-only findings.

#### Two-workflow strategy

Use `homeboy-pr.yml` for scoped PR checks:

```yaml
name: Homeboy PR

on:
  pull_request:

concurrency:
  group: homeboy-pr-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

jobs:
  quality:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: review lint,review test,review audit
          scope: changed
          php-version: '8.3'
```

Use `homeboy-main.yml` for full checks and issue maintenance:

```yaml
name: Homeboy Main

on:
  push:
    branches: [main]
  workflow_dispatch:

concurrency:
  group: homeboy-main-${{ github.ref }}
  cancel-in-progress: false

jobs:
  quality:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      issues: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: review lint,review test,review audit
          scope: full
          auto-issue: 'true'
          php-version: '8.3'
```

If you also run continuous release, keep release as its own workflow or separate job after the full quality gate. Release jobs should run `commands: release`; they should not be the only place full `review lint,review test,review audit` runs.

> **Avoid cron-based release triggers.** A `schedule` cron fires whether there are new commits or not. Push-to-main triggers the quality/release pipeline only when there is new code to evaluate.

#### Single workflow with default scope

If you prefer one workflow, keep the default `scope: auto` behavior and make only the issue-filing policy event-aware:

```yaml
name: Homeboy CI

on:
  pull_request:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  quality:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      issues: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: review lint,review test,review audit
          auto-issue: ${{ github.event_name != 'pull_request' && 'true' || 'false' }}
          php-version: '8.3'
```

#### Compatibility notes

`scope: changed` is a thin wrapper around the Homeboy CLI. When a pull request base SHA is available, the action resolves the base SHA and passes `--changed-since <base-sha>` to Homeboy for scoped commands.

Use changed scope for a command only when the installed Homeboy CLI and extension implement changed-file semantics for that command. If a command does not support changed scope yet, prefer one of these patterns:

- Omit that command from the PR lane and keep it in the main lane.
- Run that command with `scope: full` in a separate PR job if the runtime cost is acceptable.
- Fix changed-scope support in Homeboy or the extension rather than emulating it in workflow YAML.

The action does **not** probe for or emulate missing CLI features. If the installed Homeboy version does not support a requested scoped command, that is a Homeboy CLI compatibility problem to fix in Homeboy itself.

### Differential gating

Set `differential-gating: 'true'` to make PR `review audit` and `review test` checks compare against the pull request base SHA instead of failing solely because the current branch has existing debt:

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: rust
    commands: review audit,review test,review lint
    differential-gating: 'true'
```

When enabled on pull requests:

1. `review audit` and `review test` run in full-scope mode on the PR branch.
2. The action temporarily checks out the base SHA in the same workspace and captures base `review audit`/`review test` JSON.
3. The final gate passes `review audit`/`review test` failures when the parsed PR count is less than or equal to the parsed base count.
4. `review lint` is unchanged and still gates on the command exit code.

If the baseline checkout cannot be run safely or the structured counts cannot be parsed, the original failure is preserved.

#### Audit signal hygiene

Use auto-filed issues as a **task queue**, not as a dumping ground for every metric Homeboy can calculate. A good auto-filed issue should be current, concrete, and safe for a human or coding agent to act on.

Recommended policy:

| Signal type | CI handling |
|-------------|-------------|
| High-confidence, low-count findings | Allow auto-issue filing. These make good task-queue entries. |
| Test failures with clear clusters | Allow auto-issue filing from the main lane. Keep PR feedback in comments. |
| Generic CI command failures | Do not file issues. Keep these in CI output unless they produce categorized audit, lint, or test findings. |
| High-count trend metrics | Keep in job summaries or dashboards. Do not turn every item into a task issue. |
| Known noisy or research-only audit rules | Suppress from auto-issue filing with Homeboy audit config; keep them visible in full audit output. |

The main lane is the right place to maintain issue state because it runs against the full repository and can update, close, or suppress stale findings consistently. PR lanes should focus on author feedback and should not maintain long-lived audit issues from partial data.

When a rule is useful as a health metric but not safe as an actionable task list, keep it in job summaries or dashboards rather than turning every item into a tracker task. Homeboy Action delegates issue policy to `homeboy issues reconcile`, so auto-issue maintenance follows the current Homeboy CLI contract.

### PR Comment Identity

PR comments are posted only with `app-token`. This keeps Homeboy comments under the `homeboy-ci[bot]` identity and avoids silently falling back to `github-actions[bot]`. Configure `app-token` with `actions/create-github-app-token`; when it is unavailable, checks still run but the comment step is skipped with a warning.

### Fork PR Note

On fork-based pull requests, GitHub App secrets may be unavailable. Homeboy Action treats the PR comment step as best-effort — lint/test/audit execution still runs and determines job pass/fail.

## Failure Digest

On failed runs, Homeboy Action emits a **Failure Digest** to the job summary and PR comment:

- Tooling versions (Homeboy CLI, extension source/revision, action ref)
- Failed test count + top failed tests
- Audit summary (drift/outliers/top findings)
- Links back to the full workflow run logs

When multiple jobs invoke Homeboy Action on the same PR, they **merge into one shared PR comment** by default.

## How It Works

1. **Installs Homeboy** — Downloads the correct binary for your runner from GitHub Releases (or builds from source with `source: '.'`)
2. **Installs Extension** — Clones and sets up the specified extension
3. **Validates Portable Config** — Requires `homeboy.json` at repo root
4. **Runs Commands** — Executes each command with `--path` pointing at your workspace
5. **Release** — If `commands` includes `release`, checks for releasable commits, bumps version, generates changelog, tags, pushes, and creates a GitHub Release

## Requirements

- Homeboy must have published releases with binary artifacts (uses `cargo-dist`)
- Extensions must be installable via `homeboy extension install`
- For WordPress: PHP must be available (use `php-version` input or set up separately)
- Repository must include `homeboy.json` at root with a top-level `id`

### WordPress PHP compatibility

Set `php-version` to match your project's `composer.json` constraint. Modern WordPress plugin development targets **PHP 8.3+** — PHPUnit 12 and many current dependencies require it.

```yaml
php-version: '8.3'
```

If CI fails with `requires php >= X.Y`, either:
- Set `php-version` to `X.Y` or higher in your workflow, or
- Adjust the dependency constraint in `composer.json`

Common mismatch: PHPUnit 12 requires PHP >= 8.3. If your workflow uses `php-version: '8.2'`, either upgrade to 8.3 or pin PHPUnit to `^11` in `require-dev`.

## License

MIT
