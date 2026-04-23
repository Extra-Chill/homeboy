# Homeboy Action

GitHub Action for running [Homeboy](https://github.com/Extra-Chill/homeboy) lint, test, audit, and release commands in CI.

Works with **any Homeboy extension** — WordPress, Rust, Node, or your own custom extension.

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

      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: wordpress
          commands: lint,test
          php-version: '8.3'
```

### Continuous Release

Fully automated releases — no human input needed. Triggers on every push to main, checks for releasable conventional commits since the last tag, computes the version, generates changelog, bumps version targets, tags, and publishes.

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

      - uses: Extra-Chill/homeboy-action@v1
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
7. Commits, creates an annotated tag, and pushes

After the tag push, downstream build/publish jobs can pick it up (e.g. cargo-dist, npm publish).

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
| `component` | No | *(repo name)* | Component name (auto-detected from repo) |
| `args` | No | | Extra arguments passed to each command |
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
| `auto-issue` | No | `false` | Auto-file issue on non-PR failures |
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
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint
    args: --errors-only
    php-version: '8.3'
```

### Full Suite with Audit

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.3'
    node-version: '20'
```

### PR Scoped Checks (Changed Files)

```yaml
- uses: Extra-Chill/homeboy-action@v1
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
      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: rust
          component: homeboy
          commands: lint

  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: rust
          component: homeboy
          commands: test

  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: rust
          component: homeboy
          commands: audit
```

All three jobs write to the **same PR comment** automatically.

### Auto-apply Fixable CI Issues (PRs)

```yaml
- uses: Extra-Chill/homeboy-action@v1
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
- uses: Extra-Chill/homeboy-action@v1
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
      - uses: Extra-Chill/homeboy-action@v1
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
      - uses: Extra-Chill/homeboy-action@v1
        id: release
        with:
          extension: rust
          commands: release

  # Build + publish (only if released)
  build:
    needs: prepare
    if: needs.prepare.outputs.released == 'true'
    # ... cargo-dist, crates.io, Homebrew, GitHub Release
```

### Recommended Org-wide CI Profile

Use two workflows:

1. **PR workflow** (fast + scoped)
   - `commands: lint,test,audit`
   - `scope: 'changed'`
   - `concurrency` group per PR number to cancel stale runs

2. **Release workflow** (continuous)
   - trigger on `push` to `main` + `workflow_dispatch`
   - `commands: lint,test,audit` with `auto-issue: 'true'`
   - separate release workflow uses `commands: release`
   - quality gate before release

> **Avoid cron-based release triggers.** A `schedule` cron fires every 15 minutes regardless of whether there are new commits — that's 96 unnecessary CI runs per day. Push-to-main triggers the release pipeline only when there's actually something to release.

### Scope behavior

`scope: 'changed'` is a thin wrapper around the Homeboy CLI. On PRs, the action resolves the base SHA and passes `--changed-since <base-sha>` to Homeboy for `audit`, `lint`, and `test`.

The action does **not** probe for or emulate missing CLI features. If the installed Homeboy version does not support a requested scoped command, that is a Homeboy CLI compatibility problem to fix in Homeboy itself.

### Fork PR Note

On fork-based pull requests, GitHub may provide a restricted `GITHUB_TOKEN` that cannot write PR comments. Homeboy Action treats the PR comment step as best-effort — lint/test/audit execution still runs and determines job pass/fail.

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
5. **Release** — If `commands` includes `release`, checks for releasable commits, bumps version, generates changelog, tags, and pushes

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
