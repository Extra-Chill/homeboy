# Code Factory

The code factory is homeboy's model for automated code maintenance: humans write features, the system maintains quality. It combines lint, test, audit, autofix, continuous release, and deployment into a self-improving pipeline.

## Core Concept

Every code change flows through a strict serial pipeline:

```
lint + fix → test + fix → audit + fix → release → deploy
```

Each stage checks for problems, fixes what it can, and commits the fixes back. If anything was fixed, the pipeline re-runs to verify. Only clean code advances to the next stage.

## Two Modes

### PR Pipeline (scoped)

When a pull request is opened, the pipeline runs scoped to changed files only. This means PRs only fail on problems they introduce, not pre-existing debt.

```
PR opened
  → lint + fix (changed files only)
  → test + fix (changed files only)
  → audit + fix (changed files only, baseline ratchet)
  → re-run if anything was fixed
  → merge when green
```

### Release Pipeline (unscoped)

When the release cron triggers, the pipeline runs against the entire codebase. This is where accumulated debt gets cleaned up — every release is an opportunity to improve.

```
cron trigger (every 15 minutes)
  → check for releasable commits (conventional commit types)
  → lint + fix (whole codebase)
  → test + fix (whole codebase)
  → audit + fix (whole codebase)
  → version bump + changelog generation
  → cross-platform binary builds
  → publish (GitHub Releases, crates.io, Homebrew)
  → deploy to fleet (planned)
```

## The Autofix Loop

The key mechanism is the autofix loop. When a stage fails:

1. The stage runs fix commands (`homeboy refactor --from all --write`)
2. If fixes produce file changes, they're committed as `chore(ci): apply homeboy autofixes`
3. The commit is pushed using a GitHub App token (this triggers a CI re-run, unlike `GITHUB_TOKEN` which doesn't)
4. The full pipeline re-runs and verifies the fixes
5. A max-commits guard prevents infinite loops

For PRs, the autofix bot commits directly to the PR branch. For releases on protected branches, it opens an autofix PR instead.

## Baseline Ratchet

Not all findings can be auto-fixed. The baseline ratchet handles this:

- Each component stores a baseline of known findings in `homeboy.json`
- CI only fails on **new** findings (not pre-existing ones)
- When autofix resolves findings, the baseline auto-ratchets down
- Over time, the baseline trends toward zero without human intervention

The ratchet rule: new findings fail CI, resolved findings update the baseline. The codebase improves monotonically.

## Stages

### Lint

Runs language-specific formatting and static analysis checks.

**Rust example:**
- `cargo fmt` — code formatting
- `cargo clippy` — lint warnings and errors

**Autofix:** `homeboy refactor --from lint --write` runs the extension's fixer (e.g., `cargo fmt` + `cargo clippy --fix` for Rust, `phpcbf` for WordPress).

### Test

Runs the project's test suite.

**Rust example:**
- `cargo test` — unit and integration tests

**Autofix:** `homeboy refactor --from test --write` runs language-specific test fixers (e.g., auto-updating test snapshots).

### Audit

Homeboy's convention-based code quality analysis. Unlike traditional linters that enforce external rules, audit discovers conventions from your codebase and flags outliers.

**What it checks:**
- **Convention compliance** — discovers naming patterns, interface contracts, and structural patterns across your codebase, then flags files that don't conform
- **Duplication** — exact duplicates, near-duplicates, and parallel implementations
- **Dead code** — unreferenced exports, orphaned private functions, unused parameters
- **Test coverage** — missing test files, missing test methods, orphaned tests
- **Structural health** — god files, high item counts
- **Documentation** — broken references, stale claims, missing feature docs

**Autofix:** `homeboy refactor --from audit --write` generates test scaffolds, narrows visibility, adds missing imports, and more. Findings that can't be auto-fixed are filed as GitHub issues.

### Release

Automated versioning and publishing from conventional commits.

**Version bumps are computed from commit types:**
- `fix:` → patch (0.0.x)
- `feat:` → minor (0.x.0)
- `BREAKING CHANGE` → major (x.0.0)
- `chore:`, `ci:`, `docs:`, `test:` → no release

**The release pipeline:**
1. Check for releasable commits since last tag
2. Quality gate (lint + test + audit, with autofix)
3. Auto-generate changelog entries from conventional commits
4. Bump version in all configured version targets
5. Git commit, tag, push
6. Cross-platform binary builds
7. Publish to configured targets (GitHub Releases, crates.io, Homebrew)

No human input needed. The cron runs every 15 minutes, and if there are releasable commits that pass the quality gate, a release happens automatically.

## Setting Up a Code Factory

### 1. Configure your component

Add a `homeboy.json` to your project root:

```json
{
  "id": "my-project",
  "extensions": {
    "rust": {}
  },
  "versionTargets": [
    { "file": "Cargo.toml", "pattern": "^version = \"(.+)\"$" }
  ]
}
```

### 2. PR pipeline (`.github/workflows/ci.yml`)

```yaml
name: CI
on:
  pull_request:
    branches: [main]

permissions:
  contents: write
  pull-requests: write

jobs:
  lint:
    name: Lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.head_ref }}
          fetch-depth: 0

      - name: Generate GitHub App token
        id: app-token
        uses: actions/create-github-app-token@v1
        continue-on-error: true
        with:
          app-id: ${{ secrets.HOMEBOY_APP_ID }}
          private-key: ${{ secrets.HOMEBOY_APP_PRIVATE_KEY }}

      - uses: Extra-Chill/homeboy-action@v1
        with:
          source: '.'
          extension: rust
          component: my-project
          commands: lint
          autofix: 'true'
          autofix-mode: 'on-failure'
          autofix-max-commits: '3'
          app-token: ${{ steps.app-token.outputs.token || '' }}

  test:
    name: Test
    needs: [lint]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.head_ref }}
          fetch-depth: 0

      - uses: Extra-Chill/homeboy-action@v1
        with:
          source: '.'
          extension: rust
          component: my-project
          commands: test

  audit:
    name: Audit
    needs: [test]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.head_ref }}
          fetch-depth: 0

      - name: Generate GitHub App token
        id: app-token
        uses: actions/create-github-app-token@v1
        continue-on-error: true
        with:
          app-id: ${{ secrets.HOMEBOY_APP_ID }}
          private-key: ${{ secrets.HOMEBOY_APP_PRIVATE_KEY }}

      - uses: Extra-Chill/homeboy-action@v1
        with:
          source: '.'
          extension: rust
          component: my-project
          commands: audit
          autofix: 'true'
          autofix-mode: 'always'
          autofix-commands: 'refactor --from audit --write'
          autofix-max-commits: '3'
          app-token: ${{ steps.app-token.outputs.token || '' }}
```

### 3. GitHub App for autofix commits

Autofix commits need a GitHub App token to trigger CI re-runs. Create a GitHub App with:
- **Repository permissions:** Contents (read/write), Pull requests (read/write)
- **No webhook URL needed**
- Store the App ID and private key as repository or organization secrets (`HOMEBOY_APP_ID`, `HOMEBOY_APP_PRIVATE_KEY`)

This is required because pushes using `GITHUB_TOKEN` never trigger workflows (hardcoded GitHub anti-loop rule). The App token bypasses this.

### 4. Portable config (`homeboy.json`)

The `homeboy.json` file in your repository root is the portable configuration. It travels with the code and provides everything CI needs without a registered component in `~/.config/homeboy/`.

Key fields:

```json
{
  "id": "my-project",
  "type": "plugin",
  "extensions": { "rust": {} },
  "versionTargets": [
    { "file": "Cargo.toml", "pattern": "^version = \"(.+)\"$" },
    { "file": "VERSION", "pattern": "^(.+)$" }
  ],
  "baselines": {
    "audit": { ... },
    "test": { ... }
  }
}
```

## Design Principles

1. **Fix, don't flag.** The default response to a problem is to fix it automatically. Filing an issue or failing CI is the fallback when autofix isn't possible.

2. **Scoped for PRs, unscoped for releases.** PRs shouldn't inherit legacy debt. Releases clean up the whole codebase.

3. **Serial pipeline.** Each stage sees the output of the previous stage's fixes. No parallel races, no conflicting commits.

4. **Convention over configuration.** Audit discovers your patterns instead of requiring you to configure rules. The codebase teaches the tool.

5. **Monotonic improvement.** The baseline ratchet ensures the codebase never gets worse. Every merge is at least as good as what came before.

6. **Humans provide features, code maintains itself.** The end state is a system where the only human work is writing new features. Formatting, test scaffolding, convention compliance, versioning, changelogs, releases, and deployment are all automated.
