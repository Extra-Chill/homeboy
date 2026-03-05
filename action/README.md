# Homeboy Action

GitHub Action for running [Homeboy](https://github.com/Extra-Chill/homeboy) lint, test, and audit commands on your PRs.

Works with **any Homeboy extension** — WordPress, Rust, Node, or your own custom extension.

## Quick Start

### WordPress Plugin/Theme

```yaml
name: Homeboy
on: [pull_request]

jobs:
  homeboy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: wordpress
          commands: lint,test
          php-version: '8.2'
```

### Required Portable Config (`homeboy.json`)

`homeboy.json` at repository root is required by Homeboy Action.
If your repo has a portable extension config, you don't need to specify the extension input:

```json
{
  "id": "my-project",
  "extensions": {
    "wordpress": {}
  }
}
```

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    commands: lint,test
    php-version: '8.2'
```

### Custom Extension

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: my-extension
    extension-source: https://github.com/my-org/my-homeboy-extensions
    commands: lint,test
```

## Inputs

| Input | Required | Default | Description |
|-------|----------|---------|-------------|
| `version` | No | `latest` | Homeboy version to install (e.g. `0.52.0`) |
| `extension` | No | | Extension ID (e.g. `wordpress`, `rust`, `node`) |
| `extension-source` | No | `Extra-Chill/homeboy-extensions` | Git URL to install the extension from |
| `commands` | No | `lint,test` | Comma-separated commands to run |
| `component` | No | *(repo name)* | Component name (auto-detected from repo) |
| `args` | No | | Extra arguments passed to each command |
| `settings` | No | | Deprecated. Use `homeboy.json` extension settings instead. |
| `php-version` | No | | PHP version (sets up via `shivammathur/setup-php`) |
| `node-version` | No | | Node.js version (sets up via `actions/setup-node`) |
| `autofix` | No | `false` | On PR failures, run safe autofixes, commit, push, and re-run checks |
| `autofix-open-pr` | No | `false` | On non-PR failures, open an autofix PR if safe fixes allow rerun to pass |
| `autofix-max-commits` | No | `2` | Safety limit for autofix commit chain depth per branch |
| `autofix-commands` | No | | Override autofix commands (comma-separated, e.g. `lint --fix,test --fix`) |
| `autofix-label` | No | | Optional PR label required before autofix runs (e.g. `autofix`) |
| `test-scope` | No | `full` | Test scope for PRs: `full` or `changed` (requires Homeboy test changed-since support) |
| `auto-issue` | No | `false` | Auto-file issue on non-PR failures (e.g. `push` to `main`) |

### Fork PR note

On fork-based pull requests, GitHub may provide a restricted `GITHUB_TOKEN` that cannot write PR comments or inline reviews.
Homeboy Action treats those publish steps as best-effort in that context:

- lint/test/audit execution still runs and determines job pass/fail
- PR comment/inline review publishing is skipped with a warning when token permissions are insufficient

This keeps CI reliable for external contributors while preserving strict token safety defaults.

## Outputs

| Output | Description |
|--------|-------------|
| `results` | JSON object with pass/fail for each command (e.g. `{"lint":"pass","test":"fail"}`) |

## Failure Digest

On failed runs, Homeboy Action now emits a compact **Failure Digest** to:

- the job summary (`GITHUB_STEP_SUMMARY`)
- the PR comment block (when running on pull requests)

Digest includes:

- tooling versions (Homeboy CLI, extension source/revision, action ref)
- failed test count + top failed tests
- audit summary (drift/outliers/top findings when structured output is available)
- links back to the full workflow run logs

Auto-filed failure issues on non-PR runs also include:

- **Primary failure** (first failed command + first fatal/error line)
- **Secondary findings** (additional failed commands)
- **Triage order** to reduce debugging time

Machine-readable files are written to the action output directory:

- `homeboy-test-failures.json`
- `homeboy-audit-summary.json`

## Examples

### Lint Only (Fast PR Check)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint
    args: --errors-only
    php-version: '8.2'
```

### Full Suite with Audit

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.2'
    node-version: '20'
```

### PR Scoped Checks (Changed Files)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.2'
    lint-changed-only: 'true'
    test-scope: 'changed'
```

> `test-scope: changed` requires Homeboy support for `homeboy test --changed-since`.
> If unsupported in your pinned Homeboy version, keep `test-scope: full`.

Homeboy Action now performs a capability probe for `test-scope: changed` on PRs.
If your installed Homeboy CLI does not support `--changed-since` for tests yet, the action automatically falls back to `full` test scope and emits a warning.

### Recommended org-wide CI profile

Use two workflows for clear signal:

1. **PR workflow** (fast + scoped)
   - `commands: lint,test,audit`
   - `lint-changed-only: 'true'`
   - `test-scope: 'changed'` (auto-falls back to `full` if unsupported)

2. **Main workflow** (authoritative)
   - trigger on `push` to `main` (or release/version bump branches)
   - `commands: lint,test,audit`
   - `test-scope: 'full'`
   - `auto-issue: 'true'`

Example main workflow step:

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    test-scope: 'full'
    auto-issue: 'true'
    php-version: '8.2'
    node-version: '20'
```

### Auto-apply Fixable CI Issues (PRs)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test
    php-version: '8.2'
    autofix: 'true'
```

When enabled, the action will:
1. Run configured commands
2. If any fail, run safe autofix commands (default: `lint --fix`, `test --fix` when present)
3. Commit changes as `chore(ci): apply homeboy autofixes`
4. Push to the PR branch
5. Re-run checks and report final status

> Autofix mode is PR-only and never force-pushes or amends commits.

### Auto-open Fix PRs on non-PR runs

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.2'
    autofix: 'true'
    autofix-open-pr: 'true'
    auto-issue: 'true'
```

Behavior:
- If CI fails, action runs safe autofix commands on a new `ci/autofix/*` branch.
- If rerun passes, action opens an autofix PR and skips auto-issue filing.
- If rerun still fails, action files/updates the CI failure issue with autofix attempt context.
- `autofix-max-commits` prevents infinite autofix loops by capping chain depth.

Optional label gate:

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test
    php-version: '8.2'
    autofix: 'true'
    autofix-label: 'autofix'
```

With `autofix-label`, no bot commit will be created unless that label is present on the PR.

### Configure Settings in `homeboy.json`

```yaml
{
  "id": "my-project",
  "extensions": {
    "wordpress": {
      "settings": {
        "database_type": "sqlite"
      }
    }
  }
}
```

### Skip Lint During Test (Run Separately)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint
    php-version: '8.2'

- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: test
    args: --skip-lint
    php-version: '8.2'
```

### Pin a Specific Homeboy Version

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    version: '0.52.0'
    extension: wordpress
    commands: lint,test
    php-version: '8.2'
```

### Use Results in Subsequent Steps

```yaml
- uses: Extra-Chill/homeboy-action@v1
  id: homeboy
  continue-on-error: true
  with:
    extension: wordpress
    commands: lint,test
    php-version: '8.2'

- name: Check results
  run: |
    echo "Results: ${{ steps.homeboy.outputs.results }}"
```

## How It Works

1. **Installs Homeboy** — Downloads the correct binary for your runner from GitHub Releases
2. **Installs Extension** — Clones and sets up the specified extension (runs `composer install`, etc.)
3. **Validates Portable Config** — Requires `homeboy.json` at repo root
4. **Runs Commands** — Executes each command with `--path` pointing at your workspace

The action is **extension-agnostic** — Homeboy is the orchestrator, extensions provide the actual lint/test/audit logic. The WordPress extension runs PHPCS, PHPUnit, and PHPStan. Other extensions can run whatever tools they need.

## Project Maintenance (Dogfooding Homeboy)

This repository dogfoods Homeboy project metadata and release bookkeeping:

- `homeboy.json` defines component metadata and changelog/version targets
- `docs/CHANGELOG.md` is the canonical changelog
- `VERSION` is the version source for Homeboy version automation

Use Homeboy to add changelog entries:

```bash
homeboy changelog add homeboy-action "Describe change" --type Changed
```

## Requirements

- Homeboy must have published releases with binary artifacts (uses `cargo-dist`)
- Extensions must be installable via `homeboy extension install`
- For WordPress: PHP must be available (use `php-version` input or set up separately)
- Repository must include `homeboy.json` at root with a top-level `id`

## License

MIT
