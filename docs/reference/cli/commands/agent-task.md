<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy agent-task` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/agent-task.md](../../../commands/agent-task.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy agent-task`

```sh
homeboy agent-task <COMMAND>
```

Run generic agent task plans

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task doctor` | Diagnose provider and runtime readiness on a runner, and optionally repair it |
| `homeboy agent-task cook` | Submit an agent task, run its gates, and open a pull request |
| `homeboy agent-task cook-continue` | Continue a detached Cook from its durable Cook ID or provider attempt ID. The persisted recipe supplies the original prompt, transport, gates, worktree, and disclosure policy |
| `homeboy agent-task loop` | Operate durable defined multi-agent loops: define, inspect, resume, and stop |
| `homeboy agent-task run-plan` | Run an `AgentTaskPlan` through extension-declared executor providers |
| `homeboy agent-task run` | Execute a previously submitted durable run |
| `homeboy agent-task run-next` | Claim and execute the oldest queued durable run |
| `homeboy agent-task submit` | Persist an agent-task plan and return a durable run id without executing it |
| `homeboy agent-task status` | Read durable run status |
| `homeboy agent-task list` | List durable runs, newest first |
| `homeboy agent-task active` | List queued and running durable runs, newest first |
| `homeboy agent-task reconcile` | Preview or apply reconciliation for one durable run |
| `homeboy agent-task reconcile-records` | Reconcile stored durable run records against authoritative provider state |
| `homeboy agent-task latest` | Show the latest durable run |
| `homeboy agent-task logs` | Read the canonical durable event stream for a run |
| `homeboy agent-task artifacts` | List artifacts and evidence refs recorded for a completed run |
| `homeboy agent-task retained-artifacts` | Discover or attach selected outputs retained in a terminal Lab Cook workspace |
| `homeboy agent-task evidence` | Retrieve selected durable evidence recorded for a run |
| `homeboy agent-task diagnose` | Compute a root cause, causal chain, and next actions for a failed run |
| `homeboy agent-task runtime-recover` | Recover a missing or corrupted immutable controller runtime pin |
| `homeboy agent-task runtime-validate` | Validate controller runtime eligibility without executing provider work |
| `homeboy agent-task replay-provider-boundary` | Hydrate the latest raw executor input and print provider-boundary fields without relaunching a provider |
| `homeboy agent-task cancel` | Mark a queued or stale-running durable run as cancelled |
| `homeboy agent-task resume` | Resume a queued or stale-running durable run |
| `homeboy agent-task retry` | Submit a fresh durable run from an existing run's plan |
| `homeboy agent-task fanout` | Cook, submit, and inspect batches of independent tasks |
| `homeboy agent-task review` | Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints |
| `homeboy agent-task promote` | Promote a completed generic patch artifact into a managed worktree |
| `homeboy agent-task adopt` | Adopt an immutable commit candidate through a tracked cook's normal gates and finalization |
| `homeboy agent-task finalize-pr` | Finalize a green run, or recover publication from a durable Cook record |
| `homeboy agent-task record-replacement-gate-proof` | Attach authorized candidate-bound replacement gate proof after an infrastructure gate failure |
| `homeboy agent-task accept` | Record an independent, durable acceptance verdict for a candidate |
| `homeboy agent-task gate-feedback` | Convert deterministic gate results into a cook retry or stop decision |
| `homeboy agent-task providers` | List extension-declared executor providers and optional secret/backend readiness |
| `homeboy agent-task prompts` | Manage markdown prompts in Homeboy-owned storage |
| `homeboy agent-task contract` | Export Homeboy's machine-readable agent-task core contract metadata |
| `homeboy agent-task compile-loop` | Compile a declarative loop definition into an agent-task plan without submitting or running it |
| `homeboy agent-task auth` | Configure and inspect provider authentication secrets |
| `homeboy agent-task controller` | Create, inspect, and resume durable multi-agent loop controller state |

## `homeboy agent-task doctor`

```sh
homeboy agent-task doctor [OPTIONS]
```

Diagnose provider and runtime readiness on a runner, and optionally repair it

| Option | Value | Description |
| --- | --- | --- |
| `--runner` | `<RUNNER>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--path` | `<PATH>` | _no help text_ |
| `--extension` | `<EXTENSION>` | _no help text_ |
| `--require-tool` | `<TOOL>` | _no help text_ |
| `--secret-env` | `<ENV>` | _no help text_ |
| `--repair` | flag | _no help text_ |

## `homeboy agent-task cook`

```sh
homeboy agent-task cook [OPTIONS]
```

Submit an agent task, run its gates, and open a pull request.

Provide the work with one `--prompt` and optional `--goal` framing, point `--to-worktree` at the existing worktree to edit (that checkout is authoritative — the agent's changes, the `--verify` gates, and the PR all operate on it), and give one or more `--verify` commands that must pass in that worktree before promotion. Cook then commits, runs the deterministic gates, and finalizes a `--base`-targeted PR (use `--no-finalize` to stop before opening the PR). Repeatable `--verify` gates all run; the run retries up to `--max-attempts` times. Use `agent-task fanout cook-batch` for independent task waves.

WAIT POLICY: Cook always persists a durable run id before materialization, so a returned command is not by itself proof of a completed cook.

By default Cook observes until the lifecycle is terminal and returns the terminal Cook report.

`--detach-after-handoff` returns once the run is durably accepted. Its result describes a submission, not an outcome. It is honored on every placement: with `--placement local` the Cook is re-executed in its own session, so it survives a client that is interrupted or times out.

Do not infer the wait policy from client interactivity. An orchestration client that needs the detached contract should pass `--detach-after-handoff` rather than rely on the default, and read the terminal outcome from `agent-task status <run-id>` in either case.

| Option | Value | Description |
| --- | --- | --- |
| `--prompt` | `<PROMPT>` | Inline prompt, `@<path>` to read a file, `-` to read stdin, or `@prompt:<id>` for a stored prompt |
| `--cwd` | `<PATH>` | Existing local repo checkout or worktree path to cook in |
| `--workspace` | `<ID_OR_PATH>` | Homeboy workspace ID or existing local workspace path to cook in |
| `--repo` | `<REPO>` | Repo/component slug for metadata and task grouping, e.g. sample-plugin |
| `--task-url` | `<URL>` | Issue, PR, or tracker URL the task is cooking |
| `--backend` | `<BACKEND>` | Executor backend to request. Defaults to the configured coding backend |
| `--selector` | `<PROVIDER_ID>` | Optional provider id when more than one provider exists for the backend |
| `--model` | `<MODEL>` | Optional model override passed through to the provider |
| `--secret-env` | `<ENV>` | Secret environment variable name to hydrate for the provider. Repeatable |
| `--concurrency` | `<N>` | Maximum number of task cells to run at once |
| `--run-id` | `<ID>` | Optional durable run id. Generated when omitted |
| `--provider-config` | `<JSON>` | Provider config JSON object, @file, or - for stdin. Merged with workspace metadata |
| `--client-context` | `<JSON>` | Opaque client context JSON object, @file, or - for stdin |
| `--max-provider-executions` | `<N>` | Maximum total provider executions per task, including same-provider retries and provider rotations. For Cook, this must be at least --max-attempts; use --max-same-provider-retries for gate and review-form remediation. `--attempts 1` runs exactly once. When omitted, defaults to the total attempts the configured provider rotation needs, or 1 when no rotation is configured |
| `--max-same-provider-retries` | `<N>` | Same-provider retries allowed after the first provider execution. Cook needs one for each possible gate or required review-form remediation; provider rotations cannot replace those retries. Defaults to 0; a configured provider rotation never funds these |
| `--max-provider-rotations` | `<N>` | Cross-provider rotations allowed after the first provider execution. Rotations are distinct from same-provider Cook remediation and do not satisfy its required review-form retry budget. When omitted, defaults to the number of entries in the configured provider rotation, or 0 when no rotation is configured |
| `--queue-only` | flag | Persist the run for a daemon/runner but do not execute immediately |
| `--timeout-ms` | `<MS>` | Provider wall-clock timeout in milliseconds. Defaults to Homeboy's provider timeout |
| `--deny-command` | `<PATTERN>` | Command pattern the provider agent must not run. Repeatable, and additive to the host-level `agent_task.command_policy` config |
| `--allow-command` | `<PATTERN>` | Command pattern the provider agent may run. Supplying any `--allow-command` switches the policy to allow-list mode: every command that does not match one of these patterns is refused |
| `--command-policy-reason` | `<TEXT>` | Why the command policy exists, returned verbatim to the agent with every refusal. Telling the agent what to do instead (e.g. "this host routes builds to CI; make your edits and push") converts a refused command into correct behaviour rather than a wasted budget |
| `--candidate-completion` | `<POLICY>` | Completion rule for isolated candidates: wait for all results (default) or promote the first successful candidate |
| `--goal` | `<TEXT>` | One-line statement of what a successful cook must achieve. Recorded as framing metadata for the provider task and used for review. Without --prompt, it supplies the one provider task |
| `--to-worktree` | `<HANDLE>` | Workspace handle the cook edits, verifies, and finalizes into. The handle is `<repo>@<branch-slug>`, where the slug replaces every character of --head outside [A-Za-z0-9_-] with `-`, so branch `fix/1234-x` is handle `repo@fix-1234-x`. Existing destinations are reused. Creating a missing one is not a built-in capability: it requires an enabled worktree provider with a `commands.ensure` argv template, and without one you must create the destination first with `homeboy worktree create`. When omitted, --repo plus --task-url derives an issue-owned destination through that same configured provider |
| `--provider-command` | `<COMMAND>` | Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`. |
| `--provider-argv` | `<ARG>` | Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`. |
| `--verify` | `<COMMAND>` | Deterministic verification command that must pass before the cook promotes its work (e.g. `--verify "cargo fmt --check"`). Required unless `--private-verify` is given — a cook that cannot verify its work cannot promote it. Runs in the destination worktree. Repeat to require multiple gates; every one must pass. Its output is included in the review evidence |
| `--private-verify` | `<COMMAND>` | Like `--verify`, but the command's output is treated as private: only a pass/fail summary is revealed by default (see `--private-gate-reveal`). Satisfies the same mandatory-gate requirement as `--verify`. Use for gates whose logs may contain secrets. Repeatable |
| `--private-gate-reveal` | `<POLICY>` | How much of a `--private-verify` gate's output to reveal: `summary-only` (default) shows just pass/fail; other policies expose more detail Values: `full-evidence`, `summary-only`, `redacted`, `no-detail`. |
| `--gate-execution-policy` | `<POLICY>` | Gate scheduling policy: `ordered-fail-fast` (default) skips downstream gates after the first failure; `continue-all` runs every declared gate Values: `ordered-fail-fast`, `continue-all`. |
| `--gate-timeout-seconds` | `<SECONDS>` | Wall-clock timeout, in seconds, for each verification gate command (default 1800 = 30 min). A gate exceeding this fails |
| `--gate-heartbeat-interval-seconds` | `<SECONDS>` | How often, in seconds, to emit a heartbeat while a gate runs so long gates are not mistaken for a stalled cook (default 5) |
| `--gate-no-progress-timeout-seconds` | `<SECONDS>` | Maximum time, in seconds, a gate may run without a structured `HOMEBOY_PROGRESS` marker (default 300 = 5 min) |
| `--rerun-completed-gates` | flag | Re-run gates that already recorded a passing result on a previous attempt instead of reusing the recorded pass. Off by default |
| `--accept-inherited-failures` | flag | Finalize only when an inherited required-gate failure was reproduced on the immutable baseline. The gate remains reported as baseline-red |
| `--gate-environment-mode` | `<MODE>` | Environment for gate commands: `inherit` (default) extends the current environment; `replace` starts from an empty environment plus `--gate-env` Values: `inherit`, `replace`. |
| `--gate-env` | `<NAME=VALUE>` | Extra environment variable for gate commands, as `NAME=VALUE`. Repeatable |
| `--gate-env-from` | `<NAME=SOURCE[/PATH]>` | Preserve a required toolchain setting from the host as `NAME=SOURCE` or `NAME=SOURCE/relative/path`. The mapping is retained in gate evidence |
| `--gate-toolchain` | `<COMMAND>` | Required executable to initialize before provider execution. Its probe is `COMMAND --version` in the final isolated gate environment. Repeatable |
| `--gate-package-artifact` | `<JSON>` | Caller-declared package resource readiness as a JSON object. The object defines its environment mapping, required paths or digests, and opaque remediation metadata. Repeat for multiple resources |
| `--gate-extension-input` | `<JSON>` | Explicit extension input as a JSON object with `id` and absolute `source`. Only selected inputs are copied into isolated HOME |
| `--isolate-gate-home` | `<ISOLATE_GATE_HOME>` | Run gates with an isolated `$HOME` so gate side effects do not touch the operator's home directory (default true) Values: `true`, `false`. |
| `--isolate-gate-xdg` | `<ISOLATE_GATE_XDG>` | Run gates with isolated XDG base directories so gate side effects do not touch the operator's config/cache/data dirs (default true) Values: `true`, `false`. |
| `--max-attempts` | `<N>` | Maximum Cook attempts before giving up. Each attempt re-runs the agent and gates; a later attempt can recover from a transient failure. Set --max-provider-executions to at least this value and --max-same-provider-retries to at least one less so gate and required review-form remediation remain possible (default 3) |
| `--no-finalize` | flag | Stop after the work is verified but before opening the pull request, leaving the committed change on the worktree branch for manual review or a later `agent-task review`/finalize |
| `--draft-pr` | flag | Complete normal verified finalization but create a draft pull request. Existing pull requests retain their current draft or ready state |
| `--full` | flag | Return the complete cook report, including nested promotion and gate evidence |
| `--no-progress` | flag | Suppress intermediate Cook progress lines after the durable run identity. The final result still contains status and evidence commands for orchestration |
| `--base` | `<BRANCH>` | Base branch the finalized pull request targets and the branch changes are diffed against (default `main`) |
| `--head` | `<BRANCH>` | Head branch to push and open the PR from. Defaults to the branch the destination worktree is already on |
| `--title` | `<TEXT>` | Title for the finalized pull request. Defaults to a title derived from the goal / commit |
| `--commit-message` | `<TEXT>` | Commit message for the cook's committed change. Defaults to a message derived from the goal |
| `--protected-branch` | `<BRANCH>` | Branch names the cook refuses to push to or target directly, as a safety guard. Repeatable; defaults to the standard protected set |
| `--ai-tool` | `<TEXT>` | AI tool disclosure recorded in the PR's assistance attribution (default `AI-assisted`) |
| `--ai-used-for` | `<TEXT>` | Legacy AI-usage disclosure. The reviewer-facing "Used for" text is now authored by the agent's `review_form.used_for` (a self-reflective process description) and validated by the cook loop's review-form gate; this flag no longer feeds the PR body. Retained only for recipe back-compatibility and defaults empty (no canned platitude) |
| `--require-acceptance` | flag | Require a separate durable acceptance verdict before PR finalization |
| `--acceptance-authority` | `<ACCEPTANCE_AUTHORITY>` | Authority allowed to issue the acceptance verdict |
| `--acceptance-policy` | `<ACCEPTANCE_POLICY>` | Policy the acceptance authority applies |

## `homeboy agent-task cook-continue`

```sh
homeboy agent-task cook-continue [OPTIONS] <COOK_OR_ATTEMPT_ID>
```

Continue a detached Cook from its durable Cook ID or provider attempt ID. The persisted recipe supplies the original prompt, transport, gates, worktree, and disclosure policy

| Argument | Required | Description |
| --- | --- | --- |
| `<COOK_OR_ATTEMPT_ID>` | yes | Durable Cook ID or one of its provider attempt IDs |

| Option | Value | Description |
| --- | --- | --- |
| `--preflight` | flag | Validate continuation admission without dispatching a provider or mutating lifecycle state |
| `--full` | flag | Include the complete Cook report rather than the compact lifecycle view |

## `homeboy agent-task loop`

```sh
homeboy agent-task loop <COMMAND>
```

Operate durable defined multi-agent loops: define, inspect, resume, and stop.

A loop is not a one-shot PR cook. It persists controller state, tracks whether it is on or off, counts revolutions, and records its continuation policy. Use `agent-task cook` for single-PR work.

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task loop define` | Define or update a durable loop from a spec |
| `homeboy agent-task loop status` | Read durable loop state: on/off, revolutions taken, and continuation policy |
| `homeboy agent-task loop resume` | Resume a stopped or exhausted durable loop, optionally raising its revolution limit |
| `homeboy agent-task loop stop` | Stop a durable loop and record the handoff |

## `homeboy agent-task loop define`

```sh
homeboy agent-task loop define [OPTIONS] <SPEC>
```

Define or update a durable loop from a spec.

`--on`/`--off` set whether the loop runs; `--revolution-limit` bounds how many revolutions it may take before it stops on its own.

| Argument | Required | Description |
| --- | --- | --- |
| `<SPEC>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--on` | flag | _no help text_ |
| `--off` | flag | _no help text_ |
| `--revolution-limit` | `<N>` | _no help text_ |
| `--resume` | flag | _no help text_ |
| `--dispatch-backend` | `<BACKEND>` | _no help text_ |
| `--dispatch-selector` | `<PROVIDER_ID>` | _no help text_ |
| `--dispatch-model` | `<MODEL>` | _no help text_ |
| `--dispatch-provider-config` | `<JSON>` | _no help text_ |

## `homeboy agent-task loop status`

```sh
homeboy agent-task loop status <LOOP_ID>
```

Read durable loop state: on/off, revolutions taken, and continuation policy

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | _no help text_ |

## `homeboy agent-task loop resume`

```sh
homeboy agent-task loop resume [OPTIONS] <LOOP_ID>
```

Resume a stopped or exhausted durable loop, optionally raising its revolution limit

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--revolution-limit` | `<N>` | _no help text_ |
| `--dispatch-backend` | `<BACKEND>` | _no help text_ |
| `--dispatch-selector` | `<PROVIDER_ID>` | _no help text_ |
| `--dispatch-model` | `<MODEL>` | _no help text_ |
| `--dispatch-provider-config` | `<JSON>` | _no help text_ |

## `homeboy agent-task loop stop`

```sh
homeboy agent-task loop stop <LOOP_ID>
```

Stop a durable loop and record the handoff

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | _no help text_ |

## `homeboy agent-task run-plan`

```sh
homeboy agent-task run-plan [OPTIONS]
```

Run an `AgentTaskPlan` through extension-declared executor providers

| Option | Value | Description |
| --- | --- | --- |
| `--plan` | `<JSON|@FILE|->` | Agent-task plan as a JSON spec: inline JSON, `@FILE` to read a file, or `-` to read stdin. A bare path is NOT accepted — use `@/path/plan.json` |
| `--record-run-id` | `<ID>` | _no help text_ |
| `--timeout-ms` | `<MS>` | _no help text_ |

## `homeboy agent-task run`

```sh
homeboy agent-task run [OPTIONS] <RUN_ID>
```

Execute a previously submitted durable run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--timeout-ms` | `<MS>` | _no help text_ |

## `homeboy agent-task run-next`

```sh
homeboy agent-task run-next
```

Claim and execute the oldest queued durable run

## `homeboy agent-task submit`

```sh
homeboy agent-task submit [OPTIONS]
```

Persist an agent-task plan and return a durable run id without executing it

| Option | Value | Description |
| --- | --- | --- |
| `--plan` | `<JSON|@FILE|->` | Agent-task plan as a JSON spec: inline JSON, `@FILE` to read a file, or `-` to read stdin. A bare path is NOT accepted — use `@/path/plan.json` |
| `--run-id` | `<ID>` | _no help text_ |

## `homeboy agent-task status`

```sh
homeboy agent-task status [OPTIONS] <RUN_ID>
```

Read durable run status

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--exact` | flag | Inspect this exact lifecycle record instead of resolving a Cook ID to its current attempt |
| `--bridge` | flag | _no help text_ |
| `--since-cursor` | `<CURSOR>` | _no help text_ |
| `--full` | flag | _no help text_ |
| `--no-runner-probe` | flag | Answer from durable controller state only, without reaching the runner |

## `homeboy agent-task list`

```sh
homeboy agent-task list [OPTIONS]
```

List durable runs, newest first.

Discovery returns a finite agent-facing page by default; use `--limit` for a different page or `--full` for every matching record.

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<N>` | _no help text_ |
| `--cursor` | `<N>` | Continue at this zero-based offset. Reuse every filter from the prior page |
| `--repo` | `<REPO>` | _no help text_ |
| `--worktree` | `<WORKTREE>` | _no help text_ |
| `--task-url` | `<TASK_URL>` | _no help text_ |
| `--submitted-after` | `<RFC3339>` | RFC3339 submission timestamp; excludes older records |
| `--state` | `<STATE>` | _no help text_ Values: `queued`, `running`, `succeeded`, `failed`, `cancelled`. |
| `--run-placement` | `<RUN_PLACEMENT>` | Filter by recorded execution placement, not the global routing policy Values: `local`, `remote`, `runner`. |
| `--parent-id` | `<PARENT_ID>` | _no help text_ |
| `--full` | flag | Return every matching record. This is intentionally explicit because discovery defaults to a finite agent-facing page |

## `homeboy agent-task active`

```sh
homeboy agent-task active [OPTIONS]
```

List queued and running durable runs, newest first.

`--reconcile` turns this into an explicit fleet operation: it previews every candidate by default and requires `--apply` to mutate the set.

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<N>` | Cap active discovery to a positive page size. Cannot be combined with `--full` or fleet-wide `--reconcile` |
| `--cursor` | `<N>` | Continue at this zero-based offset from the prior active page. Cannot be combined with `--full` or fleet-wide `--reconcile` |
| `--full` | flag | Return every matching record. This is intentionally explicit because discovery defaults to a finite agent-facing page and cannot scope fleet-wide `--reconcile` |
| `--reconcile` | flag | _no help text_ |
| `--dry-run` | flag | _no help text_ |
| `--apply` | flag | _no help text_ |

## `homeboy agent-task reconcile`

```sh
homeboy agent-task reconcile [OPTIONS] <RUN_ID>
```

Preview or apply reconciliation for one durable run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | _no help text_ |
| `--apply` | flag | _no help text_ |

## `homeboy agent-task reconcile-records`

```sh
homeboy agent-task reconcile-records [OPTIONS]
```

Reconcile stored durable run records against authoritative provider state

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | _no help text_ |

## `homeboy agent-task latest`

```sh
homeboy agent-task latest [OPTIONS]
```

Show the latest durable run

| Option | Value | Description |
| --- | --- | --- |
| `--limit` | `<N>` | _no help text_ |

## `homeboy agent-task logs`

```sh
homeboy agent-task logs [OPTIONS] <RUN_ID>
```

Read the canonical durable event stream for a run.

`--raw` additionally emits transport frames for diagnostics.

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--raw` | flag | Include unprojected runner transport frames under `raw_events` for diagnostics |

## `homeboy agent-task artifacts`

```sh
homeboy agent-task artifacts [OPTIONS] <RUN_ID>
```

List artifacts and evidence refs recorded for a completed run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--exact` | flag | Inspect this exact lifecycle record instead of resolving a Cook ID to its current attempt |
| `--bridge` | flag | _no help text_ |
| `--since-cursor` | `<CURSOR>` | _no help text_ |
| `--full` | flag | _no help text_ |
| `--no-runner-probe` | flag | Answer from durable controller state only, without reaching the runner |

## `homeboy agent-task retained-artifacts`

```sh
homeboy agent-task retained-artifacts <COMMAND>
```

Discover or attach selected outputs retained in a terminal Lab Cook workspace

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task retained-artifacts discover` | Resolve the retained workspace and print bounded, run-ID-only attach guidance |
| `homeboy agent-task retained-artifacts attach` | Attach one repository-relative file or directory from the retained workspace |

## `homeboy agent-task retained-artifacts discover`

```sh
homeboy agent-task retained-artifacts discover <RUN_ID>
```

Resolve the retained workspace and print bounded, run-ID-only attach guidance

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

## `homeboy agent-task retained-artifacts attach`

```sh
homeboy agent-task retained-artifacts attach [OPTIONS] <RUN_ID>
```

Attach one repository-relative file or directory from the retained workspace

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Repository-relative path below the retained workspace |
| `--name` | `<NAME>` | Durable artifact name to record on the owning run |

## `homeboy agent-task evidence`

```sh
homeboy agent-task evidence [OPTIONS] <RUN_ID>
```

Retrieve selected durable evidence recorded for a run.

Narrow the result with `--task` or `--kind`; `--full` returns the unprojected evidence.

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | _no help text_ |
| `--task` | `<TASK_ID>` | _no help text_ |
| `--failure-only` | flag | _no help text_ |
| `--full` | flag | Return every matching evidence record rather than the bounded preview |

## `homeboy agent-task diagnose`

```sh
homeboy agent-task diagnose [OPTIONS] <RUN_ID>
```

Compute a root cause, causal chain, and next actions for a failed run.

Next actions are derived from the failure classification, not from prose.

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--full` | flag | Hydrate every available evidence summary rather than the bounded preview |

## `homeboy agent-task runtime-recover`

```sh
homeboy agent-task runtime-recover [OPTIONS] <RUN_ID>
```

Recover a missing or corrupted immutable controller runtime pin

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Durable run whose exact controller executable should be rematerialized |

| Option | Value | Description |
| --- | --- | --- |
| `--source` | `<PATH>` | Trusted source checkout used to rebuild the recorded runtime revision |
| `--artifact` | `<PATH>` | Exact prebuilt controller executable. Its hash and self identity must match the durable pin |

## `homeboy agent-task runtime-validate`

```sh
homeboy agent-task runtime-validate <RUN_ID>
```

Validate controller runtime eligibility without executing provider work

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Durable run to validate without executing its provider lifecycle |

## `homeboy agent-task replay-provider-boundary`

```sh
homeboy agent-task replay-provider-boundary [OPTIONS] <RUN_ID>
```

Hydrate the latest raw executor input and print provider-boundary fields without relaunching a provider.

Persists the inspection as `provider-boundary-replay` evidence. Use `--task <task-id>` for multi-task runs.

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--task` | `<TASK_ID>` | _no help text_ |

## `homeboy agent-task cancel`

```sh
homeboy agent-task cancel [OPTIONS] <RUN_ID>
```

Mark a queued or stale-running durable run as cancelled

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--reason` | `<TEXT>` | _no help text_ |

## `homeboy agent-task resume`

```sh
homeboy agent-task resume [OPTIONS] <RUN_ID>
```

Resume a queued or stale-running durable run

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--exact` | flag | Inspect this exact lifecycle record instead of resolving a Cook ID to its current attempt |
| `--bridge` | flag | _no help text_ |
| `--since-cursor` | `<CURSOR>` | _no help text_ |
| `--full` | flag | _no help text_ |
| `--no-runner-probe` | flag | Answer from durable controller state only, without reaching the runner |

## `homeboy agent-task retry`

```sh
homeboy agent-task retry [OPTIONS] <RUN_ID>
```

Submit a fresh durable run from an existing run's plan

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--new-run-id` | `<ID>` | _no help text_ |
| `--run` | flag | _no help text_ |
| `--force` | flag | Permit a new retry after every prior retry in this lineage is terminal |

## `homeboy agent-task fanout`

```sh
homeboy agent-task fanout <COMMAND>
```

Cook, submit, and inspect batches of independent tasks.

Each child declares its own target worktree and optional head branch, runs through the same cook-loop path as a single PR cook, and finalizes its own pull request when its deterministic gates pass.

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task fanout cook-batch` | Cook a wave of independent tasks, one child cook per issue |
| `homeboy agent-task fanout plan` | Normalize and inspect a batch-cook plan without submitting or running it |
| `homeboy agent-task fanout submit` | Submit a batch of independent cooks and print the exact per-cook commands for runner or operator execution |
| `homeboy agent-task fanout submit-batch` | Submit a durable batch of independent `AgentTaskPlan` tasks as one queued child run per packet |
| `homeboy agent-task fanout status` | Read durable batch state and per-child run status |
| `homeboy agent-task fanout resume` | Resume a durable fanout batch after coordinator loss: idempotently harvest terminal children through gates, commit, push, and PR finalization |
| `homeboy agent-task fanout artifacts` | List artifacts recorded by a durable batch's child runs |
| `homeboy agent-task fanout run-plan` | Execute each cook in a batch-cook plan through the cook-loop service and return a batch summary |

## `homeboy agent-task fanout cook-batch`

```sh
homeboy agent-task fanout cook-batch [OPTIONS] <ISSUE_URL>...
```

Cook a wave of independent tasks, one child cook per issue.

Every child requires a deterministic gate from shared --verify/ --private-verify inputs or --verification-profiles. A child that cannot verify its work cannot promote it (#9838).

| Argument | Required | Description |
| --- | --- | --- |
| `<ISSUE_URL>...` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--repo` | `<REPO>` | _no help text_ |
| `--from` | `<REF>` | _no help text_ |
| `--base` | `<BRANCH>` | _no help text_ |
| `--branch-prefix` | `<PREFIX>` | _no help text_ |
| `--fanout-id` | `<ID>` | _no help text_ |
| `--prompt-template` | `<TEXT>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--model` | `<MODEL>` | _no help text_ |
| `--provider-profile` | `<PROFILE>` | _no help text_ |
| `--secret-env` | `<ENV>` | _no help text_ |
| `--provider-config` | `<JSON>` | _no help text_ |
| `--ai-tool` | `<TEXT>` | AI tool disclosure recorded in every child PR's assistance attribution. When omitted, each child derives its disclosure from its effective provider and model selection |
| `--verify` | `<COMMAND>` | Deterministic verification command that must pass before the cook promotes its work (e.g. `--verify "cargo fmt --check"`). Required unless `--private-verify` is given — a cook that cannot verify its work cannot promote it. Runs in the destination worktree. Repeat to require multiple gates; every one must pass. Its output is included in the review evidence |
| `--private-verify` | `<COMMAND>` | Like `--verify`, but the command's output is treated as private: only a pass/fail summary is revealed by default (see `--private-gate-reveal`). Satisfies the same mandatory-gate requirement as `--verify`. Use for gates whose logs may contain secrets. Repeatable |
| `--private-gate-reveal` | `<POLICY>` | How much of a `--private-verify` gate's output to reveal: `summary-only` (default) shows just pass/fail; other policies expose more detail Values: `full-evidence`, `summary-only`, `redacted`, `no-detail`. |
| `--gate-execution-policy` | `<POLICY>` | Gate scheduling policy: `ordered-fail-fast` (default) skips downstream gates after the first failure; `continue-all` runs every declared gate Values: `ordered-fail-fast`, `continue-all`. |
| `--gate-timeout-seconds` | `<SECONDS>` | Wall-clock timeout, in seconds, for each verification gate command (default 1800 = 30 min). A gate exceeding this fails |
| `--gate-heartbeat-interval-seconds` | `<SECONDS>` | How often, in seconds, to emit a heartbeat while a gate runs so long gates are not mistaken for a stalled cook (default 5) |
| `--gate-no-progress-timeout-seconds` | `<SECONDS>` | Maximum time, in seconds, a gate may run without a structured `HOMEBOY_PROGRESS` marker (default 300 = 5 min) |
| `--rerun-completed-gates` | flag | Re-run gates that already recorded a passing result on a previous attempt instead of reusing the recorded pass. Off by default |
| `--accept-inherited-failures` | flag | Finalize only when an inherited required-gate failure was reproduced on the immutable baseline. The gate remains reported as baseline-red |
| `--gate-environment-mode` | `<MODE>` | Environment for gate commands: `inherit` (default) extends the current environment; `replace` starts from an empty environment plus `--gate-env` Values: `inherit`, `replace`. |
| `--gate-env` | `<NAME=VALUE>` | Extra environment variable for gate commands, as `NAME=VALUE`. Repeatable |
| `--gate-env-from` | `<NAME=SOURCE[/PATH]>` | Preserve a required toolchain setting from the host as `NAME=SOURCE` or `NAME=SOURCE/relative/path`. The mapping is retained in gate evidence |
| `--gate-toolchain` | `<COMMAND>` | Required executable to initialize before provider execution. Its probe is `COMMAND --version` in the final isolated gate environment. Repeatable |
| `--gate-package-artifact` | `<JSON>` | Caller-declared package resource readiness as a JSON object. The object defines its environment mapping, required paths or digests, and opaque remediation metadata. Repeat for multiple resources |
| `--gate-extension-input` | `<JSON>` | Explicit extension input as a JSON object with `id` and absolute `source`. Only selected inputs are copied into isolated HOME |
| `--isolate-gate-home` | `<ISOLATE_GATE_HOME>` | Run gates with an isolated `$HOME` so gate side effects do not touch the operator's home directory (default true) Values: `true`, `false`. |
| `--isolate-gate-xdg` | `<ISOLATE_GATE_XDG>` | Run gates with isolated XDG base directories so gate side effects do not touch the operator's config/cache/data dirs (default true) Values: `true`, `false`. |
| `--verification-profiles` | `<JSON>` | JSON verification profile declaration, inline or @file.json. Profiles append to or replace shared --verify/--private-verify gates per issue |
| `--dry-run` | flag | _no help text_ |
| `--run-plan` | flag | _no help text_ |

## `homeboy agent-task fanout plan`

```sh
homeboy agent-task fanout plan [OPTIONS]
```

Normalize and inspect a batch-cook plan without submitting or running it

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<SPEC>` | _no help text_ |
| `--fanout-id` | `<ID>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--model` | `<MODEL>` | _no help text_ |

## `homeboy agent-task fanout submit`

```sh
homeboy agent-task fanout submit [OPTIONS]
```

Submit a batch of independent cooks and print the exact per-cook commands for runner or operator execution

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<SPEC>` | _no help text_ |
| `--fanout-id` | `<ID>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--model` | `<MODEL>` | _no help text_ |
| `--run-id` | `<ID>` | _no help text_ |

## `homeboy agent-task fanout submit-batch`

```sh
homeboy agent-task fanout submit-batch [OPTIONS]
```

Submit a durable batch of independent `AgentTaskPlan` tasks as one queued child run per packet.

Provider-neutral by design: drive execution with `agent-task run-next` or an existing runner queue loop, then reconcile with `fanout status` and `fanout artifacts`.

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<SPEC>` | _no help text_ |
| `--fanout-id` | `<ID>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--model` | `<MODEL>` | _no help text_ |
| `--batch-id` | `<ID>` | _no help text_ |

## `homeboy agent-task fanout status`

```sh
homeboy agent-task fanout status <BATCH_ID>
```

Read durable batch state and per-child run status

| Argument | Required | Description |
| --- | --- | --- |
| `<BATCH_ID>` | yes | _no help text_ |

## `homeboy agent-task fanout resume`

```sh
homeboy agent-task fanout resume <BATCH_ID>
```

Resume a durable fanout batch after coordinator loss: idempotently harvest terminal children through gates, commit, push, and PR finalization

| Argument | Required | Description |
| --- | --- | --- |
| `<BATCH_ID>` | yes | _no help text_ |

## `homeboy agent-task fanout artifacts`

```sh
homeboy agent-task fanout artifacts <BATCH_ID>
```

List artifacts recorded by a durable batch's child runs

| Argument | Required | Description |
| --- | --- | --- |
| `<BATCH_ID>` | yes | _no help text_ |

## `homeboy agent-task fanout run-plan`

```sh
homeboy agent-task fanout run-plan [OPTIONS]
```

Execute each cook in a batch-cook plan through the cook-loop service and return a batch summary.

Successful child cooks open or update their own pull requests.

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<SPEC>` | _no help text_ |
| `--fanout-id` | `<ID>` | _no help text_ |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--model` | `<MODEL>` | _no help text_ |
| `--record-run-id` | `<ID>` | _no help text_ |
| `--ai-tool` | `<TEXT>` | AI tool disclosure recorded in every child PR's assistance attribution. Overrides the persisted plan value for this execution |

## `homeboy agent-task review`

```sh
homeboy agent-task review [OPTIONS] <RUN_ID>
```

Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--to-worktree` | `<HANDLE>` | _no help text_ |
| `--provider-command` | `<COMMAND>` | Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`. |
| `--provider-argv` | `<ARG>` | Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`. |

## `homeboy agent-task promote`

```sh
homeboy agent-task promote [OPTIONS] <SOURCE>
```

Promote a completed generic patch artifact into a managed worktree

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--to-worktree` | `<HANDLE>` | _no help text_ |
| `--base` | `<BRANCH>` | Declared base branch resolved immediately before promotion gates run |
| `--provider-command` | `<COMMAND>` | Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`. |
| `--provider-argv` | `<ARG>` | Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`. |
| `--task-id` | `<TASK_ID>` | _no help text_ |
| `--artifact-id` | `<ARTIFACT_ID>` | _no help text_ |
| `--dry-run` | flag | _no help text_ |
| `--verify` | `<COMMAND>` | Deterministic verification command that must pass before the cook promotes its work (e.g. `--verify "cargo fmt --check"`). Required unless `--private-verify` is given — a cook that cannot verify its work cannot promote it. Runs in the destination worktree. Repeat to require multiple gates; every one must pass. Its output is included in the review evidence |
| `--private-verify` | `<COMMAND>` | Like `--verify`, but the command's output is treated as private: only a pass/fail summary is revealed by default (see `--private-gate-reveal`). Satisfies the same mandatory-gate requirement as `--verify`. Use for gates whose logs may contain secrets. Repeatable |
| `--private-gate-reveal` | `<POLICY>` | How much of a `--private-verify` gate's output to reveal: `summary-only` (default) shows just pass/fail; other policies expose more detail Values: `full-evidence`, `summary-only`, `redacted`, `no-detail`. |
| `--gate-execution-policy` | `<POLICY>` | Gate scheduling policy: `ordered-fail-fast` (default) skips downstream gates after the first failure; `continue-all` runs every declared gate Values: `ordered-fail-fast`, `continue-all`. |
| `--gate-timeout-seconds` | `<SECONDS>` | Wall-clock timeout, in seconds, for each verification gate command (default 1800 = 30 min). A gate exceeding this fails |
| `--gate-heartbeat-interval-seconds` | `<SECONDS>` | How often, in seconds, to emit a heartbeat while a gate runs so long gates are not mistaken for a stalled cook (default 5) |
| `--gate-no-progress-timeout-seconds` | `<SECONDS>` | Maximum time, in seconds, a gate may run without a structured `HOMEBOY_PROGRESS` marker (default 300 = 5 min) |
| `--rerun-completed-gates` | flag | Re-run gates that already recorded a passing result on a previous attempt instead of reusing the recorded pass. Off by default |
| `--accept-inherited-failures` | flag | Finalize only when an inherited required-gate failure was reproduced on the immutable baseline. The gate remains reported as baseline-red |
| `--gate-environment-mode` | `<MODE>` | Environment for gate commands: `inherit` (default) extends the current environment; `replace` starts from an empty environment plus `--gate-env` Values: `inherit`, `replace`. |
| `--gate-env` | `<NAME=VALUE>` | Extra environment variable for gate commands, as `NAME=VALUE`. Repeatable |
| `--gate-env-from` | `<NAME=SOURCE[/PATH]>` | Preserve a required toolchain setting from the host as `NAME=SOURCE` or `NAME=SOURCE/relative/path`. The mapping is retained in gate evidence |
| `--gate-toolchain` | `<COMMAND>` | Required executable to initialize before provider execution. Its probe is `COMMAND --version` in the final isolated gate environment. Repeatable |
| `--gate-package-artifact` | `<JSON>` | Caller-declared package resource readiness as a JSON object. The object defines its environment mapping, required paths or digests, and opaque remediation metadata. Repeat for multiple resources |
| `--gate-extension-input` | `<JSON>` | Explicit extension input as a JSON object with `id` and absolute `source`. Only selected inputs are copied into isolated HOME |
| `--isolate-gate-home` | `<ISOLATE_GATE_HOME>` | Run gates with an isolated `$HOME` so gate side effects do not touch the operator's home directory (default true) Values: `true`, `false`. |
| `--isolate-gate-xdg` | `<ISOLATE_GATE_XDG>` | Run gates with isolated XDG base directories so gate side effects do not touch the operator's config/cache/data dirs (default true) Values: `true`, `false`. |

## `homeboy agent-task adopt`

```sh
homeboy agent-task adopt [OPTIONS] <RUN_OR_COOK_ID>
```

Adopt an immutable commit candidate through a tracked cook's normal gates and finalization

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_OR_COOK_ID>` | yes | Existing durable run id or cook id whose recipe owns the candidate lifecycle |

| Option | Value | Description |
| --- | --- | --- |
| `--attempt` | `<N>` | Select an exact durable attempt from the Cook recipe. Required when attempts use different policies |
| `--candidate-ref` | `<SHA>` | Immutable commit revision in the recorded source worktree |
| `--ai-model` | `<MODEL>` | Concrete model that prepared the externally supplied candidate |
| `--replace-interrupted` | flag | Replace a stale interrupted adoption while retaining its lifecycle evidence |
| `--full` | flag | Return the complete cook adoption report, including nested gate evidence |

## `homeboy agent-task finalize-pr`

```sh
homeboy agent-task finalize-pr [OPTIONS]
```

Finalize a green run, or recover publication from a durable Cook record.

This is the core-owned publication boundary for external runtimes.

| Option | Value | Description |
| --- | --- | --- |
| `--recover` | `<RUN_OR_COOK_ID>` | Hydrate finalization from a durable Cook recipe and its applied promotion |
| `--run-id` | `<ID>` | _no help text_ |
| `--path` | `<PATH>` | _no help text_ |
| `--base` | `<BRANCH>` | _no help text_ |
| `--verified-base-sha` | `<SHA>` | Immutable base commit SHA recorded before the declared verification gates ran |
| `--head` | `<BRANCH>` | _no help text_ |
| `--title` | `<TEXT>` | _no help text_ |
| `--commit-message` | `<TEXT>` | _no help text_ |
| `--attempt-summary` | `<TEXT>` | Attempt summary to include in the PR body |
| `--source-ref` | `<REF>` | Source tracker/reference URL or identifier. Repeatable |
| `--artifact-ref` | `<REF>` | Artifact/evidence URL, path, or identifier. Repeatable |
| `--ai-tool` | `<TEXT>` | AI tool disclosure line for the PR body |
| `--ai-model` | `<MODEL>` | Actual model identifier for AI disclosure. Finalization requires a recorded model |
| `--related-finding-id` | `<ID>` | Source finding id shared by sibling generated PRs |
| `--source-packet-id` | `<ID>` | Source validation packet id shared by sibling generated PRs |
| `--change-kind` | `<KIND>` | Generated change kind, e.g. evidence-only, runtime-fix, or test-only |
| `--supersedes` | `<REF>` | Generated PR or artifact this PR supersedes. Repeatable |
| `--depends-on` | `<REF>` | Generated PR or artifact this PR depends on. Repeatable |
| `--targeted-check-run` | `<COMMAND>` | Targeted verification command that ran before finalization. Repeatable |
| `--targeted-checks-unavailable` | `<TEXT>` | Exact backend limitation when targeted checks could not be run |
| `--ci-expected` | `<CHECK>` | CI check expected to run after push. Repeatable |
| `--manual-reviewer-check` | `<TEXT>` | Manual reviewer verification requested when targeted checks/CI do not cover behavior |
| `--why-not-broader-than-packet` | `<TEXT>` | Runtime-fix evidence bound for generated predicates/semantics |
| `--evidence-discriminator` | `<TEXT>` | Evidence-specific discriminator preserved by the runtime fix. Repeatable |
| `--nearby-contract-preserved` | `<TEXT>` | Nearby predicate/contract preserved by the runtime fix. Repeatable |
| `--changed-public-contract` | `<ID=>SUMMARY>` | Declared changed public contract as ID=>SUMMARY. Requires the complete compatibility/external-usage evidence bundle below |
| `--compatibility-impact` | `<TEXT>` | Compatibility impact for declared public contracts |
| `--external-consumer-impact` | `<TEXT>` | External-consumer impact for declared public contracts |
| `--external-usage-status` | `<STATUS>` | External usage evidence status: completed or unavailable_manual_review |
| `--external-usage-source` | `<TEXT>` | Source used for external usage evidence |
| `--external-usage-limitations` | `<TEXT>` | Limitations of the external usage evidence or manual review |
| `--external-usage-url` | `<URL>` | Reviewer-resolvable HTTPS URL for external usage evidence |
| `--gate-result` | `<NAME=STATUS[:DETAIL]>` | _no help text_ |
| `--changed-file` | `<PATH>` | _no help text_ |
| `--protected-branch` | `<BRANCH>` | _no help text_ |
| `--ai-used-for` | `<TEXT>` | _no help text_ |
| `--summary` | `<TEXT>` | _no help text_ |
| `--what-changed` | `<TEXT>` | _no help text_ |
| `--test-step` | `<COMMAND=>EXPECTED>` | Reviewer test step. Strict shape: COMMAND=>EXPECTED |
| `--compatibility` | `<TEXT>` | _no help text_ |
| `--closes` | `<ISSUE_REF>` | Closing issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL |
| `--relates-to` | `<ISSUE_REF>` | Related issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL |
| `--review-override` | `<TARGET=VALUE@PROVENANCE>` | _no help text_ |
| `--preflight` | flag | Validate the complete hydrated dossier and candidate without publishing |
| `--manual-finalization` | flag | Publish corrected, independently verified work without a promotion lineage. The ID must identify a failed attempt (a Cook ID resolves to its newest attempt, which must be failed), or be unused so Homeboy can reserve a durable manual-finalization record for its intent and receipt |

## `homeboy agent-task record-replacement-gate-proof`

```sh
homeboy agent-task record-replacement-gate-proof [OPTIONS] <RUN_ID>
```

Attach authorized candidate-bound replacement gate proof after an infrastructure gate failure

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | Durable Cook attempt whose applied candidate has infrastructure-invalid gates |

| Option | Value | Description |
| --- | --- | --- |
| `--promotion` | `<JSON|@FILE|->` | Complete typed promotion report from the replacement gate executor: inline JSON, `@FILE`, or `-` |
| `--authorize-external-proof` | `<TEXT>` | Explicit operator authorization for externally produced proof |

## `homeboy agent-task accept`

```sh
homeboy agent-task accept [OPTIONS] <RUN_ID>
```

Record an independent, durable acceptance verdict for a candidate

| Argument | Required | Description |
| --- | --- | --- |
| `<RUN_ID>` | yes | _no help text_ |

| Option | Value | Description |
| --- | --- | --- |
| `--verdict` | `<VERDICT>` | _no help text_ Values: `accepted`, `rejected`. |
| `--token` | `<TOKEN>` | Opaque credential consumed by the configured acceptance verifier |
| `--evidence-ref` | `<EVIDENCE_REFS>` | _no help text_ |

## `homeboy agent-task gate-feedback`

```sh
homeboy agent-task gate-feedback [OPTIONS]
```

Convert deterministic gate results into a cook retry or stop decision

| Option | Value | Description |
| --- | --- | --- |
| `--promotion` | `<JSON|@FILE|->` | Promotion report as a JSON spec: inline JSON, `@FILE` to read a file, or `-` to read stdin. A bare path is NOT accepted — use `@/path/promotion.json` |
| `--source-task` | `<JSON|@FILE|->` | Source task as a JSON spec: inline JSON, `@FILE` to read a file, or `-` to read stdin. A bare path is NOT accepted — use `@/path/task.json` |
| `--attempt` | `<N>` | _no help text_ |
| `--max-attempts` | `<N>` | _no help text_ |
| `--source-run-id` | `<ID>` | _no help text_ |
| `--current-diff` | `<SPEC>` | _no help text_ |

## `homeboy agent-task providers`

```sh
homeboy agent-task providers [OPTIONS]
```

List extension-declared executor providers and optional secret/backend readiness.

`--backend X` filters the presentation to X so output stays within caller display limits; pass `--catalog` for the full multi-backend catalog.

| Option | Value | Description |
| --- | --- | --- |
| `--backend` | `<BACKEND>` | _no help text_ |
| `--selector` | `<PROVIDER_ID>` | _no help text_ |
| `--runtime` | `<RUNTIME>` | Restrict results to the runtime that owns the provider |
| `--status` | `<STATUS>` | Restrict results to `default`, `available`, or `unavailable` providers |
| `--secret-env` | `<ENV>` | _no help text_ |
| `--validate-readiness` | flag | _no help text_ |
| `--refresh` | flag | _no help text_ |
| `--catalog` | flag | Return the full multi-backend catalog even when `--backend` is set. Without this, `--backend X` filters the presentation to X so the output stays within caller display limits (#9654) |
| `--full` | flag | Return the complete provider declarations and discovery diagnostics |

## `homeboy agent-task prompts`

```sh
homeboy agent-task prompts <COMMAND>
```

Manage markdown prompts in Homeboy-owned storage.

Prompts are stored under Homeboy's data directory, not the current repo/worktree, and are referenced as `prompt:<id>` wherever a prompt string is accepted.

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task prompts save` | Save a markdown prompt in Homeboy's agent-task prompt store |
| `homeboy agent-task prompts list` | List stored agent-task prompts |
| `homeboy agent-task prompts show` | Show a stored agent-task prompt |
| `homeboy agent-task prompts remove` | Remove a stored agent-task prompt |

## `homeboy agent-task prompts save`

```sh
homeboy agent-task prompts save [OPTIONS] <NAME>
```

Save a markdown prompt in Homeboy's agent-task prompt store

| Argument | Required | Description |
| --- | --- | --- |
| `<NAME>` | yes | Stable prompt name. Unsafe path characters are normalized for storage |

| Option | Value | Description |
| --- | --- | --- |
| `--input` | `<PROMPT>` | Prompt markdown content, @file, or - for stdin |

## `homeboy agent-task prompts list`

```sh
homeboy agent-task prompts list
```

List stored agent-task prompts

## `homeboy agent-task prompts show`

```sh
homeboy agent-task prompts show <NAME>
```

Show a stored agent-task prompt

| Argument | Required | Description |
| --- | --- | --- |
| `<NAME>` | yes | Stored prompt name or id |

## `homeboy agent-task prompts remove`

```sh
homeboy agent-task prompts remove <NAME>
```

Remove a stored agent-task prompt

| Argument | Required | Description |
| --- | --- | --- |
| `<NAME>` | yes | Stored prompt name or id |

## `homeboy agent-task contract`

```sh
homeboy agent-task contract [OPTIONS]
```

Export Homeboy's machine-readable agent-task core contract metadata

| Option | Value | Description |
| --- | --- | --- |
| `--format` | `<FORMAT>` | _no help text_ Values: `json`. |

## `homeboy agent-task compile-loop`

```sh
homeboy agent-task compile-loop [OPTIONS]
```

Compile a declarative loop definition into an agent-task plan without submitting or running it

| Option | Value | Description |
| --- | --- | --- |
| `--definition` | `<SPEC>` | _no help text_ |

## `homeboy agent-task auth`

```sh
homeboy agent-task auth <COMMAND>
```

Configure and inspect provider authentication secrets

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task auth status` | Show redacted readiness for provider secret environment variables |
| `homeboy agent-task auth set-keychain` | Store a provider secret in the OS keychain and map it to a required env name |
| `homeboy agent-task auth set-config` | Store a provider secret in Homeboy global config and map it to a required env name |
| `homeboy agent-task auth set-keychain-bundle` | Store a JSON secret bundle in one OS keychain item |
| `homeboy agent-task auth map-env` | Map a required provider env name to another process env var |
| `homeboy agent-task auth map-keychain-bundle` | Map a required provider env name to a field in a JSON keychain bundle |
| `homeboy agent-task auth remove` | Remove a provider secret source mapping |

## `homeboy agent-task auth status`

```sh
homeboy agent-task auth status [OPTIONS]
```

Show redacted readiness for provider secret environment variables

| Option | Value | Description |
| --- | --- | --- |
| `--backend` | `<BACKEND>` | Executor backend whose required secrets to report. Defaults to the same backend cook/dispatch would use when omitted |
| `--selector` | `<PROVIDER_ID>` | Provider id to disambiguate when more than one provider exists for the backend |
| `--secret-env` | `<ENV>` | Secret environment variable name to check without exposing its value. Repeatable. When omitted, the selected backend's required secrets are used |

## `homeboy agent-task auth set-keychain`

```sh
homeboy agent-task auth set-keychain [OPTIONS] <ENV> [VALUE]
```

Store a provider secret in the OS keychain and map it to a required env name

| Argument | Required | Description |
| --- | --- | --- |
| `<ENV>` | yes | Required provider environment variable name to satisfy |
| `[VALUE]` | no | Secret value. Omit to prompt securely |

| Option | Value | Description |
| --- | --- | --- |
| `--value-stdin` | flag | Read the secret value from stdin |
| `--scope` | `<SCOPE>` | Keychain scope. Defaults to agent-task |
| `--name` | `<NAME>` | Keychain entry name. Defaults to ENV |

## `homeboy agent-task auth set-config`

```sh
homeboy agent-task auth set-config [OPTIONS] <ENV> [VALUE]
```

Store a provider secret in Homeboy global config and map it to a required env name

| Argument | Required | Description |
| --- | --- | --- |
| `<ENV>` | yes | Required provider environment variable name to satisfy |
| `[VALUE]` | no | Secret value. Omit to prompt securely |

| Option | Value | Description |
| --- | --- | --- |
| `--value-stdin` | flag | Read the secret value from stdin |

## `homeboy agent-task auth set-keychain-bundle`

```sh
homeboy agent-task auth set-keychain-bundle [OPTIONS] <BUNDLE> [JSON]
```

Store a JSON secret bundle in one OS keychain item

| Argument | Required | Description |
| --- | --- | --- |
| `<BUNDLE>` | yes | Logical bundle id to store |
| `[JSON]` | no | JSON bundle value. Omit to prompt securely |

| Option | Value | Description |
| --- | --- | --- |
| `--value-stdin` | flag | Read the JSON bundle value from stdin |
| `--scope` | `<SCOPE>` | Keychain scope. Defaults to agent-task |
| `--name` | `<NAME>` | Keychain entry name. Defaults to BUNDLE |

## `homeboy agent-task auth map-env`

```sh
homeboy agent-task auth map-env [OPTIONS] <ENV>
```

Map a required provider env name to another process env var

| Argument | Required | Description |
| --- | --- | --- |
| `<ENV>` | yes | Required provider environment variable name to satisfy |

| Option | Value | Description |
| --- | --- | --- |
| `--from` | `<ENV>` | Source process environment variable. Defaults to ENV |

## `homeboy agent-task auth map-keychain-bundle`

```sh
homeboy agent-task auth map-keychain-bundle [OPTIONS] <ENV>
```

Map a required provider env name to a field in a JSON keychain bundle

| Argument | Required | Description |
| --- | --- | --- |
| `<ENV>` | yes | Required provider environment variable name to satisfy |

| Option | Value | Description |
| --- | --- | --- |
| `--bundle` | `<BUNDLE>` | Logical bundle id to read |
| `--field` | `<FIELD>` | Field path inside the JSON bundle, using dots for nested objects |
| `--scope` | `<SCOPE>` | Keychain scope. Defaults to agent-task |
| `--name` | `<NAME>` | Keychain entry name. Defaults to BUNDLE |

## `homeboy agent-task auth remove`

```sh
homeboy agent-task auth remove [OPTIONS] <ENV>
```

Remove a provider secret source mapping

| Argument | Required | Description |
| --- | --- | --- |
| `<ENV>` | yes | Required provider environment variable name whose mapping should be removed |

| Option | Value | Description |
| --- | --- | --- |
| `--keychain` | flag | Also remove the mapped keychain entry when the mapping points at keychain |

## `homeboy agent-task controller`

```sh
homeboy agent-task controller <COMMAND>
```

Create, inspect, and resume durable multi-agent loop controller state

| Subcommand | Summary |
| --- | --- |
| `homeboy agent-task controller init` | Create a durable loop controller record |
| `homeboy agent-task controller from-spec` | Initialize or resume a durable loop controller from a repo-authored JSON spec |
| `homeboy agent-task controller run-from-spec` | Materialize, initialize, and run a bounded controller loop from a repo-authored JSON spec |
| `homeboy agent-task controller materialize` | Materialize a repo-authored loop spec with explicit run inputs |
| `homeboy agent-task controller validate-proof` | Validate a proof, materialized spec, or controller record for deterministic handoff |
| `homeboy agent-task controller plan` | Compile a controller spec into a dry Homeboy plan without writing state |
| `homeboy agent-task controller status` | Read a durable loop controller record |
| `homeboy agent-task controller diagnose` | Render the controller failure tree for failed child actions |
| `homeboy agent-task controller list` | List durable loop controller records |
| `homeboy agent-task controller events` | Apply a generic external controller event |
| `homeboy agent-task controller apply-event` | Apply an external event and resume matching waits |
| `homeboy agent-task controller run-next` | Claim and execute the next pending controller action |
| `homeboy agent-task controller run` | Claim and execute one pending controller action |
| `homeboy agent-task controller resume` | Execute pending controller actions until no executable action remains |
| `homeboy agent-task controller mark-human-ready` | Mark a tracked entity as human-ready work |
| `homeboy agent-task controller proof` | Run a one-command end-to-end controller proof from a named profile + runner |

## `homeboy agent-task controller init`

```sh
homeboy agent-task controller init [OPTIONS] <LOOP_ID>
```

Create a durable loop controller record

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id. Unsafe path characters are normalized for storage |

| Option | Value | Description |
| --- | --- | --- |
| `--phase` | `<PHASE>` | Initial controller phase |
| `--config-version` | `<VERSION>` | Declared graph/config version for resume compatibility |

## `homeboy agent-task controller from-spec`

```sh
homeboy agent-task controller from-spec [OPTIONS] <SPEC>
```

Initialize or resume a durable loop controller from a repo-authored JSON spec.

With a configured default Lab runner, --resume uses automatic Lab offload unless local execution is explicitly forced.

| Argument | Required | Description |
| --- | --- | --- |
| `<SPEC>` | yes | Repo loop spec JSON, @file, or - for stdin |

| Option | Value | Description |
| --- | --- | --- |
| `--resume` | flag | Execute pending actions after applying the spec |
| `--inputs` | `<JSON>` | Explicit controller run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects |
| `--policy-result` | `<JSON>` | Declarative policy result JSON, @file, or - for stdin. Repeatable |
| `--max-actions` | `<N>` | Maximum controller actions to execute when --resume is supplied |
| `--reconcile-stale` | flag | On --resume, automatically reset stale persisted controller state and re-create it from this spec |
| `--replace` | flag | On --resume, discard stale persisted controller state and re-create it from this spec |
| `--fork` | flag | On --resume, apply this spec under a derived fork loop id, leaving the original untouched |
| `--resume-existing` | flag | On --resume, accept stale/mismatched persisted state and resume the existing controller as-is |
| `--doctor` | flag | Compile and preflight generic controller prerequisites without writing state |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller run-from-spec`

```sh
homeboy agent-task controller run-from-spec [OPTIONS] <SPEC>
```

Materialize, initialize, and run a bounded controller loop from a repo-authored JSON spec.

With a configured default Lab runner, this uses automatic Lab offload unless local execution is explicitly forced.

| Argument | Required | Description |
| --- | --- | --- |
| `<SPEC>` | yes | Repo loop spec JSON, @file, -, or a generator manifest that writes a spec file |

| Option | Value | Description |
| --- | --- | --- |
| `--inputs` | `<JSON>` | Explicit run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects |
| `--policy-result` | `<JSON>` | Declarative policy result JSON, @file, or - for stdin. Repeatable |
| `--max-actions` | `<N>` | Maximum controller actions to execute before returning a bounded partial result |
| `--reconcile-stale` | flag | One-flag safe proof-run mode: automatically reset stale persisted controller state and re-derive isolated run-scoped state from this spec, with no manual state cleanup. Use this for proof/rerun workflows when the persisted spec fingerprint conflicts with the requested spec |
| `--replace` | flag | Discard stale persisted controller state and re-create it from this spec before running |
| `--fork` | flag | Apply this spec under a derived fork loop id, leaving the original controller untouched |
| `--resume-existing` | flag | Accept stale/mismatched persisted state and resume the existing controller as-is |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller materialize`

```sh
homeboy agent-task controller materialize [OPTIONS] <SPEC>
```

Materialize a repo-authored loop spec with explicit run inputs.

With a configured default Lab runner, this uses automatic Lab offload unless local execution is explicitly forced.

| Argument | Required | Description |
| --- | --- | --- |
| `<SPEC>` | yes | Repo loop spec JSON, @file, -, or a generator manifest that writes a spec file |

| Option | Value | Description |
| --- | --- | --- |
| `--inputs` | `<JSON>` | Explicit run inputs JSON, @file, or - for stdin. Supports `inputs` and `metadata` objects |
| `--policy-result` | `<JSON>` | Declarative policy result JSON, @file, or - for stdin. Repeatable |

## `homeboy agent-task controller validate-proof`

```sh
homeboy agent-task controller validate-proof <JSON>
```

Validate a proof, materialized spec, or controller record for deterministic handoff

| Argument | Required | Description |
| --- | --- | --- |
| `<JSON>` | yes | Proof JSON, materialize output JSON, controller record JSON, @file, or - for stdin |

## `homeboy agent-task controller plan`

```sh
homeboy agent-task controller plan <SPEC>
```

Compile a controller spec into a dry Homeboy plan without writing state

| Argument | Required | Description |
| --- | --- | --- |
| `<SPEC>` | yes | Controller spec JSON, @file, or - for stdin |

## `homeboy agent-task controller status`

```sh
homeboy agent-task controller status [OPTIONS] <LOOP_ID>
```

Read a durable loop controller record

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--spec` | `<SPEC>` | Optional repo loop spec JSON, @file, or - to compare against persisted controller state |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller diagnose`

```sh
homeboy agent-task controller diagnose [OPTIONS] <LOOP_ID>
```

Render the controller failure tree for failed child actions

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--spec` | `<SPEC>` | Optional repo loop spec JSON, @file, or - to compare against persisted controller state |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller list`

```sh
homeboy agent-task controller list
```

List durable loop controller records

## `homeboy agent-task controller events`

```sh
homeboy agent-task controller events [OPTIONS] <LOOP_ID>
```

Apply a generic external controller event

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--event-type` | `<TYPE>` | External event type, for example github.pr.merged or task.completed |
| `--event-id` | `<ID>` | Stable event id. Generated from the loop history length when omitted |
| `--event-key` | `<KEY>` | Optional deterministic event key, such as repo#pr or a check-suite id |
| `--entity-id` | `<ID>` | Optional target entity id for wait matching and lineage |
| `--payload` | `<JSON>` | Event payload JSON, @file, or - for stdin. May contain a `policy` object to evaluate |

## `homeboy agent-task controller apply-event`

```sh
homeboy agent-task controller apply-event [OPTIONS] <LOOP_ID>
```

Apply an external event and resume matching waits

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--event-type` | `<TYPE>` | External event type, for example github.pr.merged or task.completed |
| `--event-id` | `<ID>` | Stable event id. Generated from the loop history length when omitted |
| `--event-key` | `<KEY>` | Optional deterministic event key, such as repo#pr or a check-suite id |
| `--entity-id` | `<ID>` | Optional target entity id for wait matching and lineage |
| `--payload` | `<JSON>` | Event payload JSON, @file, or - for stdin. May contain a `policy` object to evaluate |

## `homeboy agent-task controller run-next`

```sh
homeboy agent-task controller run-next [OPTIONS] <LOOP_ID>
```

Claim and execute the next pending controller action

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller run`

```sh
homeboy agent-task controller run [OPTIONS] <LOOP_ID>
```

Claim and execute one pending controller action

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--action-id` | `<ID>` | Pending controller action id to execute |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller resume`

```sh
homeboy agent-task controller resume [OPTIONS] <LOOP_ID>
```

Execute pending controller actions until no executable action remains

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--dispatch-backend` | `<BACKEND>` | Executor backend to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-selector` | `<PROVIDER_ID>` | Extension-provider selector: the Homeboy executor provider id (e.g. `sample.executor-provider`) that runs controller-spawned dispatch actions when the action omits one. This is not model/runtime provider configuration; pass runtime-specific values in --dispatch-provider-config. Run `homeboy agent-task providers` for valid ids |
| `--dispatch-model` | `<MODEL>` | Model override to use for controller-spawned dispatch actions when the action omits one |
| `--dispatch-provider-config` | `<JSON>` | Agent/model provider config (JSON, @file, or -): the nested AI runtime/provider/model the selected executor uses for controller-spawned dispatch actions when the action omits one. Put runtime-specific provider selection here, not in --dispatch-selector |

## `homeboy agent-task controller mark-human-ready`

```sh
homeboy agent-task controller mark-human-ready [OPTIONS] <LOOP_ID>
```

Mark a tracked entity as human-ready work

| Argument | Required | Description |
| --- | --- | --- |
| `<LOOP_ID>` | yes | Durable loop id returned by `agent-task controller init` |

| Option | Value | Description |
| --- | --- | --- |
| `--entity-id` | `<ID>` | Entity id to mark human-ready |
| `--reason` | `<TEXT>` | Operator-visible reason stored in loop history |

## `homeboy agent-task controller proof`

```sh
homeboy agent-task controller proof [OPTIONS]
```

Run a one-command end-to-end controller proof from a named profile + runner

| Option | Value | Description |
| --- | --- | --- |
| `--profile` | `<NAME>` | Named proof profile (intent + policy). Resolved from the registry passed via --profiles; the orchestration never branches on the profile name |
| `--runner` | `<RUNNER>` | Runner to dispatch the proof through (for example a Lab runner id) |
| `--profiles` | `<JSON>` | Proof profile registry JSON, @file, or - for stdin: a generic object mapping profile names to profile definitions. Keeps profile data out of core so adding a profile is pure data |
| `--seed` | `<SEED>` | Optional explicit seed material for run-scoped identity. Defaults to a fresh timestamp so each invocation derives an isolated run/loop id |
| `--max-actions` | `<N>` | Maximum controller actions to execute once preflight passes |
| `--preflight-only` | flag | Run preflight reconciliation only; do not dispatch even when it passes |
