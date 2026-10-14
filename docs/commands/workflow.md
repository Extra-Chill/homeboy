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
- **Every decision is a pure function.** `advance(definition, state, event)` returns the next state and its commands: submit a step, cancel a step, complete the instance. The daemon commits the new checkpoint, the new step jobs, and their submission keys in **one store transaction**, and only then starts the steps. A crash at any point either loses a whole decision or replays it against the same keys.
- **Keys are deterministic.** A step job's key is `<instance>/<step>/<iteration>/<attempt>`; a `foreach` item's is `<instance>/<step>/<iteration>/item-<n>/<attempt>`; a command gate's key is `<instance>/<step>/<iteration>/when-<n>` or `<instance>/~until/<iteration>/<n>`. A replayed submission converges on the existing job, so no step runs twice.
- **The instance id is deterministic too.** It is `wf-<definition-id>-<sha256(definition digest, inputs, --instance)[..12]>`. Re-running the same definition with the same inputs returns the existing instance; new inputs (or a new `--instance` name) start a new one.
- **The definition is pinned.** Its digest and body are stored in the instance at admission, so editing the file never changes a running instance.

Statuses:

| Thing | Terminal statuses |
|---|---|
| Step | `succeeded`, `failed`, `timed_out`, `cancelled`, `skipped`; a `cook` step reports its Cook's run state (`succeeded`, `candidate_recoverable`, `partial_recoverable`, `partial_failure`, `failed`, `cancelled`) |
| Instance | `succeeded`, `failed`, `cancelled`, `budget_exhausted` |

## The `homeboy/workflow/v1` format

```jsonc
{
  "schema": "homeboy/workflow/v1",
  "id": "string",                       // registry id: ~/.config/homeboy/workflows/<id>.json, or run any path; letters, digits, `_`, `-`, `.`
  "inputs":  { "<name>": { "required": true, "default": null } },
  "env":     { "public": ["NAME"], "secrets": ["SECRET_REF"] },   // the admitted environment; nothing else is inherited
  "budget":  { "max_step_runs": 20, "deadline": "12h" },          // hard stop → budget_exhausted
  "iterate": { "until": <gate>, "max_iterations": 4 },            // re-run the step graph until the gate passes
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

`notify` belongs to a later release. A definition that uses it is refused with a message that names the field.

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

A gate is a closed, deterministic predicate. There is no expression language.

| Gate | Passes when |
|---|---|
| `{"step":"id","outcome":["succeeded","candidate_recoverable"]}` | the step's latest outcome is in the set |
| `{"step":"id","output":"/json/pointer","equals":X}` (or `in`, `exists`, `lt`, `gt`) | the test holds on the step's latest output |
| `{"command":["homeboy","review","lint"]}` | the command exits 0. It runs as a step job, so it is logged and auditable |
| `{"all":[…]}`, `{"any":[…]}`, `{"not":…}` | the combination holds, evaluated left to right; a command gate runs only once the gates before it are decided |

A step's *latest* outcome and output carry into the next iteration until the step settles again. That is what lets a step gate on its own earlier result: with `"when": {"not": {"step": "capture", "outcome": ["succeeded"]}}`, `capture` runs once and later iterations keep its output. A step whose `when` is false keeps its carried result; without one it is `skipped`.

### Templating

String substitution only: `{{inputs.x}}`, `{{steps.<id>.output<pointer>}}`, `{{steps.<id>.run_id}}`, `{{iteration}}`, `{{instance}}`, and in a `foreach` step's work `{{item}}` / `{{item.<field>}}`. A string substitutes as itself; any other JSON value substitutes as its compact JSON.

### Validation

`workflow validate` and admission both check:

- The `needs` graph is acyclic and names real steps.
- Every template reference resolves to a declared input or an upstream step (a step reachable through `needs`).
- A `when` gate reads only upstream steps or the step itself.
- Every `cook` key is a real `agent-task cook` flag.
- Every environment name is declared once; a credential-named variable is a secret, never a public value.
- Durations (`timeout`, `retry.backoff`, `budget.deadline`) parse.

### Failure, retry, budgets

- **A failed step fails the instance.** `failed`, `timed_out`, `cancelled`, and `partial_failure` are failures. No new step starts after one unless it is `always`; when everything in flight has settled the instance is `failed`. It never parks.
- **`retry`** re-submits the step as attempt `n+1` when its outcome is in `on` (default `failed` and `timed_out`), up to `max` retries. `backoff` delays the retry through the step job's `not_before` wait.
- **`timeout`** cancels the step's job when it has run that long; the outcome is `timed_out`.
- **`budget.max_step_runs`** counts every step job, including command gates. Reaching it cancels whatever is running and ends the instance `budget_exhausted`. So does passing `budget.deadline`, and so does an `until` gate that has not passed after `max_iterations`.

### Environment

A step runs with `env_clear()` plus its admitted environment:

- `env.public` names are captured from the environment that runs `workflow run`. A declared name that is unset there is refused.
- `env.secrets` are agent-task secret references, resolved when each step spawns. The daemon's own environment is never a source.
- `homeboy` and `cook` steps also get the runtime environment a detached Cook gets (provider process variables and Homeboy's `HOMEBOY_*` configuration).

### Cadence (`every`)

`workflow run` on a definition with `every` registers it with the daemon instead of admitting one instance. The daemon then admits one instance per due slot.

- **Registration.** The registration is the admitted request: the definition, the inputs, the captured environment, and the resolved programs. It is keyed by the workflow id; running `workflow run` again replaces it. An unchanged cadence keeps its slot grid; a changed `interval` or `at` starts a new grid. `--wait` and `--instance` do not apply. `--on RUNNER` registers it on that runner's daemon.
- **Slots.** Slots are `anchor + k × interval`. With `at` (a UTC time of day, `HH:MM` or `HH:MMZ`) the anchor is that time, so `{"interval": "24h", "at": "03:00Z"}` fires daily at 03:00 UTC; without it the first slot is the moment of registration. `interval` is at least `1m`.
- **Admission.** The admitting daemon's orchestration tick (60 seconds by default) admits each due slot. Slots missed while the daemon was stopped coalesce into the latest one.
- **One instance per slot.** A slot's instance is named `every-<slot time>`, so its instance id is deterministic. A tick that re-fires a slot after a crash converges on the instance it already admitted, even across a daemon restart.
- **Overlap skip.** A slot that comes due while the previous instance of the same cadence is not terminal is skipped and counted. `skip` is the only `overlap` policy.
- **Where it lives.** Registrations are stored in `workflow-cadences.sqlite` in the daemon state directory, so they survive daemon restarts and build rotations. A daemon that is not running admits nothing: run the always-on cadences on a resident daemon (`homeboy daemon start`, or a runner's service daemon).
- **Observe and stop.** `workflow list --registered` shows every cadence with its next due time, fire and skip counts, last instance, and last admission error. `workflow cancel <workflow-id>` stops a cadence; its instances are ordinary instances you can cancel separately.

**Built-in `homeboy.retention`.** Homeboy ships one workflow, `homeboy.retention`: every hour, one `homeboy` step runs `cleanup automatic-retention` with a 50-minute timeout. The daemon registers it when it starts, first due one hour later, unless an operator registered their own `homeboy.retention` or cancelled it; both are kept. `workflow run homeboy.retention` and `workflow validate homeboy.retention` resolve the built-in when no registry file has that id.

### Daemon restarts

An instance's checkpoint is the daemon's job store. `workflow status`, `list`, and `approve` go to the serving daemon.

When the daemon that owns an instance dies or is stopped, the instance resumes on its own. The next Homeboy command restarts the daemon in the same state directory, and the replacement resumes the instance from its checkpoint. That directory's owner lock admits one daemon, so the instance has one driver, and submission keys keep every step to one job. A step whose child was still running when the daemon died is protected until the child exits; its result is then read from the job ledger (and, for a `cook` step, from the Cook's run record), so no step runs twice. `homeboy daemon recover --yes` applies the same restart immediately.

## Commands

- **`workflow validate`** parses and validates a definition and prints its digest and step graph.
- **`workflow run`** admits an instance in the local daemon and prints its status; a definition with `every` is registered as a cadence instead. With `--wait` it follows the instance until it is terminal and exits non-zero unless it succeeded. `--on RUNNER` admits the same definition on that runner's daemon through `homeboy runner exec`, so the environment is admitted on the runner and an always-on runner can own the instance.
- **`workflow status`** is a pure projection of the job ledger: the instance status, each step's phase, outcome, attempt, key, and step job, each `foreach` item with its step job, Cook run id, and the live state of a running Cook, the gates, and every finished iteration. `--watch` polls until the instance is terminal.
- **`workflow cancel`** cancels the instance; the daemon cancels every step job it is running. Once the cancellation is durable the instance admits no more steps. Given a workflow id that is not an instance, it stops that cadence.
- **`workflow approve`** records an approval for a step that is awaiting one in the current iteration.
- **`workflow list`** lists instances in the daemon; `--registered` lists the definitions in `~/.config/homeboy/workflows/`, the built-ins, and every registered cadence.

## Examples

### A command gate that launches a Cook, then re-checks

`evaluate` writes `{"verdict": …}` to `$HOMEBOY_STEP_OUTPUT`. On `findings` the gate launches a Cook with its own `--verify` gate; the `until` gate re-runs the graph until the verdict is `accepted`.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "evaluate-and-fix",
  "inputs": { "repo": { "required": true } },
  "budget": { "max_step_runs": 8, "deadline": "4h" },
  "iterate": {
    "until": { "step": "evaluate", "output": "/verdict", "equals": "accepted" },
    "max_iterations": 3
  },
  "steps": [
    { "id": "evaluate", "exec": { "program": "./evaluate.sh", "args": ["{{iteration}}"] } },
    { "id": "fix", "needs": ["evaluate"],
      "when": { "step": "evaluate", "output": "/verdict", "equals": "findings" },
      "cook": { "repo": "{{inputs.repo}}", "prompt": "Address the evaluation findings (iteration {{iteration}}).",
                "verify": ["test -f FIXED"] } }
  ]
}
```

### Example A: proof loop (command → gate → Cook → re-check)

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "ssi-url-proof",
  "inputs": { "url": {"required": true}, "candidate_sha": {"required": true}, "workspace": {"required": true} },
  "env": { "public": ["SSI_BLOCKS_ENGINE_PATH", "HOMEBOY_WP_CODEBOX_BIN"] },
  "budget": { "max_step_runs": 12, "deadline": "10h" },
  "iterate": {
    "until": { "step": "evaluate", "output": "/verdict", "equals": "accepted" },
    "max_iterations": 3
  },
  "steps": [
    { "id": "capture", "timeout": "15m",
      "when": { "not": { "step": "capture", "outcome": ["succeeded"] } },
      "homeboy": ["runner", "exec", "lab", "--cwd", "{{inputs.workspace}}", "--",
                  "node", "tools/url-loop-controller.mjs", "capture", "--url", "{{inputs.url}}"] },
    { "id": "evaluate", "needs": ["capture"], "timeout": "90m",
      "homeboy": ["runner", "exec", "lab", "--", "node", "tools/url-loop-controller.mjs", "evaluate",
                  "--fixture", "{{steps.capture.output/fixture_root}}", "--candidate-sha", "{{inputs.candidate_sha}}"] },
    { "id": "fix", "needs": ["evaluate"],
      "when": { "step": "evaluate", "output": "/verdict", "equals": "findings" },
      "retry": { "max": 1, "on": ["failed"] },
      "cook": { "repo": "static-site-importer", "placement": "lab",
                "prompt": "Fix the import findings in {{steps.evaluate.output/report_path}} for {{inputs.url}}.",
                "verify": ["composer test", "node tools/url-loop-controller.mjs evaluate --fixture {{steps.capture.output/fixture_root}}"] } }
  ]
}
```

- **Iteration 1:** `capture` → `evaluate`. On `findings` the gate launches `fix`, a Cook with its own `--verify` gates.
- **The `until` gate** then decides whether to stop. Iteration 2 re-runs `evaluate` (`capture` keeps its carried output because its `when` is now false) and `fix`, up to 3 iterations or 12 step runs.
- **A `capture` failure** (for example missing Chromium) fails the instance with its output in the step job's log under `artifacts/jobs/<job_id>/`. It cannot park.
- **A new candidate SHA** runs `homeboy workflow run ssi-url-proof --input url=… --input candidate_sha=<new> --input workspace=…`. New inputs are a new instance; the same inputs are a no-op.

### Example B: nightly repeating task (cadence + gate → Cook → gate)

Requires `notify`, which a later release implements; this release refuses the definition. Without `notify` it registers as a daily cadence at 07:00 UTC.

```json
{
  "schema": "homeboy/workflow/v1",
  "id": "homeboy-nightly-red-main",
  "every": { "interval": "24h", "at": "07:00Z", "overlap": "skip" },
  "env": { "secrets": ["github-token"] },
  "budget": { "max_step_runs": 6, "deadline": "4h" },
  "notify": { "on": ["failed", "budget_exhausted", "succeeded"] },
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
                "base": "{{steps.prepare.output/base}}", "title": "{{item.title}}",
                "commit-message": "{{item.commit_message}}",
                "verify": ["test -f DONE"], "placement": "lab" } }
  ]
}
```
