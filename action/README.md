# Homeboy Action

GitHub Action for running [Homeboy](https://github.com/Extra-Chill/homeboy) lint, test, audit, bench, and release commands in CI.

Works with **any Homeboy extension** — WordPress, Rust, Node, or your own custom extension.

## Action Channel

Use the floating `v2` channel for normal GitHub Actions workflows:

```yaml
- uses: Extra-Chill/homeboy-action@v2
```

The action release stream is aligned with that channel. Release commits and tags use `v2.x.y`, and the floating `v2` tag moves to the latest compatible `v2.x.y` release.

## Quick Start

### PR Quality Checks

```yaml
name: CI
on: [pull_request]

concurrency:
  group: ci-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

jobs:
  homeboy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: lint,test
          php-version: '8.3'
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
      - uses: actions/checkout@v4
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

After the tag push, downstream build/publish jobs can pick it up (e.g. cargo-dist, npm publish).

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

Bench runs write the exact `homeboy bench --output` payload to `homeboy-ci-results/bench.json`, upload it as the `homeboy-ci-results` artifact, and render a compact PR-summary section when PR comments are enabled.

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
| `commands` | No | `lint,test` | Comma-separated commands to run |
| `expected-commands` | No | *(falls back to `commands`)* | Full set of command types expected to run across the workflow (e.g. `audit,lint,test`). Set this on every invocation when a workflow splits audit/lint/test across separate steps, otherwise each invocation will close sibling invocations' issues during reconciliation. |
| `component` | No | *(repo name)* | Component name (auto-detected from repo) |
| `args` | No | | Extra arguments passed to each command |
| `rig` | No | | Bench rig pair/list passed to `homeboy bench --rig` |
| `scenario` | No | | Bench scenario ID passed to `homeboy bench --scenario` |
| `runs` | No | | Bench run count passed to `homeboy bench --runs` |
| `iterations` | No | | Bench iteration count passed to `homeboy bench --iterations` |
| `regression-threshold` | No | | Bench regression threshold passed to `homeboy bench --regression-threshold` |
| `differential-gating` | No | `false` | On PRs, compare `audit`/`test` counts against the base SHA and fail only when the PR is worse. Opt-in; `lint` still gates on exit code. PR autofix is skipped while enabled. |
| `php-version` | No | | PHP version (sets up via `shivammathur/setup-php`) |
| `node-version` | No | | Node.js version (sets up via `actions/setup-node`) |
| `autofix` | No | `false` | On PR failures, run safe autofixes, commit, push, and re-run checks |
| `autofix-open-pr` | No | `false` | On non-PR failures, open an autofix PR if safe fixes allow rerun to pass |
| `autofix-max-commits` | No | `2` | Safety limit for autofix commit chain depth per branch |
| `autofix-commands` | No | | Override autofix commands (comma-separated, e.g. `lint --fix,test --fix`) |
| `autofix-label` | No | | Optional PR label required before autofix runs (e.g. `autofix`) |
| `scope` | No | `changed` | Execution scope: `changed` for PRs or `full` for entire codebase |
| `lint-changed-only` | No | `true` | Deprecated: use `scope` instead |
| `test-scope` | No | `changed` | Deprecated: use `scope` instead |
| `auto-issue` | No | *(auto)* | Auto-file issue on non-PR failures. Empty means enabled for non-PR events and disabled for PRs; set `false` to suppress issue filing. |
| `comment-key` | No | *(auto)* | Shared PR comment key so multiple jobs aggregate into one sticky comment |
| `comment-section-key` | No | *(auto)* | Section key within the shared PR comment |
| `comment-section-title` | No | *(auto)* | Visible heading for this section in the shared PR comment |
| `release-dry-run` | No | `false` | Preview the release without making changes |
| `release-branch` | No | `main` | Branch that releases are allowed from |
| `release-skip-changelog` | No | `false` | Skip auto-generating changelog entries from conventional commits |

## Outputs

| Output | Description |
|--------|-------------|
| `results` | JSON object with pass/fail for each command (e.g. `{"lint":"pass","test":"fail"}`) |
| `binary-source` | How the binary was obtained: `source`, `fallback`, or `release` |
| `released` | Whether a release was created (`true`/`false`) |
| `release-version` | The released version number (e.g. `1.2.3`) |
| `release-tag` | The release git tag (e.g. `v1.2.3`) |
| `release-bump-type` | The bump type used (`patch`, `minor`, `major`) |

## Examples

### Lint Only (Fast PR Check)

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: lint
    args: --errors-only
    php-version: '8.3'
```

### Full Suite with Audit

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.3'
    node-version: '20'
```

### PR Scoped Checks (Changed Files)

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.3'
    scope: 'changed'
```

### Split Jobs, Shared PR Comment

```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: lint

  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: test

  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: homeboy
          commands: audit
```

All three jobs write to the **same PR comment** automatically.

### Auto-apply Fixable CI Issues (PRs)

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: lint,test
    php-version: '8.3'
    autofix: 'true'
```

When enabled, the action will:
1. Run configured commands
2. If any fail, run safe autofix commands
3. Re-fetch the latest PR branch head and recompute fixes there when the branch moved underneath CI
4. Commit changes as `chore(ci): homeboy autofix ...`
5. Push directly to the PR branch when credentials allow
6. Re-run checks and report final status

For fork PRs, Homeboy Action now attempts the same direct-to-PR autofix flow first. Actual push success still depends on the token/permission model available to the workflow run.

**Merge guard:** If the PR is merged or closed while CI is running, autofix and PR comments are automatically skipped. This prevents zombie commits to deleted branches and stale result noise on already-merged PRs. Pair with a `concurrency` group to cancel the entire run early.

### Auto-open Fix PRs on non-PR runs

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.3'
    autofix: 'true'
    autofix-open-pr: 'true'
    auto-issue: 'true'
```

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
      - uses: actions/checkout@v4
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
      - uses: actions/checkout@v4
      - run: cargo fmt --check && cargo clippy && cargo test
      - uses: Extra-Chill/homeboy-action@v2
        with:
          source: '.'
          extension: rust
          commands: audit

  # Version bump + changelog + tag
  prepare:
    needs: [check, gate]
    runs-on: ubuntu-latest
    outputs:
      released: ${{ steps.release.outputs.released }}
      release-tag: ${{ steps.release.outputs.release-tag }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
          token: ${{ secrets.GITHUB_TOKEN }}
      - uses: Extra-Chill/homeboy-action@v2
        id: release
        with:
          extension: rust
          commands: release

  # Build + publish (only if released)
  build:
    needs: prepare
    if: needs.prepare.outputs.released == 'true'
    # ... cargo-dist, crates.io, Homebrew
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
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: lint,test,audit
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
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: lint,test,audit
          scope: full
          auto-issue: 'true'
          php-version: '8.3'
```

If you also run continuous release, keep release as its own workflow or separate job after the full quality gate. Release jobs should run `commands: release`; they should not be the only place full `lint,test,audit` runs.

> **Avoid cron-based release triggers.** A `schedule` cron fires whether there are new commits or not. Push-to-main triggers the quality/release pipeline only when there is new code to evaluate.

#### Single workflow with event-aware inputs

If you prefer one workflow, make the scope and issue-filing policy event-aware:

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
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: wordpress
          commands: lint,test,audit
          scope: ${{ github.event_name == 'pull_request' && 'changed' || 'full' }}
          auto-issue: ${{ github.event_name != 'pull_request' && 'true' || 'false' }}
          php-version: '8.3'
```

#### Compatibility notes

`scope: changed` is a thin wrapper around the Homeboy CLI. On PRs, the action resolves the base SHA and passes `--changed-since <base-sha>` to Homeboy for `audit`, `lint`, and `test`.

Use changed scope for a command only when the installed Homeboy CLI and extension implement changed-file semantics for that command. If a command does not support changed scope yet, prefer one of these patterns:

- Omit that command from the PR lane and keep it in the main lane.
- Run that command with `scope: full` in a separate PR job if the runtime cost is acceptable.
- Fix changed-scope support in Homeboy or the extension rather than emulating it in workflow YAML.

The action does **not** probe for or emulate missing CLI features. If the installed Homeboy version does not support a requested scoped command, that is a Homeboy CLI compatibility problem to fix in Homeboy itself.

### Differential gating

Set `differential-gating: 'true'` to make PR `audit` and `test` checks compare against the pull request base SHA instead of failing solely because the current branch has existing debt:

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    extension: rust
    commands: audit,test,lint
    differential-gating: 'true'
```

When enabled on pull requests:

1. `audit` and `test` run in full-scope mode on the PR branch.
2. The action temporarily checks out the base SHA in the same workspace and captures base `audit`/`test` JSON.
3. The final gate passes `audit`/`test` failures when the parsed PR count is less than or equal to the parsed base count.
4. `lint` is unchanged and still gates on the command exit code.

If the baseline checkout cannot be run safely or the structured counts cannot be parsed, the original failure is preserved.

#### Audit signal hygiene

Use auto-filed issues as a **task queue**, not as a dumping ground for every metric Homeboy can calculate. A good auto-filed issue should be current, concrete, and safe for a human or coding agent to act on.

Recommended policy:

| Signal type | CI handling |
|-------------|-------------|
| High-confidence, low-count findings | Allow auto-issue filing. These make good task-queue entries. |
| Test failures with clear clusters | Allow auto-issue filing from the main lane. Keep PR feedback in comments. |
| High-count trend metrics | Keep in job summaries or dashboards. Do not turn every item into a task issue. |
| Known noisy or research-only audit rules | Suppress from auto-issue filing with Homeboy audit config; keep them visible in full audit output. |

The main lane is the right place to maintain issue state because it runs against the full repository and can update, close, or suppress stale findings consistently. PR lanes should focus on author feedback and should not maintain long-lived audit issues from partial data.

When a rule is useful as a health metric but not safe as an actionable task list, configure it as dashboard-only or suppress it from issue reconciliation in Homeboy's audit config. Homeboy Action passes `--suppress-from-config` to `homeboy issues reconcile`, so repository-level audit policy is honored during auto-issue maintenance.

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
