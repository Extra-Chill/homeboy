# `homeboy workflow`

```text
homeboy workflow validate <file|id|->
homeboy workflow run <file|id|-> [--input NAME=VALUE]... [--instance NAME] [--on RUNNER] [--wait]
homeboy workflow status [<instance>] [--watch]
homeboy workflow cancel <instance|cadence-workflow-id>
homeboy workflow approve <instance> <step>
homeboy workflow list [--registered]
```

A workflow pre-defines multi-step and repeating agent work and runs it mechanically: steps run in dependency order, and gates decide which step runs next. The definition format is `homeboy/workflow/v1` (JSON).

## How an instance runs

- **One instance is one daemon job.** `workflow run` admits a `workflow` job in the daemon job store. The daemon is its only writer.
- **The instance never runs work itself.** It submits `step` jobs and evaluates gates. Every step job is a supervised child with an admitted environment (see `homeboy daemon` and the `step` job kind).
- **One instance is one pass.** A step's outcome and output are the result of its run in this instance. A definition that wants another pass makes its final step re-admit the definition with the next pass's inputs, which chains a new instance (see [Self-admitting chains](#self-admitting-chains)).
- **Every decision is a pure function.** `advance(definition, state, event)` returns the next state and its commands: submit a step, cancel a step, complete the instance. The daemon commits the new checkpoint, the new step jobs, and their submission keys in **one store transaction**, and only then starts the steps. A crash at any point either loses a whole decision or replays it against the same keys.
- **Keys are deterministic.** A step job's key is `<instance>/<step>/<iteration>/<attempt>`; a `foreach` item's is `<instance>/<step>/<iteration>/item-<n>/<attempt>`. An instance runs iteration 1. A replayed submission converges on the existing job, so no step runs twice.
- **The instance id is deterministic too.** It is `wf-<definition-id>-<sha256(definition digest, inputs, --instance)[..12]>`. Re-running the same definition with the same inputs returns the existing instance; new inputs (or a new `--instance` name) start a new one.
- **The definition is pinned.** Its digest and body are stored in the instance at admission, so editing the file never changes a running instance.

Statuses:

| Thing | Terminal statuses |
|---|---|
| Step | `succeeded`, `failed`, `timed_out`, `cancelled`, `skipped`; a `cook` step reports its Cook's run state (`succeeded`, `candidate_recoverable`, `partial_failure`, `failed`, `cancelled`) |
| Instance | `succeeded`, `failed`, `cancelled`, `budget_exhausted` |

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

`iterate` and `command` gates are not part of this version. Admission refuses them with a message that names the replacement: an `iterate` is replaced by a final `homeboy` step that runs `workflow run <this definition>` with the next pass's inputs, gated by `when`; a `{"command": [...]}` gate is replaced by an `exec` step plus an `outcome` gate. Unknown top-level fields are refused like any unknown field.

### Fan-out and join

A step with `foreach` expands, once its `needs` and `when` pass, into one step job per item. `foreach` is a JSON array or a template that resolves to one (a whole-value reference such as `{{steps.prepare.output/children}}` keeps the array's JSON). Inside the step's own work, `{{item}}` is the item and `{{item.<field>}}` reads a field of it.

- **`max_parallel`** bounds how many items run at once; the next item starts as one settles.
- **Items are independent.** One item's failure neither cancels nor skips its siblings. `retry` and `timeout` apply to each item.
- **`join`** decides the step once every item has settled: `all` (default) needs every item to succeed, `any` one, `{"min_succeeded": N}` at least `N`. A passing join makes the step `succeeded`; otherwise it is `partial_failure` (some items succeeded) or `failed` (none did), which fails the instance like any failed step.
- **The step's output** is `{"total", "succeeded", "join", "items": [{"item", "outcome", "run_id", "output", "error"}]}`.
- **Cancelling the instance** cancels every running item, the Cook a `cook` item runs included, and starts no further one.

`agent-task fanout cook-batch` compiles to exactly this shape (Example C).

### Step kinds

- **`exec`** runs a program directly, never through a shell. The program is resolved to an absolute path when the workflow is admitted. Its output is the JSON it writes to the file named by `$HOMEBOY_STEP_OUTPUT`.
- **`homeboy`** runs `homeboy <argv>` with this Homeboy build. Its output is the `data` of the command's JSON envelope.
- **`cook`** runs `homeboy agent-task cook <flags> --wait --cook-id <id>`. Every key must be a real `agent-task cook` flag; this is checked against the live command tree, so there is no second Cook contract. A string becomes `--key value`, `true` becomes `--key`, and an array repeats the flag. `wait`, `cook-id`, `run-id`, `output`, and `preview` belong to the workflow. The step's outcome is the Cook's terminal run state, read from its run record; `{{steps.<id>.run_id}}` is the Cook id.
- **`approval`** waits for `homeboy workflow approve <instance> <step>`. It runs no job.

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

### Self-admitting chains

The replacement for `iterate` is a chain of instances: the definition's final step is a `homeboy` step that runs `workflow run <this definition>` with the next pass's inputs, gated by `when`. Each pass is one instance, so every pass has its own status, step jobs, and budget.

- **The parent link.** An instance admitted by a step of another instance records that instance as its `parent`. `workflow status <instance>` shows `parent` and `children`, so the chain is readable from either end.
- **Replays converge.** The instance id hashes the definition and inputs, so a step re-running `workflow run` with the same inputs (a crash, a replayed decision) returns the existing child instead of admitting a second one.
- **Watching follows the chain.** `workflow run --wait` and `workflow status --watch` observe their instance to its terminal status, then follow `children` to the last instance of the chain and report that instance's terminal status.
- **Termination is the author's job.** The re-admitting step's `when` gate decides whether the chain continues; with a false gate the chain ends and the last instance is the report. `workflow cancel` stops a chain by cancelling the instance that would re-admit.

### Cadence (`every`)

`workflow run` on a definition with `every` registers it with the daemon instead of admitting one instance. The daemon then admits one instance per due slot.

- **Registration.** The registration is the admitted request: the definition, the inputs, the captured environment, and the resolved programs. It is keyed by the workflow id; running `workflow run` again replaces it. An unchanged cadence keeps its slot grid; a changed `interval` or `at` starts a new grid. `--wait` and `--instance` do not apply. `--on RUNNER` registers it on that runner's daemon.
- **Slots.** Slots are `anchor + k × interval`. With `at` (a UTC time of day, `HH:MM` or `HH:MMZ`) the anchor is that time, so `{"interval": "24h", "at": "03:00Z"}` fires daily at 03:00 UTC; without it the first slot is the moment of registration. `interval` is at least `1m`.
- **Admission.** The admitting daemon's orchestration tick (60 seconds by default) admits each due slot. Slots missed while the daemon was stopped coalesce into the latest one.
- **One instance per slot.** A slot's instance is named `every-<slot time>`, so its instance id is deterministic. A tick that re-fires a slot after a crash converges on the instance it already admitted, even across a daemon restart.
- **Overlap skip.** A slot that comes due while the previous instance of the same cadence is not terminal is skipped and counted. `skip` is the only `overlap` policy.
- **Where it lives.** Registrations are stored in the observation store beside the daemon job ledger, so they survive daemon restarts. A store schema bump starts fresh and drops them. A daemon that is not running admits nothing: run the always-on cadences on a resident daemon (`homeboy daemon start`, or a runner's service daemon).
- **Observe and stop.** `workflow list --registered` shows every cadence with its next due time, fire and skip counts, last instance, and last admission error. `workflow cancel <workflow-id>` stops a cadence; its instances are ordinary instances you can cancel separately.

**Built-in `homeboy.retention`.** Homeboy ships one workflow, `homeboy.retention`: every hour, one `homeboy` step runs `cleanup automatic-retention` with a 50-minute timeout. The daemon registers it when it starts, first due one hour later, unless an operator registered their own `homeboy.retention` or cancelled it; both are kept. `workflow run homeboy.retention` and `workflow validate homeboy.retention` resolve the built-in when no registry file has that id.

### Daemon restarts

An instance's checkpoint is the daemon's job store. `workflow status`, `list`, and `approve` go to the serving daemon.

When the daemon that owns an instance dies or is stopped, the instance resumes on its own. The next Homeboy command restarts the daemon in the same state directory, and the replacement resumes the instance from its checkpoint. That directory's owner lock admits one daemon, so the instance has one driver, and submission keys keep every step to one job. A step whose child was still running when the daemon died is protected until the child exits; its result is then read from the job ledger (and, for a `cook` step, from the Cook's run record), so no step runs twice. `homeboy daemon recover --yes` applies the same restart immediately.

## Commands

- **`workflow validate`** parses and validates a definition and prints its digest and step graph.
- **`workflow run`** admits an instance in the local daemon and prints its status; a definition with `every` is registered as a cadence instead. With `--wait` it follows the instance — and any chain of children it admits — until the last instance is terminal, and exits non-zero unless that instance succeeded. `--on RUNNER` admits the same definition on that runner's daemon through `homeboy runner exec`, so the environment is admitted on the runner and an always-on runner can own the instance.
- **`workflow status`** is a pure projection of the job ledger: the instance status, its `parent` and `children`, each step's phase, outcome, attempt, key, and step job, each `foreach` item with its step job, Cook run id, and the live state of a running Cook. `--watch` polls until the instance — and any chain it admits — is terminal.
- **`workflow cancel`** cancels the instance; the daemon cancels every step job it is running. Once the cancellation is durable the instance admits no more steps. Given a workflow id that is not an instance, it stops that cadence.
- **`workflow approve`** records an approval for a step that is awaiting one.
- **`workflow list`** lists instances in the daemon; `--registered` lists the definitions in `~/.config/homeboy/workflows/`, the built-ins, and every registered cadence.

## Examples

### Example A: a self-admitting proof loop

One definition, one instance per pass. `capture` and `evaluate` decide, `fix` is a Cook that only runs on findings, and `again` re-admits the same definition with the next iteration number when `evaluate` says to continue. Three iterations are three instances; each pass has its own status and budget.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "ssi-url-proof",
  "inputs": {
    "url": {"required": true},
    "candidate_sha": {"required": true},
    "workspace": {"required": true},
    "iteration": {"default": 1}
  },
  "env": { "public": ["SSI_BLOCKS_ENGINE_PATH", "HOMEBOY_WP_CODEBOX_BIN"] },
  "budget": { "max_step_runs": 6, "deadline": "4h" },
  "steps": [
    { "id": "capture", "timeout": "15m",
      "homeboy": ["runner", "exec", "lab", "--cwd", "{{inputs.workspace}}", "--",
                  "node", "tools/url-loop-controller.mjs", "capture", "--url", "{{inputs.url}}"] },
    { "id": "evaluate", "needs": ["capture"], "timeout": "90m",
      "homeboy": ["runner", "exec", "lab", "--", "node", "tools/url-loop-controller.mjs", "evaluate",
                  "--fixture", "{{steps.capture.output/fixture_root}}", "--candidate-sha", "{{inputs.candidate_sha}}",
                  "--iteration", "{{inputs.iteration}}"] },
    { "id": "fix", "needs": ["evaluate"],
      "when": { "step": "evaluate", "output": "/verdict", "equals": "findings" },
      "retry": { "max": 1, "on": ["failed"] },
      "cook": { "repo": "static-site-importer", "placement": "lab",
                "prompt": "Fix the import findings in {{steps.evaluate.output/report_path}} for {{inputs.url}}.",
                "verify": ["composer test", "node tools/url-loop-controller.mjs evaluate --fixture {{steps.capture.output/fixture_root}}"] } },
    { "id": "again", "needs": ["evaluate"],
      "when": { "step": "evaluate", "output": "/continue", "exists": true },
      "homeboy": ["workflow", "run", "ssi-url-proof",
                  "--input", "url={{inputs.url}}",
                  "--input", "candidate_sha={{inputs.candidate_sha}}",
                  "--input", "workspace={{inputs.workspace}}",
                  "--input", "iteration={{steps.evaluate.output/next_iteration}}"] }
  ]
}
```

The contract between passes is in the inputs and the evaluate output:

- **`evaluate` emits `{"verdict": …, "continue": …, "next_iteration": …}`** — a `continue` member means another pass is needed, and `next_iteration` is the number the next pass runs with. The `again` step re-admits the definition with it; the admission happens inside this pass's step, so this pass's status view lists the next pass under `children`.
- **Each pass is one instance** with its own `wf-…` id (the definition and inputs decide it). `capture` runs in every pass; the controller reuses the fixture it already captured for the URL, so a later pass returns quickly and passes its `fixture_root` on to `evaluate`.
- **A new candidate SHA is just new inputs:** `homeboy workflow run ssi-url-proof --input url=… --input candidate_sha=<new> --input workspace=…`. The same inputs are a no-op that returns the existing instance.
- **`--wait` follows the chain:** `homeboy workflow run ssi-url-proof … --wait` exits when the last pass of the chain is terminal, non-zero unless it succeeded.
- **A `capture` failure** (for example missing Chromium) fails that pass's instance with its output in the step job's log under `artifacts/jobs/<job_id>/`. It cannot park; the chain stops with it.

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
