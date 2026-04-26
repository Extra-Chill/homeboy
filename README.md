# Homeboy

Code factory + fleet ops CLI. Audits for slop, lints, tests, refactors, releases, deploys, manages dev rigs, and ratchets performance benchmarks — all with a stable JSON envelope so AI agents and CI scripts can drive it without screen-scraping. If it can be fixed mechanically, Homeboy will find it and fix it without human input.

Homeboy ships four pillars from one binary:

- **Code Factory** — `audit` / `lint` / `test` / `refactor` / `release` with the autofix loop.
- **Fleet & Ops** — `deploy`, `ssh`, `file`, `db`, `logs`, `transfer`, `server`, `project`, `component`, `fleet`.
- **Dev Rig** — `rig`, `rig-spec`, and `stack` for reproducible, code-defined local dev environments and combined-fixes branches.
- **Bench** — performance benchmarks with baseline ratchet, sibling of `lint` / `test` / `build`.

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

## Capabilities

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

- **Safe** — deterministic fixes auto-applied with preflight validation (imports, registrations, namespace fixes, visibility changes, doc updates)
- **PlanOnly** — method stubs, function removals (human review required)

### Rig

Code-defined, reproducible local dev environments. A rig is a JSON spec at `~/.config/homeboy/rigs/<id>.json` that captures everything a dev setup needs — which components, which background services, which symlinks, which pre-flight invariants — and a linear pipeline that materializes it.

- **Service supervision** — `http-static` and `command` service kinds run detached, while `external` services let rigs adopt and stop processes they did not spawn.
- **Pipeline steps** — `service`, `command`, `symlink`, `shared-path`, `check`, `git`, `build`, and `patch`. Typed primitives reuse Homeboy's existing build/git plumbing instead of shelling out blindly.
- **Git ops** — `status`, `pull`, `push`, `fetch`, `checkout`, `current-branch`, `rebase`, and `cherry-pick`.
- **Stack specs** — `stack` materializes combined-fixes branches from a base ref plus a declared PR list, with status/sync helpers for dropping merged PRs.
- **Verbs** — `rig up` materializes the env, `rig check` reports health without fail-fast, `rig down` tears it down, `rig status` reports running services and last run timestamps.
- **Variable expansion** — `${components.<id>.path}`, `${env.<NAME>}`, and `~` work across `cwd`, `command`, `link`, `target`, and check fields.

The use case: cross-repo setups that today live as wiki runbooks (Studio + Playground combined-fixes, WordPress core + Gutenberg dev, sandbox + tunnel, etc).

### Bench

Performance benchmarks as a first-class capability, sibling of `lint` / `test` / `build`. Extensions provide the runner; Homeboy owns regression detection and the baseline ratchet.

- **Baseline storage** — per-scenario snapshots stored in `homeboy.json` under `baselines.bench`. `--baseline` saves, `--ratchet` auto-updates on improvement, `--ignore-baseline` skips comparison.
- **Regression policy** — runners declare `metric_policies` for arbitrary metrics (latency, throughput, error rate, memory). Direction (`lower_is_better` / `higher_is_better`) and percent/absolute tolerances are per-metric. Legacy fallback compares `p95_ms` with `--regression-threshold` (default 5%).
- **Rig-pinned baselines** — `--rig <id>` keys the baseline as `bench.rig.<id>` so per-environment runs don't fight each other.
- **Strict envelope** — runner output schema is locked at the top level; scenario-level extras are tolerated for diagnostics. Regressions exit `1` regardless of the runner's own exit code.

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

### Code Factory

| Command | What it does |
|---------|-------------|
| `audit` | Discover conventions, flag drift, autofix. Baseline ratchet. |
| `lint` | Format and static analysis with autofix. |
| `test` | Run tests. Drift detection for renamed/deleted symbols. |
| `refactor` | Structural renaming, decomposition, and auto-refactor with safety tiers. |
| `review` | Scoped audit + lint + test umbrella for PR-style changes. |
| `release` | Automated version bump + changelog + tag + push from conventional commits. |
| `version` | Semantic version management with configurable file targets. |
| `changelog` | Add/finalize categorized changelog entries. |
| `changes` | Show commits and diffs since last version tag. |
| `build` | Build a component using its configured build command. |
| `validate` | Run extension parse/compile validation without a full test suite. |
| `git` | Component-aware git operations. |
| `issues` | Reconcile audit findings against an issue tracker. |
| `status` | Repo state overview: uncommitted, needs-bump, ready, docs-only. |

### Fleet & Ops

| Command | What it does |
|---------|-------------|
| `deploy` | Push components to projects. Single, multi-project, fleet, or shared. |
| `ssh` | Managed SSH connections to configured servers. |
| `file` | Remote file operations: list, read, write, find, grep. |
| `db` | Remote database queries, search, and tunneling. |
| `logs` | Remote log viewing and searching with live tailing. |
| `transfer` | File transfer between servers or local/remote. |
| `server` | Manage server connection definitions. |
| `project` | Manage project definitions and their server bindings. |
| `component` | Manage component definitions (plugins, themes, CLIs, libraries). |
| `fleet` | Create and manage named groups of projects. |

### Dev Rig

| Command | What it does |
|---------|-------------|
| `rig` | Bring up / tear down / health-check reproducible local dev environments. |
| `rig-spec` | Inspect and validate the JSON spec format used by `rig`. |
| `stack` | Manage combined-fixes branches from base refs plus cherry-picked PRs. |

### Bench

| Command | What it does |
|---------|-------------|
| `bench` | Run performance benchmarks with baseline ratchet and regression gating. |

### Meta

| Command | What it does |
|---------|-------------|
| `init` | Deprecated alias for `status --full`. |
| `auth` | Authenticate with a project's API; credentials stored in OS keychain. |
| `api` | Direct authenticated calls against a project's API. |
| `audit-rules` | Inspect and manage audit rules and confidence gating. |
| `extension` | Install, list, and update extensions. |
| `list` | List registered components, projects, servers, fleets. |
| `config` | Read and write Homeboy configuration. |
| `undo` | Roll back the last Homeboy write operation when an undo snapshot exists. |
| `upgrade` | Self-upgrade the homeboy binary. |
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
