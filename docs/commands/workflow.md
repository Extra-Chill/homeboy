# `homeboy workflow`

```text
homeboy workflow validate <file|id|->
homeboy workflow run <file|id|-> [--input NAME=VALUE]... [--instance NAME] [--resume-from INSTANCE] [--on RUNNER] [--wait]
homeboy runs show <instance>
homeboy runs watch <instance> [--interval 2s] [--timeout 5m | --forever]
homeboy workflow cancel <instance|cadence-workflow-id>
homeboy workflow approve <instance> <step>
homeboy workflow list
homeboy runs list --kind workflow
homeboy [--placement auto|lab|lab-or-local|local] workflow ab <component|path> --base <ref> --candidate <ref> --run <command> --compare <path-or-glob>... [--instance NAME] [--wait]
```

A workflow pre-defines multi-step and repeating agent work and runs it mechanically: steps run in dependency order, and gates decide which step runs next. The definition format is `homeboy/workflow/v1` (JSON).

## How an instance runs

- **One instance is one daemon job.** `workflow run` admits a `workflow` job in the daemon job store. The daemon is its only writer.
- **The instance never runs work itself.** It submits `step` jobs and evaluates gates. Every step job is a supervised child with an admitted environment (see `homeboy daemon` and the `step` job kind).
- **One instance is one pass.** A step's outcome and output are the result of its run in this instance. A definition that wants another pass makes its final step re-admit the definition with the next pass's inputs, which chains a new instance (see [Self-admitting chains](#self-admitting-chains)).
- **Every decision is a pure function.** `advance(definition, state, event)` returns the next state and its commands: submit a step, cancel a step, complete the instance. The daemon commits the new checkpoint, the new step jobs, and their submission keys in **one store transaction**, and only then starts the steps. A crash at any point either loses a whole decision or replays it against the same keys.
- **Keys are deterministic.** A step job's key is `<instance>/<step>/<iteration>/<attempt>`; a `foreach` item's is `<instance>/<step>/<iteration>/item-<n>/<attempt>`. An instance runs iteration 1. A replayed submission converges on the existing job, so no step runs twice.
- **The instance id is deterministic too.** It is `wf-<definition-id>-<sha256(definition digest, inputs, --instance)[..12]>`. Re-running the same definition with the same inputs returns the existing instance while it is running or succeeded. A failed, cancelled or budget-exhausted one is retried as a fresh instance resumed from it (its succeeded steps are reused), and that retry's id derives from the one it retries. New inputs (or a new `--instance` name) start a new one.
- **The definition is pinned.** Its digest and body are stored in the instance at admission, so editing the file never changes a running instance.

Statuses:

| Thing | Terminal statuses |
|---|---|
| Step | `succeeded`, `failed`, `timed_out`, `cancelled`, `skipped`; a `cook` step reports its Cook's run state (`succeeded`, `candidate_recoverable`, `partial_failure`, `failed`, `cancelled`) |
| Instance | `succeeded`, `failed`, `cancelled`, `budget_exhausted` |

## Resume after fixing a definition

`homeboy workflow run <file|id|-> --resume-from <instance>` admits the current definition as a new instance. Inputs the definition still declares keep the source's values unless passed again with `--input`; a `next_pass` instance also keeps its `iteration` and `previous`, so the chain continues from that pass. The source must be terminal and belong to the same workflow on the selected daemon. A succeeded step is reused only when its complete templated step-definition digest matches and all its `needs` were reused. Changing a step therefore reruns its downstream steps. Failed, skipped, and unfinished steps run normally.

Reused steps retain their recorded outputs, run ids, item records, and original job keys (the references to their artifacts); artifacts are not copied or recreated. `runs show` includes `resumed_from` and each step's `reused` and `launched` flags. Pending steps have both flags false. Resume provenance participates in the deterministic instance id; omitting the flag preserves existing identity and pinning semantics. Cadences do not accept `--resume-from`.

## The `homeboy/workflow/v1` format

```jsonc
{
  "schema": "homeboy/workflow/v1",
  "id": "string",                       // registry id: ~/.config/homeboy/workflows/<id>.json, or run any path; letters, digits, `_`, `-`, `.`
  "inputs":  { "<name>": { "required": true, "default": null } },
  "env":     { "public": ["NAME"], "secrets": ["SECRET_REF"] },   // the admitted environment; nothing else is inherited
  "budget":  { "max_step_runs": 20, "deadline": "12h" },          // hard stop → budget_exhausted
  "every":   { "interval": "24h", "at": "03:00Z", "overlap": "skip" },  // optional cadence (see below)
  "steps": [
    {
      "id": "string",
      "needs": ["step-id"],          // run after these succeed; a need that did not succeed skips this step
      "when": <gate>,                // evaluated after needs settle; false skips the step
      "always": false,               // run even if a need failed (cleanup)
      "foreach": "{{steps.prepare.output/children}}",   // fan-out: one step job per item (or a JSON array)
      "max_parallel": 4,             // at most this many items run at once (default: all)
      "join": "all",                 // or "any", or {"min_succeeded": N}; decided once every item settled
      "retry": { "max": 2, "on": ["failed", "timed_out"], "backoff": "10m" },
      "timeout": "90m",
      // exactly one of:
      "cook":     { /* keys are `homeboy agent-task cook` flags */ },
      "homeboy":  ["argv", "..."],
      "exec":     { "program": "node", "args": ["..."], "cwd": "..." },
      "approval": { "prompt": "string" }
    }
  ]
}
```

Use `next_pass` for a bounded chain of passes. Unknown fields (including the removed `iterate`) are refused. Replace command gates with an `exec` step plus an `outcome` gate.

### Fan-out and join

A step with `foreach` expands, once its `needs` and `when` pass, into one step job per item. `foreach` is a JSON array or a template that resolves to one (a whole-value reference such as `{{steps.prepare.output/children}}` keeps the array's JSON). Inside the step's own work, `{{item}}` is the item and `{{item.<field>}}` reads a field of it.

- **`max_parallel`** bounds how many items run at once; the next item starts as one settles.
- **Items are independent.** One item's failure neither cancels nor skips its siblings. `retry` and `timeout` apply to each item.
- **`join`** decides the step once every item has settled: `all` (default) needs every item to succeed, `any` one, `{"min_succeeded": N}` at least `N`. A passing join makes the step `succeeded`; otherwise it is `partial_failure` (some items succeeded) or `failed` (none did), which fails the instance like any failed step.
- **The step's output** is `{"total", "succeeded", "join", "items": [{"item", "outcome", "run_id", "output", "error"}]}`.
- **Cancelling the instance** cancels every running item, the Cook a `cook` item runs included, and starts no further one.

Use `{"min_succeeded": 0}` when item failures are expected outcomes, such as Cooks or `git pr merge-when-green` holds in a `next_pass` loop: the step succeeds once every item settles, preserving each item's `outcome` and `error` (Example D).

`agent-task fanout cook-batch` compiles to exactly this shape (Example C).

### Step kinds

For a reviewed-head merge, use a `homeboy` step invoking
[`git pr merge-when-green`](git.md#exact-head-green-merge). It owns bounded check
and mergeability polling, typed holds, repository method selection, and the
atomic head guard; workflows do not need an `exec` polling/merge wrapper.

- **`exec`** runs a program directly, never through a shell. The program is resolved to an absolute path when the workflow is admitted. Its output is the JSON it writes to the file named by `$HOMEBOY_STEP_OUTPUT`.
- **`homeboy`** runs `homeboy <argv>` with this Homeboy build. Its output is the `data` of the command's JSON envelope.
- **`cook`** runs `homeboy agent-task cook <flags> --wait --cook-id <id>`. Every key must be a real `agent-task cook` flag; this is checked against the live command tree, so there is no second Cook contract. A string becomes `--key value`, `true` becomes `--key`, and an array repeats the flag. A value that is exactly one template resolving to an array (such as `{{item.gate_inputs}}`) also repeats the flag, once per element. `wait`, `cook-id`, `run-id`, `output`, and `preview` belong to the workflow. The step's outcome is the Cook's terminal run state, read from its run record; `{{steps.<id>.run_id}}` is the Cook id.
- **`approval`** waits for `homeboy workflow approve <instance> <step>` or `homeboy workflow reject <instance> <step> [--reason <text>]`. It runs no job. Approval settles `succeeded`; rejection settles `rejected` (not an instance failure). Both write output with `decision` (`approved` or `rejected`), `reason` (text or null), and `actor` (the submitting user's name, or `local-operator`). `runs show` includes this output.

  Rejected dependencies skip their dependents by default. A passing `when` that explicitly matches the rejected dependency's `/decision` to `rejected` (using `equals` or `in`), or its `outcome` to `rejected`, permits that branch to run. For example:

  ```json
  {"id":"revise","needs":["approve_merge"],
   "when":{"step":"approve_merge","output":"/decision","equals":"rejected"},
   "cook":{"prompt":"Rework the candidate: {{steps.approve_merge.output/reason}}"}}
  ```

  Gate the merge branch on `{"step":"approve_merge","output":"/decision","equals":"approved"}`. `always` retains its usual dependency override.

  When the instance has a notification route, entering this gate sends an `awaiting_approval` announcement with the rendered prompt, upstream output artifact links, and the approve and reject commands. Delivered or queued announcements are deduplicated per instance, step, and iteration across recovery; status projections never send notifications.

  Pass the global `--notification-transport TRANSPORT --notification-route ROUTE` flags to `workflow run` to bind a destination. Explicit flags win over `HOMEBOY_NOTIFICATION_TRANSPORT` / `HOMEBOY_NOTIFICATION_ROUTE`. The resolved route is persisted with the instance (or cadence template), propagated to all step processes, and inherited by workflows admitted from those steps. Recovery uses the persisted route rather than the daemon's ambient destination.

### Gates

A gate is a closed, deterministic predicate: a pure read of the outcomes and outputs of the steps in the same instance. There is no expression language and no side effect.

| Gate | Passes when |
|---|---|
| `{"step":"id","outcome":["succeeded","candidate_recoverable"]}` | the step's outcome is in the set |
| `{"step":"id","output":"/json/pointer","equals":X}` (or `in`, `exists`, `lt`, `gt`) | the test holds on the step's output |
| `{"all":[…]}`, `{"any":[…]}`, `{"not":…}` | the combination holds |

A gate is decided when the step it belongs to is scheduled, so it reads steps that have already settled — its `needs`, or the output of an upstream step.

### Templating

String substitution only: `{{inputs.x}}`, `{{steps.<id>.output<pointer>}}`, `{{steps.<id>.run_id}}`, `{{instance}}`, and in a `foreach` step's work `{{item}}` / `{{item.<field>}}`. A string substitutes as itself; any other JSON value substitutes as its compact JSON.

### Validation

`workflow validate` and admission both check:

- The `needs` graph is acyclic and names real steps.
- Every template reference resolves to a declared input or an upstream step (a step reachable through `needs`).
- A `when` gate reads only upstream steps or the step itself.
- Every `cook` key is a real `agent-task cook` flag.
- Every environment name is declared once; a credential-named variable is a secret, never a public value.
- Durations (`timeout`, `retry.backoff`, `budget.deadline`) parse.
- No `iterate` and no `command` gates (see the format above).

### Failure, retry, budgets

- **A failed step fails the instance.** `failed`, `timed_out`, `cancelled`, and `partial_failure` are failures. No new step starts after one unless it is `always`; when everything in flight has settled the instance is `failed`. It never parks.
- **`retry`** re-submits the step as attempt `n+1` when its outcome is in `on` (default `failed` and `timed_out`), up to `max` retries. `backoff` delays the retry through the step job's `not_before` wait.
- **`timeout`** cancels the step's job when it has run that long; the outcome is `timed_out`.
- **`budget.max_step_runs`** counts every step job. Reaching it cancels whatever is running and ends the instance `budget_exhausted`. So does passing `budget.deadline`.

### Environment

A step runs with `env_clear()` plus its admitted environment:

- `env.public` names are captured from the environment that runs `workflow run`. A declared name that is unset there is refused.
- `env.secrets` are agent-task secret references, resolved when each step spawns. The daemon's own environment is never a source.
- `homeboy` and `cook` steps also get the runtime environment a detached Cook gets (provider process variables and Homeboy's `HOMEBOY_*` configuration).
- Every step also gets `HOMEBOY_WORKFLOW_INSTANCE`, the id of the instance whose step is running. `workflow run` reads it and records the admitting instance as the new instance's `parent`.

### Native next passes (`next_pass`)

`next_pass` starts another instance only after the current pass succeeds and its optional `when` gate passes. It forwards every input, replacing only `next_pass.inputs` (typed JSON values and templates). No self-admitting `again`/`retry` commands or caller-maintained stall counters are needed. A chain that ends records why in the last instance's `reason` (for example "chain ended: step `evaluate` output `/blocker_signatures` unchanged for 3 passes"), so `runs show` distinguishes a finished chain from a stalled one.

```json
{
"schema": "homeboy/workflow/v1",
"id": "bounded-loop",
"inputs": {"cursor": {"default": null}},
"steps": [{"id": "evaluate", "exec": {"program": "evaluate"}}],
"next_pass": {
  "max_iterations": 10,
  "when": {"step": "evaluate", "output": "/continue", "equals": true},
  "inputs": {"cursor": "{{steps.evaluate.output/cursor}}"},
  "stop_unchanged": {"step": "evaluate", "output": "/remaining", "passes": 3}
}
}
```

- `inputs.iteration` is a reserved positive JSON integer, starting at 1. `max_iterations` includes the first pass.
- `inputs.previous` is null on the first pass; thereafter it contains `outputs` (a map keyed by step id) and `unchanged_passes`. A step can read the whole object with `{{inputs.previous}}`.
- `stop_unchanged` counts consecutive passes with equal JSON values at the chosen output pointer, including the first observation. A changed value resets the count to 1; a missing output is an error.
- Each pass has its own status, step jobs and budget. Parent/children links and `--wait` follow the chain. Child admission is idempotent across recovery.
- Each pass is pinned to the definition it ran. For file-based admission, the next pass reads the file again, so an edit takes effect on the next pass without restarting the chain. An edit the chain's admission does not cover (a changed `env`, a new exec program, or a first `cook`/`homeboy` step) ends the chain with an error naming the `workflow run <file> --resume-from <instance>` command that admits it; so does a missing or invalid file. Registry files are checked too; stdin/inline/built-in definitions remain pinned snapshots. `next_pass` cannot be combined with `every`.

### Cadence (`every`)

`workflow run` on a definition with `every` registers it with the daemon instead of admitting one instance. The daemon then admits one instance per due slot.

- **Registration.** The registration is the admitted request: the definition, the inputs, the captured environment, and the resolved programs. It is keyed by the workflow id; running `workflow run` again replaces it. An unchanged cadence keeps its slot grid; a changed `interval` or `at` starts a new grid. `--wait` and `--instance` do not apply. `--on RUNNER` registers it on that runner's daemon.
- **Slots.** Slots are `anchor + k × interval`. With `at` (a UTC time of day, `HH:MM` or `HH:MMZ`) the anchor is that time, so `{"interval": "24h", "at": "03:00Z"}` fires daily at 03:00 UTC; without it the first slot is the moment of registration. `interval` is at least `1m`.
- **Admission.** The admitting daemon's orchestration tick (60 seconds by default) admits each due slot. Slots missed while the daemon was stopped coalesce into the latest one.
- **One instance per slot.** A slot's instance is named `every-<slot time>`, so its instance id is deterministic. A tick that re-fires a slot after a crash converges on the instance it already admitted, even across a daemon restart.
- **Overlap skip.** A slot that comes due while the previous instance of the same cadence is not terminal is skipped and counted. `skip` is the only `overlap` policy.
- **Where it lives.** Registrations are stored in the observation store beside the daemon job ledger, so they survive daemon restarts. A store schema bump starts fresh and drops them. A daemon that is not running admits nothing: run the always-on cadences on a resident daemon (`HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS=0 homeboy daemon ensure-running`, or a runner's service daemon).
- **Observe and stop.** `workflow list` shows every cadence with its next due time, fire and skip counts, last instance, and last admission error. `workflow cancel <workflow-id>` stops a cadence; its instances are ordinary instances you can cancel separately.

**Built-in `homeboy.retention`.** Homeboy ships a retention workflow, `homeboy.retention`: every hour, one `homeboy` step runs `cleanup automatic-retention` with a 50-minute timeout. The daemon registers it when it starts, first due one hour later, unless an operator registered their own `homeboy.retention` or cancelled it; both are kept. `workflow run homeboy.retention` and `workflow validate homeboy.retention` resolve the built-in when no registry file has that id.

### Daemon restarts

An instance's checkpoint is the daemon's job store. `runs show`, `runs watch`, `runs list --kind workflow`, and `workflow approve` go to the serving daemon.

When the daemon that owns an instance dies or is stopped, the instance resumes on its own. The next Homeboy command restarts the daemon in the same state directory, and the replacement resumes the instance from its checkpoint. That directory's owner lock admits one daemon, so the instance has one driver, and submission keys keep every step to one job. A step whose child was still running when the daemon died is protected until the child exits; its result is then read from the job ledger (and, for a `cook` step, from the Cook's run record), so no step runs twice. `homeboy daemon recover --yes` applies the same restart immediately.

## A/B parity (`workflow ab`)

`homeboy workflow ab` proves a refactor is byte-identical. It runs one command at two refs and compares the files it produces:

```text
homeboy --wait workflow ab blocks-engine --base origin/main --candidate my-refactor \
  --run 'composer build' --compare build --compare 'out/**/*.html'
```

- **Inputs.** The target is a component id or a repository path. `--base` and `--candidate` are any refs; both resolve to commit SHAs on the controller first. `--run` runs with `sh -c` in each checkout. `--compare` (repeatable) is a file, directory, or glob relative to the checkout. A directory includes every file under it, and `.git` is never compared.
- **Placement.** `--placement` follows the usual rules. `auto` (the default) and `lab-or-local` use a ready Lab runner and run here when none is ready. `lab` requires a runner. `local` runs on this controller. On a runner, each commit is synced as its own snapshot-git workspace, and the recipe is admitted on that runner's daemon (`workflow run --on`).
- **Execution.** `workflow ab` admits the built-in `homeboy.ab` recipe. Its `side` step fans out over `base` and `candidate` with `max_parallel: 2`, so both sides run at once as supervised step jobs. Each side gets an isolated checkout: a detached `git worktree` locally, or its own Lab workspace. Before running the command, the side asserts that the checkout's HEAD equals the resolved SHA, and refuses to run if it does not. A side records its SHA, run id (its step job), exit code, a 4 KiB log tail, and the sha256 of every compared file.
- **Verdict.** The `verdict` step writes `homeboy/workflow-ab-verdict/v1`. It is `identical` when every compared file matches byte for byte on both sides and the exit codes are equal. Otherwise it is `different` and carries `differing` (path plus both sha256 values; a file missing on one side has `null`), a unified diff bounded to 64 KiB (`diff_truncated` marks a cut), the per-file sha256 for both sides, and both run ids. A compare set that matches no files on either side is an error, never a vacuous `identical`. The verdict is the step's output, so `homeboy runs show <instance>` keeps it as evidence.
- **Output.** With `--wait`, human output is a short Markdown summary you can paste into a PR. The JSON envelope adds `ab` (refs and SHAs) and `verdict`. The exit code is 0 for `identical` and 1 for `different`. Without `--wait`, the command returns the admitted instance with its `status_command` and `cancel_command`.
- **Cancel.** `homeboy workflow cancel <instance>` cancels both sides. On a Lab runner, run it through `homeboy runner exec <runner> -- …`, which is the `cancel_command` the front end prints. Each side is a step job that leads its own process group, and the command runs inside that group, so cancelling stops the whole tree with nothing left behind. The `always` `cleanup` step removes local worktrees after a verdict or a failed side. A cancelled instance runs no further steps, so its checkouts stay under the temp directory (`homeboy-ab/<instance>`) until `git worktree prune`.
- **Identity.** The same refs, command, and compare set name the same instance. Pass `--instance NAME` to run them again.

## Commands

- **`workflow validate`** parses and validates a definition and prints its digest and step graph.
- **`workflow run`** admits an instance in the local daemon and prints its status; a definition with `every` is registered as a cadence instead. With `--wait` it follows the instance — and any chain of children it admits — until the last instance is terminal, and exits non-zero unless that instance succeeded. `--on RUNNER` admits the same definition on that runner's daemon through `homeboy runner exec`, so the environment is admitted on the runner and an always-on runner can own the instance.
- **`runs show`** is a pure projection of the job ledger: the instance status, its `parent` and `children`, each step's phase, outcome, attempt, key, and step job, each `foreach` item with its step job, Cook run id, and the live state of a running Cook. `runs watch <instance>` polls until the instance — and any chain it admits — is terminal. It defaults to a five-minute timeout; use `--forever` for an unbounded watch.
- **`workflow cancel`** cancels the instance; the daemon cancels every step job it is running. Once the cancellation is durable the instance admits no more steps. Given a workflow id that is not an instance, it stops that cadence.
- **`workflow approve`** records an approval for a step that is awaiting one.
- **`workflow reject <instance> <step> [--reason <text>]`** records a rejection for an awaiting approval, with optional feedback for downstream steps.
- **`workflow ab`** compiles and admits the built-in `homeboy.ab` A/B parity recipe (see [A/B parity](#ab-parity-workflow-ab)). Its hidden `ab-step` verb runs the recipe's steps.
- **`workflow list`** lists the definitions in `~/.config/homeboy/workflows/`, the built-ins, and every registered cadence. `runs list --kind workflow` lists instances in the daemon without discovery filters.

## Examples

### Example B: nightly repeating task (cadence + gate → Cook → review)

A daily cadence at 07:00 UTC. The `ci` step reads the latest `main` run; the `fix` Cook runs only on a red build, and `review` reviews the Cook it produced.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "homeboy-nightly-red-main",
  "every": { "interval": "24h", "at": "07:00Z", "overlap": "skip" },
  "env": { "secrets": ["github-token"] },
  "budget": { "max_step_runs": 6, "deadline": "4h" },
  "steps": [
    { "id": "ci", "exec": { "program": "gh", "args": ["run", "list", "-R", "Extra-Chill/homeboy", "-b", "main", "-L", "1", "--json", "conclusion,url"] } },
    { "id": "fix", "needs": ["ci"],
      "when": { "step": "ci", "output": "/0/conclusion", "equals": "failure" },
      "cook": { "repo": "homeboy", "placement": "lab-or-local",
                "prompt": "main is red ({{steps.ci.output/0/url}}). Find the root cause and fix it.",
                "verify": ["cargo test --workspace"] } },
    { "id": "review", "needs": ["fix"],
      "when": { "step": "fix", "outcome": ["succeeded"] },
      "homeboy": ["review", "{{steps.fix.run_id}}"] }
  ]
}
```

### Example C: fan-out batch with a join gate

This is the shape `homeboy --placement lab agent-task fanout cook-batch --repo homeboy-cook-scratch --verify 'test -f DONE' --max-concurrency 2 --join 2 ISSUE...` compiles to (abridged to two issues). `prepare` resolves the base once and transfers its commit to the Lab runner once; `cook` runs one Cook per child, two at a time, and succeeds when at least two children do.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "cook-batch",
  "steps": [
    { "id": "prepare",
      "homeboy": ["agent-task", "batch-prepare", "--repo", "homeboy-cook-scratch", "--branch-prefix", "fix",
                  "--bundle-runner", "homeboy-lab",
                  "https://github.com/chubes4/homeboy-cook-scratch/issues/1",
                  "https://github.com/chubes4/homeboy-cook-scratch/issues/2"] },
    { "id": "cook", "needs": ["prepare"],
      "foreach": "{{steps.prepare.output/children}}", "max_parallel": 2,
      "join": { "min_succeeded": 2 },
      "cook": { "repo": "homeboy-cook-scratch", "task-url": "{{item.issue_url}}", "prompt": "{{item.prompt}}",
                "head": "{{item.branch}}", "to-worktree": "{{item.worktree}}",
                "base": "{{steps.prepare.output/base}}",
                "verify": ["test -f DONE"], "placement": "lab" } }
  ]
}
```

### Example D: a fix loop (measure → Cook → confirm → approve → merge → next pass)

A loop for any task whose steps you know. Only the domain-specific steps are your programs:
- `measure` reports what is still wrong, one item per tracked issue;
- `confirm` checks each fix PR at its head and lists the ones to merge.

Homeboy owns the rest: the parallel Cooks, the human approval, the exact-head merge, and the chain of passes. `"join": {"min_succeeded": 0}` keeps one pass's failed Cooks or held merges from ending the chain, because the next pass measures again and retries them. The chain stops when nothing is left to fix, when `measure` reports the same failures three passes in a row, or after 20 passes.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "fix-loop",
  "steps": [
    { "id": "measure", "exec": { "program": "./measure" } },
    { "id": "cook", "needs": ["measure"],
      "when": { "step": "measure", "output": "/count", "gt": 0 },
      "foreach": "{{steps.measure.output/items}}", "max_parallel": 3,
      "join": { "min_succeeded": 0 },
      "cook": { "repo": "{{item.repo}}", "task-url": "{{item.issue_url}}",
                "prompt": "Fix {{item.issue_url}}.", "verify": ["{{item.verify}}"] } },
    { "id": "confirm", "needs": ["cook"], "always": true,
      "when": { "step": "measure", "output": "/count", "gt": 0 },
      "exec": { "program": "./confirm" } },
    { "id": "approve", "needs": ["confirm"],
      "when": { "step": "confirm", "output": "/merge_count", "gt": 0 },
      "approval": { "prompt": "Merge the confirmed fixes?" } },
    { "id": "merge", "needs": ["approve"],
      "when": { "step": "approve", "output": "/decision", "equals": "approved" },
      "foreach": "{{steps.confirm.output/merge}}", "join": { "min_succeeded": 0 },
      "homeboy": ["git", "pr", "merge-when-green", "--repo", "{{item.github}}",
                  "--number", "{{item.number}}", "--sha", "{{item.sha}}", "--wait-seconds", "3600"],
      "timeout": "65m" }
  ],
  "next_pass": {
    "max_iterations": 20,
    "when": { "step": "measure", "output": "/count", "gt": 0 },
    "stop_unchanged": { "step": "measure", "output": "/ids", "passes": 3 }
  }
}
```

`measure` writes `{"count", "ids", "items": [{"repo", "issue_url", "verify"}]}` to `$HOMEBOY_STEP_OUTPUT`. `confirm` writes `{"merge": [{"github", "number", "sha"}], "merge_count"}`. Static Site Importer's import findings loop (`tools/import-loop/loop.json`) is a full instance of this shape: there, `measure` is the import, evaluate and triage steps, and a `shipped` step waits for releases before the next pass.
