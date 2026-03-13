# Homeboy

The code factory that audits for slop, lints, tests, refactors, updates your changelog, and releases a new version in CI.

## How It Works

You push code. Homeboy does the rest.

```
merge to main
     |
     v
  ┌──────────────────────────────────────────────┐
  │  cron wakes up (every 15 min)                │
  │                                              │
  │  1. releasable commits?  (feat: / fix:)      │
  │  2. audit    → find slop, autofix, ratchet   │
  │  3. lint     → format, autofix, commit back  │
  │  4. test     → run suite, fix what it can    │
  │  5. version bump   (from commit types)       │
  │  6. changelog      (from commit messages)    │
  │  7. tag + push                               │
  │  8. cross-platform builds (5 targets)        │
  │  9. publish: GitHub + crates.io + Homebrew   │
  │ 10. auto-refactor  (post-release cleanup)    │
  └──────────────────────────────────────────────┘
     |
     v
  humans provide features, code maintains itself
```

No version files to edit. No changelog to write. No release button to click.

- `fix:` commit → **patch** release
- `feat:` commit → **minor** release
- `BREAKING CHANGE` → **major** release
- `chore:` / `ci:` / `docs:` / `test:` → no release

## Quick Start

### 1. Add `homeboy.json` to your repo

```json
{
  "id": "my-project",
  "extensions": {
    "rust": {}
  }
}
```

### 2. Add CI

```yaml
name: CI
on: [pull_request]

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: rust
          commands: audit,lint,test
          autofix: 'true'
```

### 3. Add continuous release

```yaml
name: Release
on:
  schedule:
    - cron: '*/15 * * * *'
  workflow_dispatch:

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: rust
          component: my-project
          commands: release
```

That's it. PRs get quality checks with autofix. Main gets continuous releases. See [code-factory.md](docs/code-factory.md) for the full pipeline architecture with quality gates, baseline ratchet, and autofix loops.

## What Homeboy Checks

### Audit

Discovers conventions from your codebase and flags drift. Unlike traditional linters that enforce external rules, audit **learns your patterns** and catches outliers.

- **Convention compliance** — naming patterns, interface contracts, structural patterns
- **Duplication** — exact duplicates, near-duplicates, parallel implementations
- **Dead code** — unreferenced exports, orphaned functions, unused parameters
- **Test coverage** — missing test files, missing test methods, orphaned tests
- **Structural health** — god files, high complexity
- **Documentation** — broken references, stale claims

The baseline ratchet ensures the codebase **never gets worse**. New findings fail CI. Resolved findings auto-ratchet the baseline down. Over time, the baseline trends toward zero.

### Lint

Language-specific formatting and static analysis. Autofix commits formatting changes back to the PR.

### Test

Runs the project's test suite. Supports test drift detection — when source symbols are renamed or deleted, Homeboy identifies affected tests.

### Refactor

Structural improvements with safety tiers:

- **SafeAuto** — import additions, doc reference updates (always auto-applied)
- **SafeWithChecks** — registration stubs, namespace fixes (auto-applied after preflight validation)
- **PlanOnly** — method stubs, function removals (human review required)

## The Autofix Loop

When a CI stage fails:

1. Run fix commands (`homeboy audit --fix --write`, `homeboy lint --fix`)
2. Commit changes as `chore(ci): apply homeboy autofixes`
3. Push using a GitHub App token (re-triggers CI — `GITHUB_TOKEN` pushes don't)
4. Re-run the full pipeline to verify
5. Max-commits guard prevents infinite loops

For PRs: fixes commit directly to the PR branch. For releases on protected branches: opens an autofix PR.

## Beyond CI: Fleet Operations

Homeboy also manages the relationship between **components**, **projects**, and **servers**.

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  COMPONENT  │────>│   PROJECT   │────>│   SERVER    │
│  Plugin,    │     │  Site or    │     │  VPS, host, │
│  theme, CLI │     │  application│     │  cloud...   │
└─────────────┘     └─────────────┘     └─────────────┘
                          │
                    ┌─────┴─────┐
                    │   FLEET   │
                    │  Group of │
                    │  projects │
                    └───────────┘
```

Deploy components to servers, manage SSH connections, run remote commands, tail logs, query databases, transfer files — all from one CLI with structured JSON output.

## Commands

| Command | What it does |
|---------|-------------|
| `audit` | Discover conventions, flag drift, autofix. Baseline ratchet. |
| `lint` | Format and static analysis with autofix. |
| `test` | Run tests. Drift detection for renamed/deleted symbols. |
| `refactor` | Structural renaming, decomposition, and auto-refactor with safety tiers. |
| `release` | Automated version bump + changelog + tag + push from conventional commits. |
| `deploy` | Push components to projects. Single, multi-project, fleet, or shared. |
| `version` | Semantic version management with configurable file targets. |
| `changelog` | Add/finalize categorized changelog entries. |
| `changes` | Show commits and diffs since last version tag. |
| `status` | Repo state overview: uncommitted, needs-bump, ready. |
| `build` | Build a component using its configured build command. |
| `git` | Component-aware git operations. |
| `ssh` | Managed SSH connections to configured servers. |
| `file` | Remote file operations: list, read, write, find, grep. |
| `db` | Remote database queries, search, and tunneling. |
| `logs` | Remote log viewing and searching with live tailing. |
| `transfer` | File transfer between servers or local/remote. |
| `fleet` | Create and manage named groups of projects. |
| `docs` | Browse embedded documentation. All docs ship in the binary. |

Extensions add platform-specific commands at runtime (e.g., `homeboy wp` for WordPress, `homeboy cargo` for Rust).

## Output Contract

Every command returns structured JSON:

```json
{
  "success": true,
  "data": { ... }
}
```

Error codes are stable and namespaced (`config.*`, `ssh.*`, `deploy.*`, `git.*`). Exit codes map to categories. This makes Homeboy reliable for AI agents and automation pipelines.

## Extensions

Extensions add platform-specific behavior. Installed from git repos, stored in `~/.config/homeboy/extensions/`.

| Extension | What it provides |
|-----------|-----------------|
| `rust` | Cargo integration, crates.io publishing, release artifacts |
| `wordpress` | WP-CLI integration, WordPress-aware build/test/lint |
| `nodejs` | PM2 process management |
| `github` | GitHub release publishing |
| `homebrew` | Homebrew tap publishing |
| `swift` | Swift testing for macOS/iOS projects |

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
```

Browse available extensions: [homeboy-extensions](https://github.com/Extra-Chill/homeboy-extensions)

## Configuration

Global config lives in `~/.config/homeboy/`. Per-repo config lives in `homeboy.json` at the repository root.

```
~/.config/homeboy/
├── homeboy.json       # Global defaults
├── components/        # Component definitions
├── projects/          # Project definitions
├── servers/           # Server connections
├── fleets/            # Fleet definitions
├── extensions/        # Installed extensions
└── keys/              # SSH keys
```

The portable `homeboy.json` in your repo is all CI needs — no registered component required.

## Hooks

Components and extensions can declare lifecycle hooks:

| Event | When | Failure mode |
|-------|------|-------------|
| `pre:version:bump` | After version files updated, before commit | Fatal |
| `post:version:bump` | After pre-bump hooks, before commit | Fatal |
| `post:release` | After release pipeline completes | Non-fatal |
| `post:deploy` | After deploy completes on remote | Non-fatal |

## GitHub Action

[homeboy-action](https://github.com/Extra-Chill/homeboy-action) runs Homeboy in CI. Installs the binary, sets up extensions, runs commands, posts PR comments with per-command status, and handles the autofix loop.

See [homeboy-action README](https://github.com/Extra-Chill/homeboy-action) for full documentation.

## Installation

```bash
# Homebrew (macOS/Linux)
brew tap Extra-Chill/homebrew-tap
brew install homeboy

# Cargo
cargo install homeboy

# From source
git clone https://github.com/Extra-Chill/homeboy.git
cd homeboy && cargo install --path .
```

## Documentation

All documentation is embedded in the binary:

```bash
homeboy docs list                           # Browse all topics
homeboy docs code-factory                   # The Code Factory pipeline
homeboy docs commands/deploy                # Command reference
homeboy docs schemas/component-schema       # Config schemas
homeboy docs architecture/release-pipeline  # System internals
```

## License

MIT License — Created by [Chris Huber](https://chubes.net)
