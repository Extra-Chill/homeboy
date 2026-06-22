# Homeboy

Headless automation for agentic software engineering workflows.

Homeboy gives every repo and multi-component project the same operational
surface: check it, review it, test it, benchmark it, trace it, release it, and
produce structured evidence that humans, CI systems, scheduled jobs, and coding
agents can act on without scraping terminal logs.

It is built for the way modern software work actually happens: many branches,
many worktrees, many agents, and many projects moving at once.

Homeboy core is intentionally domain-agnostic. The CLI owns orchestration,
configuration, structured output, persisted runs, baselines, remote execution,
and release/evidence workflows. Domain-specific behavior lives in extensions:
Rust, WordPress, Node.js, GitHub, Homebrew, Swift, and future ecosystems plug in
without teaching core what those platforms mean.

## What It Is For

Homeboy is most useful when you want repeatable commands across local
development, CI, and agent-driven work:

- **Local and CI quality gates** — Run `audit`, `lint`, `test`, `build`, and
  `review` through the same component-aware interface.
- **AI-ready evidence** — Write stable JSON artifacts with `--output` so coding
  agents can inspect results, make decisions, and hand findings back to humans
  without parsing terminal logs.
- **Parallel development loops** — Keep worktrees, changed-file checks,
  baselines, ratchets, PR comments, and review artifacts consistent while many
  fixes are cooking at once.
- **Benchmarks and behavioral traces** — Capture performance and black-box
  behavior with persisted runs, baselines, comparisons, and regression gates.
- **Multi-component dev environments** — Use rigs and stacks to reproduce
  local environments and combined-fixes branches.
- **Release and deploy workflows** — Plan versions, changelogs, tags, publish
  steps, and deploys from component metadata and commit history.
- **Headless orchestration** — Run the same workflows from a terminal, GitHub
  Actions, scheduled automation, a local daemon/API, or a connected runner.
- **Remote and fleet operations** — For configured environments, operate
  projects and server fleets over SSH. This supports server updates and remote
  inspection, but it is an advanced workflow rather than the core local loop.

## Mental Model

Homeboy starts with a component and keeps the command model consistent across
where that component runs. Core supplies the generic automation substrate;
extensions supply ecosystem knowledge.

```text
                          ┌─────────────────────────────┐
                          │        homeboy.json          │
                          │ component id + extensions   │
                          └──────────────┬──────────────┘
                                         │
         ┌───────────────────────────────┼───────────────────────────────┐
         │                               │                               │
         v                               v                               v
┌────────────────┐              ┌────────────────┐              ┌────────────────┐
│ Local terminal │              │ GitHub Actions │              │ Coding agents  │
│ audit/test/run │              │ checks/release │              │ JSON evidence  │
└───────┬────────┘              └───────┬────────┘              └───────┬────────┘
        │                               │                               │
        v                               v                               v
┌────────────────┐              ┌────────────────┐              ┌────────────────┐
│ Dev substrate  │              │ Release/deploy │              │ Attention loop │
│ rig/stack/bench│              │ tag/changelog  │              │ triage/report  │
└────────────────┘              └────────────────┘              └────────────────┘
```

The portable repo-level `homeboy.json` is enough for local checks and CI.
Global config in `~/.config/homeboy/` adds reusable components, projects,
servers, fleets, rigs, stacks, and extensions.

```text
┌────────────────────────────────────────────────────────────────────┐
│ Homeboy core                                                       │
│ commands, config, scope resolution, JSON output, runs, runners     │
└───────────────────────────────┬────────────────────────────────────┘
                                │ extension contracts
                                v
┌────────────────────────────────────────────────────────────────────┐
│ homeboy-extensions and custom extensions                           │
│ Rust/Cargo, WordPress/WP-CLI, Node.js, GitHub, Homebrew, Swift      │
└────────────────────────────────────────────────────────────────────┘
```

## Command Families

| Family | Commands | Purpose |
|--------|----------|---------|
| **Quality** | `audit`, `lint`, `test`, `build`, `review`, `refactor` | Keep codebases healthy and reviewable. |
| **Evidence** | `bench`, `trace`, `runs`, `report` | Capture benchmark, behavior, and review artifacts. |
| **Dev substrate** | `rig`, `stack`, `git`, `status`, `doctor` | Manage local context, combined branches, and diagnostics. |
| **Release** | `release`, `version`, `changelog`, `changes` | Turn commit history into versions, tags, and release notes. |
| **Attention** | `deps`, `triage`, `issues` | Find dependency drift, failing work, and tracked findings. |
| **Remote ops** | `deploy`, `ssh`, `file`, `db`, `logs`, `server`, `project`, `component`, `fleet` | Operate configured projects, servers, and fleets. |
| **Platforms** | `extension`, extension-provided verbs such as `wp` and `cargo` | Route platform-specific behavior through extensions. |
| **Meta** | `config`, `docs`, `daemon`, `self`, `undo`, `auth`, `api`, `upgrade` | Manage Homeboy itself and its API surfaces. |

For exhaustive command docs, see
[docs/commands/commands-index.md](docs/commands/commands-index.md) or run
`homeboy docs list`.

Agents can inspect the recursive command safety manifest with `homeboy list --json`.
The manifest includes command paths, mutation/operator classification, dry-run
flags, dangerous flags, structured-output notes, Lab routing metadata, and docs
paths while `homeboy list` continues to print the raw top-level help.

## Quick Start

### 1. Add `homeboy.json`

Put a portable component config at the repo root:

```json
{
  "id": "my-project",
  "extensions": {
    "rust": {}
  }
}
```

Use the extension that matches the repo. For example, Rust projects can use
`rust`, and WordPress projects can use `wordpress`.

### 2. Run local quality gates

Start locally before wiring CI:

```bash
homeboy audit
homeboy lint
homeboy test
homeboy review
```

The same commands can target a registered component or a specific checkout:

```bash
homeboy review my-project --changed-since origin/main
homeboy lint my-project --path /path/to/worktree
```

### 3. Produce artifacts for agents and CI

Most commands can write command-specific JSON with `--output <path>` while
still printing human output to stdout:

```bash
homeboy review --changed-since origin/main --output homeboy-ci-results/review.json
homeboy bench my-project --output homeboy-ci-results/bench.json
homeboy triage workspace --output homeboy-ci-results/triage.json
```

That contract is the handoff point for automation. A person can read the
terminal output; a CI job, scheduled task, or coding agent can read the JSON.

### 4. Add CI when the local loop is useful

`Extra-Chill/homeboy-action@v2` installs Homeboy in GitHub Actions, sets up
extensions, runs commands, posts PR comments, and can apply safe autofixes.

```yaml
name: Homeboy
on:
  pull_request:
  workflow_dispatch:

jobs:
  quality:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          commands: audit,lint,test,review
```

If a pull request needs a fresh quality run without pushing another commit,
rerun the workflow from the GitHub Actions UI or with
`gh workflow run <workflow-file> --ref <branch>`.

## Core Workflows

### Code Quality And Review

`homeboy audit` detects convention drift, duplication, dead code, test coverage
gaps, stale docs, and structural risk. Baselines let existing debt stay visible
while preventing new regressions.

`homeboy lint` delegates formatting and static analysis to the configured
extension. `homeboy test` runs the component's test suite. `homeboy review` is
the PR-shaped umbrella that runs scoped audit, lint, and test checks and can
render a PR-comment report.

```bash
homeboy audit --changed-since origin/main
homeboy lint --changed-only
homeboy test
homeboy review --changed-since origin/main --report pr-comment
```

`homeboy refactor` handles structural mechanical edits such as naming changes.
Safe rewrites can be applied automatically; higher-risk plans stay explicit.

### Coding Many Things At Once

Homeboy is intentionally useful in a crowded workspace: human branches, agent
branches, stacked fixes, and CI all need comparable signals.

```text
┌──────────────┐     ┌──────────────┐     ┌────────────────────┐
│ worktree A   │     │ worktree B   │     │ CI / scheduled job │
│ agent fix    │     │ human fix    │     │ review + bench     │
└──────┬───────┘     └──────┬───────┘     └─────────┬──────────┘
       │                    │                       │
       v                    v                       v
  homeboy review       homeboy test            homeboy bench
  --changed-since      --changed-since         --output results.json
       │                    │                       │
       └────────────────────┴───────────┬───────────┘
                                        v
                             structured evidence + PR comments
```

Useful commands for parallel work:

```bash
homeboy status --all
homeboy git status
homeboy review --changed-since origin/main --report pr-comment
homeboy report failure-digest --output-dir .homeboy/runs/<run-id> --results @results.json
homeboy runs list
```

The goal is not to hide complexity. It is to make every branch and agent return
evidence in the same shape.

### Benchmarks, Traces, And Runs

`homeboy bench` makes benchmarks a first-class quality gate. Extensions provide
the benchmark runner; Homeboy owns baseline storage, regression policies,
ratchets, and rig-scoped comparisons.

```bash
homeboy bench my-project --baseline
homeboy bench my-project --ratchet
homeboy bench my-project --rig app
```

`homeboy trace` captures black-box behavioral traces for declared scenarios.
Traces can compare spans against baselines, render Markdown reports, and apply a
temporary overlay patch for a run.

```bash
homeboy trace my-project checkout-flow --baseline
homeboy trace my-project checkout-flow --report markdown
homeboy trace my-project checkout-flow --overlay experiment.patch
```

`homeboy runs` inspects persisted observation runs and artifacts:

```bash
homeboy runs list
homeboy runs show <run-id>
homeboy runs artifacts <run-id>
```

### Local Dev Rigs And Stacks

`homeboy rig` manages reproducible local environments: components, background
services, symlinks, checks, git operations, builds, and patch steps. A rig can
bring up a multi-repo setup, check its health, sync stacks, and tear it down.

```bash
homeboy rig install https://github.com/your-org/rigs.git//packages/app
homeboy rig up app
homeboy rig check app
homeboy rig sync app
homeboy rig down app
```

`homeboy stack` manages combined-fixes branches built from a base branch plus
declared PRs. It can inspect, apply, rebase, sync, diff, and push the target
branch.

```bash
homeboy stack status app-combined
homeboy stack sync app-combined
homeboy stack push app-combined
```

### Release And Changelog

Homeboy's release path is driven by conventional commits:

- `fix:` commits produce patch releases.
- `feat:` commits produce minor releases.
- `BREAKING CHANGE` produces major releases.
- `docs:`, `test:`, `ci:`, and `chore:` commits do not require a release by default.

The release family keeps version targets, changelogs, tags, and release
artifacts tied to the same plan:

```bash
homeboy changes
homeboy version show
homeboy changelog show
homeboy release --dry-run
```

Release automation can be manual, scheduled, or CI-driven. Use the same action
when you want release jobs to run in GitHub Actions:

```yaml
name: Release
on:
  workflow_dispatch:

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: Extra-Chill/homeboy-action@v2
        with:
          extension: rust
          component: my-project
          commands: release
```

### Dependencies And Attention

`homeboy deps` inspects and updates component dependencies. `homeboy triage`
gathers attention reports across components, projects, fleets, rigs, or the
whole workspace. Snapshot report mode is read-only; watch mode only mutates
GitHub when `--auto-merge` is explicitly passed.

PR items include a `ci_readiness` JSON summary when GitHub exposes check rollup
data. The summary groups queued, pending, running, failed, skipped, and passed
checks by required, optional, or unknown requirement status, includes failure
URLs, reports the oldest pending check duration when timestamps are available,
and emits next steps for the merge path.

```bash
homeboy deps status my-project
homeboy deps update vendor/package my-project --to '^1.2'
homeboy status my-project
homeboy triage workspace --mine --failing-checks
```

`homeboy issues` reconciles findings against an issue tracker, and
`homeboy report` renders structured output artifacts such as review results.

### Remote And Fleet Operations

Homeboy can also model deployed work as components attached to projects,
projects attached to servers, and projects grouped into fleets.

```text
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ component   │────>│ project     │────>│ server      │
│ plugin/CLI  │     │ site/app    │     │ VPS/host    │
└─────────────┘     └──────┬──────┘     └─────────────┘
                           │
                           v
                    ┌─────────────┐
                    │ fleet       │
                    │ group       │
                    └─────────────┘
```

That model backs SSH-driven operations:

```bash
homeboy deploy my-project
homeboy ssh production
homeboy logs show production --follow
homeboy db query production "select 1"
homeboy file read production /path/to/file
homeboy file copy production:/path/to/file staging:/path/to/file
homeboy fleet exec production --check -- wp plugin list
homeboy fleet exec production --apply -- wp plugin list
```

These commands are valuable for configured projects and server fleets. They are
not required for the local/CI/agent quality loop. `fleet exec` requires
`--apply` for real SSH fan-out; `--check` keeps the operation in plan mode.

## Extensions

Extensions add platform-specific behavior while keeping the Homeboy command
model consistent. The shared extension set lives in
[Extra-Chill/homeboy-extensions](https://github.com/Extra-Chill/homeboy-extensions),
and private teams can ship their own manifests, scripts, hooks, docs, and CLI
verbs using the same contracts.

| Extension | What it provides |
|-----------|------------------|
| `rust` | Cargo integration, Rust build/test/lint/release behavior, and crates.io artifacts. |
| `wordpress` | WP-CLI integration, WordPress-aware build/test/lint/release behavior. |
| `nodejs` | Node/PM2 process management. |
| `github` | GitHub release and issue/PR integration. |
| `homebrew` | Homebrew tap publishing. |
| `swift` | Swift testing for macOS/iOS projects. |

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
homeboy extension list
```

Installed extensions can expose their own top-level verbs, such as `homeboy wp`
and `homeboy cargo`.

The important boundary is ownership: Homeboy core should stay generic, while
extension repositories own domain semantics such as Cargo flags, WP-CLI flows,
PHPUnit behavior, GitHub release steps, package managers, or framework-specific
audit rules.

## Configuration

Global config lives in `~/.config/homeboy/`. Portable component config lives in
`homeboy.json` at the repository root.

```text
~/.config/homeboy/
├── homeboy.json       # global defaults
├── components/        # registered component definitions
├── projects/          # project definitions and server bindings
├── servers/           # server connections
├── fleets/            # project groups
├── rigs/              # local dev rig specs
├── stacks/            # stack specs
├── extensions/        # installed extensions
└── keys/              # SSH keys
```

The portable repo-level `homeboy.json` is enough for local commands and CI.
Global component/project/server/fleet config is for reusable local and ops
workflows.

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

Command reference and deeper docs are checked into this repo and embedded in the
binary.

```bash
homeboy docs list
homeboy docs commands/commands-index
homeboy docs code-factory
homeboy docs schemas/component-schema
```

Start with [docs/commands/commands-index.md](docs/commands/commands-index.md)
for the command index.

## Name

Named after ["Homeboy" by SUSTO](https://www.youtube.com/watch?v=-bBvwfn2ibU), a Charleston band.

## License

MIT License — Created by [Chris Huber](https://chubes.net)
