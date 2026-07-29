<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy review` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/review.md](../../../commands/review.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy review`

```sh
homeboy review [OPTIONS] [COMPONENT] [COMMAND]
```

Run scoped audit + lint + test umbrella against PR-style changes

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--run-id` | `<RUN_ID>` | Attach to an already-persisted review run instead of starting another audit/lint/test execution |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--changed-since` | `<REF>` | Run audit + lint + test only against files changed since this git ref (branch, tag, or SHA). CI-friendly — mirrors the per-stage flag |
| `--changed-only` | flag | Run only against files modified in the working tree (staged, unstaged, untracked). Only the lint stage scopes natively; audit and test run on the full component with a hint noting the limitation. Use `--changed-since` for full umbrella scoping |
| `--summary` | flag | Show compact summary instead of full per-stage output |
| `--ci-profile` | `<ID>` | Run an extension-declared CI profile as an additional review gate |
| `--audit-profile` | `<PROFILE>` | Audit detector profile for the audit stage. Defaults to `pr` for changed-file review and `full` for full review Values: `full`, `pr`, `architecture`. |
| `--report` | `<FORMAT>` | Output format. Default JSON envelope; `--report=pr-comment` emits a markdown PR-comment section instead, suitable for piping to `homeboy git pr comment --body-file` Values: `pr-comment`. |
| `--banner` | `<KEY=VALUE>` | Action-level banner rendered above the PR-comment scope line. Repeatable as `--banner key=value` |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |

| Subcommand | Summary |
| --- | --- |
| `homeboy review audit` | Audit code conventions and detect architectural drift |
| `homeboy review lint` | Lint a component |
| `homeboy review test` | Run tests for a component |
| `homeboy review build` | Run a local build quality gate for a component |
| `homeboy review ci` | Inspect CI reproduction profiles and discovered CI surfaces |

## `homeboy review audit`

```sh
homeboy review audit [OPTIONS] [COMPONENT]
```

Audit code conventions and detect architectural drift

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--conventions` | flag | Only show discovered conventions (skip findings) |
| `--only` | `<kind>` | Restrict findings to these kinds (repeatable) |
| `--exclude` | `<kind>` | Exclude findings of these kinds (repeatable) |
| `--profile` | `<PROFILE>` | Detector profile to run. `full` preserves the default full audit; `pr` runs cheap root-level blockers for changed-file review Values: `full`, `pr`, `architecture`. |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--changed-since` | `<CHANGED_SINCE>` | Only audit files changed since a git ref (branch, tag, or SHA) |
| `--json-summary` | flag | Include compact machine-readable summary for CI wrappers. Also accepts `--summary` |
| `--fixability` | flag | Include automated-fixability metadata. This can be expensive because it runs the refactor planner after audit completes |

## `homeboy review lint`

```sh
homeboy review lint [OPTIONS] [COMPONENT]
```

Lint a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--summary` | flag | Show compact summary instead of full output |
| `--file` | `<FILE>` | Lint only a single file (path relative to component root) |
| `--glob` | `<GLOB>` | Lint only files matching a repo-relative glob pattern |
| `--changed-only` | flag | Lint modified files in the working tree (file-scoped, not hunk-scoped) |
| `--changed-since` | `<CHANGED_SINCE>` | Lint only files changed since a git ref (branch, tag, or SHA) — CI-friendly |
| `--ci-job` | `<ID>` | Run using env from a single extension-declared CI lint job |
| `--errors-only` | flag | Show only errors, suppress warnings |
| `--sniffs` | `<SNIFFS>` | Only check specific sniffs (comma-separated codes) |
| `--exclude-sniffs` | `<EXCLUDE_SNIFFS>` | Exclude sniffs from checking (comma-separated codes) |
| `--category` | `<CATEGORY>` | Filter by category: security, i18n, yoda, whitespace |
| `--fix` | flag | Apply auto-fixable lint findings in place using the lint fixer pipeline |
| `--force` | flag | Allow --fix to edit the current dirty working tree for unbounded runs |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--json-summary` | flag | Print compact machine-readable summary (for CI wrappers) |

## `homeboy review test`

```sh
homeboy review test [OPTIONS] [COMPONENT] [ARGS]...
```

Run tests for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |
| `[ARGS]...` | no | Additional arguments to pass to the test runner (must follow --) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--skip-lint` | flag | Skip linting before running tests |
| `--coverage` | flag | Collect code coverage when the selected extension supports it |
| `--coverage-min` | `<PERCENT>` | Minimum coverage percentage — fail if below this threshold (implies --coverage) |
| `--baseline` | flag | Persist the current run as the new baseline |
| `--ignore-baseline` | flag | Skip baseline comparison for this run |
| `--ratchet` | flag | Auto-update the baseline when the current run improves on it |
| `--analyze` | flag | Analyze test failures — cluster by root cause and suggest fixes |
| `--drift` | flag | Detect test drift — cross-reference production changes with test files |
| `--write` | flag | Write fixes to disk for workflows that support it |
| `--since` | `<REF>` | Git ref to compare against for drift detection (tag, commit, branch) |
| `--changed-since` | `<REF>` | Limit test execution to files changed since this git ref (PR impact scope) |
| `--ci-job` | `<ID>` | Run using env and passthrough args from a single extension-declared CI test job |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--json-summary` | flag | Print compact machine-readable summary (for CI wrappers). Also accepts `--summary` |

## `homeboy review build`

```sh
homeboy review build [OPTIONS] [TARGET_ID] [COMPONENT_IDS]...
```

Run a local build quality gate for a component

| Argument | Required | Description |
| --- | --- | --- |
| `[TARGET_ID]` | no | Target ID: component ID or project ID (when using --all) |
| `[COMPONENT_IDS]...` | no | Additional component IDs (enables project/component order detection) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]} |
| `--all` | flag | Build all components in the project |
| `--project` | `<ID>` | Scope: project id |
| `--fleet` | `<ID>` | Scope: fleet id |
| `--component` | `<ID>` | Scope: registered component id |
| `--rig` | `<ID>` | Scope: local rig id |
| `--path` | `<PATH>` | Scope: checkout path, bypassing the registry |
| `--workspace` | flag | Scope: every configured workspace repo |
| `--changed-since` | `<CHANGED_SINCE>` | Ask the build provider to resolve the build scope from files changed since this git ref |

## `homeboy review ci`

```sh
homeboy review ci <COMMAND>
```

Inspect CI reproduction profiles and discovered CI surfaces

| Subcommand | Summary |
| --- | --- |
| `homeboy review ci list` | List declared CI profiles and shallow discovered CI surfaces |
| `homeboy review ci plan` | Resolve a CI command request into a structured execution plan |
| `homeboy review ci run` | Run an extension-declared CI job or profile locally |
| `homeboy review ci autofix` | Run the end-to-end CI autofix transaction (branch prep, drift-only filtering, push-target resolution, commit, and push) |
| `homeboy review ci scope` | Resolve a CI event context into the Homeboy scope (changed vs full) and the per-command `--changed-since` flags |
| `homeboy review ci differential-gate` | Classify differential CI results without blaming a PR for a red baseline |
| `homeboy review ci triage` | Summarize failed GitHub Actions runs for a pull request without dumping raw logs |

## `homeboy review ci list`

```sh
homeboy review ci list [OPTIONS] [COMPONENT]
```

List declared CI profiles and shallow discovered CI surfaces

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |

## `homeboy review ci plan`

```sh
homeboy review ci plan [OPTIONS]
```

Resolve a CI command request into a structured execution plan.

This is the core-owned orchestration the action calls instead of inferring commands, splitting quality vs operations, enforcing canonical order, and deriving output filenames in shell. It is pure and emits JSON; it does not execute anything.

| Option | Value | Description |
| --- | --- | --- |
| `--commands` | `<COMMANDS>` | Raw, comma-separated command request (e.g. `audit,lint,test` or `refactor --from all`). When empty, commands are inferred from `--context` |
| `--context` | `<CONTEXT>` | Event context driving inference: `pr`, `push`, `cron`, or `manual`. Unknown values fall back to `manual`. Defaults to `manual` |

## `homeboy review ci run`

```sh
homeboy review ci run [OPTIONS] [COMPONENT]
```

Run an extension-declared CI job or profile locally

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--job` | `<JOB>` | Run a single extension-declared CI job |
| `--profile` | `<PROFILE>` | Run all jobs in an extension-declared CI profile |

## `homeboy review ci autofix`

```sh
homeboy review ci autofix [OPTIONS] [COMPONENT]
```

Run the end-to-end CI autofix transaction (branch prep, drift-only filtering, push-target resolution, commit, and push).

This is the core-owned transaction the action calls instead of re-implementing branch/commit/push orchestration in shell. It assumes the working tree already contains the autofix changes to commit.

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--extension` | `<ID>` | One-shot extension override for the current invocation |
| `--target-repo` | `<TARGET_REPO>` | Target repository to push to (`owner/repo`). Defaults to `origin` |
| `--origin-repo` | `<ORIGIN_REPO>` | Repository backing the current `origin` remote (`owner/repo`) |
| `--target-branch` | `<TARGET_BRANCH>` | Branch to push to (PR head branch or autofix branch) |
| `--token` | `<TOKEN>` | GitHub App / access token for the push (enables workflow re-runs and cross-repo pushes). Falls back to the `APP_TOKEN` env var |
| `--git-identity` | `<GIT_IDENTITY>` | Git identity to commit as. Defaults to the CI bot identity |
| `--message` | `<MESSAGE>` | Commit message for authored (non-drift) fixes. Defaults to a generic autofix subject |
| `--dry-run` | flag | Classify and resolve the push target without committing or pushing |

## `homeboy review ci scope`

```sh
homeboy review ci scope [OPTIONS]
```

Resolve a CI event context into the Homeboy scope (changed vs full) and the per-command `--changed-since` flags.

This is the core-owned translation the action calls instead of deriving event-context → scope in shell (`scripts/scope/*.sh`). With `--github-actions` the context is read from the standard GitHub Actions environment; explicit flags override individual fields.

| Option | Value | Description |
| --- | --- | --- |
| `--github-actions` | flag | Read the event context from the GitHub Actions environment (`GITHUB_EVENT_NAME`, `BASE_SHA`, `PR_HEAD_REPO`, `GITHUB_REPOSITORY`) |
| `--event-name` | `<EVENT_NAME>` | Override the event name (e.g. `pull_request`, `push`, `schedule`) |
| `--base-sha` | `<BASE_SHA>` | Override the PR base SHA used for changed-file diffs |
| `--head-repo` | `<HEAD_REPO>` | Override the PR head repository (`owner/repo`) for fork detection |
| `--base-repo` | `<BASE_REPO>` | Override the base repository (`owner/repo`) |
| `--repo-path` | `<REPO_PATH>` | Checkout to resolve the merge base against (deepens shallow clones). When omitted, the supplied base ref is trusted verbatim |
| `--for` | `<FOR_COMMANDS>` | Emit `--changed-since` flags for this command in addition to the resolved scope. May be repeated |

## `homeboy review ci differential-gate`

```sh
homeboy review ci differential-gate [OPTIONS]
```

Classify differential CI results without blaming a PR for a red baseline

| Option | Value | Description |
| --- | --- | --- |
| `--baseline-command` | `<BASELINE_COMMAND>` | Exact command run against the baseline checkout |
| `--baseline-exit-code` | `<BASELINE_EXIT_CODE>` | Exit code from the baseline command |
| `--baseline-evidence` | `<BASELINE_EVIDENCE>` | Evidence for baseline failures, such as log excerpts or artifact refs |
| `--head-command` | `<HEAD_COMMAND>` | Exact command run against the candidate/PR-head checkout |
| `--head-exit-code` | `<HEAD_EXIT_CODE>` | Exit code from the candidate/PR-head command |
| `--head-evidence` | `<HEAD_EVIDENCE>` | Evidence for candidate failures, such as log excerpts or artifact refs |

## `homeboy review ci triage`

```sh
homeboy review ci triage [OPTIONS] <REFERENCE>
```

Summarize failed GitHub Actions runs for a pull request without dumping raw logs

| Argument | Required | Description |
| --- | --- | --- |
| `<REFERENCE>` | yes | Pull request number, owner/repo#number, or GitHub PR URL |

| Option | Value | Description |
| --- | --- | --- |
| `--repo` | `<REPO>` | GitHub repository in owner/repo form. Required when `reference` is only a number |
| `--max-runs` | `<MAX_RUNS>` | Maximum failed workflow runs to inspect. Use 0 for the default |
| `--max-snippets-per-job` | `<MAX_SNIPPETS_PER_JOB>` | Maximum relevant log snippet lines to retain per failed job. Use 0 for the default |
| `--context-lines` | `<CONTEXT_LINES>` | Context lines around relevant log matches. Use 0 for the default |

