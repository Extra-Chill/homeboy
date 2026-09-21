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
The candidate Test command is then invoked with `HOMEBOY_TEST_INVENTORY_ONLY=1`;
the selected extension writes `homeboy-test-inventory.json` with schema
`homeboy/test-inventory/v1`, stable test IDs, and optional `duration_ms` values.
Each shard receives a digest-bound `HOMEBOY_TEST_SHARD_MANIFEST`. Default and
empty `test-shards` values retain the established unsharded Test path.

Changed-scope resolution can map a tiny diff onto a huge test-file selection.
Set `max-changed-test-files` to a positive integer to define the largest
changed-scope Test selection permitted in one process. The reusable workflow
`homeboy/test-inventory/v1` contains generic test IDs, not selected file counts
or a test-to-file mapping. `max-changed-test-files` therefore remains enforced
only by Homeboy's changed-file selection contract; sharding never reinterprets
inventory entry count as files. Complete per-test `duration_ms` inventory
evidence enables a post-plan budget check against `test-timeout-seconds`; the
shipped durationless inventory contract reports budget fit as unknown. A shard
count above available inventory tests is reduced to the available count.

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

### Reusable Workflow Permissions

The reusable workflow requests `contents: write`, `pull-requests: write`, and
`issues: write`. Its caller must grant those permissions because GitHub rejects
a called workflow that asks for a scope the caller did not grant before it
schedules any jobs. The canonical minimal caller is maintained in
[`fixtures/reusable-workflow-minimal-consumer.yml`](fixtures/reusable-workflow-minimal-consumer.yml)
and is release-gated against `ci.yml`.

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

Prefer the reusable release workflow so Homeboy Action owns the whole pipeline. The caller is ~15 lines:

```yaml
name: Release
on:
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      dry-run:
        description: 'Preview the release without making changes'
        type: boolean
        default: false

concurrency:
  group: release
  cancel-in-progress: false

jobs:
  release:
    uses: Extra-Chill/homeboy-action/.github/workflows/release.yml@v2
    with:
      dry-run: ${{ inputs.dry-run || false }}
      args: '--setting database_type=mysql'   # optional, passed through
    secrets: inherit
```

The caller must grant `contents: write`, `issues: write`, and
`pull-requests: write` — GitHub fails a called workflow that requests a scope
the caller did not grant before it schedules any jobs. The canonical minimal
caller is maintained in
[`fixtures/reusable-release-minimal-consumer.yml`](fixtures/reusable-release-minimal-consumer.yml).

Available inputs: `dry-run`, `args`, `extension`, `extension-ref`, `component`,
`prepared-ref`,
`php-version`, `node-version`, `release-skip-publish`,
`release-skip-github-release`, `release-branch`, `execution-timeout-seconds`,
`publisher-known-hosts`, `dispatch-repo`, `dispatch-event`.
Workflow outputs mirror the composite action (`released`, `release-version`,
`release-tag`, `release-bump-type`, `skipped-reason`, `tooling-identity`) so a
post-release job can chain on the result. The workflow also exposes
`source-sha` and `released-source-sha` for prepared-source consumers.

Publisher SSH credentials are optional. Map an existing consumer secret to the
declared reusable-workflow secret and provide pinned host keys as a workflow
input; the private key is loaded into an agent only for the real release and is
removed before the action finishes:

```yaml
jobs:
  release:
    uses: Extra-Chill/homeboy-action/.github/workflows/release.yml@v2
    with:
      publisher-known-hosts: |
        publish.example.com ssh-ed25519 AAAA...pinned-host-key...
    secrets:
      PUBLISH_SSH_KEY: ${{ secrets.EXISTING_PUBLISHER_SSH_KEY }}
```

Do not use `ssh-keyscan` output for `publisher-known-hosts`. The dispatch
payload's `sha` and `released-source-sha` identify the source repository commit;
any publisher or mirror commit is a separate downstream identity.

#### Release credentials

Set `HOMEBOY_APP_ID` and `HOMEBOY_APP_PRIVATE_KEY` as repository secrets to
release with a GitHub App token (enables auto-issue filing and workflow
re-triggers). Both are optional: without them the workflow falls back to the
automatic `GITHUB_TOKEN`, which is sufficient for tag and GitHub Release
creation in the same repository. Pass `secrets: inherit` from the caller.

#### Failed-release retry protection

A failed release records its SHA in an Actions cache keyed by
`release-last-failed-<branch>-<source-sha>-<component>-<tooling-identity>`.
Unattended pushes of the same source SHA, component, and resolved tooling are
then skipped — without this,
every push retriggers a doomed release in a loop. Because the key includes the
resolved tooling identity, a fixed homeboy binary or extension revision
produces a fresh key and a previously-blocked SHA self-heals
(homeboy-action#257). Dispatching the caller workflow manually always
bypasses the marker: a human dispatch is an explicit "retry this now". Pushes
whose head commit is itself a `release:` commit skip the pipeline entirely.

For dependency-preparation jobs, `prepared-ref` selects the source to release.
The check job resolves it to an immutable SHA, requires it to equal the current
`release-branch` tip, and checks out that same SHA for the real release. This
validation is repeated in the real release job immediately before the action
starts, preventing a stale prepared commit from releasing over newer branch
state. Pass a branch such as `prepared-ref: main` when sequential monorepo jobs
need the newest branch tip after an earlier component release commit; the
workflow resolves that branch again and exposes `source-sha` plus
`released-source-sha` for downstream callers. Empty `component` and
`prepared-ref` preserve root-consumer behavior.

Inside a reusable workflow `uses: ./` would resolve to the caller's checkout,
so the workflow pins `Extra-Chill/homeboy-action` via an `action-ref` input
(default `v2`) resolved to one immutable revision per run — the same contract
as the reusable CI workflow.

#### Post-release dispatch

Set `dispatch-repo` to have the workflow send a `repository_dispatch` to
another repository after a real, successful release — for example a central
deploy workflow that receives every component's releases. A dry run never
dispatches. A dispatch failure fails only the `dispatch` job; the release is
already tagged and published and is not rolled back.

```yaml
jobs:
  release:
    uses: Extra-Chill/homeboy-action/.github/workflows/release.yml@v2
    with:
      dispatch-repo: my-org/my-deploy-repo
      dispatch-event: component-released   # default
    secrets: inherit
```

The receiving workflow declares `on: repository_dispatch: types: [component-released]`
and reads `github.event.client_payload`:

```json
{
  "component": "my-plugin",
  "repository": "my-org/my-plugin",
  "version": "1.2.3",
  "tag": "v1.2.3",
  "bump": "minor",
  "sha": "0123456789abcdef0123456789abcdef01234567",
  "run_url": "https://github.com/my-org/my-plugin/actions/runs/123"
}
```

`component` is the Homeboy portable id from `homeboy.json`, not the repository
name. The dispatch `sha` is the peeled commit SHA of the emitted tag, so it
identifies the actual released source even when `prepared-ref` differs from the
triggering event SHA.

`GITHUB_TOKEN` cannot dispatch to another repository. The job uses
`secrets.DISPATCH_TOKEN` when the caller provides one (a token with
`contents: write` on the target), otherwise a `HOMEBOY_APP_*` installation
token scoped to exactly `dispatch-repo` — so the Homeboy App must be installed
on the target repository. With neither, the job fails with an explicit error.
Outputs `dispatched` and `dispatch-repo` are exported for chaining.

#### Direct composite action

Components that need the release inside a larger job can still call the action directly:

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
| `ssh-key` | No | | SSH private key for `deploy`/`fleet` operations commands. Starts an agent and loads it; if empty, SSH is assumed pre-configured. |
| `ssh-known-hosts` | No | | Extra `known_hosts` entries for the servers those commands reach. |
| `ssh-require-known-hosts` | No | `false` | Require pinned `ssh-known-hosts` and strict host verification; used by the reusable publisher credential handoff. |
| `config-dir` | No | | Repo-relative Homeboy config root (`projects/`, `servers/`, `components/`, `fleets/`), exported as `HOMEBOY_CONFIG_ROOT` so operations commands can resolve checked-in targets. See [Deploy from CI](#deploy-from-ci). |
| `rig` | No | | Bench rig pair/list passed to `homeboy bench --rig` |
| `scenario` | No | | Bench scenario ID passed to `homeboy bench --scenario` |
| `runs` | No | | Bench run count passed to `homeboy bench --runs` |
| `iterations` | No | | Bench iteration count passed to `homeboy bench --iterations` |
| `regression-threshold` | No | | Bench regression threshold passed to `homeboy bench --regression-threshold` |
| `differential-gating` | No | `false` | On PRs, compare `review audit`/`review test` counts against the base SHA and fail only when the PR is worse. Opt-in; `review lint` still gates on exit code. |
| `baseline-commands` | No | `auto` | Commands to rerun at the PR base when `differential-gating` is true. `auto` reruns requested `review audit`/`review lint`/`review test` commands; use a comma-separated subset such as `review audit` or `none` to skip baseline reruns. |

Test reconciliation consumes Homeboy's `homeboy/test-outcomes/v1` and `homeboy/test-inventory/v1` sidecars named `<command-stem>.test-outcomes.json` and `<command-stem>.test-inventory.json`. The inventory carries the complete nonempty selected test-ID set; outcomes carry only the duplicate-free `failed_test_ids` observed by the adapter. Both sidecars bind the canonical Test command identity, runner, runner/workspace/execution fingerprints, and a canonical SHA-256 inventory fingerprint; configured suffixes remain part of the output stem and actual invocation. Shard aggregation validates every pair against its immutable manifest, unions exact inventory and observed failure identities, and emits a deterministic aggregate pair. Failed candidate, baseline, and shard Test phases require complete, provenance-matched evidence; unavailable evidence fails closed. This consumer contract is paired with Extra-Chill/homeboy#13371 and a homeboy-extensions adapter follow-up.
| `observation-window` | No | `24h` | Duration window passed to best-effort `homeboy runs export --since` for the separate matrix-safe observations artifact. |
| `execution-timeout-seconds` | No | `1800` | Per-command wall-clock budget. The action writes liveness notices and returns timeout evidence when the budget expires. |
| `test-timeout-seconds` | No | inherited / `1500` | Homeboy test-child budget. Direct action calls preserve `HOMEBOY_TEST_TIMEOUT_SECONDS` when omitted; the reusable workflow defaults to `1500`. Must leave cleanup margin below `execution-timeout-seconds`. |
| `max-changed-test-files` | No | *(unset)* | Existing changed-scope file-selection cap, enforced by Homeboy. Generic Test inventory does not expose the file mapping needed to apply it per shard. |
| `test-shards` | No | `1` | Number of deterministic Test shards. Empty and `1` preserve the unsharded path; integers above `1` activate inventory planning and cap preflight. |
| `allow-oversized-test-scope` | No | `false` | Explicitly allow an opted-in sharded plan with complete duration evidence whose estimated process duration exceeds `test-timeout-seconds`. |
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

### Using the installed toolchain in your own steps

The action installs the Homeboy CLI and its extensions, and phpunit, phpcs,
wpcs, phpstan and the WordPress stubs all live inside an extension rather than
in your repository. After the action runs, it publishes where they are:

| Variable | Meaning |
|----------|---------|
| `HOMEBOY_EXTENSIONS_ROOT` | Directory containing every installed extension |
| `HOMEBOY_EXTENSION_PATH` | The extension this invocation resolved (singular; `homeboy.json` may declare several) |
| `PATH` | Extended to include the resolved `homeboy` binary |

So a custom step can use the same tools CI uses, instead of your repository
declaring its own copies with pins free to drift:

```yaml
- uses: Extra-Chill/homeboy-action@v2
  with:
    commands: review test

- name: Run a bespoke suite with the shared phpunit
  run: |
    "$HOMEBOY_EXTENSION_PATH/vendor/bin/phpunit" \
      --configuration phpunit.xml.dist tests/MyCoordinatedTest.php
```

These are set by the action, so a step that needs them must run **after** it.
A step that must run earlier still needs its own tooling.

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

### Deploy from CI

Operations commands (`deploy`, `fleet`) need a Homeboy **project** (server id,
base path, component attachments) and a **server** (host, user, port), which a
fresh runner does not have. Check a Homeboy config root into the repository
and point `config-dir` at it; the action exports `HOMEBOY_CONFIG_ROOT`
(homeboy#14783) so every homeboy invocation in the run resolves from it. Nothing
is copied and the runner user's real config is never read.

```
deploy/homeboy/
├── projects/
│   └── my-site/
│       └── my-site.json      # domain, server_id, base_path, path_roots, components[]
├── servers/
│   └── prod.json             # id, host, user, port, identity_file: null
└── components/
    └── my-plugin.json        # id, remote_url (GitHub), remote_path
```

Host, user, and port are not secrets. Leave `identity_file` as `null` and
supply the key through `ssh-key`, which loads it into an agent for the run.

Attachments in `components[]` need only `id` and `remote_path`; leave
`local_path` empty. With a GitHub `remote_url` in the standalone registry
entry, Homeboy resolves the component's `homeboy.json` and release asset from
the repository at the tag — no source checkout on the runner
(homeboy#14782). Homeboy needs a GitHub token for that; the runner's
`GITHUB_TOKEN` covers public and org repositories.

```yaml
name: Deploy
on:
  workflow_dispatch:
    inputs:
      component: { type: string, required: true }
      version:   { type: string, required: true }

concurrency:
  group: deploy-my-site
  cancel-in-progress: false

permissions:
  contents: read

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: Extra-Chill/homeboy-action@v2
        with:
          config-dir: deploy/homeboy
          ssh-key: ${{ secrets.DEPLOY_SSH_KEY }}
          ssh-known-hosts: ${{ secrets.DEPLOY_KNOWN_HOSTS }}
          commands: deploy my-site ${{ inputs.component }} --version ${{ inputs.version }}
```

`config-dir` fails the run before any command if `<config-dir>/projects` is
missing, and prints the project and server ids it found. The reusable
`ci.yml` and `release.yml` workflows forward the same input.

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

## Process Containment

Every command runs under a supervisor that bounds it with `execution-timeout-seconds`
and refuses to report a pass for a command whose descendants outlived it. The
kernel primitives that make containment provable are not portable, so the
supervisor selects a backend from the runner platform and names it in the job log.

**Linux runners** get `subreaper-reparented pidfd containment`. `PR_SET_CHILD_SUBREAPER`
reparents every orphaned descendant onto the supervisor, so the process tree
always leads back to it, and pidfds signal that tree with no PID-reuse window.
This is the strongest guarantee available and remains the default everywhere it
can be established.

**macOS and other POSIX runners** get `session-scoped termination with
inherited-descriptor containment proof`. Darwin has neither a subreaper nor
pidfds, so termination and proof are separated:

- **Termination** works through the command's own session and process group, plus
  its parent chain. That covers every descendant which did not deliberately
  detach — including a double-fork reparented to `launchd`, because it keeps the
  inherited process group.
- **Proof** works through an inherited pipe. The command is spawned holding the
  write end of a pipe the supervisor keeps the read end of, and every descendant
  inherits that descriptor across `fork` and `exec`. EOF is positive evidence that
  nothing is left holding it, including a descendant that detached far enough that
  no scan of the process table could name it.

A command whose descendant escapes the session and keeps the descriptor open past
`cleanup-timeout-seconds` **fails with exit 125** and a `could not prove descendant
containment` error. That is deliberate: the supervisor cannot signal a descendant
it cannot name, so it refuses to certify the command rather than laundering the
escape into a pass. Ordinary builds — including Xcode builds driven through
`HOMEBOY_NODE_BUILD_COMMAND` — release the descriptor when they exit and are
unaffected.

macOS-native gates therefore work the same way as Linux ones:

```yaml
jobs:
  build:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v6
      - uses: Extra-Chill/homeboy-action@v2
        with:
          commands: review build
          component: my-app
        env:
          HOMEBOY_NODE_BUILD_COMMAND: xcodebuild -scheme MyApp build
```

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
