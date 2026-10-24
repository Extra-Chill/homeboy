# agent-task

Run provider-neutral task plans through Homeboy's durable agent-task lifecycle.

Homeboy owns durable orchestration and provider-neutral outcomes. Runtime
providers own backend-specific execution. For the provider fanout ownership seam,
see [`docs/architecture/provider-fanout-boundary.md`](../architecture/provider-fanout-boundary.md).

## Boundaries

`agent-task` is split into four operator-facing seams:

- **Lifecycle:** durable run submission, execution, inspection, cancellation, and retry.
- **Cook/review:** workspace task conveniences that compose lifecycle runs with promotion, gates, and PR finalization.
- **Provider:** executor discovery, machine-readable contracts, and redacted auth readiness.
- **Prompt store:** Homeboy-owned markdown prompts for reusable cook input.

## Subcommands

### Lifecycle

| Subcommand | Purpose |
|---|---|
| `run-plan` | Run an `AgentTaskPlan` through extension-declared executor providers. |
| `run <run-id>` | Execute one exact durable run id, bypassing older queued work. |
| `status <run-id> [--exact] [--watch]` | Read durable run status; `--exact` bypasses Cook alias resolution to inspect that concrete lifecycle record. `--watch` emits changed snapshots and exits when terminal or timed out. |
| `list [--latest] [--limit <n>] [--cursor <cursor>] [--branch <branch>]` | List durable runs, newest first; `--latest` returns the newest record matching the supplied filters. |
| `active [--limit <n>] [--cursor <cursor>] [--branch <branch>] [--reconcile [--dry-run\|--apply]]` | List queued and running durable runs, newest first, or preview/reconcile the explicit fleet mutation set. |
| `reconcile <run-id> [--dry-run\|--apply]` | Preview or reconcile one durable run after refreshing its authoritative provider state. |
| `logs <run-id> [--cursor <cursor>]` | Read or resume the canonical durable event stream. |
| `artifacts <run-id>` | List artifacts and evidence refs recorded for a completed run. |
| `replay-provider-boundary <run-id> [--task <task-id>]` | Hydrate the latest raw executor input and print provider-boundary fields without relaunching a provider. |
| `cancel <run-id>` | Mark a queued or stale-running durable run as cancelled. |
| `quarantine <run-id> --reason <text>` | Exclude one exact queued run while retaining its nonterminal lifecycle evidence. |
| `rearm <run-id>` | Return one exact quarantined queued run to eligibility after repair. |
| `resume <run-id>` | Resume a queued or stale-running durable run. Given a Cook ID whose durable state names a recovery step, it performs that step through `recover <cook-id>`. |
| `retry <run-id>` | Submit a fresh durable run from an existing run's plan. A stopped Cook is recovered with `recover <cook-or-attempt-id>`, which owns the Cook's next-attempt provider-route overrides; `retry <cook-id> --run` performs the Cook's recovery step through `recover`, and any other retry of a Cook ID is refused toward it (pass the exact attempt ID for a plain retry). |
| `prompts save\|list\|show\|remove` | Manage markdown prompts in Homeboy-owned storage. |

`agent-task list` and `agent-task active` accept `--limit <n>` to cap discovery output. `list` and `active` default to 20 newest rows and return `next_cursor` when another page exists; continue active discovery with `agent-task active --limit <n> --cursor <next_cursor>`. `list` and `active` also accept `--branch <branch>`, matching the branch scope persisted from the submitted plan's first task workspace; older records without that submitted scope do not match. The opaque cursor binds every effective filter, including `--branch`, and rejects mismatched continuations. `agent-task list --latest` walks durable history newest-first in keyset pages, stopping at the first match, and returns the newest record matching the same `--task-url`, `--repo`, `--worktree`, `--submitted-after`, `--state`, `--run-placement`, `--parent-id`, and `--branch` filters. Its `global_record_health` describes only the pages read. Active discovery emits at most eight prioritized `next_actions`: fleet reconciliation when stale records exist, page continuation when truncated, and focused per-run inspection actions. `list --full` and `active --full` return every matching row. `list --latest` rejects `--limit`, `--cursor`, and `--full` because a one-record result cannot continue, expand, or cap a page. `agent-task reconcile <run-id>` is the recovery path emitted by status and activity: it previews only that run by default, refreshes runner/provider state before classification, and requires `--apply` to mutate it. Applied reconciliation returns the canonical `ControlPlaneActionAcknowledgement` used by the HTTP action endpoint; the workflow-specific reconcile report remains nested under `result`. If ownership or provider state changes before apply, it reports a no-op. `agent-task active --reconcile` is an explicit fleet operation: it previews every candidate by default and requires `--apply` to reconcile the fleet-wide candidate set. Its `--limit`, `--cursor`, and `--full` selectors are rejected so discovery pagination cannot imply a reconciliation scope.

Paged `agent-task list` and `agent-task active` inspect at most 100 physical lifecycle rows per request and return at most 100 matching rows. A supplied `--limit` above 100 is retained as `requested_limit`; `limit` reports the effective 100-row ceiling. `search_status: "complete"` means the scoped history was exhausted. `"matching_limit_reached"` means the matching output cap was reached. `"search_window_exhausted"` means discovery stopped at its bounded physical window, so an empty `runs` array is incomplete rather than proof of no matches; resume with the returned opaque `next_cursor`.

`agent-task status <run-id>` returns the canonical `ControlPlaneRun` resource (`homeboy/control-plane-run/v1`). Human summaries read that resource directly; they do not reconstruct `homeboy/agent-task-status-scope/v1` or wrap the run under `control_plane_run`. It is a bounded, non-reconciling read; use `agent-task logs <run-id> [--cursor <cursor>]` for canonical `ControlPlaneEventPage`s, `agent-task diagnose <run-id> --full` for detailed failure evidence, and `agent-task reconcile <run-id> --apply` for explicit state repair.

Terminal non-success status uses the current delivery/controller blocker, or, when none exists, the first non-success aggregate outcome's recorded `failure_classification` and first diagnostic. The existing `blocker.message` carries that cause and `blocker.reason` carries its matching next action (timeout/route retry, credential repair, or candidate recovery); no parallel reason record is written. Missing diagnostics are reported explicitly rather than presented as a failure cause. Status reads the record and mirrored aggregate from one authoritative SQLite observation, never from an independently updated `aggregate.json` cache.

A controller publication receipt proving `review_ready` or `draft_published` takes precedence over older terminal or promotion snapshots. Status reports success, the publication state and PR URL (number when no URL was recorded), with no stale failure blocker. Atomic Lab staging completion receipts and resolved-route credential admission are tracked separately in [#16326](https://github.com/Extra-Chill/homeboy/issues/16326).

Each status `artifacts` entry has a control-plane reference `id`, distinct from an
artifact-store byte ID. A producer-artifact `uri` retains the exact task and artifact,
for example `homeboy://agent-task/run/<run-id>/artifacts#task=<task-id>&artifact=<artifact-id>`.
Use that URI for artifact selection and `agent-task artifacts <run-id>` to inspect
the producer's recorded artifact. For byte downloads, obtain the storage-owned
ID from `runs artifacts <run-id>` and pass it to `runs artifact get`. Unknown
fragment fields are omitted from status; artifact selection does not preserve
credential-bearing fragments or arbitrary URLs.

`agent-task status <run-id> --watch` polls the same canonical resource. Every material resource change is emitted immediately as one bounded `homeboy/agent-task-status-watch-event/v2` JSONL event on stderr. Stdout is the bounded `homeboy/agent-task-status-watch/v2` final envelope. Polls default to every `5s` and stop after `30m`; `--interval <duration>` and `--timeout <duration>` require `--watch`. A terminal failure exits nonzero; a timeout returns the latest partial status and exits `124`.

### Resource Behavior

Resource admission follows command capability rather than command names. Bounded
controller metadata reads (`status`, `logs`, metadata-only `artifacts`, `list`,
`active`, `list --latest`, and their loop/controller equivalents) run locally by
default and produce no hot-machine or Lab-routing guidance. This keeps recovery
inspection available when a runner is disconnected, stale, or reports conflicting
readiness. An explicit `--runner` retains runner-resident polling for status,
logs, artifacts, list, active, and other state reads when the durable
state lives there. Provider execution, deterministic gates, explicit evidence or
provider-boundary hydration, and review remain subject to their admission and
routing contracts. Reconciliation is an explicit workload even when it begins
with an inspection.

Machine-readable `cook`, `resume`, `adopt`, and `status` responses default to bounded summaries: stable ids, states, totals, timestamps when present, artifact/evidence references, and actionable failure reasons are retained while nested provider evidence is projected away. Use `cook --full`, `resume --full`, `adopt --full`, or the emitted `full_command`; use the emitted `evidence_command` (with `--task` or `--kind`) to retrieve selected durable evidence.

`agent-task replay-provider-boundary <run-id>` is a focused inspect/replay path for
provider-boundary debugging. It loads saved `executor-input` evidence, projects the
normalized `runtime_task`, provider config, `runtime_component_paths`,
`runtime_env`, artifact declarations, and package descriptor, then persists the
inspection as `provider-boundary-replay` evidence. Use `--task <task-id>` for
multi-task runs.

### Cook/Review

### File-backed verification gates

Use `--verify-file <path>` or `--private-verify-file <path>` for shell programs
with loops, quotes, newlines, or `$variables`. Each option is repeatable. Homeboy
reads the regular UTF-8 file from the controller invocation directory before it
provisions a worktree or dispatches a provider, then executes and persists that
exact snapshot. Changing the source file afterwards cannot change the Cook.

```sh
homeboy agent-task cook --prompt @task.md --to-worktree project@fix-gate \
  --verify-file ./quality-gate.sh \
  --private-verify-file ./private-gate.sh
```

For example, `quality-gate.sh` can contain this program without shell-argument
interpolation:

```sh
for file in src/*.rs; do
  cargo fmt --check -- "$file"
done
```

Relative paths resolve from the directory where the `homeboy` controller command
starts, not from the destination worktree. Quote inline gates with single quotes
when they are concise, such as `--verify 'cargo fmt --check'`; use a file for
complex programs. Files must be readable, non-empty, regular files no larger than
1 MiB. Durable gate policy records `source_kind`, SHA-256 digest, byte size, and
redaction policy. Public file paths are retained as provenance; private file paths
and private program text are redacted from that provenance. Existing inline
`--verify` and `--private-verify` commands remain supported.

Private gate programs, whether inline or file-backed, use the existing trusted
durable Cook recipe boundary so retries and adoption can replay the declared gate.
They are not protected as secret material at rest. Homeboy excludes private gate
text and paths from preflight summaries, reviewer-facing
evidence, and agent feedback according to `--private-gate-reveal`; operators who
need encrypted secret storage should supply a non-secret command that retrieves
credentials at execution time rather than place credentials in a gate program.

A Cook-batch passes private gates to each child Cook as `--private-verify` flags
in its workflow definition, which the daemon job store retains like any other
admitted workflow; the same `--private-gate-reveal` policy applies to each child.

### Provider Execution Budgets

Every agent-task plan serializes one `execution_budget` per task: total provider
executions, same-provider retries, and cross-provider rotations. Configured
rotation is funded by default; a total cap that would remove configured
fallbacks is rejected before dispatch.

Explicit budget objects require `version: 1`. Other versions are rejected on
plan admission and readback; reading a plan never upgrades or rewrites it.
Omitting the budget in an authored plan uses the current schedule default.

```bash
# Use configured rotation and derive its execution allowance.
homeboy agent-task cook --prompt @task.md

# Allow one same-provider remediation retry in addition to rotation.
homeboy agent-task cook --prompt @task.md --max-attempts 2

# Explicitly select and confirm a model pin instead of configured rotation.
homeboy agent-task cook --prompt @task.md --model openai/gpt-5.6-terra \
  --acknowledge-model-override
```

`--attempts N` remains accepted as a legacy alias for `--max-provider-executions N`.
The alias cannot remove configured fallbacks. It cannot be combined with the
canonical total execution flag. Plan and `agent-task status` output show the
resolved defaults before provider execution;
an exhausted run records which budget stopped further execution.

The rotation allowance is derived from the configured
`agent_task.rotation` policy: the rotation chain funds its own reachability, so
`N` effective fallback entries resolve to `N` rotations and `N + 1` total executions
(bounded by the policy's own `max_attempts`). Without a configured rotation the
derived budget is still one execution and no rotation. Same-provider retries
fund gate and required review-form remediation on the same provider identity.
Cook derives their allowance from `--max-attempts`; generic dispatches use
`--max-same-provider-retries`. Their executions are funded in addition to
configured rotation. An explicit model override can pin a run only after
`--acknowledge-model-override` or interactive confirmation, including when the
selected model already belongs to the configured chain. Backend or selector
overrides retain configured rotation.
Cook states the effective rotation on submission, e.g.
`cook: rotation: 2 fallback provider(s), up to 3 provider execution(s)` or
`cook: rotation: disabled (1 provider execution(s))`.

The same submission also states the resolved wall-clock budget of one provider
execution, e.g.
`cook: provider timeout: 1200s per provider execution (override with --timeout-ms)`.
It defaults to 1200000 ms (20 min) and is overridden per run with `--timeout-ms`.
A provider still running at that deadline is cancelled and the attempt is
classified `timeout`, so a task has to be sized against this number rather than
discovered to exceed it.

### Command Policy

An execution budget bounds *how long* a provider may run. A command policy
bounds *what it may run*. On a resource-constrained host the two are not
interchangeable: an agent that spends its whole budget compiling produces zero
edits, and prompt text prohibiting the compile is a request the agent is free to
ignore — it has been, twice, escalating (#11481).

```bash
# Refuse the heavy commands for this cook, and say why.
homeboy agent-task cook --prompt @task.md \
  --deny-command 'cargo test' \
  --deny-command 'cargo build' \
  --command-policy-reason 'this host routes builds to CI; make your edits and push'

# Allow-list mode: only these patterns may run.
homeboy agent-task cook --prompt @task.md \
  --allow-command 'cargo fmt' --allow-command 'git *'
```

Set it once for the whole machine so every cook inherits it without a flag:

```bash
homeboy config set /agent_task/command_policy '{
  "deny": [
    { "pattern": "cargo test", "reason": "this host routes builds to CI" },
    { "pattern": "cargo build" }
  ],
  "reason": "shared host; heavy compiles OOM the box"
}' --json
```

Per-cook flags **extend** the host policy rather than replacing it, so a
host-level refusal cannot be dropped by forgetting a flag.

**Pattern matching.** A pattern is a token sequence matched anywhere in the
command line, so `cargo test` also refuses
`timeout 1200 cargo test -q -p homeboy-agents`. Shell operators are token
separators, so `make deps && cargo build` still matches `cargo build`. `*` globs
within a token (`cargo *`); `**` spans whole tokens (`cargo ** test` matches
`cargo --quiet test`). In deny-list mode an `--allow-command` is an explicit
exemption that beats a deny rule; in allow-list mode a deny rule wins.

**What a refusal looks like.** The agent receives an
`homeboy/agent-tool-result/v1` with status `denied`, an
`agent_tool.command_denied` diagnostic carrying the matched pattern, the
operator's reason, and the alternative to take instead. The denial is also
recorded on the `homeboy/agent-tool-dispatch-evidence/v1` record, so "the agent
tried to compile and was refused" is visible in run evidence afterwards rather
than being invisible or indistinguishable from a command that silently failed.

**Where it is enforced — read this before trusting it.** Homeboy structurally
enforces the policy at the boundary it owns: every request reaching
`homeboy agent-task tool dispatch` (and the in-process control-plane dispatcher)
is evaluated and refused before it runs, including for tools whose policy
execution location is `runner`. The policy travels to the provider inside
`request.policy.tools.commands` and via `HOMEBOY_AGENT_TOOL_POLICY_JSON`, and its
`homeboy/agent-command-policy/v1` schema is advertised in the core contract.

A provider runtime that executes shell commands **inside its own process** never
crosses that boundary. For those runtimes — which is most coding-agent runtimes
today — this is a declaration the runtime is expected to honour, plus a hard
constraint restated in the provider prompt. It is not containment. That gap is
deliberate and stated here rather than papered over; closing it requires the
runtime to route its shell tool through the dispatch command Homeboy already
hands it in `HOMEBOY_AGENT_TOOL_DISPATCH_COMMAND`.

### Supervision policy: how much a provider session may consume

The command policy bounds *what* an agent may run. It cannot see an agent that
stays entirely inside its permitted commands and still grows to nine gigabytes
across forty child processes, and neither can an execution budget that counts
attempts. Homeboy used to learn the cost of a session only after the process
boundary closed.

`agent_task.supervision_policy` is the resource half. It is host config, and it
is opt-in: with nothing declared, a cook behaves exactly as it did before.

```bash
homeboy config set /agent_task/supervision_policy '{
  "budgets": [
    { "metric": "rss_mib", "limit": 6144, "action": "warn" },
    { "metric": "rss_mib", "limit": 10240, "action": "stop",
      "reason": "15Gi box with four agents on it" },
    { "metric": "no_progress_seconds", "limit": 1800, "action": "stop" }
  ],
  "backends": {
    "bench": []
  },
  "reason": "shared host"
}' --json
```

**Metrics** are things any host can observe about any provider, so supervision
stays runtime-neutral:

| Metric | Meaning |
|---|---|
| `elapsed_seconds` | Wall-clock time since provider execution began. |
| `rss_mib` | Resident memory across the provider's whole process tree. Summing double-counts shared pages, so it is an upper bound. |
| `child_processes` | Processes observed under the cook. |
| `no_progress_seconds` | Time since the destination worktree last changed. This is the stall detector: a cook burning memory while writing files is working, one that has written nothing for twenty minutes is not. |

**The ladder** is `warn` → `nudge` → `stop`. Several budgets on one metric are
how you say "tell me at 6 GiB, stop it at 10". A breach is announced once and
again only when it climbs a rung, so the heartbeat channel stays worth reading.
A `stop` terminates the provider's process tree (SIGTERM, then SIGKILL for
survivors) and leaves the controller alive to record why.

`nudge` currently records and surfaces an escalation between `warn` and `stop`;
it does **not** yet inject feedback into the running session, because Homeboy
has no channel into a live provider's reasoning (#7451, #7530).

A `backends` entry **replaces** the global budgets for that backend rather than
extending them — unlike the command policy, where extension is the safe default.
An empty list opts a backend out entirely.

**Unobserved is never zero.** Every metric is optional at the sample, and an
absent metric never breaches a budget. A host with no `ps` has no memory or
process reading; a cook with no destination worktree has no progress reading.
Reporting those as `0` would make every budget fire immediately on exactly the
hosts that can least afford a spurious kill.

**Evidence.** Each heartbeat appends to `cook_resource_timeline` (a rolling
window of samples) and every decision appends to `cook_supervision_events`,
which also records whether an ordered termination actually succeeded. The two
are separate arrays so an hour of quiet samples cannot evict the decision that
explains a stop. `agent-task status` projects both under
`liveness.supervision`.

| Subcommand | Purpose |
|---|---|
| `cook` | Run one workspace task through the patch-artifact handoff workflow. |
| `fanout cook-batch <issue-url>... --repo <repo>` | Cook independent issues as one workflow instance: one child Cook per issue, a bounded wave, and a join gate. |
| `batch-prepare <issue-url>... --repo <repo>` | The `prepare` step of a cook-batch: resolve the base once, name each child, and prepare the shared Lab workspace bundle. |
| `recover <cook-or-attempt-id> [--preflight] [--artifact-id <id>]` | Plan and perform the one recovery step a Cook needs (continue, retry, reconcile, choose an artifact, fork a replacement, replace gates, or publish). `--preflight` reports the step and its read-only admission without mutation; `--timeout-ms`, `--review-form-timeout-ms`, `--backend`, `--selector`, and `--model` re-dispatch a continuation with an explicit provider override. `--authorize-external-proof <TEXT>` with corrected gates (`--verify`/`--verify-file`, optionally `--authorize-interrupted-rerun <TEXT>`) or candidate-bound proof (`--promotion <SPEC>`) replaces the gates of an applied candidate that failed them; `--review-form <SPEC>` with `--review-form-tool`, `--review-form-model`, and `--review-form-author`, plus `--review-override` and `--component`, feed publication. |
| `review <run-id>` | Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints. |
| `promote <source>` | Promote a completed generic patch artifact into a managed worktree. |
| `adopt <run-or-cook-id> --candidate-ref <sha>` | Adopt an immutable commit candidate through the recorded cook gates and finalization policy. |
| `finalize-pr` | Finalize a green manual-finalization candidate. A stopped Cook publishes through `recover`. |
| `accept <run-id> --verdict accepted\|rejected --token <token> --evidence-ref <ref>` | Record an independently verified acceptance decision for an applied candidate. |
| `gate-feedback` | Convert deterministic gate results into a cook retry or stop decision. |

`adopt` accepts only immutable commits from the recorded cook source workspace.
Use `promote` for patch-artifact candidates; it remains the controller-owned
patch-artifact ingestion and gate path.

If a first Cook admission finds an already-implemented dirty candidate, it
fails before provider execution and returns the durable Cook/run plus this
recovery sequence: commit the candidate in the named worktree, run `agent-task
review <run-id>`, then run the emitted `agent-task adopt <cook-id>
--candidate-ref HEAD --model <model>` command. Adoption verifies the immutable
commit through the recorded gates and finalization policy; it does not create or
apply provider patch content.

`finalize-pr` is the core-owned publication boundary for external runtimes. Its
`homeboy/agent-task-pr-finalization/v1` report keeps the legacy top-level
`status`, `pr_action`, `pr_number`, and `pr_url` fields, and also emits explicit
structured contracts:

- `publication_intent`: `homeboy/agent-task-publication-intent/v1`, describing
  the requested review publication target, source refs, artifact refs, changed
  files, and Homeboy proof.
- `publication_proof`: `homeboy/agent-task-publication-proof/v1`, describing the
  adapter action/ref that satisfied the intent.
- `finalization_outcome`: `homeboy/agent-task-pr-finalization-outcome/v1`, tying
  the terminal finalization status to publication status/action, branch refs, PR
  refs, changed files, and commit/push/publish booleans.

Extensions and lab runners should delegate PR creation/update and proof assembly
to this command instead of recreating GitHub publication logic locally.

#### Independent Acceptance

`cook --require-acceptance --acceptance-authority <authority> --acceptance-policy <policy>`
adds an independent acceptance boundary. Homeboy creates its durable `pending`
record only after an applied promotion has recorded the candidate and verified
base, following successful deterministic gates. Provider output and review forms
remain evidence but cannot satisfy this decision.

The runtime must register an `AgentTaskAcceptanceVerifier`. Its attestation must
match the declared authority and policy and include an actor, timestamp, provider
reference, and opaque verifier/configuration provenance. The CLI token is passed
only to that verifier and is never persisted. `accept` requires at least one
evidence reference. Replaying an identical decision is idempotent. A changed
candidate or verified base archives the old decision and creates a new pending
record. A rejection preserves its evidence and permits one repair continuation.
Finalization reports and persisted Cook finalization evidence project the same
authoritative acceptance record.

A stopped Cook publishes through its `publish` recovery step. `recover`
resolves a Cook ID to the attempt `agent-task status` reports, then hydrates the
run, worktree, base snapshot, candidate, changed files, gates, source
references, model/tool disclosure, and accepted review form from the durable
Cook recipe and applied promotion. It fails closed when either record is absent,
malformed, belongs to another attempt, or no longer matches the candidate.
`--preflight` performs the same hydration and complete validation without
commit, push, or pull-request mutation:

```bash
homeboy agent-task recover cook-9750 --preflight
homeboy agent-task recover cook-9750 \
  --review-override 'compatibility=No public compatibility impact.@reviewed issue #9750'
```

An applied candidate that failed its deterministic gates takes the
`replace_gates` step. Corrected gates run against the already-applied candidate
and record replacement proof; `--promotion` attaches externally produced
candidate-bound proof instead. Both require an explicit authorization, and the
next `recover` publishes:

```bash
homeboy agent-task recover cook-9750 --preflight \
  --authorize-external-proof 'approved corrected gate evidence' \
  --verify 'cargo test exact::replacement'
homeboy agent-task recover cook-9750 \
  --authorize-external-proof 'approved corrected gate evidence' \
  --verify 'cargo test exact::replacement'
```

Manual finalization first reuses every authoritative green gate from the same
run's durable promotion receipt when its base, target worktree, candidate tree
and digest, and exact commands remain bound to the current candidate. The report
labels these gates as `verified_inherited`; any stale, red, or malformed receipt
requires fresh verification instead of silently accepting it.

When multiple registered components share the candidate repository, pass
`--component <component-id>` with `--manual-finalization` to select the component
whose review profile applies. Homeboy verifies that the explicit component is
registered and matches the candidate path. Cook publication through `recover`
derives component identity from its durable record; its `--component` is only a
fallback for a record that names none.

Fresh manual finalization can execute repeated candidate-bound verification
commands and derive separate gate results plus reviewer evidence from them:

```bash
homeboy agent-task finalize-pr --manual-finalization \
  --verify 'homeboy review test homeboy' ...
```

Each `--verify` runs against the same clean committed candidate in an isolated
detached checkout, before Homeboy reserves a manual-finalization lifecycle
record. Homeboy hydrates declared dependencies there, retains structured setup
provenance, and derives one durable gate result and reviewer test step per
command. Existing `--gate-result`, `--targeted-check-run`, and `--test-step` remain
the explicit external-proof import path for verification that already occurred;
they cannot be combined with `--verify`.

Strict repeated-value shapes are `--gate-result NAME=STATUS[:DETAIL]`,
`--test-step COMMAND=>EXPECTED`, `--changed-public-contract ID=>SUMMARY`, and
`--review-override TARGET=VALUE@PROVENANCE`. Issue references accept `#NUMBER`,
`OWNER/REPO#NUMBER`, or an HTTPS `github.com/OWNER/REPO/issues/NUMBER` URL.
Declaring a changed public contract requires one complete evidence bundle:

```bash
homeboy agent-task finalize-pr ... \
  --changed-public-contract 'cli.finalize-pr=>Adds durable recovery input' \
  --compatibility-impact 'Additive CLI input; existing manual mode remains supported.' \
  --external-consumer-impact 'External runtimes may replace reconstructed commands.' \
  --external-usage-status completed \
  --external-usage-source 'repository call-site search and CLI integration tests' \
  --external-usage-limitations 'No consumers outside indexed repositories were inspected.' \
  --external-usage-url 'https://github.com/Extra-Chill/homeboy/issues/9750'
```

#### Review Dossier Migration

`finalize-pr` accepts typed reviewer fields without breaking existing invocations.
Explicit `--summary`, repeated `--what-changed`, `--test-step COMMAND=>EXPECTED`,
and `--compatibility` take precedence. When omitted, Homeboy derives the dossier
from the legacy fields: `--title` supplies Summary, `--attempt-summary` supplies
What changed, each `--targeted-check-run` supplies a `COMMAND=>passes` test step,
and `--manual-reviewer-check` supplies a reviewer command with its expected
observation. Compatibility becomes an explicit legacy disclosure. `--closes` and
`--relates-to` are the only issue-relationship inputs; source URLs never imply
closure. Use `--review-override TARGET=VALUE@PROVENANCE` only for `summary`,
`what_changed`, or `compatibility`; override provenance remains in JSON output.
Normal finalization hydrates `--run-id`; `--manual-finalization` is the explicit
migration mode for a manually prepared candidate with no durable run.

#### Single-Issue Cook

Use `agent-task cook` for one issue or one workspace task. It is the non-fanout
path: create or reuse one task worktree, dispatch one provider run, promote the
patch artifact into `--to-worktree`, run deterministic gates, retry red gates up
to `--max-attempts`, and finalize the green result into a PR unless
`--no-finalize` is set.

```bash
homeboy worktree create homeboy \
  --branch fix/issue-6453 \
  --from origin/main \
  --task-url https://github.com/Extra-Chill/homeboy/issues/6453

homeboy agent-task cook \
  --repo homeboy \
  --task-url https://github.com/Extra-Chill/homeboy/issues/6453 \
  --workspace homeboy@fix-issue-6453 \
  --to-worktree homeboy@fix-issue-6453 \
  --verify 'homeboy review test homeboy' \
  --backend sandbox \
  --selector wordpress.sandbox-agent-task-executor \
  --prompt @task.txt
```

For a caller-owned existing checkout, `cook` already accepts its path directly
with `--cwd` (also `--dir`); it does not re-materialize that checkout. Use
`--prompt @task.md` (also `--prompt-file @task.md`) to snapshot a prompt file and
`--model` together with `--acknowledge-model-override` to confirm a pinned route.
Omit the model override to use configured rotation. The ordinary
Cook durable run is then inspectable with `agent-task status`, `watch`, and
`logs`:

```bash
homeboy agent-task cook --dir /path/to/existing-worktree \
  --prompt-file @task.md --model openai/gpt-5.6-terra \
  --acknowledge-model-override --no-finalize
```

Cook snapshots the prompt once for the durable plan; provider rotations retry
that same task against the same destination checkout.

**Completion evidence.** An accepted durable handoff (including a Lab job) means
an owner will carry the Cook forward, not that the change has been delivered.
Neither a zero submission exit code nor provider success proves delivery. For a
patch-producing Cook that requests a draft PR, follow the evidence chain:

- **Provider candidate:** retain the patch/commit identity and its attempt/run
  reference. Provider checks and `outputs.review_form` are supporting evidence,
  not authoritative gate results or publication proof.
- **Controller-owned passing gates:** inspect the durable promotion gate results
  bound to that candidate, verified base, and target worktree. Every declared
  deterministic gate must pass; deferred gates are not passing gates.
- **Authoritative destination:** confirm the promotion receipt records application
  to `--to-worktree`, or the canonical `--cwd` destination when no handle is given.
  Changes present only in the isolated provider/runner checkout are not delivery.
- **Final draft PR receipt:** normal Cook owns commit, push, and PR publication.
  Require durable finalization evidence with `status: draft_published` and a PR
  number or URL, then confirm the draft and its head in GitHub. The nearby
  `publication_proof` and `finalization_outcome` contracts expose those effects;
  `homeboy/agent-task-cook-completion/v1` reports `pr_finalized`, rather than merely
  `candidate_produced` or `candidate_awaiting_finalization`.

For the documentation delivery tracker [Extra-Chill/homeboy#15017](https://github.com/Extra-Chill/homeboy/issues/15017),
reuse the existing-checkout form with normal draft finalization:

```bash
homeboy agent-task cook --cwd /path/to/task-worktree --placement lab \
  --task-url https://github.com/Extra-Chill/homeboy/issues/15017 \
  --prompt @task.md --verify 'git diff --check' --draft-pr
```

Use the existing `status`, `logs`, and emitted evidence commands to inspect the
run. `--no-finalize` intentionally stops short of PR delivery. For an untouched
delivery acceptance, the supervisor observes raw worker artifacts, read-only
stores, and GitHub before recording its first verdict; it does not repair,
promote, or publish on Cook's behalf.

Use `agent-task fanout cook-batch` only when there are multiple independent
issues that should each get its own worktree, branch, gates, and PR.

#### Multi-Issue Cook Batch

`agent-task fanout cook-batch` is shorthand for one `homeboy/workflow/v1`
instance (see [`homeboy workflow`](workflow.md)). It compiles the batch into two
steps and admits that workflow on the local daemon:

- **`prepare`** runs `homeboy agent-task batch-prepare`. It resolves the base
  branch and its commit once, names each child's branch
  (`<prefix>/issue-<number>-<repo>`) and worktree, renders each child's prompt
  from `--prompt-template` (the issue URL when no template is given), and, when
  the children are placed on Lab, transfers the base commit to that runner
  once. Every child at that commit then installs it from the runner's git object
  cache instead of shipping its own bundle (#15677); the step output reports
  that transfer as `workspace_bundle`.
- **`cook`** fans out one ordinary `agent-task cook` per child
  (`foreach` over the prepared children), at most `--max-concurrency` at a time.
  Each child creates its worktree, runs its deterministic gates, and finalizes
  its own PR exactly like a single Cook. `--join` decides the batch outcome:
  `all` (default) needs every child to succeed, `any` one, and a number `N` at
  least `N`.

Every child needs a deterministic gate from `--verify`, `--verify-file`,
`--private-verify`, or `--private-verify-file`.

Cook and cook-batch share the same provider, model, evidence, attempt and gate
options. Batch forwards explicitly supplied options unchanged to each child;
relative verification-file paths are anchored at the operator's working directory.
`--provider-profile NAME` is also available on a single `agent-task cook`:
Cook resolves the installed profile's backend, selector, model and provider-config
defaults, with explicit flags taking precedence. Batch only forwards the profile
name; each child resolves it. `--max-attempts` defaults to 3 for both commands.

```bash
homeboy --placement lab agent-task fanout cook-batch \
  --repo homeboy \
  --verify 'homeboy review test homeboy' \
  --max-concurrency 2 --join 2 \
  https://github.com/Extra-Chill/homeboy/issues/6453 \
  https://github.com/Extra-Chill/homeboy/issues/6454 \
  https://github.com/Extra-Chill/homeboy/issues/6455 \
  --run-plan
```

Without `--run-plan` (or with `--preview`) the command validates the batch and
prints the compiled workflow and its instance id without admitting anything or
performing ambient notification discovery.
With `--run-plan` it admits the instance and returns its status plus
`status_command` and `cancel_command`. The instance id is derived from the
compiled definition, so re-running the same command reports the same instance;
`--fanout-id NAME` runs the same batch as a separate instance.

Observe and control the batch through the workflow verbs:

- `homeboy workflow status <instance> [--watch]` shows each child's phase,
  outcome, step job, Cook run id, and the live state of its running Cook.
- `homeboy workflow cancel <instance>` cancels the instance and every child step
  it is running; no further child starts.

`--max-duration SECONDS` becomes the workflow's `budget.deadline`: on expiry the
instance ends `budget_exhausted` and cancels running children. `--secret-env`
names become workflow secret references that the daemon resolves when each
child spawns.

## Lab Guardrails

Use global `--placement lab` with long-running or
patch-producing `agent-task cook` waves that must not execute
provider processes on the controller. If Lab routing cannot select or prepare a
runner, Homeboy fails before local execution instead of falling back.

`cook` uses *split placement*: the coordinator always stays on the
controller — it owns target resolution, the durable record, artifact ingestion,
promotion, gates, and finalization — while each provider attempt is dispatched
to the selected Lab runner. `fanout cook-batch` passes the same placement to
each child Cook. `--placement lab` and `--runner <runner-id>` select
the runner for those attempts; neither offloads the coordinator itself. When no
Lab runner is ready, these commands report the runner readiness verdict and its
remediation commands rather than claiming Lab placement is unsupported. Use
`--placement lab-or-local` to authorize controller execution as a fallback.

Concurrent single-cook submissions do not race: controller-generation admission
is a FIFO queue, so parallel cooks wait their turn and re-running an identical
command after a contended attempt queues rather than collides. Admission
diagnostics always name the current holder (PID, verified liveness, and the
owning admission request) or the actionable lock state.

Eligible top-level durable work returns after verified durable controller
handoff by default, with run identity and inspection/watch commands. Acceptance
is not terminal success. Global `--wait` observes until terminal completion and
returns the terminal exit status. Placement and durable ownership are independent
of this wait policy, for both local and Lab execution.

Before a runner job is accepted, deferred source construction remains
controller-owned. Status reports `source_materialization`, with checkpoint
heartbeats during packaging and Git transport, rather than attributing a planned
runner selection to an execution that does not exist yet. Construction honors
the plan's absolute lifecycle deadline; without one it has a 20-minute fallback
budget, separate from the longer budget for observing an accepted runner job.
Cancelling the exact attempt interrupts its owned source processes and fences
handoff. A construction deadline records a typed pre-execution failure so the
attempt does not remain queued waiting for nonexistent runner evidence.

Submit a Lab Cook and return after handoff:

```bash
homeboy --runner homeboy-lab agent-task cook \
  --to-worktree homeboy@fix-issue-6453 --verify 'homeboy review test homeboy' --prompt @task.txt
```

Wait for the completed Cook when the caller owns a synchronous workflow:

```bash
homeboy --wait --runner homeboy-lab agent-task cook \
  --to-worktree homeboy@fix-issue-6453 --verify 'homeboy review test homeboy' --prompt @task.txt
```

Synchronous orchestration should pass `--wait`. Runner-owned attempts and
controller re-executions complete their assigned work without recursively
detaching. Ordinary synchronous commands, including previews and planning-only
fanout commands, retain their synchronous behavior. Reconnect and retrieve durable
state with:

```bash
homeboy agent-task status <run-id>
homeboy runs watch <run-id>
homeboy agent-task evidence <run-id> --full
```

Detaching after handoff is the default. Use `--wait` to observe until terminal
completion; the old `--detach-after-handoff` flag is no longer accepted.

#### Provider activity

A locally executed Cook samples what the provider is actually doing and carries
it on both the heartbeat line and `agent-task status`, under
`liveness.provider_activity`:

| Field | Meaning |
|---|---|
| `files_changed` | Uncommitted files in the destination worktree, untracked included. `0` after several minutes is the clearest sign a cook is not producing work. Homeboy keeps all of its run state outside the worktree, so this counts provider edits only. |
| `commits_written` | Commits made in that worktree since the provider started. A provider that commits leaves a clean tree, so this is what distinguishes "finished" from "did nothing". |
| `command` / `command_elapsed_seconds` | The longest-running command the *provider* is running, and its age — `cargo test -p homeboy-agents`, six minutes in. Homeboy's own processes are never reported here: not the cook controller, not the nested controller a `--placement local` cook re-enters through, not a gate, and not a `homeboy …` call the agent made as a tool. |
| `command_unavailable` | Present instead of `command` when the process tree was sampled and no provider process was found. Homeboy states the absence rather than naming its own command, which is what made this signal misleading in #11598. |
| `elapsed_seconds` | Time since provider execution began. |
| `observed_at` | When the sample was taken. A retained sample keeps its own observation time, so a stale reading is visible as stale. |

Each field is absent when it could not be sampled; an unsampled cook reports no
activity rather than a zero that would read as a measurement. Lab-offloaded
attempts run the provider on the runner and therefore report no local activity.

`--placement local` is safe only when local execution on this
controller is intentional. For agent-task waves with concurrency greater than 1
or multiple tasks, Homeboy prints `HOMEBOY_LOCAL_FANOUT_WARNING` before provider
processes start. Compact `agent-task status` includes `execution_location` as
`local` or `runner:<id>`.

### Provider

| Subcommand | Purpose |
|---|---|
| `providers` | List extension-declared executor providers and optional secret/backend readiness. |
| `contract` | Export Homeboy's machine-readable agent-task core contract metadata. |
| `auth` | Configure and inspect provider authentication secrets. |

#### Coding backend: Pi

Pi is Cook's coding backend. When no `--backend` is passed and no component,
extension, or Homeboy config policy names `agent_task.default_backend`, Cook
selects the built-in `pi` backend if the provider catalog declares it; the
backend selection reports `source: builtin`. Pi routes bill through the Roadie
credential pool with a per-runner client key, so the reference rotation uses
pool-backed `roadie/<rotation>` models:

```bash
homeboy config set /agent_task/default_backend pi
homeboy config set /agent_task/rotation \
  '{"entries":[{"backend":"pi","model":"roadie/anthropic-claude-sonnet-5-5"}]}' --json
homeboy agent-task providers --validate-readiness
```

Cook ships no second coding runtime (#15932). A `--backend`,
`agent_task.default_backend`, or `agent_task.rotation` entry that names a
backend no installed provider declares fails provider resolution instead of
routing; the error names the missing backend and tells the operator to replace
it with `pi`. `agent-task providers --set-default` puts `pi` first when it is
ready.

`agent-task contract --format=json` includes `agent_runtime_handshake`, the
Homeboy-owned extension-facing protocol registry for runtime capability
manifests, readiness checks, resolved execution contracts, materialization plans,
secret env plans, and result/artifact declarations. The registry is generic by
design: extensions provide runtime-specific declarations and results, while
Homeboy owns schema ids, required wire fields, redaction boundaries, and resolved
handoff vocabulary. See
[`docs/architecture/agent-runtime-contract-handshake.md`](../architecture/agent-runtime-contract-handshake.md).

### Prompt Store

`agent-task prompts` stores markdown prompt files under Homeboy's data directory,
not under the current repo/worktree. Save prompt content with inline text,
`@file`, or `-` for stdin, then reference it from `cook` or workflow `cook`
steps with `prompt:<id>` anywhere a prompt string is accepted.

```bash
homeboy agent-task prompts save issue-123 --input @prompt.md
homeboy agent-task prompts list
homeboy agent-task cook --repo homeboy --prompt prompt:issue-123
```

Cook accepts one source through `--prompt`: `--prompt @file`, `--prompt -`, inline
prompt text, and `prompt:<id>` references all use the same resolution behavior.
Use `agent-task fanout cook-batch` for independent task waves.

Each Cook attempt's framing beyond your prompt (first attempt, gate fix, no-change
retry, review form) comes from `agent_task.prompt_templates`; see
[Agent-task prompt templates](../reference/prompt-templates.md).

### Workflows

Pre-defined multi-step and repeating agent work (a command gate that launches a Cook, then re-checks) is a `homeboy/workflow/v1` definition run by [`homeboy workflow`](workflow.md). `agent-task cook` is one step kind inside it.

## Internal Bridge

`agent-task tool` is a hidden provider-runtime bridge. It remains parseable for
runtime adapters that dispatch tool requests through Homeboy, but it is omitted
from `homeboy agent-task --help` and the visible command surface because it is
not an operator-facing workflow.

## Cook

`agent-task cook` is the one-shot end-to-end PR workflow. It dispatches an agent,
promotes the selected patch into the target worktree, runs deterministic gates,
retries red gates within the configured budget, then commits, pushes, and opens or
updates a PR.

Use `--draft-pr` to retain that verified commit/push/PR workflow while creating a
draft PR. A retry updates an existing PR but preserves its observed draft or ready
state; after acceptance evidence is attached, use `homeboy git pr ready <component>
--number <number>` for the explicit ready-for-review transition.

`--goal` is one-line Cook framing metadata, recorded on the durable plan and its
single provider task. Pair explicit work with `--prompt`; `--goal` never creates
an additional task. A goal without explicit work supplies the one provider task.

```bash
homeboy agent-task cook \
  --repo sample-plugin \
  --cwd /path/to/worktree \
  --to-worktree sample-plugin@fix-issue \
  --provider-config @provider-config.json \
  --client-context @client-context.json \
  --verify "npm test" \
  --prompt @task.txt
```

Homeboy core treats `--client-context` as an optional opaque JSON object. Client
adapters may include whatever correlation data they need to reconcile their own
notifications or UI state, but Homeboy does not interpret transport-specific
identifiers in core lifecycle state. Provider-specific execution settings belong
in `--provider-config`; durable lifecycle commands remain headless and can be
inspected later with `agent-task status`, `agent-task logs`, or `agent-task review`.

To verify that an installed notification resolver receives the current bridge
context without launching provider work, run the normal read-only Cook preview:

```bash
homeboy agent-task cook --preview --repo <repo> --task-url <issue-url> --prompt "verify notification resolver context" --no-finalize
```

Inspect `resolved.notification_resolution`: `resolver` proves the context selected
a route, while `route_less`, `resolver_transport`, and `missing_context` provide
safe remediation without exposing the opaque destination. Detached route-less
submissions emit the same diagnosis before durable handoff, and `agent-task
status <cook-id>` preserves it with the latest notification delivery outcome.

`--backend` selects the generic executor backend, `--dispatch-provider-id` (also
accepted as `--selector`) selects a specific provider id for that backend, and
`--model` is only a provider-owned model override. Provider ids come from
`homeboy agent-task providers`; they are not model names or provider families.
The same `--model` value records adopted-candidate and manual-finalization AI
provenance. `finalize-pr` still accepts `--ai-model` as a deprecated alias
because external publication tooling calls it; `adopt` accepts only `--model`.

## Gate Command Contracts

Before Cook provisions a worktree or dispatches a provider, Homeboy validates
each exact simple `homeboy ...` deterministic gate against the installed Clap
contract, including global flags and required arguments. Compound shell gates
remain external and unvalidated during admission. Gate validation is recorded as
`gate_contract_validation` in the Cook plan; gate execution remains separate
promotion evidence.

Repository `homeboy.json` entries under `scripts.lint` and `scripts.test` are
capability identities, not top-level CLI verbs. Use their canonical gate forms:

```sh
homeboy review lint --path .
homeboy review test --path .
```

Other shell gates remain unvalidated during admission. Declare a `--gate-toolchain`
when Homeboy must probe an external executable before provider dispatch.
## Headless Fleet-Cooking Review

The authoritative non-chat workflow is the durable `agent-task` lifecycle. Chat
clients, Discord threads, GitHub Actions, cron, and terminal operators can all
submit the same run id, inspect it later, and promote selected artifacts without
depending on transport-local state.

```bash
run_id="homeboy-3357-$(date +%s)"

homeboy agent-task cook \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --to-worktree homeboy@fix-issue-3357-agent-task-non-chat-flow \
  --task-url https://github.com/Extra-Chill/homeboy/issues/3357 \
  --concurrency 4 \
  --attempts 2 \
  --verify "homeboy review test homeboy" \
  --run-id "$run_id" \
  --prompt @task.txt

# One review envelope contains lifecycle state, logs, artifacts, aggregate
# reconciliation, promotion candidates, and next actions.
homeboy agent-task review "$run_id" \
  --to-worktree homeboy@fix-issue-3357-agent-task-non-chat-flow
```

`agent-task review` returns `homeboy/agent-task-review/v1` with:

- `record`: the durable run record from `status`.
- `logs`: scheduler events from queued or completed lifecycle state.
- `artifacts`: artifacts and evidence refs from the completed aggregate.
- `aggregate_review`: apply/retry/issue-report/review candidate reconciliation.
- `promotion_candidates`: generated `homeboy agent-task promote <run-id>` command
  arrays for apply candidates, completed with `--to-worktree` when supplied.
- `transport.chat_state_required: false`, making Homeboy the source of truth.

This is the terminal/daemon-owned review surface for fleet cooking. Kimaki or any
other chat UI should submit, poll, render, and call these commands rather than
owning scheduling, state, artifacts, reconciliation, or promotion.

## Provider Capacity

`agent-task capacity` reports live plan capacity for every configured route:
`agent_task.default_backend` plus each `agent_task.rotation` entry. A bare
default route is skipped when the rotation already names models on its
backend. Narrow the walk with `--backend`, `--model`, or `--selector`.

```bash
homeboy agent-task capacity
```

Desktop and other local daemon clients can read the same capacity service over
`GET /v1/control-plane/capacity` (optional `backend`, `model`, and `selector`
query parameters). The daemon caches snapshots for 15 seconds; repeated reads
return `snapshot_age_seconds`, while a failed refresh returns the last snapshot
with `stale: true` rather than presenting it as current.

Each route's readiness invocation runs with `mode: "capacity"`. Runtimes that
support this mode answer from provider usage lookups only, so the command
spends no inference. For runtimes that don't support it,
the command reports only what the capacity result contains.

- Route probes are bounded. A route that fails is
  reported as `unknown` with its diagnostic, and the other routes still
  return.
- Routes that report the same `capacity.scope` share one account pool, so they
  appear as a single entry that lists all of their models.
- The result schema is `homeboy/agent-task-capacity/v1`:
  - `generated_at`
  - `stale` and `snapshot_age_seconds` on the shared CLI/HTTP service projection.
  - `routes[]`, each with `backend`, `selector`, `models[]`, `scope`, and
    `capacity`. `capacity` carries `state`, `remaining`, `limit`, `unit`,
    `reset_at`, and per-account `accounts[]`.
  - `next_reset`: the soonest reset among exhausted accounts.
- The summary prints one block per pool and one line per account:

```
anthropic (4 plans): 75% remaining
  chris.huber@a8c.com    available  75%  resets 2026-09-25T00:40:00Z
  chubes@chubes.net      exhausted  resets 2026-09-29T16:00:00Z
openai (2 plans): exhausted until 2026-09-27T15:26:10Z
xai/grok-4.7: capacity not published
```

## Provider Contracts

`agent-task providers` returns `capability_contract` with Homeboy-owned schema ids
for executor provider manifests, requests, and outcomes. Extensions should read
that metadata, or import the matching `homeboy::core::agent_tasks::provider`
constants, instead of copying schema strings into downstream code. Provider
manifests may omit `schema`, `request_schema`, and `outcome_schema`; Homeboy
defaults them to the current core contract ids.

### Provider discovery is scope-sensitive

Controller and runner carry different extensions, runtime defaults, secrets, and
provider readiness, so a provider catalog only means something alongside *where*
it was read. `agent-task providers` therefore reports an additive
`observed_scope` object naming the observed location (`controller` or `lab` plus
`runner_id`), the Homeboy build identity, the extension/runtime sources behind
the catalog, and the observation timestamp.

Unscoped `homeboy agent-task providers` is always a controller-local read: it is
never relocated to a Lab runner because the controller is under load, since that
would silently answer a different question. Ask for a runner's catalog
explicitly with `homeboy agent-task providers --runner <runner-id>` (or
`--placement lab`). The explicit runner probe is runner-resident — it
materializes no workload workspace — and its dispatch is bounded
(`HOMEBOY_LAB_PROVIDER_DISCOVERY_DISPATCH_TIMEOUT_SECS`, default 90s) so it
returns a labelled timeout with durable runner/job identity instead of hanging.

Per-runtime revision probes (`git rev-parse HEAD`) are bounded too. A wedged
runtime checkout yields a partial catalog plus an
`agent_runtime_manifest.revision_probe_timeout` diagnostic naming the runtime,
rather than an unbounded discovery.

Use `homeboy agent-task providers --backend <backend> --validate-readiness` to
fail fast when the selected backend is registered but its declared runner
readiness is not usable in the current environment. Lab offload runs this check
on the selected runner before `agent-task cook` dispatches work internally, so a
missing provider executable/config blocks the run before a multi-cell task wave
is queued.

Run `homeboy agent-task providers --validate-readiness` with **no** `--backend`
to discover which backend to pass. Without `--backend` the command validates
every declared backend and reports each verdict in
`readiness_validation.backends[]` — one entry per backend, carrying the same
fields as the single-backend `readiness_validation` object plus the `backend` it
describes, with a failed backend's detail in its `reason`. The usable values are
listed directly in `readiness_validation.ready_backends`. A backend that fails
readiness is reported, not propagated, so this sweep answers `agent-task cook`'s
missing-`--backend` error instead of failing with the same precondition (#12569).
A supplied `--backend` still fails fast: that query names one backend and has no
fuller picture to report.

Provider queries pinned to a runner must use the daemon-backed runner command,
which does not require durable workspace staging:

```sh
homeboy runner exec <runner-id> -- homeboy agent-task providers --backend <backend> --validate-readiness
```

The returned provider catalog is observed on that runner. A disconnected or
stale runner fails runner-exec admission; controller-local readiness is never
used as a substitute.

`dispatchability.checks.credentials.status` distinguishes `missing`, `present`,
`unverified`, `verified`, `unusable`, and `not_required`. Readable credential
material is only `present`: a revoked or expired provider-owned credential stays
on disk after revocation. A provider that declares the `provider_owned_auth`
capability is dispatchable only after its bounded `readiness_invocation` runs
and passes, producing `verified`. A failed probe reports `unusable` and preserves
the provider-owned reason and remediation; an absent probe reports `unverified`
and tells the operator to update the provider or select a verified backend.
Cook performs this live admission against the final provider configuration
before scheduler reservation can consume provider-execution budget (#13628).

## Repo-Local Gate Tasks

Use `execution_kind: repo_local_gate` for deterministic, repo-local gate
evaluation that should run inside the task workspace without selecting an AI
runtime. The gate executor runs direct `argv` or a relative `script` path without
a shell, rejects paths that escape `workspace.root`, materializes JSON inputs as
`<INPUT_KEY>_PATH` files, materializes declared JSON outputs as
`<OUTPUT_KEY>_PATH` files, and returns `homeboy/agent-task-outcome/v1` with
typed artifacts and `outputs.<key>` payloads.

Minimal task config:

```json
{
  "execution_kind": "repo_local_gate",
  "script": ".github/scripts/evaluate-publish-gate.mjs",
  "inputs": {
    "import_validation_result": "{{outputs.import_validation_result}}",
    "visual_parity_artifact": "{{outputs.visual_parity_artifact}}"
  },
  "artifact_outputs": {
    "static_site_publish_gate": {
      "schema": "example/StaticSitePublishGate/v1",
      "type": "StaticSitePublishGate"
    }
  }
}
```

Gate scripts should read JSON from the generated input path env vars and write
JSON to the generated output path env vars. For example,
`IMPORT_VALIDATION_RESULT_PATH`, `VISUAL_PARITY_ARTIFACT_PATH`, and
`STATIC_SITE_PUBLISH_GATE_PATH`. `node_script` is accepted only as a legacy alias
for existing plans; new plans should use `repo_local_gate` so the contract stays
portable beyond Node.

`agent-task contract --format=json` returns `homeboy/agent-task-core-contract/v1`,
the machine-readable Homeboy-owned contract export for downstream integrations.
It includes schema ids, provider capability metadata, status/failure enum values,
and default redaction policy metadata without naming or depending on any specific
executor provider.

`agent-task status`, `logs`, and `artifacts` are read-only durable lifecycle
inspection commands. They do not start workloads and are not gated by warm-machine
resource policy; use `homeboy runner exec <runner> -- homeboy agent-task status
<run-id>` when the durable state lives on a Lab runner host. `agent-task review`
hydrates aggregate evidence and remains resource-managed.

### Status never requires the runner

`agent-task status` assembles `ControlPlaneRun` from durable controller state and never contacts or reconciles a runner. This keeps inspection available while a Lab runner is unavailable. Use `agent-task reconcile` when the caller explicitly wants to refresh authoritative provider state.

## Deterministic Smoke Gate

Issue #3392 is covered by a no-secret fixture plan at
`tests/fixtures/agent_task_smoke_plan.json`. It exercises the operator path
without provider credentials, chat state, or long-running external services.

The plan selects `"backend": "fixture"`. That backend is a **test double, not an
agent runtime**: it has no provider manifest under `_extensions/agent-runtimes/`
and is compiled only behind the `test-support` feature, so a released `homeboy`
binary rejects it with the normal "no selectable provider found for backend"
diagnostic. Run the gate from a source checkout with the feature enabled:

```bash
alias homeboy='cargo run --features test-support --quiet --'
```

Run it from a disposable Homeboy worktree:

```bash
run_id="agent-task-smoke-$(date +%s)"
target_worktree="homeboy@fix-3392-agent-task-smoke"

homeboy agent-task run-plan \
  --plan @tests/fixtures/agent_task_smoke_plan.json \
  --record-run-id "$run_id"

homeboy agent-task status "$run_id"
homeboy agent-task logs "$run_id"
homeboy agent-task artifacts "$run_id"
homeboy agent-task review "$run_id" \
  --to-worktree "$target_worktree"
homeboy agent-task promote "$run_id" \
  --to-worktree "$target_worktree" \
  --dry-run
```

The gate passes when:

- `run-plan` exits successfully and writes the aggregate lifecycle record under the recorded run id.
- `status` shows `state: "succeeded"` and `logs` show the fixture cell.
- `artifacts` lists a patch artifact, an agent result artifact, and a transcript evidence ref.
- `review` returns a `homeboy/agent-task-review/v1` envelope with `transport.chat_state_required: false`, aggregate reconciliation, and promotion candidates.
- `promote <run-id> --dry-run` resolves the aggregate from the durable run id and reports the selected non-empty patch plus changed files without requiring the operator to look up `aggregate_path` manually.

When `promote <run-id>` applies a patch, Homeboy records the promotion report in
the run's versioned `metadata.cook_delivery` record. That report includes the source run id, source task id,
patch artifact id/path, target worktree, discovered target branch/head when
available, changed files, and an operator notification. `agent-task status
<run-id>` surfaces the latest promotion so callers can tell whether promotion
completed or is blocked without spelunking Lab artifact paths.

When promotion runs without `--dry-run`, each `--verify <command>` is treated as
a visible deterministic gate in the promoted worktree. Gates run in declaration
order and, by default, a failure records every remaining gate as `skipped` with
its blocking gate ID without invoking its command; use
`--gate-execution-policy continue-all` for independent or exhaustive suites.
Promotion reports gate results as `deterministic_gates[]` using
`homeboy/agent-task-gate-report/v3`. Failed visible gates set promotion
`status: "gate_failed"`, exit nonzero, and include
`failure_evidence.agent_feedback` plus stdout/stderr tails so the next cook
agent task can receive exact failure context instead of a generic shell error.

Gate stdout and stderr retain only their final 64 KiB. Each report's `capture`
contains per-stream `bytes_seen`, `bytes_retained`, `bytes_truncated`,
`byte_limit`, `truncated`, and full-stream `sha256`, so reviewers can identify
the complete command output without materializing it in the run record. The
report keeps the exact shell argv, `termination` (`completed`, `timed_out`,
`no_progress`, or `cancelled`), and failure tail as resolvable evidence.

Supervised gates persist `gate_output_tail`, `gate_elapsed_ms`, and the age of
their most recent structured progress update in the active adoption record. A
portable gate may emit this ordinary output line on stdout or stderr to mark
forward progress: `HOMEBOY_PROGRESS {"phase":"test","current":"case name"}`.
`phase` is required and `current` is optional. Until the first marker any
output counts as liveness for the configured no-progress deadline; after it
only markers count, so ordinary output remains evidence but cannot mask a
stalled test case. A marker with `"liveness":"output"` hands a delegated phase
(a compiler, an uninstrumented test runner) back to output liveness until the
next marker; `homeboy review` emits one for each lint and test stage. Markers
add durable phase/current context to that liveness signal. It is separate from
the wall-clock gate timeout. A nonzero command exit remains
`termination: "completed"`, while controller deadline outcomes have distinct
termination values.

Use `--private-verify <command>` for orchestrator-only completion gates that
should decide completion without exposing hidden evaluator details to the next
agent attempt. Private gate reports still appear in the promotion report for
human/orchestrator evidence, but `agent-task gate-feedback` applies
`--private-gate-reveal <policy>` before building the follow-up request. Supported
policies are `summary-only` (default), `redacted`, `no-detail`, and
`full-evidence`. Visible gate failures continue to provide full deterministic
evidence to the agent.

`agent-task gate-feedback` converts a promotion report and the original
`AgentTaskRequest` into a provider-neutral cook feedback decision:

```bash
homeboy agent-task gate-feedback \
  --promotion @promotion.json \
  --source-task @source-task.json \
  --source-run-id "$run_id" \
  --attempt 1 \
  --max-attempts 3 \
  --current-diff @current.diff
```

The command returns `homeboy/agent-task-cook-feedback-report/v1`. Red gates with
remaining budget produce `status: "retry_requested"` and a complete
`follow_up_request` containing the failed command, exit status, log tails,
changed files, patch artifact ref, current diff context, and source run/task
refs. When a gate producer supplies versioned diagnostic records in its
declared versioned sidecar evidence, Homeboy carries their stable identities, bounded
summaries, opaque locations/actions, and resolvable evidence refs without
parsing raw command output. Follow-up attempts retain and compare compact
failure sets, prioritizing new failures and shared unchanged failures without
expanding default output. Red gates with exhausted budget return
`status: "retries_exhausted"`.
Green promotion returns `status: "green_completed"` and no follow-up task.

Queued runs that should not execute can be cancelled without chat/session state:

```bash
homeboy agent-task cancel "$run_id" --reason "not selected by controller"
```

`cancel` marks queued runs and stale-running records as `cancelled` in the
durable lifecycle store. It refuses to claim live provider cancellation for an
active runner process until a provider-owned cancellation channel is available.

Cancellation is only partly synchronous, so `cancel` reports what actually
happened under `cancellation` rather than an unqualified success word. When the
teardown is owned elsewhere — a controller-owned staging job, or a provider tree
that is not reachable from this host — the command waits a bounded 15s for the
durable record to converge and then reports one of:

| `cancellation.outcome` | Meaning | Exit |
| --- | --- | --- |
| `cancelled` | The run is durably cancelled. | `0` |
| `terminal_without_cancellation` | The run reached another terminal state first; that result is authoritative. | `0` |
| `deferred_for_terminal_provider` | A provider had already reserved a terminal result, so cancellation was deliberately not applied and the run stays joinable for that import. | `0` |
| `cancellation_requested` | Cancellation is durably accepted, but the run had not reached a terminal state when the bound expired. | `124` |

`cancellation_requested` carries the run id, the wait accounting, and a
`status_command` to check convergence. The bound means `cancel` always returns:
it never prints a terminal-sounding word and then blocks.

## Component Contracts

Agent-task plans may declare generic top-level `component_contracts`. Homeboy
preserves these objects as executor request inputs and does not attach product,
provider, or sandbox-specific semantics to them:

```json
{
  "schema": "homeboy/agent-task-plan/v1",
  "plan_id": "site-generation-loop",
  "component_contracts": [
    {
      "slug": "domain-component",
      "path": "/workspace/domain-component",
      "loadAs": "plugin",
      "activate": true
    }
  ],
  "tasks": []
}
```

When a plan is Lab-offloaded, controller-local `component_contracts[].path`
values are discovered, synced, and remapped with the same local-to-remote
workspace mapping used for provider configs, runtime component paths, provider
plugin paths, workspace roots, and path-valued settings. Lab offload evidence
records the original and remapped paths in `workspace_mapping.workspaces` using
the `component_contract` role.

When the intended checkout already exists on a Lab runner, cook from that
runner-side checkout through `runner exec` instead of forcing a controller-local
hot run:

```bash
homeboy runner exec --cwd /srv/homeboy/checkouts/homeboy homeboy-lab \
  -- homeboy agent-task cook \
    --repo homeboy \
    --cwd /srv/homeboy/checkouts/homeboy \
    --to-worktree homeboy@remote-cook \
    --verify "homeboy review test homeboy" \
    --prompt @task.txt
```

`runner exec` marks non-local jobs as runner-hosted, so nested `agent-task cook`
commands pass the non-interactive resource preflight without a public placement override.

## Cook Workspaces

`agent-task cook` accepts generic Homeboy workspace inputs and does not
resolve product-specific workspace handles itself.

Use `--cwd <PATH>` when the caller already knows the checkout or worktree path:

```bash
homeboy agent-task cook \
  --repo homeboy \
  --cwd /path/to/homeboy@fix-issue \
  --to-worktree homeboy@fix-issue \
  --verify "homeboy review test homeboy" \
  --prompt @task.txt
```

Use `--workspace <ID_OR_PATH>` for a Homeboy-managed task worktree ID or an
existing workspace path:

```bash
homeboy worktree create homeboy --branch fix/issue-123
homeboy agent-task cook \
  --workspace homeboy@fix-issue-123 \
  --to-worktree homeboy@fix-issue-123 \
  --verify "homeboy review test homeboy" \
  --prompt @task.txt
```

External workspace managers should resolve their own handles to local paths and
call cook with `--cwd <resolved-path>`.

When `agent-task cook` is Lab-offloaded with a
patch-producing provider, `--cwd` must point at a clean git checkout with
`remote.origin.url` configured. Homeboy uses that contract to materialize a real
runner-side git checkout/worktree before provider dispatch so generated files can
come back as patch artifacts. Non-git directories, dirty worktrees, and checkouts
without `origin` fail on the controller before offload with a supported-path
diagnostic; use a Homeboy worktree or another clean checkout
for write-capable agent tasks.

## Provider Runtime Contracts

Agent runtime manifests may declare portable provider contracts that Homeboy uses
before and after execution without learning provider-specific APIs. These fields
belong on each `agent_task_executors[]` entry:

```json
{
  "schema": "homeboy/agent-task-executor-provider/v1",
  "id": "example.default",
  "backend": "example",
  "invocation": {
    "argv": ["example-provider"]
  },
  "request_schema": "homeboy/agent-task-request/v1",
  "outcome_schema": "homeboy/agent-task-outcome/v1",
  "secret_env_requirements": [
    {
      "env": ["EXAMPLE_API_TOKEN"],
      "secret_env_sources": {
        "EXAMPLE_API_TOKEN": { "kind": "env", "name": "EXAMPLE_API_TOKEN" }
      }
    }
  ],
  "runner_readiness": [
    {
      "id": "example-auth",
      "label": "Example provider auth",
      "secret_env": ["EXAMPLE_API_TOKEN"],
      "remediation": "Configure EXAMPLE_API_TOKEN with homeboy agent-task auth."
    }
  ],
  "readiness_invocation": {
    "argv": ["example-provider", "--readiness"],
    "timeout_ms": 30000
  },
  "workspace_materialization": {
    "cwd": "git_checkout",
    "requires_git": true,
    "write_scope": "workspace",
    "artifact_paths": ["artifacts"]
  },
  "timeout_artifact_discovery": {
    "config_path_keys": ["provider_artifact_root"],
    "paths": ["/var/tmp/example-provider/latest"],
    "artifact_patterns": [
      {
        "kind": "metrics",
        "filename_patterns": ["*-metrics.ndjson"],
        "mime": "application/x-ndjson",
        "metadata": { "role": "telemetry" }
      }
    ]
  }
}
```

Homeboy treats these declarations as generic contracts:

- `secret_env_requirements` and `runner_readiness` describe required secret env
  names and redacted readiness probes without exposing values.
- `readiness_invocation.timeout_ms` bounds the complete provider-owned readiness
  command, including all child probes. It defaults to 20,000 milliseconds and
  must be between 1 and 120,000 milliseconds; providers must keep their own
  sequential child budgets within this total.
- `workspace_materialization` describes the checkout shape a provider needs; it
  does not name any workspace manager or product runtime.
- `timeout_artifact_discovery` extends timeout evidence recovery with declared
  paths, request metadata/config path keys, and typed filename/extension patterns.
  Discovered files are normalized into `AgentTaskArtifact` entries with generic
  `kind`, `mime`, and opaque metadata.
- Provider-specific sessions, APIs, artifact namespaces, and backend payloads stay
  outside Homeboy core and are represented only as artifacts, evidence refs,
  diagnostics, workflow steps, or opaque metadata.

## Fixture Backend

The built-in `fixture` backend is intentionally narrow. It exists for smoke
proofs and unit tests, not production task execution. A successful fixture cell
writes:

- `changes.patch` as a non-empty unified diff.
- `agent-result.json` as a structured `homeboy/agent-task-outcome/v1` artifact.
- `transcript.log` as transcript evidence.

Useful fixture `executor.config` fields:

- `artifact_root`: directory where fixture artifacts are written.
- `changed_file`: diff path recorded in the generated patch.
- `metadata`: optional JSON object copied into the fixture outcome metadata.
- `mode`: omit or set to `success`; set to `empty_patch` or `empty_runtime_bundle` for classification checks.

## Independent Task Plans

`agent-task run-plan` executes independent tasks with the plan's concurrency and
resource limits. Multi-prompt dispatch and bench matrices use this batch shape.
Use `homeboy/workflow/v1` for multi-step orchestration and dependency graphs.

## Failure Classifications

The deterministic smoke and existing provider path expose these failure classes:

| Case | Diagnostic/classification |
| --- | --- |
| no-op or empty patch | `agent_task.fixture_empty_patch` plus promotion rejecting `promotion refuses an empty patch artifact` |
| provider timeout | `agent_task.provider_timeout`, `failure_classification: "timeout"` |
| missing secrets/preflight | `agent_task.secret_env_missing`, `failure_classification: "invalid_input"` |
| empty runtime bundle | `agent_task.fixture_empty_runtime_bundle` |
| stale/non-terminal status | `status` annotates running records with `metadata.stale_running` and `metadata.stale_running_reason` |

### Diagnose derives its next actions from the classification

`agent-task diagnose` computes a root cause, a causal chain, the recorded
failure classification, and the declared artifacts that were not produced. It
projects that diagnosis into the shared `_homeboy_actionable` envelope (`run`,
`refs`, `next_actions`, `artifacts`, `evidence`) instead of returning only
prose. The existing `next_commands` field is unchanged.

The default diagnosis keeps bounded causal excerpts and artifact refs. Use
`--full`, `agent-task evidence --kind <kind> --full`, or their `--output` forms
to hydrate the complete durable detail explicitly.

`next_action_basis` reports how `next_actions` was produced: `diagnosis` when a
classification or a concrete missing-artifact set mapped to specific commands,
`generic_fallback` when nothing in the diagnosis was specific enough to act on.

| `failure_classification` | Derived next actions |
| --- | --- |
| `provider` | failure evidence for the task, `agent-task providers`, the runner readiness chain when a runner owns the run, then retry |
| `transient` | retry first (documented as safe to retry), then failure evidence |
| `timeout` | failure evidence, `agent-task review` (a timeout can still leave a candidate patch), then retry |
| `stalled` | `agent-task reconcile --dry-run`, `runner status`/`runner doctor --repair` for the owning runner, failure evidence, then retry |
| `rate_limited` | failure evidence (retry-after hint), `agent-task providers` to rotate to, then retry |
| `policy_denied` | failure evidence and the full run record. No retry: an identical request is denied identically |
| `capability_missing` | failure evidence, `agent-task providers --full`, and `agent-task doctor --runner <id>` with its `--repair` form |
| `invalid_input` | failure evidence and `agent-task replay-provider-boundary` for the rejected input. No retry: the same input fails the same way |
| `execution_failed` | failure evidence for the failing step (gate/verify, harvest, required typed artifacts), `agent-task review`, `agent-task artifacts --full`, then retry |
| `unknown` | no specific step is substantiable; the generic fallback set is emitted |

Declared-but-missing artifacts add their own actions regardless of
classification: `agent-task artifacts --full`, plus a task-scoped
`agent-task replay-provider-boundary` and failure-evidence command naming the
artifacts that were not produced. Repair-class actions carry
`kind: "repair"`; runner-scoped actions are emitted only when the run records a
runner id.
